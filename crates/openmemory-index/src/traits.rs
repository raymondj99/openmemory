use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::IndexResult;

/// A piece of text to index. Carries the embedding vector when one is
/// available and optional caller-supplied fielded metadata (title,
/// short summary, concept tags, source files, source-kind discriminator,
/// entity name and entity type). The keyword backend uses the fielded
/// shape for BM25 weighting; the vector backend looks at `vector` and
/// `text` and ignores the rest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub uri: String,
    pub text: String,
    #[serde(default)]
    pub chunk_index: u32,
    #[serde(default)]
    pub vector: Vec<f32>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub concepts: Vec<String>,
    #[serde(default)]
    pub source_files: Vec<String>,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(default)]
    pub entity_name: Option<String>,
}

impl IndexEntry {
    #[must_use]
    pub fn new(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            text: text.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_vector(mut self, vector: Vec<f32>) -> Self {
        self.vector = vector;
        self
    }

    #[must_use]
    pub fn with_chunk_index(mut self, chunk_index: u32) -> Self {
        self.chunk_index = chunk_index;
        self
    }

    #[must_use]
    pub fn with_title(mut self, title: Option<String>) -> Self {
        self.title = title;
        self
    }

    #[must_use]
    pub fn with_summary(mut self, summary: Option<String>) -> Self {
        self.summary = summary;
        self
    }

    #[must_use]
    pub fn with_concepts(mut self, concepts: Vec<String>) -> Self {
        self.concepts = concepts;
        self
    }

    #[must_use]
    pub fn with_source_files(mut self, source_files: Vec<String>) -> Self {
        self.source_files = source_files;
        self
    }

    #[must_use]
    pub fn with_source_kind(mut self, source_kind: Option<String>) -> Self {
        self.source_kind = source_kind;
        self
    }

    #[must_use]
    pub fn with_entity_type(mut self, entity_type: Option<String>) -> Self {
        self.entity_type = entity_type;
        self
    }

    #[must_use]
    pub fn with_entity_name(mut self, entity_name: Option<String>) -> Self {
        self.entity_name = entity_name;
        self
    }
}

/// A single result returned by a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub uri: String,
    pub text: String,
    pub chunk_index: u32,
    pub score: f32,
}

/// Search execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Hybrid,
    VectorOnly,
    KeywordOnly,
}

/// A vector similarity search store. The minimal mutable surface every vector
/// backend (flat, HNSW) implements.
pub trait VectorStore: Send + Sync {
    /// Insert entries into the store.
    fn insert(&self, entries: &[IndexEntry]) -> IndexResult<()>;

    /// Top-k vector similarity search.
    fn search(&self, query_vector: &[f32], top_k: usize) -> IndexResult<Vec<SearchResult>>;

    /// Delete every entry whose `uri` matches. Returns the number removed.
    fn delete_by_uri(&self, uri: &str) -> IndexResult<u64>;

    /// Total number of entries stored.
    fn count(&self) -> IndexResult<u64>;
}

/// Vector index that can be persisted and bulk-exported.
pub trait VectorIndex: VectorStore {
    /// Persist the index to `path`.
    fn save(&self, path: &Path) -> IndexResult<()>;

    /// Export every entry, including its vector. Used by migration and rebuild.
    fn export_all(&self) -> IndexResult<Vec<ExportEntry>>;
}

/// One exported entry. Same shape as [`IndexEntry`], kept as a separate type
/// so the export/import API can evolve independently from insert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportEntry {
    pub uri: String,
    pub text: String,
    pub chunk_index: u32,
    pub vector: Vec<f32>,
}

impl From<ExportEntry> for IndexEntry {
    fn from(e: ExportEntry) -> Self {
        Self {
            uri: e.uri,
            text: e.text,
            chunk_index: e.chunk_index,
            vector: e.vector,
            ..Self::default()
        }
    }
}

impl From<IndexEntry> for ExportEntry {
    fn from(e: IndexEntry) -> Self {
        Self {
            uri: e.uri,
            text: e.text,
            chunk_index: e.chunk_index,
            vector: e.vector,
        }
    }
}

/// A keyword (BM25 / FTS5) search store.
pub trait FullTextStore: Send + Sync {
    /// Index entries for keyword search. The vector field is ignored.
    fn insert(&self, entries: &[IndexEntry]) -> IndexResult<()>;

    /// Top-k keyword search.
    fn search(&self, query: &str, top_k: usize) -> IndexResult<Vec<SearchResult>>;

    /// Delete every entry whose `uri` matches. Returns the number removed.
    fn delete_by_uri(&self, uri: &str) -> IndexResult<u64>;

    /// Total number of keyword rows stored.
    fn count(&self) -> IndexResult<u64>;

    /// Persist any buffered state. Default is a no-op for stores that commit
    /// on every mutation (FTS5).
    fn flush(&self) -> IndexResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_entry_builder() {
        let e = IndexEntry::new("u", "t")
            .with_vector(vec![1.0, 2.0])
            .with_chunk_index(7);
        assert_eq!(e.uri, "u");
        assert_eq!(e.text, "t");
        assert_eq!(e.chunk_index, 7);
        assert_eq!(e.vector, vec![1.0, 2.0]);
    }

    #[test]
    fn search_mode_default_is_hybrid() {
        assert_eq!(SearchMode::default(), SearchMode::Hybrid);
    }

    #[test]
    fn search_mode_serde_round_trip() {
        for mode in [
            SearchMode::Hybrid,
            SearchMode::VectorOnly,
            SearchMode::KeywordOnly,
        ] {
            let s = serde_json::to_string(&mode).unwrap();
            let back: SearchMode = serde_json::from_str(&s).unwrap();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn search_mode_serde_snake_case() {
        let s = serde_json::to_string(&SearchMode::VectorOnly).unwrap();
        assert_eq!(s, "\"vector_only\"");
    }

    #[test]
    fn export_entry_round_trip_to_index_entry() {
        let original = IndexEntry::new("uri", "text")
            .with_vector(vec![0.1, 0.2])
            .with_chunk_index(3);
        let export: ExportEntry = original.clone().into();
        let back: IndexEntry = export.into();
        assert_eq!(back, original);
    }

    #[test]
    fn full_text_store_flush_default_is_no_op() {
        struct FakeFts;
        impl FullTextStore for FakeFts {
            fn insert(&self, _entries: &[IndexEntry]) -> IndexResult<()> {
                Ok(())
            }
            fn search(&self, _q: &str, _k: usize) -> IndexResult<Vec<SearchResult>> {
                Ok(vec![])
            }
            fn delete_by_uri(&self, _u: &str) -> IndexResult<u64> {
                Ok(0)
            }
            fn count(&self) -> IndexResult<u64> {
                Ok(0)
            }
        }
        FakeFts.flush().unwrap();
    }

    #[test]
    fn entry_serde_round_trip() {
        let e = IndexEntry::new("u", "t").with_vector(vec![1.0, 2.0]);
        let s = serde_json::to_string(&e).unwrap();
        let back: IndexEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
    }
}
