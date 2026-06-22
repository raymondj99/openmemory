//! Source adapters: turn external data into [`RememberRequest`] streams.
//!
//! The contract is deliberately thin: an adapter understands one source
//! shape (meeting notes, chat exports, transcripts, ...) and yields
//! batches of requests; the engine stays a dumb, fast, ordered ingestion
//! lane. Chunking, entity extraction, and relation heuristics all live
//! on this side of the seam.
//!
//! Two reference adapters ship here:
//!
//! - [`MarkdownNotesAdapter`] — a directory of meeting-note Markdown
//!   files. One entity per file (H1 title or file stem), one observation
//!   per `##` section, `Attendees:` lines become `has_participant`
//!   relations to `Person` entities.
//! - [`ChatJsonlAdapter`] — a JSONL chat export (one message per line:
//!   `{"channel", "user", "ts", "text"}`). One entity per channel, one
//!   observation per message stamped with the message timestamp,
//!   `has_participant` relations to the sending user.
//!
//! Audio and similar media are expected to pass through an external
//! transcription step first and enter as one of the text shapes above.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use openmemory_graph::{
    EntityType, MemoryError, MemoryResult, ObservationInput, RelationInput, RememberRequest,
};
use serde::Deserialize;

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

/// Adapter over a directory of Markdown meeting notes. Processes one
/// file per [`SourceAdapter::next_batch`] call.
pub struct MarkdownNotesAdapter {
    files: Vec<PathBuf>,
    cursor: usize,
}

impl MarkdownNotesAdapter {
    /// Collect every `.md` / `.markdown` file under `dir` (recursive,
    /// sorted for determinism).
    pub fn open(dir: &Path) -> MemoryResult<Self> {
        let mut files = Vec::new();
        collect_markdown(dir, &mut files)?;
        files.sort();
        Ok(Self { files, cursor: 0 })
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

impl SourceAdapter for MarkdownNotesAdapter {
    fn name(&self) -> &'static str {
        "markdown-notes"
    }

    fn next_batch(&mut self) -> MemoryResult<Vec<RememberRequest>> {
        let Some(path) = self.files.get(self.cursor) else {
            return Ok(Vec::new());
        };
        self.cursor += 1;
        let text = std::fs::read_to_string(path)?;
        match parse_markdown_note(path, &text) {
            Some(request) => Ok(vec![request]),
            // Empty file: skip to the next one rather than signalling
            // exhaustion.
            None => self.next_batch(),
        }
    }
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
        sections
            .last_mut()
            .expect("seeded with one section")
            .1
            .push(line);
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
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
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

/// Adapter over a Slack-style JSONL chat export: one JSON object per
/// line with `channel`, `user`, optional `ts`, and `text` fields.
/// Yields up to `batch_size` requests per [`SourceAdapter::next_batch`].
pub struct ChatJsonlAdapter {
    reader: std::io::Lines<std::io::BufReader<std::fs::File>>,
    batch_size: usize,
    line_no: u64,
}

impl ChatJsonlAdapter {
    pub fn open(path: &Path) -> MemoryResult<Self> {
        let file = std::fs::File::open(path)?;
        Ok(Self {
            reader: std::io::BufReader::new(file).lines(),
            batch_size: 256,
            line_no: 0,
        })
    }
}

impl SourceAdapter for ChatJsonlAdapter {
    fn name(&self) -> &'static str {
        "chat-jsonl"
    }

    fn next_batch(&mut self) -> MemoryResult<Vec<RememberRequest>> {
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

            let mut obs = ObservationInput::new(msg.text)
                .with_source(format!("chat:{}", msg.user))
                .with_source_kind("chat-message");
            obs.valid_from = msg.ts;

            batch.push(
                RememberRequest::new(format!("#{}", msg.channel), EntityType::Concept)
                    .with_observations(vec![obs])
                    .with_relations(vec![RelationInput::new(
                        "has_participant",
                        msg.user,
                        EntityType::Person,
                    )])
                    .with_source("chat-jsonl"),
            );
        }
        Ok(batch)
    }
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
