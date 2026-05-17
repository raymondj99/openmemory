//! Loader for the CodingMem benchmark fixture tree.
//!
//! Shares the JSONL-per-stream shape used by [`crate::longmem_s`]; the
//! adapter is intentionally just a wrapper that points the shared
//! [`crate::io::read_jsonl`] loader at a different directory and names
//! the resulting dataset `coding-mem`. The harness keeps the two
//! adapters separate so a CI ablation can score each independently and
//! so future work can swap in a benchmark-specific parser without
//! disturbing the LongMemEval-S path.

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
pub struct CodingMemDataset {
    inner: InMemoryDataset,
}

impl CodingMemDataset {
    /// Load the dataset from disk. Returns an error when any of the
    /// three JSONL files is missing or malformed, or when any judgment
    /// row's `relevance` falls outside `0..=Judgment::MAX_RELEVANCE`.
    pub fn from_path(path: &Path) -> Result<Self> {
        let corpus = read_jsonl::<Document>(&path.join("corpus.jsonl"))
            .context("reading coding-mem corpus.jsonl")?;
        let queries = read_jsonl::<Query>(&path.join("queries.jsonl"))
            .context("reading coding-mem queries.jsonl")?;
        let raw_judgments = read_jsonl::<JudgmentLine>(&path.join("judgments.jsonl"))
            .context("reading coding-mem judgments.jsonl")?;

        let mut judgments: BTreeMap<String, Vec<Judgment>> = BTreeMap::new();
        for j in raw_judgments {
            let judgment = Judgment {
                uri: j.uri,
                relevance: j.relevance,
            };
            judgment
                .validate()
                .map_err(|e| anyhow!("coding-mem judgments.jsonl: {e}"))?;
            judgments.entry(j.query_id).or_default().push(judgment);
        }

        Ok(Self {
            inner: InMemoryDataset {
                name: "coding-mem".into(),
                documents: corpus,
                queries,
                judgments,
            },
        })
    }

    /// Borrow the underlying in-memory dataset.
    pub fn inner(&self) -> &InMemoryDataset {
        &self.inner
    }
}

impl Dataset for CodingMemDataset {
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
    fn coding_mem_loads_from_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        write_lines(
            &dir.path().join("corpus.jsonl"),
            &[r#"{"uri":"doc://x","text":"fn foo()"}"#],
        );
        write_lines(
            &dir.path().join("queries.jsonl"),
            &[r#"{"id":"q1","text":"foo"}"#],
        );
        write_lines(
            &dir.path().join("judgments.jsonl"),
            &[r#"{"query_id":"q1","uri":"doc://x","relevance":1}"#],
        );
        let ds = CodingMemDataset::from_path(dir.path()).unwrap();
        assert_eq!(ds.name(), "coding-mem");
        assert_eq!(ds.corpus().count(), 1);
    }

    #[test]
    fn coding_mem_rejects_out_of_range_relevance() {
        let dir = tempfile::tempdir().unwrap();
        write_lines(
            &dir.path().join("corpus.jsonl"),
            &[r#"{"uri":"doc://x","text":"fn foo()"}"#],
        );
        write_lines(
            &dir.path().join("queries.jsonl"),
            &[r#"{"id":"q1","text":"foo"}"#],
        );
        write_lines(
            &dir.path().join("judgments.jsonl"),
            &[r#"{"query_id":"q1","uri":"doc://x","relevance":7}"#],
        );
        let err = CodingMemDataset::from_path(dir.path()).unwrap_err();
        assert!(err.to_string().contains("0..=3"), "got {err:#}");
    }
}
