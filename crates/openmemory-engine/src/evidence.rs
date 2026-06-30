//! Evidence records for source ingestion.
//!
//! Evidence is intentionally pre-semantic: adapters preserve what was
//! seen, where it came from, and the hash of the extraction-relevant
//! payload. Extractors can later turn these records into memory
//! candidates; only validated candidates should become graph writes.

use openmemory_graph::MemoryResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Raw source evidence from an adapter.
///
/// This is OpenMemory's local equivalent of the "episode" layer used
/// by temporal agent-memory systems: a non-lossy source unit that can be
/// searched, re-extracted, and audited before semantic memories are
/// committed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    /// Stable source-unit identifier.
    pub uri: String,
    /// Source family tag, e.g. `file:markdown` or `chat:jsonl`.
    pub source_type: String,
    /// Source-native stable ID, when available.
    pub external_id: Option<String>,
    /// Parent file, conversation, thread, or transcript URI.
    pub parent_uri: Option<String>,
    /// Zero-based index within the parent.
    pub chunk_index: u32,
    /// Canonical UTF-8 text for indexing and extraction.
    pub text: String,
    /// Source-native title or heading, not a graph entity decision.
    pub title: Option<String>,
    /// Source-native author/speaker/sender, when unambiguous.
    pub author: Option<String>,
    /// Source observation timestamp, when known.
    pub observed_at: Option<i64>,
    /// Source-native validity start, when known.
    pub valid_from: Option<i64>,
    /// Source-native validity end, when known.
    pub valid_until: Option<i64>,
    /// Local source paths or vendor URLs for provenance.
    pub source_files: Vec<String>,
    /// Structured source metadata that should not be flattened into text.
    pub metadata_json: Value,
    /// Adapter implementation version that participates in the hash.
    pub adapter_version: String,
    /// Chunking strategy version that participates in the hash.
    pub chunking_version: String,
    /// BLAKE3 hash over canonical text plus extraction-relevant metadata.
    pub content_hash: [u8; 32],
}

impl EvidenceRecord {
    /// Build a minimal evidence record and compute its content hash.
    pub fn new(
        uri: impl Into<String>,
        source_type: impl Into<String>,
        text: impl Into<String>,
        adapter_version: impl Into<String>,
        chunking_version: impl Into<String>,
    ) -> MemoryResult<Self> {
        let mut record = Self {
            uri: uri.into(),
            source_type: source_type.into(),
            external_id: None,
            parent_uri: None,
            chunk_index: 0,
            text: text.into(),
            title: None,
            author: None,
            observed_at: None,
            valid_from: None,
            valid_until: None,
            source_files: Vec::new(),
            metadata_json: Value::Object(serde_json::Map::new()),
            adapter_version: adapter_version.into(),
            chunking_version: chunking_version.into(),
            content_hash: [0; 32],
        };
        record.refresh_content_hash()?;
        Ok(record)
    }

    /// Recompute the content hash after changing extraction-relevant
    /// fields.
    pub fn refresh_content_hash(&mut self) -> MemoryResult<()> {
        self.content_hash = self.compute_content_hash()?;
        Ok(())
    }

    /// Compute the BLAKE3 content hash without mutating the record.
    pub fn compute_content_hash(&self) -> MemoryResult<[u8; 32]> {
        let metadata_json = canonical_json_value(&self.metadata_json);
        let payload = EvidenceHashPayload {
            schema_version: 1,
            source_type: &self.source_type,
            external_id: &self.external_id,
            parent_uri: &self.parent_uri,
            chunk_index: self.chunk_index,
            text: &self.text,
            title: &self.title,
            author: &self.author,
            observed_at: self.observed_at,
            valid_from: self.valid_from,
            valid_until: self.valid_until,
            metadata_json: &metadata_json,
            adapter_version: &self.adapter_version,
            chunking_version: &self.chunking_version,
        };
        let bytes = serde_json::to_vec(&payload)?;
        Ok(*blake3::hash(&bytes).as_bytes())
    }
}

#[derive(Serialize)]
struct EvidenceHashPayload<'a> {
    schema_version: u8,
    source_type: &'a str,
    external_id: &'a Option<String>,
    parent_uri: &'a Option<String>,
    chunk_index: u32,
    text: &'a str,
    title: &'a Option<String>,
    author: &'a Option<String>,
    observed_at: Option<i64>,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    metadata_json: &'a Value,
    adapter_version: &'a str,
    chunking_version: &'a str,
}

fn canonical_json_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_json_value).collect()),
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort_unstable();

            let mut sorted = serde_json::Map::new();
            for key in keys {
                if let Some(value) = map.get(key) {
                    sorted.insert(key.clone(), canonical_json_value(value));
                }
            }
            Value::Object(sorted)
        }
        value => value.clone(),
    }
}

/// Optional cursor for streaming evidence sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceCheckpoint {
    pub key: String,
    pub value: String,
}

/// Bounded batch of evidence records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceBatch {
    pub records: Vec<EvidenceRecord>,
    pub checkpoint: Option<EvidenceCheckpoint>,
}

impl EvidenceBatch {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            records: Vec::new(),
            checkpoint: None,
        }
    }

    #[must_use]
    pub fn one(record: EvidenceRecord) -> Self {
        Self {
            records: vec![record],
            checkpoint: None,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// Evidence upsert outcome. The first implementation slice exposes
/// the type before wiring a durable ledger behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceOutcome {
    Inserted,
    Updated,
    Unchanged,
    Deleted,
    Skipped(EvidenceSkipReason),
}

/// Structured reason an adapter skipped a source unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceSkipReason {
    TooLarge,
    EmptyContent,
    UnreadableUtf8,
    UnsupportedFormat,
    IgnoredPath,
    MalformedRecord,
}

/// A source of raw evidence records. Implementations are pull-based:
/// the runner keeps calling [`Self::next_batch`] until it returns an
/// empty batch.
pub trait EvidenceAdapter {
    /// Stable adapter name, used in logs and reports.
    fn name(&self) -> &'static str;
    /// Produce the next evidence batch. An empty batch means exhausted.
    fn next_batch(&mut self) -> MemoryResult<EvidenceBatch>;
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map, Value};

    use super::EvidenceRecord;

    #[test]
    fn evidence_hash_ignores_metadata_object_insertion_order() {
        let mut first_metadata = Map::new();
        first_metadata.insert("channel".to_string(), json!("eng-infra"));
        first_metadata.insert("line".to_string(), json!(1));
        first_metadata.insert(
            "nested".to_string(),
            json!({
                "b": ["second", {"z": true, "a": false}],
                "a": "first",
            }),
        );

        let mut second_metadata = Map::new();
        second_metadata.insert(
            "nested".to_string(),
            json!({
                "a": "first",
                "b": ["second", {"a": false, "z": true}],
            }),
        );
        second_metadata.insert("line".to_string(), json!(1));
        second_metadata.insert("channel".to_string(), json!("eng-infra"));

        let mut first = EvidenceRecord::new(
            "chat-jsonl:///tmp/export.jsonl#line=1",
            "chat:jsonl",
            "same text",
            "1",
            "row-v1",
        )
        .unwrap();
        first.metadata_json = Value::Object(first_metadata);
        first.refresh_content_hash().unwrap();

        let mut second = EvidenceRecord::new(
            "chat-jsonl:///tmp/export.jsonl#line=1",
            "chat:jsonl",
            "same text",
            "1",
            "row-v1",
        )
        .unwrap();
        second.metadata_json = Value::Object(second_metadata);
        second.refresh_content_hash().unwrap();

        assert_eq!(first.content_hash, second.content_hash);
    }
}
