//! Dataset shape every benchmark adapter has to expose.
//!
//! A [`Dataset`] is three streams: the corpus of indexable documents,
//! the queries to run against the corpus, and the relevance judgments
//! the harness scores each query's ranked output against. Adapters
//! materialise the three streams from whatever format the underlying
//! benchmark ships in.

use openmemory_index::SearchMode;
use serde::{Deserialize, Serialize};

/// A single indexable document. The `uri` is the stable identifier the
/// harness uses to score a query's ranked output against the dataset's
/// judgments. `entity_hint` is optional and lets adapters carry a
/// caller-chosen entity name through to the graph store; adapters that
/// only ingest free-text content may leave it `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub uri: String,
    pub text: String,
    #[serde(default)]
    pub entity_hint: Option<String>,
}

/// A single query plus its caller-supplied identifier. The id is matched
/// against the relevance judgments emitted by the same adapter. An
/// optional `mode_override` lets a dataset opt out of the harness-wide
/// search mode for a specific query (e.g. a keyword-only baseline mixed
/// with a hybrid mainline).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub mode_override: Option<SearchMode>,
}

/// One relevance judgment. Graded relevance is supported (0..=3) so the
/// NDCG computation can credit partial matches; binary judgments use
/// `relevance = 1` for relevant and 0 for irrelevant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Judgment {
    pub uri: String,
    pub relevance: u8,
}

impl Judgment {
    /// Highest graded-relevance value the harness scores. Anything above
    /// this is rejected at load time so out-of-range fixture values
    /// cannot silently distort NDCG.
    pub const MAX_RELEVANCE: u8 = 3;

    /// Reject judgments whose `relevance` falls outside `0..=MAX_RELEVANCE`.
    /// Loaders should call this on every parsed row.
    pub fn validate(&self) -> Result<(), String> {
        if self.relevance > Self::MAX_RELEVANCE {
            return Err(format!(
                "Judgment.relevance for {:?} = {} is outside 0..={}",
                self.uri,
                self.relevance,
                Self::MAX_RELEVANCE
            ));
        }
        Ok(())
    }
}

/// Contract every benchmark adapter implements. The harness pulls the
/// three streams in order: corpus first (ingested into a fresh store),
/// queries next (run against the store), judgments per query (used to
/// score each ranked output).
pub trait Dataset {
    /// Display name used in the report and the CLI.
    fn name(&self) -> &str;
    /// Documents to ingest. Adapters that materialise the corpus on the
    /// fly should return an iterator that streams from disk rather than
    /// allocating the whole corpus up front.
    fn corpus(&self) -> Box<dyn Iterator<Item = Document> + '_>;
    /// Queries to run.
    fn queries(&self) -> Box<dyn Iterator<Item = Query> + '_>;
    /// Relevance judgments for a given query id. Empty when the dataset
    /// does not have judgments for that query.
    fn judgments_for(&self, query_id: &str) -> Vec<Judgment>;
}

/// In-memory dataset used in tests and by the CLI when the caller wants
/// to pin a small fixture. Adapters can also wrap on-disk fixtures into
/// this struct to keep the harness path uniform.
#[derive(Debug, Clone, Default)]
pub struct InMemoryDataset {
    pub name: String,
    pub documents: Vec<Document>,
    pub queries: Vec<Query>,
    pub judgments: std::collections::BTreeMap<String, Vec<Judgment>>,
}

impl Dataset for InMemoryDataset {
    fn name(&self) -> &str {
        &self.name
    }

    fn corpus(&self) -> Box<dyn Iterator<Item = Document> + '_> {
        Box::new(self.documents.iter().cloned())
    }

    fn queries(&self) -> Box<dyn Iterator<Item = Query> + '_> {
        Box::new(self.queries.iter().cloned())
    }

    fn judgments_for(&self, query_id: &str) -> Vec<Judgment> {
        self.judgments.get(query_id).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_fixture() -> InMemoryDataset {
        let mut ds = InMemoryDataset {
            name: "fixture".into(),
            documents: vec![
                Document {
                    uri: "doc://a".into(),
                    text: "alpha".into(),
                    entity_hint: None,
                },
                Document {
                    uri: "doc://b".into(),
                    text: "beta".into(),
                    entity_hint: None,
                },
            ],
            queries: vec![Query {
                id: "q1".into(),
                text: "alpha".into(),
                mode_override: None,
            }],
            judgments: std::collections::BTreeMap::new(),
        };
        ds.judgments.insert(
            "q1".into(),
            vec![Judgment {
                uri: "doc://a".into(),
                relevance: 1,
            }],
        );
        ds
    }

    #[test]
    fn in_memory_dataset_round_trip() {
        let ds = small_fixture();
        assert_eq!(ds.name(), "fixture");
        assert_eq!(ds.corpus().count(), 2);
        assert_eq!(ds.queries().count(), 1);
        assert_eq!(ds.judgments_for("q1").len(), 1);
        assert!(ds.judgments_for("missing").is_empty());
    }
}
