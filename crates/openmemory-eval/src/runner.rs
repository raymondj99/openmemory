//! Glue that drives a [`Dataset`] through a fresh `MemoryStore`.
//!
//! The runner is deliberately mode-agnostic: it ingests the corpus via
//! the same `engine.insert` path the MCP server uses, runs each query
//! through the hybrid engine, then hands the ranked URI list to
//! [`crate::metrics`] for scoring. The store is in-memory so each call
//! starts from a clean state and the eval result is fully deterministic
//! when the embedder is a stable test double.
//!
//! Production callers that benchmark `Hybrid` or `VectorOnly` mode must
//! attach the same embedder they use at runtime. Keyword-only runs need no
//! embedder.

#[cfg(any(feature = "testing", feature = "embeddings"))]
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use openmemory_core::config::Config;
#[cfg(any(feature = "testing", feature = "embeddings"))]
use openmemory_core::testing::Embedder;
use openmemory_graph::MemoryStore;
use openmemory_index::SearchMode;

use crate::dataset::Dataset;
use crate::metrics::{
    mrr as mrr_metric, ndcg_at_k, r_at_k, Metrics, PerQueryReport, RetrievalReport,
};

/// Knobs the runner reads at construction time. Kept small on purpose;
/// every knob has a sensible default so a barebones call (`run` against
/// the in-memory dataset) Just Works.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Default search mode for queries that do not specify a
    /// `mode_override`. Defaults to [`SearchMode::Hybrid`].
    pub mode: SearchMode,
    /// Top-K to fetch from the engine. Recall-at-5/10 and NDCG-at-5/10
    /// are computed from the same single ranked list.
    pub top_k: usize,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            mode: SearchMode::Hybrid,
            top_k: 10,
        }
    }
}

/// Owns the fresh in-memory store and ferries the dataset's three streams
/// through it. Each `run` call builds and tears down the store so the
/// harness is reentrant and side-effect free. Hybrid and vector modes
/// require an attached embedder; keyword mode does not.
pub struct EvalRunner {
    pub config: RunnerConfig,
    #[cfg(any(feature = "testing", feature = "embeddings"))]
    embedder: Option<Arc<dyn Embedder>>,
}

impl EvalRunner {
    /// Build a runner with the given config.
    #[must_use]
    pub fn new(config: RunnerConfig) -> Self {
        Self {
            config,
            #[cfg(any(feature = "testing", feature = "embeddings"))]
            embedder: None,
        }
    }

    /// Attach an embedder for vector and hybrid retrieval-quality runs.
    #[cfg(any(feature = "testing", feature = "embeddings"))]
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Run the dataset end to end. Ingests every document under its
    /// caller-supplied URI, runs each query, scores the ranked URI list
    /// against the dataset's judgments, and produces a
    /// [`RetrievalReport`].
    pub fn run<D: Dataset + ?Sized>(&self, dataset: &D) -> Result<RetrievalReport> {
        let store = MemoryStore::open_in_memory(&Config::default())
            .context("opening in-memory MemoryStore for eval")?;
        #[cfg(any(feature = "testing", feature = "embeddings"))]
        let store = if let Some(embedder) = &self.embedder {
            store.with_embedder(Arc::clone(embedder))
        } else {
            store
        };
        #[cfg(any(feature = "testing", feature = "embeddings"))]
        let has_embedder = self.embedder.is_some();
        #[cfg(not(any(feature = "testing", feature = "embeddings")))]
        let has_embedder = false;

        for doc in dataset.corpus() {
            let vector = store.embed_document(&doc.text);
            let mut entry = openmemory_index::IndexEntry::new(doc.uri.clone(), doc.text.clone());
            if !vector.is_empty() {
                entry = entry.with_vector(vector);
            }
            store
                .engine()
                .engine
                .insert(&[entry])
                .context("inserting eval corpus row")?;
        }

        let mut per_query = Vec::new();
        let mut totals = Metrics::default();
        let mut latency_sum_ms = 0.0;

        let mut q_count = 0usize;
        for query in dataset.queries() {
            let mode = query.mode_override.unwrap_or(self.config.mode);
            if mode_needs_embedder(mode) && !has_embedder {
                bail!("eval mode {mode:?} requires an embedder; attach one or use keyword mode");
            }
            let vector = store.embed_query(&query.text);

            // R@10 and NDCG@10 are reported regardless of the configured
            // top_k, so the engine must produce at least 10 candidates or
            // those metrics under-report due to truncation.
            let effective_k = self.config.top_k.max(10);

            let start = Instant::now();
            let results = store
                .engine()
                .engine
                .search(&vector, &query.text, effective_k, mode, 0)
                .context("running eval query through engine")?;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

            let ranked: Vec<String> = results.into_iter().map(|r| r.uri).collect();
            let judgments = dataset.judgments_for(&query.id);

            let r5 = r_at_k(&ranked, &judgments, 5);
            let r10 = r_at_k(&ranked, &judgments, 10);
            let rr = mrr_metric(&ranked, &judgments);
            let n5 = ndcg_at_k(&ranked, &judgments, 5);
            let n10 = ndcg_at_k(&ranked, &judgments, 10);

            per_query.push(PerQueryReport {
                query_id: query.id.clone(),
                r_at_5: r5,
                r_at_10: r10,
                reciprocal_rank: rr,
                ndcg_at_5: n5,
                ndcg_at_10: n10,
                latency_ms: elapsed_ms,
            });

            totals.r_at_5 += r5;
            totals.r_at_10 += r10;
            totals.mrr += rr;
            totals.ndcg_at_5 += n5;
            totals.ndcg_at_10 += n10;
            latency_sum_ms += elapsed_ms;
            q_count += 1;
        }

        if q_count > 0 {
            let n = q_count as f64;
            totals.r_at_5 /= n;
            totals.r_at_10 /= n;
            totals.mrr /= n;
            totals.ndcg_at_5 /= n;
            totals.ndcg_at_10 /= n;
            latency_sum_ms /= n;
        }

        Ok(RetrievalReport {
            dataset: dataset.name().to_string(),
            n_queries: q_count,
            metrics: totals,
            mean_latency_ms: latency_sum_ms,
            per_query,
        })
    }
}

fn mode_needs_embedder(mode: SearchMode) -> bool {
    matches!(mode, SearchMode::Hybrid | SearchMode::VectorOnly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::{Document, InMemoryDataset, Judgment, Query};
    use std::collections::BTreeMap;

    fn keyword_fixture() -> InMemoryDataset {
        let documents = vec![
            Document {
                uri: "doc://alpha".into(),
                text: "the alpha document mentions architecture".into(),
                entity_hint: None,
            },
            Document {
                uri: "doc://beta".into(),
                text: "the beta document mentions performance".into(),
                entity_hint: None,
            },
            Document {
                uri: "doc://gamma".into(),
                text: "the gamma document mentions onboarding".into(),
                entity_hint: None,
            },
        ];
        let queries = vec![Query {
            id: "q1".into(),
            text: "architecture".into(),
            mode_override: Some(SearchMode::KeywordOnly),
        }];
        let mut judgments = BTreeMap::new();
        judgments.insert(
            "q1".into(),
            vec![Judgment {
                uri: "doc://alpha".into(),
                relevance: 1,
            }],
        );
        InMemoryDataset {
            name: "keyword-fixture".into(),
            documents,
            queries,
            judgments,
        }
    }

    #[test]
    fn runner_scores_keyword_only_fixture() {
        let runner = EvalRunner::new(RunnerConfig {
            mode: SearchMode::KeywordOnly,
            top_k: 10,
        });
        let report = runner.run(&keyword_fixture()).unwrap();
        assert_eq!(report.dataset, "keyword-fixture");
        assert_eq!(report.n_queries, 1);
        assert!(
            (report.metrics.r_at_5 - 1.0).abs() < 1e-9,
            "expected R@5 = 1.0, got {}",
            report.metrics.r_at_5
        );
        assert!(report.metrics.mrr > 0.0);
        assert_eq!(report.per_query.len(), 1);
    }

    #[test]
    fn runner_rejects_hybrid_without_embedder() {
        let runner = EvalRunner::new(RunnerConfig {
            mode: SearchMode::Hybrid,
            top_k: 10,
        });
        let mut dataset = keyword_fixture();
        dataset.queries[0].mode_override = None;
        let err = runner.run(&dataset).unwrap_err();
        assert!(err.to_string().contains("requires an embedder"));
    }

    #[cfg(feature = "testing")]
    #[test]
    fn runner_accepts_hybrid_with_embedder() {
        use openmemory_core::testing::{Embedder, FakeEmbedder};
        let runner = EvalRunner::new(RunnerConfig {
            mode: SearchMode::Hybrid,
            top_k: 10,
        })
        .with_embedder(std::sync::Arc::new(FakeEmbedder::new(16)) as std::sync::Arc<dyn Embedder>);
        let mut dataset = keyword_fixture();
        dataset.queries[0].mode_override = None;
        let report = runner.run(&dataset).unwrap();
        assert_eq!(report.n_queries, 1);
    }
}
