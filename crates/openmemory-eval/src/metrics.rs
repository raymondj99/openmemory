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

/// Recall at K normalised by what a K-long list could hold:
/// `hits@K / min(K, |relevant|)`.
///
/// Raw `R@K` divides by the whole judgment set, so a question with more
/// than K right answers cannot reach 1.0 however good retrieval is, and
/// part of its score is then a property of the question. This divides by
/// the most a K-limited ranking could possibly contain, so 1.0 means
/// "the top K is entirely right answers" — attainable for any query.
///
/// It is **not** recall and is reported under its own name for that
/// reason. For every query with `|relevant| <= K` the two are identical,
/// so this is a strict generalisation rather than a competing
/// definition; the difference appears only where raw recall had stopped
/// being answerable.
#[must_use]
pub fn capacity_r_at_k(ranked: &[String], judgments: &[Judgment], k: usize) -> f64 {
    let relevant: std::collections::HashSet<&str> = judgments
        .iter()
        .filter(|j| j.relevance > 0)
        .map(|j| j.uri.as_str())
        .collect();
    if relevant.is_empty() || k == 0 {
        return 0.0;
    }
    let top: std::collections::HashSet<&str> = ranked.iter().take(k).map(String::as_str).collect();
    let hits = relevant.iter().filter(|uri| top.contains(*uri)).count();
    hits as f64 / relevant.len().min(k) as f64
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

    #[test]
    fn partial_labels_can_overstate_or_understate_and_nothing_says_which() {
        // This project printed "recall over incomplete qrels is a lower
        // bound" on every affected report, and it is wrong in both
        // directions. Encoding the counterexamples as tests because a
        // claim about what a number means is exactly the kind of thing
        // that survives unchallenged in prose.

        // Case 1 — reported recall is an UPPER bound.
        // One relevant document is known and retrieved; ninety-nine
        // equally relevant documents were never judged and never
        // retrieved. Reported R@10 is a perfect 1.0 against a true
        // recall of 0.01.
        let known = vec![Judgment {
            uri: "known".into(),
            relevance: 1,
        }];
        let ranked = vec!["known".to_string()];
        let reported = r_at_k(&ranked, &known, 10);
        assert!(
            (reported - 1.0).abs() < f64::EPSILON,
            "expected a perfect reported score, got {reported}"
        );
        let true_recall = 1.0 / 100.0;
        assert!(
            reported > true_recall,
            "the reported score must be able to exceed the truth: \
             {reported} vs {true_recall}"
        );

        // Case 2 — reported recall is a LOWER bound.
        // The one judged document is missed, while a genuinely relevant
        // but unjudged document is retrieved at rank 1. Reported 0.0
        // against a true recall above zero.
        let ranked_unjudged = vec!["relevant-but-unjudged".to_string()];
        let reported = r_at_k(&ranked_unjudged, &known, 10);
        assert!(
            reported.abs() < f64::EPSILON,
            "expected a reported zero, got {reported}"
        );

        // Together: the two cases bracket the truth from opposite sides
        // using the same qrels, so no one-sided bound exists.
    }

    #[test]
    fn pairing_does_not_cancel_partial_labels() {
        // The other half of the retracted claim: "paired comparisons
        // against another arm on this same query set remain valid".
        //
        // Baseline ranks the known answer first and scores 1.0. The
        // better arm ranks a correct-but-unjudged answer first, pushing
        // the known one to rank 11 — outside K. It retrieves *more*
        // relevant documents and scores 0.0. The gate sees a total
        // regression.
        let known = vec![Judgment {
            uri: "known".into(),
            relevance: 1,
        }];

        let baseline: Vec<String> = std::iter::once("known".to_string())
            .chain((0..10).map(|i| format!("filler{i}")))
            .collect();
        let better: Vec<String> = (0..10)
            .map(|i| format!("unjudged-correct{i}"))
            .chain(std::iter::once("known".to_string()))
            .collect();

        let base_score = r_at_k(&baseline, &known, 10);
        let better_score = r_at_k(&better, &known, 10);

        assert!((base_score - 1.0).abs() < f64::EPSILON);
        assert!(
            better_score.abs() < f64::EPSILON,
            "expected the better arm to score zero on partial labels"
        );
        assert!(
            better_score < base_score,
            "the arm retrieving more relevant documents must be able to \
             score worse; that is why pairing does not cancel this"
        );

        // Same shape on the rank-sensitive metrics, so the caveat cannot
        // be dodged by quoting MRR or NDCG instead.
        assert!(mrr(&better, &known) < mrr(&baseline, &known));
        assert!(ndcg_at_k(&better, &known, 10) < ndcg_at_k(&baseline, &known, 10));
    }

    #[test]
    fn ndcg_can_still_reach_one_with_more_relevant_docs_than_k() {
        // The gate tells readers that a query judged against more than K
        // targets caps raw R@K but leaves NDCG@K attainable, and that is
        // only true because the ideal DCG is also truncated at K. If that
        // ever changed, the gate's advice would become wrong in a way
        // nobody would notice until a perfectly-ranked run scored below
        // 1.0 and was read as a regression.
        let judged: Vec<Judgment> = (0..12)
            .map(|i| Judgment {
                uri: format!("d{i}"),
                relevance: 1,
            })
            .collect();
        let perfect: Vec<String> = (0..12).map(|i| format!("d{i}")).collect();

        let ndcg = ndcg_at_k(&perfect, &judged, 10);
        assert!(
            (ndcg - 1.0).abs() < 1e-12,
            "NDCG@10 could not reach 1.0 with 12 relevant docs: {ndcg}"
        );

        // Raw recall, by contrast, genuinely cannot: 10 of 12 is its
        // maximum, which is the number the ceiling gate reports.
        let recall = r_at_k(&perfect, &judged, 10);
        assert!(
            (recall - 10.0 / 12.0).abs() < 1e-12,
            "expected the recall ceiling of 10/12, got {recall}"
        );

        // And MRR is untouched: the first hit is still at rank 1.
        assert!((mrr(&perfect, &judged) - 1.0).abs() < 1e-12);
    }

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
    fn capacity_recall_equals_recall_when_the_answers_fit() {
        // The property that makes it safe to report both: below the
        // capacity limit they are the same number, so the new name only
        // ever differs where the old one had stopped being answerable.
        let ranked = rank(&["a", "x", "y"]);
        let j = judgments(&[("a", 1), ("b", 1)]);
        for k in [2usize, 5, 10] {
            assert!(
                (capacity_r_at_k(&ranked, &j, k) - r_at_k(&ranked, &j, k)).abs() < 1e-12,
                "k={k}"
            );
        }
    }

    #[test]
    fn capacity_recall_is_attainable_when_recall_is_not() {
        // 30 right answers, K=10. Raw recall tops out at 1/3; capacity
        // recall reaches 1.0 when the top 10 are all correct, which is
        // the most a ten-long list can do.
        let all: Vec<(&str, u8)> = (0..30)
            .map(|i| (Box::leak(format!("k{i}").into_boxed_str()) as &str, 1u8))
            .collect();
        let j = judgments(&all);
        let perfect: Vec<String> = (0..10).map(|i| format!("k{i}")).collect();

        assert!((r_at_k(&perfect, &j, 10) - 1.0 / 3.0).abs() < 1e-9);
        assert!((capacity_r_at_k(&perfect, &j, 10) - 1.0).abs() < 1e-12);
        // Half the slots right is half the attainable maximum.
        let half: Vec<String> = (0..5)
            .map(|i| format!("k{i}"))
            .chain((0..5).map(|i| format!("noise{i}")))
            .collect();
        assert!((capacity_r_at_k(&half, &j, 10) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn capacity_recall_is_zero_without_judgments() {
        assert!(capacity_r_at_k(&rank(&["a"]), &[], 10).abs() < 1e-12);
        assert!(capacity_r_at_k(&rank(&["a"]), &judgments(&[("a", 1)]), 0).abs() < 1e-12);
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
