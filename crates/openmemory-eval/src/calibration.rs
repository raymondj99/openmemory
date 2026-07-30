//! Retrieval confidence: does this result set contain an answer?
//!
//! Three independent findings needed the same missing capability and
//! none of them could be fixed without it:
//!
//! - **abstention** needs "is anything here relevant?" — the system
//!   returns ten confident results for questions with no answer;
//! - **cross-space composition** needs "are these two spaces' scores
//!   comparable?" — one space silently contributed zero results;
//! - **adaptive channel weighting** needs "did direct retrieval
//!   succeed?" — a graph channel strong enough to rescue a failing query
//!   destroys the queries that were already working.
//!
//! All three reduce to: the system has no calibrated notion of result
//! quality. Raw scores cannot supply one. They are unbounded in keyword
//! mode, compressed by RRF in hybrid mode, and multiplied by priors
//! whose values depend on corpus age — so the same number means
//! different things for different queries and different corpora.
//!
//! What *is* comparable is the **shape** of a result set. When retrieval
//! has found something, the top result usually stands apart from the
//! rest; when it has not, the scores are flat and arbitrary. Shape
//! features are scale-free by construction: each is a ratio or a
//! normalised spread, so they transfer across modes and corpora in a way
//! absolute scores do not.
//!
//! This module computes those features and combines them into one
//! bounded confidence value. It deliberately does not learn weights: a
//! fitted model would need held-out data the project does not yet have,
//! and an unfitted transparent formula can be inspected, argued with,
//! and regression-tested.

use serde::{Deserialize, Serialize};

/// Scale-free descriptors of one result set's score distribution.
///
/// Every field is a ratio or a normalised statistic. None is an absolute
/// score, because absolute scores are not comparable across modes,
/// corpora, or queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ScoreShape {
    /// Results considered.
    pub n: usize,
    /// `(top - second) / top`. High when one result stands apart, which
    /// is what a confident retrieval looks like.
    pub top_margin: f64,
    /// `(top - mean) / top`. High when the top result beats the field
    /// rather than the field being uniformly high.
    pub top_lift: f64,
    /// Coefficient of variation, `stddev / mean`. Near zero means the
    /// scores are flat and retrieval has not distinguished anything.
    pub dispersion: f64,
    /// `mean(top third) / mean(bottom third)`. Captures whether the head
    /// of the list is meaningfully better than its tail.
    pub head_tail_ratio: f64,
}

impl ScoreShape {
    /// Describe a descending-ordered score list.
    ///
    /// Non-finite and negative scores are dropped rather than allowed to
    /// poison the statistics; a NaN would otherwise propagate into every
    /// feature and silently produce a meaningless confidence.
    #[must_use]
    pub fn measure(scores: &[f64]) -> Self {
        let clean: Vec<f64> = scores
            .iter()
            .copied()
            .filter(|s| s.is_finite() && *s > 0.0)
            .collect();
        if clean.is_empty() {
            return Self::default();
        }

        let n = clean.len();
        let top = clean[0];
        let mean = clean.iter().sum::<f64>() / n as f64;

        let top_margin = if n > 1 && top > 0.0 {
            ((top - clean[1]) / top).clamp(0.0, 1.0)
        } else {
            // A single result has nothing to stand apart from. Treating
            // that as maximal confidence would make every one-hit query
            // look certain, so it scores neutral instead.
            0.0
        };

        let top_lift = if top > 0.0 {
            ((top - mean) / top).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let variance = clean.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / n as f64;
        let dispersion = if mean > 0.0 {
            (variance.sqrt() / mean).clamp(0.0, 10.0)
        } else {
            0.0
        };

        let third = (n / 3).max(1);
        let head: f64 = clean.iter().take(third).sum::<f64>() / third as f64;
        let tail: f64 = clean.iter().rev().take(third).sum::<f64>() / third as f64;
        let head_tail_ratio = if tail > 0.0 {
            (head / tail).clamp(1.0, 100.0)
        } else {
            1.0
        };

        Self {
            n,
            top_margin,
            top_lift,
            dispersion,
            head_tail_ratio,
        }
    }

    /// Combine the features into a bounded confidence in `[0, 1]`.
    ///
    /// The weights are chosen, not fitted, and the reasoning is
    /// deliberately visible so it can be argued with:
    ///
    /// - `top_margin` is weighted highest because "one result stands
    ///   clearly apart" is the strongest available evidence that
    ///   retrieval located something specific rather than returning a
    ///   neighbourhood;
    /// - `top_lift` says the leader beats the field, which is weaker
    ///   than beating the runner-up but still informative;
    /// - `dispersion` distinguishes a flat list (nothing found, scores
    ///   arbitrary) from a structured one, and is squashed because its
    ///   natural range varies by mode;
    /// - `head_tail_ratio` is the mildest signal and is log-squashed,
    ///   since it grows without bound on some corpora.
    ///
    /// Fitting these against held-out judgments is the obvious next
    /// step. An unfitted formula is used now because the alternative is
    /// fitting on the same 24 negatives the result would be evaluated
    /// on, which measures nothing.
    #[must_use]
    pub fn confidence(&self) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        let dispersion_term = (self.dispersion / (1.0 + self.dispersion)).clamp(0.0, 1.0);
        let head_tail_term = (self.head_tail_ratio.ln() / 3.0).clamp(0.0, 1.0);

        let score = 0.40 * self.top_margin
            + 0.25 * self.top_lift
            + 0.20 * dispersion_term
            + 0.15 * head_tail_term;
        score.clamp(0.0, 1.0)
    }
}

/// Confidence for a result set, with the shape that produced it.
///
/// Both are returned so a caller acting on the confidence can show why,
/// and so a regression can identify which feature moved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Confidence {
    pub value: f64,
    pub shape: ScoreShape,
}

impl Confidence {
    /// Measure a descending-ordered score list.
    #[must_use]
    pub fn measure(scores: &[f64]) -> Self {
        let shape = ScoreShape::measure(scores);
        Self {
            value: shape.confidence(),
            shape,
        }
    }
}

/// How well a confidence signal separates queries that have an answer
/// from queries that do not.
///
/// This is the only question that matters about a calibration signal,
/// and it is deliberately reported as a separation rather than an
/// accuracy: a signal can be useful without any threshold being
/// perfect, and useless while looking accurate on an unbalanced set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeparationReport {
    pub answerable_n: usize,
    pub unanswerable_n: usize,
    pub answerable_mean: f64,
    pub unanswerable_mean: f64,
    /// Probability that a randomly chosen answerable query scores above
    /// a randomly chosen unanswerable one. 0.5 is no signal; 1.0 is
    /// perfect separation. Equivalent to the area under the ROC curve,
    /// computed directly from the pairwise comparison so no threshold
    /// choice is involved.
    pub auc: f64,
}

impl SeparationReport {
    /// Compare two sets of confidence values.
    #[must_use]
    pub fn compare(answerable: &[f64], unanswerable: &[f64]) -> Self {
        if answerable.is_empty() || unanswerable.is_empty() {
            return Self::default();
        }
        let mut wins = 0.0_f64;
        for a in answerable {
            for u in unanswerable {
                if a > u {
                    wins += 1.0;
                } else if (a - u).abs() < f64::EPSILON {
                    wins += 0.5;
                }
            }
        }
        let pairs = (answerable.len() * unanswerable.len()) as f64;

        Self {
            answerable_n: answerable.len(),
            unanswerable_n: unanswerable.len(),
            answerable_mean: answerable.iter().sum::<f64>() / answerable.len() as f64,
            unanswerable_mean: unanswerable.iter().sum::<f64>() / unanswerable.len() as f64,
            auc: wins / pairs,
        }
    }

    /// Whether the signal carries usable information.
    ///
    /// 0.5 is chance. The 0.6 floor is a judgement call about what is
    /// worth building on, not a statistical threshold.
    #[must_use]
    pub fn is_useful(&self) -> bool {
        self.auc >= 0.6
    }
}

/// A rate estimated with the *cluster* as the sampling unit.
///
/// Generated evaluation rows usually come in groups — several phrasings
/// of one subject, several chunks of one document — and rows within a
/// group share vocabulary, template, and author. Treating them as
/// independent Bernoulli trials understates uncertainty by roughly the
/// square root of the group size, which is enough to turn an
/// inconclusive result into an apparently decisive one.
///
/// This estimates the rate as the mean of per-group rates and bounds it
/// by resampling *groups*, so the interval reflects how many independent
/// things were actually observed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusteredRate {
    /// Individual observations.
    pub rows: usize,
    /// Independent clusters those rows came from — the real sample size.
    pub clusters: usize,
    /// Mean of per-cluster rates.
    pub rate: f64,
    /// Percentile bootstrap bounds over resampled clusters.
    pub lower: f64,
    pub upper: f64,
}

impl ClusteredRate {
    /// Estimate from `(group, hit)` observations.
    ///
    /// Deterministic: the bootstrap uses a fixed-seed linear congruential
    /// sequence so a reported interval can be reproduced exactly.
    #[must_use]
    pub fn estimate(observations: &[(String, bool)]) -> Self {
        use std::collections::BTreeMap;

        let mut groups: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for (group, hit) in observations {
            let entry = groups.entry(group.as_str()).or_insert((0, 0));
            entry.0 += 1;
            if *hit {
                entry.1 += 1;
            }
        }
        let rates: Vec<f64> = groups
            .values()
            .map(|(n, k)| *k as f64 / *n as f64)
            .collect();
        if rates.is_empty() {
            return Self {
                rows: 0,
                clusters: 0,
                rate: 0.0,
                lower: 0.0,
                upper: 0.0,
            };
        }

        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let point = mean(&rates);

        // Resample clusters with replacement.
        const DRAWS: usize = 2000;
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut samples = Vec::with_capacity(DRAWS);
        for _ in 0..DRAWS {
            let mut drawn = Vec::with_capacity(rates.len());
            for _ in 0..rates.len() {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let idx = (seed >> 33) as usize % rates.len();
                drawn.push(rates[idx]);
            }
            samples.push(mean(&drawn));
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        Self {
            rows: observations.len(),
            clusters: rates.len(),
            rate: point,
            lower: samples[(DRAWS as f64 * 0.025) as usize],
            upper: samples[((DRAWS as f64 * 0.975) as usize).min(DRAWS - 1)],
        }
    }

    /// Width of the interval; the honest measure of how much was learned.
    #[must_use]
    pub fn width(&self) -> f64 {
        self.upper - self.lower
    }
}

/// A *mean* estimated with the cluster as the sampling unit.
///
/// [`ClusteredRate`] answers "what fraction of things succeeded"; this
/// answers "what is the average value", which is what a graded metric
/// (MRR, NDCG, R@10) needs. The two share the same estimator — mean of
/// per-cluster means, bounded by resampling clusters — because the
/// reason for clustering is the same in both cases.
///
/// The intended use is a **paired** comparison: feed the per-query
/// *difference* between two configurations rather than two independent
/// samples. Pairing removes per-query difficulty, which is by far the
/// largest source of variance in a retrieval set, so an interval on the
/// paired difference is both narrower and answers the right question:
/// "did this change help *these* queries", not "are these two samples
/// drawn from different populations".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusteredMean {
    /// Individual observations.
    pub rows: usize,
    /// Independent clusters those rows came from — the real sample size.
    pub clusters: usize,
    /// Mean of per-cluster means.
    pub mean: f64,
    /// Percentile bootstrap bounds over resampled clusters.
    pub lower: f64,
    pub upper: f64,
}

impl ClusteredMean {
    /// Estimate from `(group, value)` observations.
    ///
    /// Deterministic: the bootstrap uses the same fixed-seed linear
    /// congruential sequence as [`ClusteredRate`], so a reported interval
    /// can be reproduced exactly.
    #[must_use]
    pub fn estimate(observations: &[(String, f64)]) -> Self {
        use std::collections::BTreeMap;

        let mut groups: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
        for (group, value) in observations {
            if !value.is_finite() {
                continue;
            }
            let entry = groups.entry(group.as_str()).or_insert((0, 0.0));
            entry.0 += 1;
            entry.1 += *value;
        }
        let means: Vec<f64> = groups.values().map(|(n, sum)| sum / *n as f64).collect();
        if means.is_empty() {
            return Self {
                rows: 0,
                clusters: 0,
                mean: 0.0,
                lower: 0.0,
                upper: 0.0,
            };
        }

        let mean_of = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let point = mean_of(&means);

        const DRAWS: usize = 2000;
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut samples = Vec::with_capacity(DRAWS);
        for _ in 0..DRAWS {
            let mut drawn = Vec::with_capacity(means.len());
            for _ in 0..means.len() {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let idx = (seed >> 33) as usize % means.len();
                drawn.push(means[idx]);
            }
            samples.push(mean_of(&drawn));
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        Self {
            rows: observations.len(),
            clusters: means.len(),
            mean: point,
            lower: samples[(DRAWS as f64 * 0.025) as usize],
            upper: samples[((DRAWS as f64 * 0.975) as usize).min(DRAWS - 1)],
        }
    }

    /// Width of the interval.
    #[must_use]
    pub fn width(&self) -> f64 {
        self.upper - self.lower
    }

    /// True when the 95% interval excludes zero.
    ///
    /// The only sanctioned way to call a paired difference real. An
    /// eyeballed gap between two point estimates is not evidence, and
    /// this crate exists partly so that judgement is not made by eye.
    #[must_use]
    pub fn excludes_zero(&self) -> bool {
        self.lower > 0.0 || self.upper < 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clustered_mean_uses_clusters_as_the_sample_size() {
        // 6 subjects x 10 near-identical rows.
        let observations: Vec<(String, f64)> = (0..60)
            .map(|i| (format!("s{}", i / 10), if i < 30 { 1.0 } else { 0.0 }))
            .collect();
        let est = ClusteredMean::estimate(&observations);
        assert_eq!(est.rows, 60);
        assert_eq!(est.clusters, 6, "the real sample size is subjects");
        assert!((est.mean - 0.5).abs() < 1e-9);
        assert!(
            est.width() > 0.3,
            "6 clusters cannot support a tight interval"
        );
    }

    #[test]
    fn clustered_mean_detects_a_consistent_paired_difference() {
        // Every cluster improved by 0.2; the interval must exclude zero.
        let observations: Vec<(String, f64)> = (0..40).map(|i| (format!("s{i}"), 0.2)).collect();
        let est = ClusteredMean::estimate(&observations);
        assert!(est.excludes_zero(), "{est:?}");
        assert!(est.lower > 0.0);

        // A difference that is zero on average must not.
        let mixed: Vec<(String, f64)> = (0..40)
            .map(|i| (format!("s{i}"), if i % 2 == 0 { 0.5 } else { -0.5 }))
            .collect();
        assert!(!ClusteredMean::estimate(&mixed).excludes_zero());
    }

    #[test]
    fn clustered_mean_is_deterministic_and_handles_degenerate_input() {
        let obs: Vec<(String, f64)> = (0..40)
            .map(|i| (format!("g{}", i % 8), f64::from(i % 3)))
            .collect();
        let a = ClusteredMean::estimate(&obs);
        let b = ClusteredMean::estimate(&obs);
        assert!((a.lower - b.lower).abs() < 1e-12 && (a.upper - b.upper).abs() < 1e-12);

        assert_eq!(ClusteredMean::estimate(&[]).clusters, 0);
        // Non-finite values are dropped rather than poisoning the mean.
        let with_nan = vec![("g".to_string(), f64::NAN), ("g".to_string(), 1.0)];
        let est = ClusteredMean::estimate(&with_nan);
        assert!((est.mean - 1.0).abs() < 1e-9);
    }

    #[test]
    fn clustering_widens_the_interval_versus_treating_rows_as_independent() {
        // 15 subjects x 8 near-identical rows. Within a subject the
        // outcome is nearly always the same, which is exactly the case
        // where row-level statistics overstate what was learned.
        let mut observations = Vec::new();
        for subject in 0..15 {
            let hit = subject % 3 != 0; // 10 of 15 subjects succeed
            for _ in 0..8 {
                observations.push((format!("subject-{subject}"), hit));
            }
        }
        let clustered = ClusteredRate::estimate(&observations);
        assert_eq!(clustered.rows, 120);
        assert_eq!(clustered.clusters, 15, "the real sample size is subjects");
        assert!((clustered.rate - 10.0 / 15.0).abs() < 1e-9);

        // A row-level Wilson interval on 80/120 spans about 0.58-0.75.
        // The cluster interval must be materially wider.
        assert!(
            clustered.width() > 0.20,
            "cluster interval {:.3} is implausibly narrow for 15 clusters",
            clustered.width()
        );
    }

    #[test]
    fn clustered_estimate_is_deterministic() {
        let obs: Vec<(String, bool)> = (0..40)
            .map(|i| (format!("g{}", i % 8), i % 3 == 0))
            .collect();
        let a = ClusteredRate::estimate(&obs);
        let b = ClusteredRate::estimate(&obs);
        assert!((a.lower - b.lower).abs() < 1e-12 && (a.upper - b.upper).abs() < 1e-12);
    }

    #[test]
    fn clustered_estimate_handles_degenerate_input() {
        let empty = ClusteredRate::estimate(&[]);
        assert_eq!(empty.clusters, 0);
        let single = ClusteredRate::estimate(&[("only".into(), true)]);
        assert_eq!(single.clusters, 1);
        assert!((single.rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_clear_winner_scores_higher_than_a_flat_list() {
        let confident = Confidence::measure(&[1.0, 0.2, 0.15, 0.1, 0.05]);
        let flat = Confidence::measure(&[0.5, 0.5, 0.5, 0.5, 0.5]);
        assert!(
            confident.value > flat.value,
            "confident {} should beat flat {}",
            confident.value,
            flat.value
        );
        assert!(flat.value < 0.1, "a flat list carries no signal: {flat:?}");
    }

    #[test]
    fn confidence_is_scale_free() {
        // The same shape at keyword scale and cosine scale must produce
        // the same confidence, which is the whole point of using shape
        // rather than absolute scores.
        let small = Confidence::measure(&[1.0, 0.2, 0.15, 0.1]);
        let large = Confidence::measure(&[100.0, 20.0, 15.0, 10.0]);
        assert!(
            (small.value - large.value).abs() < 1e-9,
            "{} vs {}",
            small.value,
            large.value
        );
    }

    #[test]
    fn confidence_is_bounded() {
        for scores in [
            vec![1.0],
            vec![1.0, 0.0001],
            vec![1e9, 1.0],
            vec![0.5; 100],
            vec![],
        ] {
            let c = Confidence::measure(&scores);
            assert!(
                (0.0..=1.0).contains(&c.value),
                "unbounded confidence {} for {scores:?}",
                c.value
            );
        }
    }

    #[test]
    fn non_finite_and_negative_scores_are_dropped() {
        let c = Confidence::measure(&[1.0, f64::NAN, -5.0, f64::INFINITY, 0.2]);
        assert_eq!(c.shape.n, 2, "only the two usable scores count");
        assert!(c.value.is_finite());
    }

    #[test]
    fn an_empty_result_set_has_zero_confidence() {
        let c = Confidence::measure(&[]);
        assert!(c.value.abs() < f64::EPSILON);
        assert_eq!(c.shape.n, 0);
    }

    #[test]
    fn a_single_result_does_not_claim_certainty() {
        // Nothing to stand apart from, so margin cannot be evidence.
        let c = Confidence::measure(&[1.0]);
        assert!(c.shape.top_margin.abs() < f64::EPSILON);
        assert!(c.value < 0.5, "single hit overclaimed: {c:?}");
    }

    #[test]
    fn separation_detects_a_useful_signal() {
        let answerable = vec![0.8, 0.75, 0.9, 0.7];
        let unanswerable = vec![0.2, 0.3, 0.15];
        let report = SeparationReport::compare(&answerable, &unanswerable);
        assert!((report.auc - 1.0).abs() < 1e-9, "{report:?}");
        assert!(report.is_useful());
    }

    #[test]
    fn separation_detects_no_signal() {
        let same = vec![0.5, 0.5, 0.5];
        let report = SeparationReport::compare(&same, &same);
        assert!(
            (report.auc - 0.5).abs() < 1e-9,
            "ties are chance: {report:?}"
        );
        assert!(!report.is_useful());
    }

    #[test]
    fn separation_detects_an_inverted_signal() {
        // A signal that is backwards must not look good.
        let report = SeparationReport::compare(&[0.1, 0.2], &[0.8, 0.9]);
        assert!(report.auc < 0.5, "{report:?}");
        assert!(!report.is_useful());
    }

    #[test]
    fn separation_is_empty_without_both_classes() {
        assert!(SeparationReport::compare(&[0.5], &[]).auc.abs() < f64::EPSILON);
        assert!(SeparationReport::compare(&[], &[0.5]).auc.abs() < f64::EPSILON);
    }

    #[test]
    fn shape_features_move_in_the_expected_direction() {
        let sharp = ScoreShape::measure(&[1.0, 0.1, 0.1, 0.1]);
        let gentle = ScoreShape::measure(&[1.0, 0.9, 0.85, 0.8]);
        assert!(sharp.top_margin > gentle.top_margin);
        assert!(sharp.top_lift > gentle.top_lift);
        assert!(sharp.dispersion > gentle.dispersion);
        assert!(sharp.head_tail_ratio > gentle.head_tail_ratio);
    }
}
