//! Loader for the LongMemEval-S benchmark fixture tree.
//!
//! The adapter expects three files under `<dataset_path>/`:
//!
//! - `corpus.jsonl`: one [`Document`] per line.
//! - `queries.jsonl`: one [`Query`] per line.
//! - `judgments.jsonl`: one entry per line of the shape
//!   `{ "query_id": "...", "uri": "...", "relevance": N }`.
//!
//! The harness compiles without these files present. [`LongMemSDataset`]
//! holds the materialised dataset in memory; the on-disk loader is a
//! thin wrapper that just deserialises the three streams via the shared
//! [`crate::io::read_jsonl`] helper. Real corpora that do not ship in
//! this exact shape can be reshaped offline; the adapter intentionally
//! does not try to parse the upstream layout verbatim because that
//! layout changes across dataset revisions.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::dataset::{Dataset, Document, InMemoryDataset, Judgment, Query};
use crate::io::read_jsonl;

#[derive(Debug, Clone, Deserialize)]
struct JudgmentLine {
    query_id: String,
    uri: String,
    relevance: u8,
}

/// Adapter handle. Wraps an [`InMemoryDataset`]; the `Dataset` impl
/// delegates straight through.
#[derive(Debug)]
pub struct LongMemSDataset {
    inner: InMemoryDataset,
}

impl LongMemSDataset {
    /// Load the dataset from disk. Returns an error when any of the
    /// three JSONL files is missing or malformed, or when any judgment
    /// row's `relevance` falls outside `0..=Judgment::MAX_RELEVANCE`.
    pub fn from_path(path: &Path) -> Result<Self> {
        let corpus = read_jsonl::<Document>(&path.join("corpus.jsonl"))
            .context("reading longmem-s corpus.jsonl")?;
        let queries = read_jsonl::<Query>(&path.join("queries.jsonl"))
            .context("reading longmem-s queries.jsonl")?;
        let raw_judgments = read_jsonl::<JudgmentLine>(&path.join("judgments.jsonl"))
            .context("reading longmem-s judgments.jsonl")?;

        let mut judgments: BTreeMap<String, Vec<Judgment>> = BTreeMap::new();
        for j in raw_judgments {
            let judgment = Judgment {
                uri: j.uri,
                relevance: j.relevance,
            };
            judgment
                .validate()
                .map_err(|e| anyhow!("longmem-s judgments.jsonl: {e}"))?;
            judgments.entry(j.query_id).or_default().push(judgment);
        }

        Ok(Self {
            inner: InMemoryDataset {
                name: "longmem-s".into(),
                documents: corpus,
                queries,
                judgments,
            },
        })
    }

    /// Borrow the underlying in-memory dataset; useful when a caller
    /// wants to merge two adapters into a single ablation run.
    pub fn inner(&self) -> &InMemoryDataset {
        &self.inner
    }
}

impl Dataset for LongMemSDataset {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn corpus(&self) -> Box<dyn Iterator<Item = Document> + '_> {
        self.inner.corpus()
    }
    fn queries(&self) -> Box<dyn Iterator<Item = Query> + '_> {
        self.inner.queries()
    }
    fn judgments_for(&self, query_id: &str) -> Vec<Judgment> {
        self.inner.judgments_for(query_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn write_lines(path: &Path, lines: &[&str]) {
        let mut f = File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    #[test]
    fn longmem_s_loads_from_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        write_lines(
            &dir.path().join("corpus.jsonl"),
            &[r#"{"uri":"doc://a","text":"alpha"}"#],
        );
        write_lines(
            &dir.path().join("queries.jsonl"),
            &[r#"{"id":"q1","text":"alpha"}"#],
        );
        write_lines(
            &dir.path().join("judgments.jsonl"),
            &[r#"{"query_id":"q1","uri":"doc://a","relevance":1}"#],
        );
        let ds = LongMemSDataset::from_path(dir.path()).unwrap();
        assert_eq!(ds.name(), "longmem-s");
        assert_eq!(ds.corpus().count(), 1);
        assert_eq!(ds.queries().count(), 1);
        assert_eq!(ds.judgments_for("q1").len(), 1);
    }

    #[test]
    fn longmem_s_missing_dir_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let err = LongMemSDataset::from_path(&dir.path().join("nope")).unwrap_err();
        assert!(err.to_string().contains("longmem-s corpus"));
    }

    #[test]
    fn longmem_s_rejects_out_of_range_relevance() {
        let dir = tempfile::tempdir().unwrap();
        write_lines(
            &dir.path().join("corpus.jsonl"),
            &[r#"{"uri":"doc://a","text":"alpha"}"#],
        );
        write_lines(
            &dir.path().join("queries.jsonl"),
            &[r#"{"id":"q1","text":"alpha"}"#],
        );
        write_lines(
            &dir.path().join("judgments.jsonl"),
            &[r#"{"query_id":"q1","uri":"doc://a","relevance":4}"#],
        );
        let err = LongMemSDataset::from_path(dir.path()).unwrap_err();
        assert!(err.to_string().contains("0..=3"), "got {err:#}");
    }
}
