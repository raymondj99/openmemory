//! What two reports must agree on before their scores mean anything.
//!
//! A gate that compares metrics without checking where they came from
//! will happily compare a baseline from one corpus against a run on
//! another, or a full query set against an easier subset, and report
//! "no regression". The strategy and dataset names stay the same, so
//! nothing objects.
//!
//! This is the report-level version of a defect already recorded for
//! corpora: a receipt that certifies less than the reader assumes. There
//! the fix was to bind the receipt to the verifier generation; here it
//! is to bind the report to the experiment that produced it.
//!
//! An [`EvaluationContract`] fingerprints everything that must be
//! identical for a comparison to be meaningful. [`crate::gates`] refuses
//! to compare mismatched contracts **before** looking at any score, so a
//! mismatch is an error rather than a number.

use serde::{Deserialize, Serialize};

use crate::recall_set::RecallQuerySet;

/// Bumped when the metric definitions themselves change.
///
/// Two reports computed by different metric code are not comparable even
/// if every input matches, because the numbers mean different things.
///
/// History:
/// - **1** — initial.
/// - **2** — the runner retrieves until K *eligible* documents survive
///   rather than fetching K rows and filtering afterwards. Under v1, a
///   query that excluded its own source document was scored over nine
///   candidates and the tenth was never fetched, so its `R@10` was
///   computed over a truncated list. Every v1 report of a query set
///   using `exclude` measured a different quantity and must not be
///   compared against a v2 one.
pub const METRIC_VERSION: u32 = 2;

/// Everything that must match for two reports to be comparable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationContract {
    pub metric_version: u32,
    /// Digest over every query's id, text, category, abstention flag,
    /// group, and graded judgments.
    ///
    /// Covers judgments deliberately: changing what counts as a correct
    /// answer changes the score without changing the query count, and
    /// that is exactly the substitution a comparison must refuse.
    pub query_set_digest: String,
    pub query_count: usize,
    /// Identity of the corpus searched. A directory name, and weak on
    /// its own — two different corpora can sit at the same path on
    /// different days.
    pub corpus: String,
    /// Digest of the corpus's verification receipt: source revision and
    /// dirty state, chunker policy, embedding model and dimension,
    /// verifier generation, and the verified row counts.
    ///
    /// This is what actually binds a report to the artifacts it
    /// measured. The directory name says where the corpus was; this says
    /// which corpus it was.
    ///
    /// `None` for runs against a store with no receipt — the in-memory
    /// stores the instrument's own sensitivity suite builds, for
    /// instance, which have no source tree to fingerprint. `None` and
    /// `Some` are treated as different, so a receipted baseline can
    /// never be compared against an unreceipted run.
    #[serde(default)]
    pub corpus_receipt: Option<String>,
    pub mode: String,
    pub top_k: usize,
    pub spreading_activation: bool,
    /// Whether the run recorded access telemetry. A run that mutated its
    /// own corpus is not comparable to one that did not.
    pub record_access: bool,
    /// Ranking-prior configuration, when the caller pinned it.
    pub decay_rate: Option<f64>,
}

impl EvaluationContract {
    /// Digest a query set: ids, texts, categories, flags, groups, and
    /// graded judgments, in a fixed order.
    #[must_use]
    pub fn digest_query_set(set: &RecallQuerySet) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"openmemory/eval-query-set/v1");

        // Sort so the digest depends on content rather than file order.
        let mut ids: Vec<&crate::recall_set::RecallQuery> = set.queries.iter().collect();
        ids.sort_by(|a, b| a.id.cmp(&b.id));

        for query in ids {
            for field in [
                query.id.as_str(),
                query.text.as_str(),
                query.category.as_str(),
                if query.expect_abstention { "1" } else { "0" },
                query.group_id.as_deref().unwrap_or(""),
            ] {
                // Length-framed so adjacent fields cannot be shifted
                // between one another without changing the digest.
                hasher.update(&(field.len() as u64).to_le_bytes());
                hasher.update(field.as_bytes());
            }
            let mut judgments: Vec<(&str, u8)> = query
                .relevant
                .iter()
                .map(|g| (g.key.as_str(), g.relevance))
                .collect();
            judgments.sort_unstable();
            hasher.update(&(judgments.len() as u64).to_le_bytes());
            for (key, relevance) in judgments {
                hasher.update(&(key.len() as u64).to_le_bytes());
                hasher.update(key.as_bytes());
                hasher.update(&[relevance]);
            }
            // Exclusions change what the ranking contains and therefore
            // what every metric means. Leaving them out of the digest
            // would let "same questions, fewer competitors" pass as the
            // same experiment — the substitution this digest exists to
            // refuse, wearing a different hat.
            let mut excluded: Vec<&str> = query.exclude.iter().map(String::as_str).collect();
            excluded.sort_unstable();
            hasher.update(&(excluded.len() as u64).to_le_bytes());
            for key in excluded {
                hasher.update(&(key.len() as u64).to_le_bytes());
                hasher.update(key.as_bytes());
            }
            // A capacity waiver excuses one query from the recall-ceiling
            // gate, so it is part of what the numbers mean. Digesting the
            // reason as well as the presence is deliberate: a waiver is
            // granted for a stated justification, and silently rewriting
            // that justification while keeping the exemption is precisely
            // how a waiver outlives the situation it was granted for.
            let waiver = query
                .capacity_waiver
                .as_ref()
                .map_or("", |w| w.reason.as_str());
            hasher.update(&(waiver.len() as u64).to_le_bytes());
            hasher.update(waiver.as_bytes());
            // Whether the judgments claim to be exhaustive changes what
            // the recall family means — a level under one, a
            // partial-label estimate of unknown direction
            // under the other. Two runs that disagree about it are not
            // reporting the same quantity.
            hasher.update(&[u8::from(query.judgments_complete)]);
        }
        hasher.finalize().to_hex()[..32].to_string()
    }

    /// Fields that differ between two contracts, in reader-facing terms.
    ///
    /// Returns the reasons rather than a bool so a failure can say what
    /// was incomparable instead of only that something was.
    #[must_use]
    pub fn differences(&self, other: &Self) -> Vec<String> {
        let mut out = Vec::new();
        let mut note = |name: &str, a: String, b: String| {
            if a != b {
                out.push(format!("{name}: baseline {a} vs current {b}"));
            }
        };
        note(
            "metric_version",
            self.metric_version.to_string(),
            other.metric_version.to_string(),
        );
        note(
            "query_set_digest",
            self.query_set_digest.clone(),
            other.query_set_digest.clone(),
        );
        note(
            "query_count",
            self.query_count.to_string(),
            other.query_count.to_string(),
        );
        note("corpus", self.corpus.clone(), other.corpus.clone());
        note(
            "corpus_receipt",
            self.corpus_receipt
                .clone()
                .unwrap_or_else(|| "<none>".into()),
            other
                .corpus_receipt
                .clone()
                .unwrap_or_else(|| "<none>".into()),
        );
        note("mode", self.mode.clone(), other.mode.clone());
        note("top_k", self.top_k.to_string(), other.top_k.to_string());
        note(
            "spreading_activation",
            self.spreading_activation.to_string(),
            other.spreading_activation.to_string(),
        );
        note(
            "record_access",
            self.record_access.to_string(),
            other.record_access.to_string(),
        );
        note(
            "decay_rate",
            format!("{:?}", self.decay_rate),
            format!("{:?}", other.decay_rate),
        );
        out
    }

    /// True when the two describe the same experiment.
    #[must_use]
    pub fn comparable_to(&self, other: &Self) -> bool {
        self.differences(other).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall_set::{GradedKey, JudgmentSource, RecallQuery};

    fn query(id: &str, text: &str, key: &str, relevance: u8) -> RecallQuery {
        RecallQuery {
            id: id.into(),
            text: text.into(),
            category: "direct".into(),
            relevant: vec![GradedKey {
                key: key.into(),
                relevance,
            }],
            exclude: Vec::new(),
            capacity_waiver: None,
            expect_abstention: false,
            judgment_source: JudgmentSource::Generated,
            group_id: None,
            note: None,
            judgments_complete: true,
        }
    }

    fn set(queries: Vec<RecallQuery>) -> RecallQuerySet {
        RecallQuerySet {
            name: "s".into(),
            queries,
        }
    }

    fn contract(digest: &str, count: usize) -> EvaluationContract {
        EvaluationContract {
            metric_version: METRIC_VERSION,
            query_set_digest: digest.into(),
            query_count: count,
            corpus: "space-a".into(),
            corpus_receipt: Some("receipt-abc".into()),
            mode: "Hybrid".into(),
            top_k: 10,
            spreading_activation: true,
            record_access: false,
            decay_rate: Some(0.01),
        }
    }

    #[test]
    fn the_digest_is_stable_and_order_independent() {
        let a = set(vec![
            query("q1", "alpha", "k1", 1),
            query("q2", "beta", "k2", 1),
        ]);
        let b = set(vec![
            query("q2", "beta", "k2", 1),
            query("q1", "alpha", "k1", 1),
        ]);
        assert_eq!(
            EvaluationContract::digest_query_set(&a),
            EvaluationContract::digest_query_set(&b),
            "file order must not change the digest"
        );
    }

    #[test]
    fn changing_a_judgment_changes_the_digest() {
        // The substitution a comparison most needs to refuse: same
        // queries, easier answers.
        let original = set(vec![query("q1", "alpha", "k1", 1)]);
        let relabelled = set(vec![query("q1", "alpha", "k1", 3)]);
        let retargeted = set(vec![query("q1", "alpha", "different-key", 1)]);

        let base = EvaluationContract::digest_query_set(&original);
        assert_ne!(base, EvaluationContract::digest_query_set(&relabelled));
        assert_ne!(base, EvaluationContract::digest_query_set(&retargeted));
    }

    #[test]
    fn changing_query_text_or_category_changes_the_digest() {
        let base = EvaluationContract::digest_query_set(&set(vec![query("q1", "alpha", "k", 1)]));
        assert_ne!(
            base,
            EvaluationContract::digest_query_set(&set(vec![query("q1", "ALPHA", "k", 1)]))
        );

        let mut moved = query("q1", "alpha", "k", 1);
        moved.category = "multi-hop".into();
        assert_ne!(
            base,
            EvaluationContract::digest_query_set(&set(vec![moved]))
        );
    }

    #[test]
    fn a_capacity_waiver_cannot_move_between_queries_unnoticed() {
        // A waiver excuses one query from the recall-ceiling gate. If it
        // were outside the digest, moving it to a newly capped query — or
        // granting a second one — would leave the baseline comparable and
        // the exception invisible.
        let waiver = |reason: &str| crate::recall_set::CapacityWaiver {
            reason: reason.into(),
        };
        let mut first = query("q1", "alpha", "k1", 1);
        first.capacity_waiver = Some(waiver("one right answer per version"));
        let second = query("q2", "beta", "k2", 1);

        let granted = set(vec![first.clone(), second.clone()]);
        let base = EvaluationContract::digest_query_set(&granted);

        // Same set with no waiver at all.
        let mut plain = first.clone();
        plain.capacity_waiver = None;
        assert_ne!(
            base,
            EvaluationContract::digest_query_set(&set(vec![plain, second.clone()]))
        );

        // Waiver moved to the other query.
        let mut moved_from = first.clone();
        moved_from.capacity_waiver = None;
        let mut moved_to = second.clone();
        moved_to.capacity_waiver = Some(waiver("one right answer per version"));
        assert_ne!(
            base,
            EvaluationContract::digest_query_set(&set(vec![moved_from, moved_to]))
        );

        // Same waiver, rewritten justification. The exemption is granted
        // for a stated reason; changing the reason changes the grant.
        let mut reworded = first.clone();
        reworded.capacity_waiver = Some(waiver("because the gate was in the way"));
        assert_ne!(
            base,
            EvaluationContract::digest_query_set(&set(vec![reworded, second]))
        );
    }

    #[test]
    fn field_boundaries_cannot_be_shifted() {
        // Without length framing, ("ab","c") and ("a","bc") would hash
        // identically and two different query sets would look the same.
        let a = EvaluationContract::digest_query_set(&set(vec![query("ab", "c", "k", 1)]));
        let b = EvaluationContract::digest_query_set(&set(vec![query("a", "bc", "k", 1)]));
        assert_ne!(a, b);
    }

    #[test]
    fn a_query_subset_is_not_comparable() {
        // The evasion the count check exists for: keep one row per
        // category so coverage passes, drop the hard ones.
        let full = contract("digest-full", 500);
        let subset = contract("digest-subset", 12);
        let diffs = full.differences(&subset);
        assert!(!full.comparable_to(&subset));
        assert!(diffs.iter().any(|d| d.starts_with("query_count")));
        assert!(diffs.iter().any(|d| d.starts_with("query_set_digest")));
    }

    #[test]
    fn a_different_corpus_or_mode_is_not_comparable() {
        let base = contract("d", 10);
        for mutate in [
            |c: &mut EvaluationContract| c.corpus = "space-b".into(),
            |c: &mut EvaluationContract| c.corpus_receipt = Some("receipt-xyz".into()),
            |c: &mut EvaluationContract| c.corpus_receipt = None,
            |c: &mut EvaluationContract| c.mode = "KeywordOnly".into(),
            |c: &mut EvaluationContract| c.top_k = 20,
            |c: &mut EvaluationContract| c.spreading_activation = false,
            |c: &mut EvaluationContract| c.record_access = true,
            |c: &mut EvaluationContract| c.decay_rate = Some(0.02),
            |c: &mut EvaluationContract| c.metric_version = 99,
        ] {
            let mut other = base.clone();
            mutate(&mut other);
            assert!(
                !base.comparable_to(&other),
                "a changed field was treated as comparable: {:?}",
                base.differences(&other)
            );
        }
    }

    #[test]
    fn an_identical_contract_is_comparable() {
        let a = contract("d", 10);
        assert!(a.comparable_to(&a.clone()));
        assert!(a.differences(&a).is_empty());
    }

    #[test]
    fn differences_name_the_field_that_moved() {
        let a = contract("d", 10);
        let mut b = a.clone();
        b.decay_rate = Some(0.02);
        let diffs = a.differences(&b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("decay_rate"), "{diffs:?}");
        assert!(
            diffs[0].contains("0.01") && diffs[0].contains("0.02"),
            "{diffs:?}"
        );
    }
}
