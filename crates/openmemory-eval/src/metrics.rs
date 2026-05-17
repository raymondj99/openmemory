//! Retrieval-quality metrics.
//!
//! Three pure functions, plus an aggregate [`Metrics`] roll-up:
//!
//! - [`r_at_k`] (recall at K): fraction of relevant documents that show
//!   up in the top K of the ranked list.
//! - [`mrr`] (mean reciprocal rank): `1 / rank_of_first_relevant`; zero
//!   if no relevant document appears anywhere in the ranking.
//! - [`ndcg_at_k`] (normalised discounted cumulative gain at K): graded
//!   relevance weighted by log-scale position; a perfect ranking
//!   yields `1.0`.
//!
//! Every function takes a ranked list of URIs and a judgment lookup so
//! callers can score without having to materialise a full TREC qrels
//! file. The harness composes these into [`RetrievalReport`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::dataset::Judgment;

/// Rolled-up per-query metrics over a dataset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub r_at_5: f64,
    pub r_at_10: f64,
    pub mrr: f64,
    pub ndcg_at_5: f64,
    pub ndcg_at_10: f64,
}

/// Final report produced by [`crate::runner::EvalRunner::run`]. Carries
/// the per-query metrics, the aggregate, the dataset name, and the
/// average per-query latency in milliseconds so a CI artifact can pin
/// both quality and cost in the same JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalReport {
    pub dataset: String,
    pub n_queries: usize,
    pub metrics: Metrics,
    pub mean_latency_ms: f64,
    pub per_query: Vec<PerQueryReport>,
}

/// Per-query metrics, included in the report so a CI diff can spot a
/// regression on a single query rather than collapsing it into the mean.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerQueryReport {
    pub query_id: String,
    pub r_at_5: f64,
    pub r_at_10: f64,
    pub reciprocal_rank: f64,
    pub ndcg_at_5: f64,
    pub ndcg_at_10: f64,
    pub latency_ms: f64,
}

/// Recall at K. Returns `0.0` when the judgment set is empty so callers
/// do not have to special-case that path.
#[must_use]
pub fn r_at_k(ranked: &[String], judgments: &[Judgment], k: usize) -> f64 {
    let relevant: Vec<&str> = judgments
        .iter()
        .filter(|j| j.relevance > 0)
        .map(|j| j.uri.as_str())
        .collect();
    if relevant.is_empty() {
        return 0.0;
    }
    let top: std::collections::HashSet<&str> = ranked.iter().take(k).map(String::as_str).collect();
    let hits = relevant.iter().filter(|uri| top.contains(*uri)).count();
    hits as f64 / relevant.len() as f64
}

/// Reciprocal rank of the first relevant document in the ranking; zero
/// if no relevant document appears anywhere in the list. Aggregated by
/// the caller (mean across queries) to yield MRR.
#[must_use]
pub fn mrr(ranked: &[String], judgments: &[Judgment]) -> f64 {
    let relevant: std::collections::HashSet<&str> = judgments
        .iter()
        .filter(|j| j.relevance > 0)
        .map(|j| j.uri.as_str())
        .collect();
    if relevant.is_empty() {
        return 0.0;
    }
    for (i, uri) in ranked.iter().enumerate() {
        if relevant.contains(uri.as_str()) {
            return 1.0 / (i + 1) as f64;
        }
    }
    0.0
}

/// Normalised discounted cumulative gain at K. Uses the standard
/// formulation: `gain_i = (2^rel_i - 1) / log2(i + 2)`, normalised by
/// the ideal DCG over the same K. Returns `0.0` when no relevant
/// document exists.
#[must_use]
pub fn ndcg_at_k(ranked: &[String], judgments: &[Judgment], k: usize) -> f64 {
    let rel_by_uri: HashMap<&str, u8> = judgments
        .iter()
        .map(|j| (j.uri.as_str(), j.relevance))
        .collect();

    let dcg: f64 = ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, uri)| {
            let rel = rel_by_uri.get(uri.as_str()).copied().unwrap_or(0);
            ((2f64.powi(i32::from(rel))) - 1.0) / ((i as f64 + 2.0).log2())
        })
        .sum();

    let mut ideal_rels: Vec<u8> = judgments.iter().map(|j| j.relevance).collect();
    ideal_rels.sort_unstable_by(|a, b| b.cmp(a));
    let ideal_dcg: f64 = ideal_rels
        .into_iter()
        .take(k)
        .enumerate()
        .map(|(i, rel)| ((2f64.powi(i32::from(rel))) - 1.0) / ((i as f64 + 2.0).log2()))
        .sum();

    if ideal_dcg == 0.0 {
        0.0
    } else {
        dcg / ideal_dcg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn judgments(pairs: &[(&str, u8)]) -> Vec<Judgment> {
        pairs
            .iter()
            .map(|(uri, rel)| Judgment {
                uri: (*uri).into(),
                relevance: *rel,
            })
            .collect()
    }

    fn rank(uris: &[&str]) -> Vec<String> {
        uris.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn r_at_k_perfect_recall() {
        let ranked = rank(&["a", "b", "c"]);
        let j = judgments(&[("a", 1), ("b", 1)]);
        assert!((r_at_k(&ranked, &j, 5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn r_at_k_half() {
        let ranked = rank(&["a", "x", "y"]);
        let j = judgments(&[("a", 1), ("b", 1)]);
        assert!((r_at_k(&ranked, &j, 5) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn r_at_k_empty_judgments_is_zero() {
        let ranked = rank(&["a"]);
        assert!(r_at_k(&ranked, &[], 5).abs() < 1e-9);
    }

    #[test]
    fn mrr_first_position() {
        let ranked = rank(&["a", "b"]);
        let j = judgments(&[("a", 1)]);
        assert!((mrr(&ranked, &j) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn mrr_relevant_at_position_five_is_one_fifth() {
        let ranked = rank(&["w", "x", "y", "z", "rel"]);
        let j = judgments(&[("rel", 1)]);
        assert!((mrr(&ranked, &j) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn mrr_no_match_is_zero() {
        let ranked = rank(&["a", "b"]);
        let j = judgments(&[("missing", 1)]);
        assert!(mrr(&ranked, &j).abs() < 1e-9);
    }

    #[test]
    fn ndcg_perfect_ranking_is_one() {
        let ranked = rank(&["a", "b"]);
        let j = judgments(&[("a", 3), ("b", 2)]);
        assert!((ndcg_at_k(&ranked, &j, 5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ndcg_reversed_ranking_is_less_than_one() {
        let ranked = rank(&["b", "a"]);
        let j = judgments(&[("a", 3), ("b", 2)]);
        let score = ndcg_at_k(&ranked, &j, 5);
        assert!(score < 1.0);
        assert!(score > 0.0);
    }

    #[test]
    fn ndcg_no_relevant_is_zero() {
        let ranked = rank(&["a", "b"]);
        let j = judgments(&[("a", 0), ("b", 0)]);
        assert!(ndcg_at_k(&ranked, &j, 5).abs() < 1e-9);
    }
}
