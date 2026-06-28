//! Source adapters for `openmemory ingest`.
//!
//! The compatibility [`SourceAdapter`] contract still turns external
//! data into [`RememberRequest`] streams. New adapter work should start
//! one step earlier with [`EvidenceAdapter`]: preserve raw source
//! evidence, stable URIs, source metadata, and BLAKE3 content hashes,
//! then let an extractor/validator decide what becomes graph memory.
//! The engine stays a dumb, fast, ordered ingestion lane.
//!
//! Two reference adapters ship here:
//!
//! - [`MarkdownEvidenceAdapter`] emits one evidence record per non-empty
//!   Markdown file. [`MarkdownNotesAdapter`] wraps it and preserves the
//!   historical deterministic graph write shape: one entity per file
//!   (H1 title or file stem), one observation per `##` section, and
//!   `Attendees:` lines as `has_participant` relations.
//! - [`ChatJsonlEvidenceAdapter`] emits one evidence record per non-empty
//!   JSONL row. [`ChatJsonlAdapter`] wraps it and preserves the
//!   historical deterministic graph write shape: one entity per channel,
//!   one timestamped observation per message, and a sender relation.
//!
//! Audio and similar media are expected to pass through an external
//! transcription step first and enter as one of the text shapes above.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use openmemory_graph::{
    EntityType, MemoryError, MemoryResult, ObservationInput, RelationInput, RememberRequest,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub use crate::evidence::{
    EvidenceAdapter, EvidenceBatch, EvidenceCheckpoint, EvidenceOutcome, EvidenceRecord,
    EvidenceSkipReason,
};

use crate::ContextEngine;

/// A source of [`RememberRequest`]s. Implementations are pull-based:
/// the runner keeps calling [`Self::next_batch`] until it returns an
/// empty vector.
pub trait SourceAdapter {
    /// Stable adapter name, used in logs and as the default source tag.
    fn name(&self) -> &'static str;
    /// Produce the next batch of requests. `Ok(vec![])` means exhausted.
    fn next_batch(&mut self) -> MemoryResult<Vec<RememberRequest>>;
}

/// Version for the evidence envelope emitted by this module's built-in
/// adapters. Bump when the canonical evidence fields or hash payload
/// semantics change.
pub const EVIDENCE_ADAPTER_VERSION: &str = "1";

/// Source type for Markdown evidence records.
pub const SOURCE_TYPE_MARKDOWN: &str = "file:markdown";
/// Source type for chat JSONL evidence records.
pub const SOURCE_TYPE_CHAT_JSONL: &str = "chat:jsonl";

const MARKDOWN_CHUNKING_VERSION: &str = "markdown-file-v1";
const CHAT_JSONL_CHUNKING_VERSION: &str = "chat-jsonl-row-v1";

/// Outcome of [`ingest_all`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestReport {
    /// Requests submitted to the engine.
    pub requests: u64,
    /// Batches the adapter produced.
    pub batches: u64,
}

/// Pump an adapter into the engine until it is exhausted, then quiesce
/// so every ingested request is durable when this returns.
pub fn ingest_all(
    engine: &ContextEngine,
    adapter: &mut dyn SourceAdapter,
) -> MemoryResult<IngestReport> {
    let mut report = IngestReport {
        requests: 0,
        batches: 0,
    };
    loop {
        let batch = adapter.next_batch()?;
        if batch.is_empty() {
            break;
        }
        report.batches += 1;
        for request in batch {
            engine.submit(request);
            report.requests += 1;
        }
    }
    engine.quiesce();
    Ok(report)
}

// ---------------------------------------------------------------------------
// Markdown meeting notes
// ---------------------------------------------------------------------------

/// Evidence adapter over a directory of Markdown notes. Processes one
/// file per [`EvidenceAdapter::next_batch`] call.
pub struct MarkdownEvidenceAdapter {
    files: Vec<PathBuf>,
    cursor: usize,
}

impl MarkdownEvidenceAdapter {
    /// Collect every `.md` / `.markdown` file under `dir` (recursive,
    /// sorted for determinism).
    pub fn open(dir: &Path) -> MemoryResult<Self> {
        let mut files = Vec::new();
        collect_markdown(dir, &mut files)?;
        files.sort();
        Ok(Self { files, cursor: 0 })
    }
}

/// Compatibility adapter over Markdown meeting notes. It delegates to
/// [`MarkdownEvidenceAdapter`] and then applies the historical
/// deterministic note parser so current ingest behavior does not
/// change while the evidence pipeline is introduced.
pub struct MarkdownNotesAdapter {
    inner: MarkdownEvidenceAdapter,
}

impl MarkdownNotesAdapter {
    /// Collect every `.md` / `.markdown` file under `dir` (recursive,
    /// sorted for determinism).
    pub fn open(dir: &Path) -> MemoryResult<Self> {
        Ok(Self {
            inner: MarkdownEvidenceAdapter::open(dir)?,
        })
    }
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> MemoryResult<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_markdown(&path, out)?;
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("md" | "markdown")
        ) {
            out.push(path);
        }
    }
    Ok(())
}

impl EvidenceAdapter for MarkdownEvidenceAdapter {
    fn name(&self) -> &'static str {
        "markdown-notes"
    }

    fn next_batch(&mut self) -> MemoryResult<EvidenceBatch> {
        while let Some(path) = self.files.get(self.cursor) {
            self.cursor += 1;
            let text = std::fs::read_to_string(path)?;
            if text.trim().is_empty() {
                continue;
            }
            let record = markdown_evidence_record(path, text)?;
            return Ok(EvidenceBatch::one(record));
        }
        Ok(EvidenceBatch::empty())
    }
}

impl SourceAdapter for MarkdownNotesAdapter {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn next_batch(&mut self) -> MemoryResult<Vec<RememberRequest>> {
        loop {
            let batch = self.inner.next_batch()?;
            if batch.is_empty() {
                return Ok(Vec::new());
            }
            let requests: Vec<RememberRequest> = batch
                .records
                .iter()
                .filter_map(markdown_record_to_request)
                .collect();
            if !requests.is_empty() {
                return Ok(requests);
            }
        }
    }
}

fn markdown_evidence_record(path: &Path, text: String) -> MemoryResult<EvidenceRecord> {
    let file_path = path.display().to_string();
    let uri = file_chunk_uri(path, 0);
    let mut record = EvidenceRecord::new(
        uri,
        SOURCE_TYPE_MARKDOWN,
        text,
        EVIDENCE_ADAPTER_VERSION,
        MARKDOWN_CHUNKING_VERSION,
    )?;
    record.parent_uri = Some(file_uri(path));
    record.chunk_index = 0;
    record.title = first_markdown_h1(&record.text);
    record.source_files = vec![file_path];
    record.metadata_json = json!({
        "adapter_version": EVIDENCE_ADAPTER_VERSION,
        "chunking_version": MARKDOWN_CHUNKING_VERSION,
        "file_stem": path.file_stem().and_then(|s| s.to_str()),
    });
    record.refresh_content_hash()?;
    Ok(record)
}

fn markdown_record_to_request(record: &EvidenceRecord) -> Option<RememberRequest> {
    let path = record
        .source_files
        .first()
        .map_or_else(|| Path::new("note"), Path::new);
    parse_markdown_note(path, &record.text)
}

fn first_markdown_h1(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
    })
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", stable_path(path).display())
}

fn file_chunk_uri(path: &Path, chunk_index: u32) -> String {
    format!("{}#chunk={chunk_index}", file_uri(path))
}

fn stable_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Parse one Markdown meeting note into a request.
///
/// - Entity: first `# ` heading, else the file stem. Type `Event`.
/// - Observations: one per `## ` section (title = heading); content
///   before the first `## ` becomes a "notes" observation.
/// - `Attendees:` line (anywhere): comma-separated names become
///   `has_participant` relations to `Person` entities.
fn parse_markdown_note(path: &Path, text: &str) -> Option<RememberRequest> {
    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("note")
        .to_string();
    let file_path = path.display().to_string();

    let mut entity_name: Option<String> = None;
    let mut attendees: Vec<String> = Vec::new();
    // (section title, body lines)
    let mut sections: Vec<(Option<String>, Vec<&str>)> = vec![(None, Vec::new())];

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(h1) = trimmed.strip_prefix("# ") {
            if entity_name.is_none() {
                entity_name = Some(h1.trim().to_string());
            }
            continue;
        }
        if let Some(h2) = trimmed.strip_prefix("## ") {
            sections.push((Some(h2.trim().to_string()), Vec::new()));
            continue;
        }
        if let Some(rest) = strip_prefix_ci(trimmed, "attendees:") {
            attendees.extend(
                rest.split(',')
                    .map(|a| a.trim().trim_start_matches('@').to_string())
                    .filter(|a| !a.is_empty()),
            );
            continue;
        }
        if let Some((_, lines)) = sections.last_mut() {
            lines.push(line);
        }
    }

    let entity = entity_name.unwrap_or(file_stem);
    let source = "markdown-notes".to_string();

    let mut observations = Vec::new();
    for (title, lines) in sections {
        let body = lines.join("\n").trim().to_string();
        if body.is_empty() {
            continue;
        }
        let mut obs = ObservationInput::new(body)
            .with_source_kind("meeting-note")
            .with_source_files(vec![file_path.clone()]);
        if let Some(title) = title {
            obs = obs.with_title(title);
        }
        observations.push(obs);
    }

    let relations: Vec<RelationInput> = attendees
        .iter()
        .map(|name| RelationInput::new("has_participant", name.clone(), EntityType::Person))
        .collect();

    if observations.is_empty() && relations.is_empty() {
        return None;
    }
    Some(
        RememberRequest::new(entity, EntityType::Event)
            .with_observations(observations)
            .with_relations(relations)
            .with_source(source),
    )
}

/// Case-insensitive prefix strip.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.get(..prefix.len())?.eq_ignore_ascii_case(prefix) {
        s.get(prefix.len()..)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Chat JSONL export
// ---------------------------------------------------------------------------

/// One line of a chat export.
#[derive(Debug, Deserialize)]
struct ChatMessage {
    channel: String,
    user: String,
    /// Unix seconds.
    #[serde(default)]
    ts: Option<i64>,
    text: String,
}

/// Evidence adapter over a Slack-style JSONL chat export: one JSON
/// object per line with `channel`, `user`, optional `ts`, and `text`
/// fields. Yields up to `batch_size` evidence records per
/// [`EvidenceAdapter::next_batch`].
pub struct ChatJsonlEvidenceAdapter {
    reader: std::io::Lines<std::io::BufReader<std::fs::File>>,
    batch_size: usize,
    line_no: u64,
    source_file: String,
    parent_uri: String,
}

impl ChatJsonlEvidenceAdapter {
    pub fn open(path: &Path) -> MemoryResult<Self> {
        let file = std::fs::File::open(path)?;
        Ok(Self {
            reader: std::io::BufReader::new(file).lines(),
            batch_size: 256,
            line_no: 0,
            source_file: path.display().to_string(),
            parent_uri: chat_parent_uri(path),
        })
    }
}

/// Compatibility adapter over chat JSONL. It delegates to
/// [`ChatJsonlEvidenceAdapter`] and then applies the historical
/// deterministic row parser so current ingest behavior does not change
/// while the evidence pipeline is introduced.
pub struct ChatJsonlAdapter {
    inner: ChatJsonlEvidenceAdapter,
}

impl ChatJsonlAdapter {
    pub fn open(path: &Path) -> MemoryResult<Self> {
        Ok(Self {
            inner: ChatJsonlEvidenceAdapter::open(path)?,
        })
    }
}

impl EvidenceAdapter for ChatJsonlEvidenceAdapter {
    fn name(&self) -> &'static str {
        "chat-jsonl"
    }

    fn next_batch(&mut self) -> MemoryResult<EvidenceBatch> {
        let mut batch = Vec::with_capacity(self.batch_size);
        while batch.len() < self.batch_size {
            let Some(line) = self.reader.next() else {
                break;
            };
            let line = line?;
            self.line_no += 1;
            if line.trim().is_empty() {
                continue;
            }
            let msg: ChatMessage = serde_json::from_str(&line).map_err(|e| {
                MemoryError::InvalidInput(format!("chat export line {}: {e}", self.line_no))
            })?;
            if msg.text.trim().is_empty() {
                continue;
            }
            batch.push(chat_evidence_record(
                &self.parent_uri,
                &self.source_file,
                self.line_no,
                msg,
            )?);
        }
        Ok(EvidenceBatch {
            records: batch,
            checkpoint: None,
        })
    }
}

impl SourceAdapter for ChatJsonlAdapter {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn next_batch(&mut self) -> MemoryResult<Vec<RememberRequest>> {
        loop {
            let batch = self.inner.next_batch()?;
            if batch.is_empty() {
                return Ok(Vec::new());
            }
            let mut requests = Vec::with_capacity(batch.records.len());
            for record in &batch.records {
                if let Some(request) = chat_record_to_request(record)? {
                    requests.push(request);
                }
            }
            if !requests.is_empty() {
                return Ok(requests);
            }
        }
    }
}

fn chat_parent_uri(path: &Path) -> String {
    format!("chat-jsonl://{}", stable_path(path).display())
}

fn chat_line_uri(parent_uri: &str, line_no: u64) -> String {
    format!("{parent_uri}#line={line_no}")
}

fn chat_evidence_record(
    parent_uri: &str,
    source_file: &str,
    line_no: u64,
    msg: ChatMessage,
) -> MemoryResult<EvidenceRecord> {
    let mut record = EvidenceRecord::new(
        chat_line_uri(parent_uri, line_no),
        SOURCE_TYPE_CHAT_JSONL,
        msg.text,
        EVIDENCE_ADAPTER_VERSION,
        CHAT_JSONL_CHUNKING_VERSION,
    )?;
    record.parent_uri = Some(parent_uri.to_string());
    record.chunk_index = u32::try_from(line_no.saturating_sub(1)).unwrap_or(u32::MAX);
    record.author = Some(msg.user.clone());
    record.observed_at = msg.ts;
    record.valid_from = msg.ts;
    record.source_files = vec![source_file.to_string()];
    record.metadata_json = json!({
        "adapter_version": EVIDENCE_ADAPTER_VERSION,
        "chunking_version": CHAT_JSONL_CHUNKING_VERSION,
        "channel": msg.channel,
        "user": msg.user,
        "line": line_no,
    });
    record.refresh_content_hash()?;
    Ok(record)
}

fn chat_record_to_request(record: &EvidenceRecord) -> MemoryResult<Option<RememberRequest>> {
    if record.text.trim().is_empty() {
        return Ok(None);
    }
    let channel = record
        .metadata_json
        .get("channel")
        .and_then(Value::as_str)
        .ok_or_else(|| MemoryError::InvalidInput(format!("{} missing channel", record.uri)))?;
    let user = record
        .metadata_json
        .get("user")
        .and_then(Value::as_str)
        .ok_or_else(|| MemoryError::InvalidInput(format!("{} missing user", record.uri)))?;

    let mut obs = ObservationInput::new(record.text.clone())
        .with_source(format!("chat:{user}"))
        .with_source_kind("chat-message");
    obs.valid_from = record.valid_from;

    Ok(Some(
        RememberRequest::new(format!("#{channel}"), EntityType::Concept)
            .with_observations(vec![obs])
            .with_relations(vec![RelationInput::new(
                "has_participant",
                user,
                EntityType::Person,
            )])
            .with_source("chat-jsonl"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EngineOptions;
    use openmemory_core::config::Config;
    use openmemory_graph::MemoryStore;
    use std::sync::Arc;

    fn test_store() -> Arc<MemoryStore> {
        let dir = tempfile::tempdir().expect("tempdir");
        Arc::new(MemoryStore::open(&Config::default(), dir.keep().as_path()).unwrap())
    }

    const NOTE: &str = "\
# Q3 Planning Sync

Attendees: Raymond, Dana Wu, @priya

Some intro context before sections.

## Decisions

We will ship the context engine behind a feature flag.

## Action items

- Raymond drafts the rollout plan
- Dana owns the load test
";

    #[test]
    fn markdown_note_parses_entity_sections_and_attendees() {
        let req = parse_markdown_note(Path::new("/tmp/q3-planning.md"), NOTE).unwrap();
        assert_eq!(req.name, "Q3 Planning Sync");
        assert_eq!(req.entity_type, EntityType::Event);

        assert_eq!(req.observations.len(), 3, "intro + two sections");
        assert!(req.observations[0].content.contains("intro context"));
        assert_eq!(req.observations[1].title.as_deref(), Some("Decisions"));
        assert!(req.observations[1].content.contains("feature flag"));
        assert_eq!(req.observations[2].title.as_deref(), Some("Action items"));

        let names: Vec<_> = req
            .relations
            .iter()
            .map(|r| r.target_name.as_str())
            .collect();
        assert_eq!(names, vec!["Raymond", "Dana Wu", "priya"]);
        assert!(req
            .relations
            .iter()
            .all(|r| r.relation_type == "has_participant" && r.target_type == EntityType::Person));
    }

    #[test]
    fn markdown_note_without_h1_uses_file_stem() {
        let req = parse_markdown_note(Path::new("/notes/standup-2026-06-11.md"), "just one line")
            .unwrap();
        assert_eq!(req.name, "standup-2026-06-11");
        assert_eq!(req.observations.len(), 1);
    }

    #[test]
    fn markdown_empty_file_is_skipped() {
        assert!(parse_markdown_note(Path::new("/x/empty.md"), "  \n\n").is_none());
    }

    #[test]
    fn markdown_attendee_prefix_strip_is_unicode_safe() {
        assert_eq!(
            strip_prefix_ci("Attendees: Raymond", "attendees:"),
            Some(" Raymond")
        );
        assert_eq!(strip_prefix_ci("🙂🙂🙂attendees: nope", "attendees:"), None);
    }

    #[test]
    fn markdown_evidence_adapter_emits_stable_file_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q3.md");
        std::fs::write(&path, NOTE).unwrap();

        let mut first = MarkdownEvidenceAdapter::open(dir.path()).unwrap();
        let record = first.next_batch().unwrap().records.pop().unwrap();
        assert_eq!(record.source_type, SOURCE_TYPE_MARKDOWN);
        assert_eq!(record.title.as_deref(), Some("Q3 Planning Sync"));
        assert_eq!(record.source_files, vec![path.display().to_string()]);
        assert!(record.uri.starts_with("file://"));
        assert!(record.uri.ends_with("#chunk=0"));
        assert_eq!(record.adapter_version, EVIDENCE_ADAPTER_VERSION);
        assert_eq!(record.chunking_version, MARKDOWN_CHUNKING_VERSION);

        let mut second = MarkdownEvidenceAdapter::open(dir.path()).unwrap();
        let same = second.next_batch().unwrap().records.pop().unwrap();
        assert_eq!(same.uri, record.uri);
        assert_eq!(same.content_hash, record.content_hash);
    }

    #[test]
    fn evidence_hash_tracks_adapter_and_chunking_versions() {
        let mut record = EvidenceRecord::new(
            "file:///tmp/note.md#chunk=0",
            SOURCE_TYPE_MARKDOWN,
            "hello",
            EVIDENCE_ADAPTER_VERSION,
            MARKDOWN_CHUNKING_VERSION,
        )
        .unwrap();
        let first = record.content_hash;

        record.chunking_version = "markdown-file-v2".into();
        record.refresh_content_hash().unwrap();
        assert_ne!(record.content_hash, first);

        let second = record.content_hash;
        record.adapter_version = "2".into();
        record.refresh_content_hash().unwrap();
        assert_ne!(record.content_hash, second);
    }

    #[test]
    fn chat_jsonl_parses_messages_with_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.jsonl");
        std::fs::write(
            &path,
            r#"{"channel":"eng-infra","user":"raymond","ts":1750000000,"text":"rolling out the context engine"}
{"channel":"eng-infra","user":"dana","text":"load test passed"}
"#,
        )
        .unwrap();

        let mut adapter = ChatJsonlAdapter::open(&path).unwrap();
        let batch = adapter.next_batch().unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].name, "#eng-infra");
        assert_eq!(batch[0].observations[0].valid_from, Some(1750000000));
        assert_eq!(batch[0].observations[0].source, "chat:raymond");
        assert_eq!(batch[1].relations[0].target_name, "dana");
        assert!(adapter.next_batch().unwrap().is_empty(), "exhausted");
    }

    #[test]
    fn chat_jsonl_evidence_uses_line_uris_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.jsonl");
        std::fs::write(
            &path,
            r#"{"channel":"eng-infra","user":"raymond","ts":1750000000,"text":"rolling out the context engine"}
{"channel":"eng-infra","user":"dana","text":"load test passed"}
"#,
        )
        .unwrap();

        let mut adapter = ChatJsonlEvidenceAdapter::open(&path).unwrap();
        let batch = adapter.next_batch().unwrap();
        assert_eq!(batch.records.len(), 2);
        let first = &batch.records[0];
        assert_eq!(first.source_type, SOURCE_TYPE_CHAT_JSONL);
        assert!(first.uri.starts_with("chat-jsonl://"));
        assert!(first.uri.ends_with("#line=1"));
        assert_eq!(
            first.parent_uri.as_deref(),
            Some(adapter.parent_uri.as_str())
        );
        assert_eq!(first.author.as_deref(), Some("raymond"));
        assert_eq!(first.valid_from, Some(1750000000));
        assert_eq!(first.source_files, vec![path.display().to_string()]);
        assert_eq!(
            first.metadata_json.get("channel").and_then(Value::as_str),
            Some("eng-infra")
        );
        assert_eq!(
            first.metadata_json.get("line").and_then(Value::as_u64),
            Some(1)
        );

        let mut again = ChatJsonlEvidenceAdapter::open(&path).unwrap();
        let same = again.next_batch().unwrap();
        assert_eq!(same.records[0].uri, first.uri);
        assert_eq!(same.records[0].content_hash, first.content_hash);
    }

    #[test]
    fn chat_jsonl_rejects_malformed_lines_with_line_number() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.jsonl");
        std::fs::write(&path, "{\"channel\":\"x\"\n").unwrap();
        let mut adapter = ChatJsonlAdapter::open(&path).unwrap();
        let err = adapter.next_batch().unwrap_err();
        assert!(err.to_string().contains("line 1"), "got: {err}");
    }

    #[test]
    fn ingest_all_lands_notes_in_the_store() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("q3.md"), NOTE).unwrap();
        std::fs::write(
            dir.path().join("retro.md"),
            "# Sprint Retro\n\n## Went well\n\nShipped the journal.\n",
        )
        .unwrap();

        let store = test_store();
        let engine = crate::ContextEngine::start(
            Arc::clone(&store),
            EngineOptions {
                shards: 4,
                ..EngineOptions::default()
            },
        )
        .unwrap();

        let mut adapter = MarkdownNotesAdapter::open(dir.path()).unwrap();
        let report = ingest_all(&engine, &mut adapter).unwrap();
        assert_eq!(report.requests, 2);

        let q3 = store.get_entity("Q3 Planning Sync").unwrap().unwrap();
        let obs = store.get_entity_observations(&q3.id).unwrap();
        assert_eq!(obs.len(), 3);
        let rels = store.get_entity_relations(&q3.id).unwrap();
        assert_eq!(rels.len(), 3, "three attendees");
        assert!(store.get_entity("Sprint Retro").unwrap().is_some());
        engine.shutdown();
    }
}
