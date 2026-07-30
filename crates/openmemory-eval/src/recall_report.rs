//! Report shape for recall-path evaluation.
//!
//! Deliberately reuses the pure functions in [`crate::metrics`] rather
//! than restating the metric math, so index-path and recall-path runs
//! cannot drift apart in how they compute R@K, MRR, or NDCG.
//!
//! What it adds:
//!
//! - **Latency percentiles.** A mean hides the tail, and the tail is
//!   what a budget is written against.
//! - **Per-category rollups.** One aggregate can improve while the
//!   category a change was meant to fix gets worse.
//! - **Abstention scoring.** Queries with no correct answer are scored
//!   on whether the system correctly stayed quiet.
//! - **Judgment provenance split.** Generated and human-reviewed
//!   judgments roll up separately so mined judgments cannot quietly
//!   decide a design question.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::dataset::Judgment;
use crate::metrics::{capacity_r_at_k, mrr, ndcg_at_k, r_at_k};
use crate::recall_set::{JudgmentSource, RecallQuery};

/// The rank cutoff the release gates compare at.
///
/// One constant because three places have to agree on it: the runner
/// measures the recall ceiling here, the gates threshold R@K and NDCG@K
/// here, and a query generator has to know how many judgments make a
/// question unanswerable. When they disagree, a generator emits queries
/// the gate then refuses, and the disagreement is invisible in both.
///
/// Not the same as `RecallRunnerConfig::top_k`: fetching more results
/// does not change the cutoff the numbers are quoted at.
pub const GATED_K: usize = 10;

/// Latency distribution for a run, in milliseconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyStats {
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl LatencyStats {
    /// Compute from raw per-query samples. Percentiles use the
    /// nearest-rank method on the sorted sample, which needs no
    /// interpolation policy and is stable for small n.
    #[must_use]
    pub fn from_samples(samples: &[f64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut sorted: Vec<f64> = samples.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let pick = |q: f64| -> f64 {
            // Nearest-rank: ceil(q * n), clamped into range.
            let rank = (q * sorted.len() as f64).ceil().max(1.0) as usize;
            sorted[rank.min(sorted.len()) - 1]
        };
        Self {
            mean_ms: sorted.iter().sum::<f64>() / sorted.len() as f64,
            p50_ms: pick(0.50),
            p95_ms: pick(0.95),
            p99_ms: pick(0.99),
            max_ms: *sorted.last().unwrap_or(&0.0),
        }
    }
}

/// Retrieval quality over a set of scored queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityMetrics {
    pub n_queries: usize,
    /// How many of `n_queries` the raw recall means were taken over.
    ///
    /// Differs from `n_queries` exactly when the slice contains queries
    /// with a declared capacity waiver, for which raw recall is not
    /// answerable. Reported so a reader can see that "R@10 over 838
    /// queries" was in fact over 836.
    #[serde(default)]
    pub n_recall_queries: usize,
    pub r_at_5: f64,
    pub r_at_10: f64,
    /// `hits@K / min(K, |relevant|)`, averaged over every ranked query
    /// including the waived ones. See [`crate::metrics::capacity_r_at_k`].
    #[serde(default)]
    pub capacity_r_at_5: f64,
    #[serde(default)]
    pub capacity_r_at_10: f64,
    pub mrr: f64,
    pub ndcg_at_5: f64,
    pub ndcg_at_10: f64,
    /// False when any query in this slice declares its judgments
    /// incomplete, which makes every metric here a **partial-label
    /// estimate of unknown direction** (see [`crate::recall_set::RecallQuery::judgments_complete`])
    /// rather than a level: an unjudged document that answers the query
    /// is counted as a miss and also displaces a judged one.
    ///
    /// Travels with the numbers so a reader cannot pick up `R@10 = 0.89`
    /// without the qualifier attached to it. Paired comparisons remain
    /// valid; the same bias sits in both arms.
    #[serde(default = "yes")]
    pub judgments_complete: bool,
}

/// Serde default for the completeness flags: an old report predates the
/// distinction, and treating it as complete matches how it was read.
fn yes() -> bool {
    true
}

impl Default for QualityMetrics {
    /// Hand-written rather than derived for one field: an empty slice
    /// has no incomplete judgments in it, and `bool::default()` would
    /// label it incomplete — a caveat attached to nothing, which is how
    /// caveats stop being read.
    fn default() -> Self {
        Self {
            n_queries: 0,
            n_recall_queries: 0,
            r_at_5: 0.0,
            r_at_10: 0.0,
            capacity_r_at_5: 0.0,
            capacity_r_at_10: 0.0,
            mrr: 0.0,
            ndcg_at_5: 0.0,
            ndcg_at_10: 0.0,
            judgments_complete: true,
        }
    }
}

impl QualityMetrics {
    /// Mean of the per-query scores.
    ///
    /// Raw `R@K` averages only over queries where it is answerable. A
    /// query holding a [`crate::recall_set::CapacityWaiver`] has more
    /// right answers than K by declaration, so including it would fold a
    /// known-unreachable number into the mean and make the aggregate
    /// drift with the size of that cohort rather than with retrieval.
    /// Those queries are counted in `n_queries` and contribute to MRR,
    /// NDCG and the capacity-normalised recall, which are all attainable
    /// for them; `n_recall_queries` says how many the raw means used.
    #[must_use]
    pub fn from_scored(scored: &[ScoredQuery]) -> Self {
        let ranked: Vec<&ScoredQuery> = scored.iter().filter(|s| !s.expect_abstention).collect();
        if ranked.is_empty() {
            return Self::default();
        }
        let n = ranked.len() as f64;
        let recallable: Vec<&&ScoredQuery> = ranked.iter().filter(|s| !s.capacity_waived).collect();
        let mean_recall = |pick: fn(&ScoredQuery) -> f64| {
            if recallable.is_empty() {
                0.0
            } else {
                recallable.iter().map(|s| pick(s)).sum::<f64>() / recallable.len() as f64
            }
        };
        Self {
            n_queries: ranked.len(),
            n_recall_queries: recallable.len(),
            r_at_5: mean_recall(|s| s.r_at_5),
            r_at_10: mean_recall(|s| s.r_at_10),
            capacity_r_at_5: ranked.iter().map(|s| s.capacity_r_at_5).sum::<f64>() / n,
            capacity_r_at_10: ranked.iter().map(|s| s.capacity_r_at_10).sum::<f64>() / n,
            mrr: ranked.iter().map(|s| s.reciprocal_rank).sum::<f64>() / n,
            ndcg_at_5: ranked.iter().map(|s| s.ndcg_at_5).sum::<f64>() / n,
            ndcg_at_10: ranked.iter().map(|s| s.ndcg_at_10).sum::<f64>() / n,
            judgments_complete: ranked.iter().all(|s| s.judgments_complete),
        }
    }
}

/// How high raw recall@K could possibly go on this query set.
///
/// A query judged against more than `k` relevant documents cannot reach
/// `R@k = 1.0` however good retrieval is, so part of its score measures
/// the judgment set rather than the system. That is not the same thing
/// as the judgments being *unreachable* — MRR and NDCG@k remain
/// well-defined and the documents are still findable — so the honest
/// statement is a ceiling, and the honest gate is a threshold on it.
///
/// This project shipped 63 such queries before noticing, which is why
/// the number now travels inside the report instead of living in a
/// helper nobody called.
///
/// ## Why a mean is not enough
///
/// A mean is a budget: one query judged against 30 targets contributes
/// `-0.67`, and 999 unrelated one-answer queries dilute that to `-0.0007`.
/// So the same defective query is refused in a 31-query set and accepted
/// in a 1000-query set, and *adding easy queries* is a way to buy its
/// acceptance. That is a property of averaging, not of the corpus.
///
/// The distribution is therefore carried alongside it: the worst single
/// query ([`Self::min_ceiling`]), how many are capped at all
/// ([`Self::capped_queries`], a count that padding cannot move), the
/// share, and the same three per category — because a category that is
/// entirely capped disappears inside a set-wide mean.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallCeiling {
    /// The K the ceiling was computed at.
    pub k: usize,
    /// Answerable queries considered, excluding declared exceptions.
    pub n_queries: usize,
    /// Mean over answerable queries of `min(|relevant|, k) / |relevant|`
    /// — the best raw `R@k` this query set admits. 1.0 means every query
    /// can in principle score perfectly.
    pub mean_ceiling: f64,
    /// Ceiling of the single worst query, which no amount of unrelated
    /// easy queries can raise. 1.0 for an empty population.
    ///
    /// `None` in reports written before this statistic existed. It is an
    /// `Option` rather than a defaulted `1.0` on purpose: defaulting
    /// would make every legacy report certify that its worst query was
    /// perfect, which is precisely the claim it cannot make. The gate
    /// refuses a report that cannot answer instead.
    #[serde(default)]
    pub min_ceiling: Option<f64>,
    /// Queries with more judged targets than `k`.
    pub capped_queries: usize,
    /// Capped queries as a fraction of those considered. Reported for
    /// scale; deliberately *not* the gated quantity, since it is exactly
    /// as dilutable as the mean.
    #[serde(default)]
    pub capped_share: f64,
    /// The worst offenders as `(query_id, judged_targets)`, most-capped
    /// first, truncated so a report stays readable.
    pub worst: Vec<(String, usize)>,
    /// The same statistics per category.
    ///
    /// A set-wide mean hides a category that is wholly capped: 18
    /// `commit-to-file` queries at a ceiling of 0.33 move a 838-query
    /// mean by 0.014, which reads as noise.
    #[serde(default)]
    pub by_category: BTreeMap<String, CategoryCeiling>,
    /// Queries excluded from every statistic above because they declared
    /// a [`crate::recall_set::CapacityWaiver`], with the reason given.
    ///
    /// Listed rather than counted: an exception whose justification is
    /// not readable next to the verdict is indistinguishable from a
    /// lowered threshold.
    #[serde(default)]
    pub waived: Vec<WaivedQuery>,
    /// Queries that declared a waiver they did not need — their judgment
    /// set fits inside `k` after all.
    ///
    /// A waiver that stops applying is the same failure class as an
    /// exclusion that stops matching: harmless today, and tomorrow it is
    /// a standing permission nobody remembers granting.
    #[serde(default)]
    pub unused_waivers: Vec<String>,
}

/// Ceiling statistics for one category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryCeiling {
    pub n_queries: usize,
    pub mean_ceiling: f64,
    pub min_ceiling: f64,
    pub capped_queries: usize,
}

/// One query holding a declared capacity exception.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaivedQuery {
    pub query_id: String,
    pub category: String,
    /// Judged targets it carries.
    pub judged: usize,
    /// The human-written justification from the query set.
    pub reason: String,
}

impl RecallCeiling {
    /// How many worst-offender rows a report carries.
    const WORST_SHOWN: usize = 10;

    /// Measure the ceiling a query set imposes at `k`.
    #[must_use]
    pub fn measure(set: &crate::recall_set::RecallQuerySet, k: usize) -> Self {
        let answerable: Vec<&RecallQuery> = set
            .queries
            .iter()
            .filter(|q| !q.expect_abstention)
            .collect();

        let mut ratios = Vec::with_capacity(answerable.len());
        let mut capped: Vec<(String, usize)> = Vec::new();
        let mut by_category: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let mut capped_by_category: BTreeMap<String, usize> = BTreeMap::new();
        let mut waived: Vec<WaivedQuery> = Vec::new();
        let mut unused_waivers: Vec<String> = Vec::new();

        for query in &answerable {
            let judged = query.relevant_keys().len();
            if judged == 0 {
                // No graded target: the query contributes nothing to a
                // recall ceiling, and counting it as 1.0 would let empty
                // judgments paper over capped ones.
                continue;
            }
            if let Some(waiver) = &query.capacity_waiver {
                if judged > k {
                    waived.push(WaivedQuery {
                        query_id: query.id.clone(),
                        category: query.category.clone(),
                        judged,
                        reason: waiver.reason.clone(),
                    });
                    continue;
                }
                // Declared but not needed. It stays in the ordinary
                // population — it is an ordinary query — and the stale
                // permission is reported.
                unused_waivers.push(query.id.clone());
            }
            let ratio = judged.min(k) as f64 / judged as f64;
            ratios.push(ratio);
            by_category
                .entry(query.category.clone())
                .or_default()
                .push(ratio);
            if judged > k {
                capped.push((query.id.clone(), judged));
                *capped_by_category
                    .entry(query.category.clone())
                    .or_default() += 1;
            }
        }

        capped.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let capped_queries = capped.len();
        capped.truncate(Self::WORST_SHOWN);
        waived.sort_by(|a, b| a.query_id.cmp(&b.query_id));
        unused_waivers.sort();

        let n = ratios.len();
        let by_category = by_category
            .into_iter()
            .map(|(category, values)| {
                let capped = capped_by_category.get(&category).copied().unwrap_or(0);
                let stats = CategoryCeiling {
                    n_queries: values.len(),
                    mean_ceiling: values.iter().sum::<f64>() / values.len() as f64,
                    min_ceiling: values.iter().copied().fold(f64::INFINITY, f64::min),
                    capped_queries: capped,
                };
                (category, stats)
            })
            .collect();

        Self {
            k,
            n_queries: n,
            mean_ceiling: if n == 0 {
                1.0
            } else {
                ratios.iter().sum::<f64>() / n as f64
            },
            min_ceiling: Some(ratios.iter().copied().fold(1.0_f64, f64::min)),
            capped_queries,
            capped_share: if n == 0 {
                0.0
            } else {
                capped_queries as f64 / n as f64
            },
            worst: capped,
            by_category,
            waived,
            unused_waivers,
        }
    }
}

/// How well the system stayed quiet when it should have.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AbstentionMetrics {
    /// Queries whose correct answer is "nothing".
    pub n_queries: usize,
    /// Independent clusters those queries came from.
    ///
    /// Negatives are generated in families — many surface forms of one
    /// subject — so the row count overstates the sample. A gate that
    /// reports 264 negatives when there are 15 independent subjects
    /// invites a confidence nobody earned. Defaulted on deserialisation
    /// so older reports still load.
    #[serde(default)]
    pub n_groups: usize,
    /// Fraction that correctly returned nothing above the score floor.
    pub abstained: f64,
    /// Mean number of results returned above the floor when it should
    /// have returned none. A system can be wrong quietly or loudly.
    pub mean_false_results: f64,
    /// Highest score any query produced when it should have abstained.
    /// Useful for choosing a defensible score floor.
    pub max_false_score: f64,
    /// Risk/coverage trade-off across candidate score thresholds.
    ///
    /// Reporting a single abstention rate at one floor answers a
    /// narrower question than it appears to: "did the system abstain?"
    /// rather than "could any threshold make it abstain usefully?".
    /// The curve answers the second, which is the one that decides
    /// whether calibration is worth building.
    pub risk_coverage: Vec<ThresholdPoint>,
}

/// One point on the risk/coverage curve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdPoint {
    /// Score threshold; results at or below it are suppressed.
    pub threshold: f64,
    /// Fraction of no-answer queries correctly silenced. Higher better.
    pub abstained: f64,
    /// Fraction of answerable queries that still return their correct
    /// answer in the top 10. Higher better; this is what a threshold
    /// costs.
    pub answer_retained: f64,
}

/// One query's outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
// Independent observations about one query, not the state of a machine.
// `expect_abstention` is an input, `capacity_waived` a policy decision,
// `corpus_exhausted` a retrieval fact, `judgments_complete` a property of
// the qrels. Collapsing them into an enum would assert relationships
// between them that do not exist, and a reader would have to reconstruct
// which combinations are legal.
pub struct ScoredQuery {
    pub query_id: String,
    pub category: String,
    /// Cluster the query belongs to, carried through from
    /// [`RecallQuery::group_id`].
    ///
    /// Without it a caller holding only the report cannot tell that
    /// several rows came from one source fact, and any interval it
    /// computes over those rows is too narrow. Defaulted on
    /// deserialisation so reports written before the field existed still
    /// load.
    #[serde(default)]
    pub group_id: Option<String>,
    pub judgment_source: JudgmentSource,
    pub expect_abstention: bool,
    pub r_at_5: f64,
    pub r_at_10: f64,
    /// Recall normalised by what a K-long list can hold. Identical to
    /// `r_at_k` unless this query has more than K right answers.
    #[serde(default)]
    pub capacity_r_at_5: f64,
    #[serde(default)]
    pub capacity_r_at_10: f64,
    /// Whether this query declared a capacity waiver, so raw recall is
    /// not answerable for it and the aggregates leave it out.
    #[serde(default)]
    pub capacity_waived: bool,
    /// Whether the query's judgments are believed exhaustive. False
    /// makes this row's metrics a partial-label estimate of unknown
    /// direction; see
    /// [`crate::recall_set::RecallQuery::judgments_complete`].
    #[serde(default = "yes")]
    pub judgments_complete: bool,
    pub reciprocal_rank: f64,
    pub ndcg_at_5: f64,
    pub ndcg_at_10: f64,
    /// Results returned above the score floor.
    pub returned: usize,
    /// Top score returned, if any — relevant or not.
    pub top_score: f64,
    /// Highest score among results that are actually *relevant*, or
    /// `None` when the query returned no relevant result at all.
    ///
    /// Distinct from `top_score` and the distinction matters: a
    /// threshold sweep that asks "did anything survive?" credits a query
    /// whose top hit is irrelevant and whose relevant hit sits below the
    /// cut. Only this field answers "would the answer still be there?".
    ///
    /// `Option` rather than a sentinel: this was `f64::NEG_INFINITY`,
    /// which `serde_json` writes as `null` and then refuses to read back
    /// as `f64`. The report could not round-trip, so no baseline could
    /// be stored — a defect that only surfaced on the first end-to-end
    /// run, because every unit test constructed the struct directly.
    pub top_relevant_score: Option<f64>,
    /// Rows the store physically returned. Distinct from `returned`,
    /// which counts candidates that were actually scored.
    #[serde(default)]
    pub physical_returned: usize,
    /// Rows whose key resolved to nothing, so they occupied a fetched
    /// slot without becoming a candidate.
    #[serde(default)]
    pub unresolved_hits: usize,
    /// Fetch rounds needed to reach the eligible target. More than one
    /// means the first fetch did not yield enough scorable documents.
    #[serde(default)]
    pub fetch_rounds: usize,
    /// Whether the corpus was exhausted before the target was reached.
    /// A query scored over fewer than K candidates is only sound if this
    /// is true; otherwise the tail was simply never fetched.
    #[serde(default)]
    pub corpus_exhausted: bool,
    pub latency_ms: f64,
    /// Judged keys that were requested but resolved to nothing in the
    /// store. A non-zero count means the query set and the corpus have
    /// drifted apart.
    pub unresolved_keys: usize,
    /// Results removed from the ranking because the query declared them
    /// its own source document.
    ///
    /// Reported rather than assumed. An exclusion that stops matching —
    /// a rebuilt corpus, a renamed key — puts the confound back in the
    /// ranking, and this is the only number that would show it. Zero
    /// here for a query that declares an exclusion means the removal did
    /// nothing, which is worth knowing.
    #[serde(default)]
    pub excluded_hits: usize,
}

/// Full report for one recall-path run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallReport {
    /// What experiment produced this report.
    ///
    /// `None` only for reports built before contracts existed; those
    /// cannot be safely compared and the gates say so rather than
    /// silently comparing them.
    #[serde(default)]
    pub contract: Option<crate::contract::EvaluationContract>,
    /// The best raw `R@k` this query set admits, measured at run time.
    ///
    /// Carried in the report so the gate can check it without being
    /// handed the query set again — the earlier design put the check in
    /// a helper the gate had no way to call, and so it never ran.
    #[serde(default)]
    pub recall_ceiling: Option<RecallCeiling>,
    /// Query set name.
    pub dataset: String,
    /// Human-readable label for the strategy under test.
    pub strategy: String,
    pub overall: QualityMetrics,
    pub abstention: AbstentionMetrics,
    pub by_category: BTreeMap<String, QualityMetrics>,
    pub by_judgment_source: BTreeMap<String, QualityMetrics>,
    pub latency: LatencyStats,
    pub per_query: Vec<ScoredQuery>,
    /// Total judged keys that did not resolve to any store row.
    pub unresolved_keys: usize,
    /// Total results removed as query source documents across the run.
    ///
    /// Carried at report level so the size of the removal is visible
    /// next to the numbers it produced, rather than only in per-query
    /// rows nobody reads.
    #[serde(default)]
    pub excluded_hits: usize,
}

impl RecallReport {
    /// Assemble a report from per-query outcomes.
    #[must_use]
    pub fn assemble(dataset: &str, strategy: &str, scored: Vec<ScoredQuery>) -> Self {
        let latency =
            LatencyStats::from_samples(&scored.iter().map(|s| s.latency_ms).collect::<Vec<_>>());

        let mut by_category: BTreeMap<String, Vec<ScoredQuery>> = BTreeMap::new();
        for s in &scored {
            by_category
                .entry(s.category.clone())
                .or_default()
                .push(s.clone());
        }
        let by_category: BTreeMap<String, QualityMetrics> = by_category
            .into_iter()
            .map(|(k, v)| (k, QualityMetrics::from_scored(&v)))
            .collect();

        let mut by_source: BTreeMap<String, Vec<ScoredQuery>> = BTreeMap::new();
        for s in &scored {
            by_source
                .entry(s.judgment_source.as_str().to_string())
                .or_default()
                .push(s.clone());
        }
        let by_judgment_source: BTreeMap<String, QualityMetrics> = by_source
            .into_iter()
            .map(|(k, v)| (k, QualityMetrics::from_scored(&v)))
            .collect();

        let abst: Vec<&ScoredQuery> = scored.iter().filter(|s| s.expect_abstention).collect();
        let answerable: Vec<&ScoredQuery> =
            scored.iter().filter(|s| !s.expect_abstention).collect();
        let abstention = if abst.is_empty() {
            AbstentionMetrics::default()
        } else {
            let n = abst.len() as f64;
            let groups: std::collections::BTreeSet<&str> = abst
                .iter()
                .map(|s| s.group_id.as_deref().unwrap_or(s.query_id.as_str()))
                .collect();
            AbstentionMetrics {
                n_queries: abst.len(),
                n_groups: groups.len(),
                abstained: abst.iter().filter(|s| s.returned == 0).count() as f64 / n,
                mean_false_results: abst.iter().map(|s| s.returned as f64).sum::<f64>() / n,
                max_false_score: abst.iter().map(|s| s.top_score).fold(0.0_f64, f64::max),
                risk_coverage: risk_coverage_curve(&abst, &answerable),
            }
        };

        Self {
            contract: None,
            recall_ceiling: None,
            dataset: dataset.to_string(),
            strategy: strategy.to_string(),
            overall: QualityMetrics::from_scored(&scored),
            abstention,
            by_category,
            by_judgment_source,
            latency,
            unresolved_keys: scored.iter().map(|s| s.unresolved_keys).sum(),
            excluded_hits: scored.iter().map(|s| s.excluded_hits).sum(),
            per_query: scored,
        }
    }
}

/// Sweep candidate score thresholds and report what each would buy and
/// cost.
///
/// At threshold `t`, a no-answer query is correctly silenced when its
/// top score is at or below `t`, and an answerable query survives when
/// its top score is above `t` *and* it actually found a relevant result.
/// Sweeping the observed scores rather than a fixed grid keeps the curve
/// meaningful whatever scale the mode uses — raw BM25 scores and cosine
/// similarities differ by orders of magnitude.
fn risk_coverage_curve(
    abstain: &[&ScoredQuery],
    answerable: &[&ScoredQuery],
) -> Vec<ThresholdPoint> {
    if abstain.is_empty() {
        return Vec::new();
    }
    let mut candidates: Vec<f64> = abstain
        .iter()
        .map(|s| s.top_score)
        .chain(answerable.iter().filter_map(|s| s.top_relevant_score))
        .filter(|v| v.is_finite())
        .collect();
    candidates.push(0.0);
    candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    candidates.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    // A dozen evenly spaced points is enough to see the shape without
    // producing an unreadable table.
    let stride = (candidates.len() / 12).max(1);

    candidates
        .iter()
        .step_by(stride)
        .map(|&threshold| {
            let silenced = abstain.iter().filter(|s| s.top_score <= threshold).count() as f64
                / abstain.len() as f64;
            // Retention must be judged on whether the *relevant* result
            // survives the cut. Using the top score would credit a query
            // whose irrelevant top hit cleared the threshold while its
            // actual answer fell below it.
            let retained = if answerable.is_empty() {
                0.0
            } else {
                answerable
                    .iter()
                    .filter(|s| s.top_relevant_score.is_some_and(|v| v > threshold))
                    .count() as f64
                    / answerable.len() as f64
            };
            ThresholdPoint {
                threshold,
                abstained: silenced,
                answer_retained: retained,
            }
        })
        .collect()
}

/// Collapse repeated keys to their first occurrence, carrying the
/// positionally aligned scores with them.
///
/// The first occurrence is the one kept because results arrive in
/// descending score order, so it is both the best rank and the highest
/// score the system gave that document.
fn dedupe_ranked(ranked: &[String], scores: &[f64]) -> (Vec<String>, Vec<f64>) {
    let aligned = scores.len() == ranked.len();
    let mut seen = std::collections::HashSet::with_capacity(ranked.len());
    let mut keys = Vec::with_capacity(ranked.len());
    let mut kept_scores = Vec::with_capacity(if aligned { scores.len() } else { 0 });
    for (i, key) in ranked.iter().enumerate() {
        if !seen.insert(key.as_str()) {
            continue;
        }
        keys.push(key.clone());
        if aligned {
            kept_scores.push(scores[i]);
        }
    }
    // An unaligned `scores` slice means "scores unavailable"; keep it
    // unaligned rather than inventing a correspondence.
    (
        keys,
        if aligned {
            kept_scores
        } else {
            scores.to_vec()
        },
    )
}

/// Score one query's ranked keys against its judgments.
///
/// `ranked` is the ordered list of resolved document keys. `returned`
/// and `top_score` describe what the system actually surfaced above the
/// score floor, which is what abstention is judged on.
#[must_use]
pub fn score_query(
    query: &RecallQuery,
    ranked: &[String],
    returned: usize,
    top_score: f64,
    latency_ms: f64,
    unresolved_keys: usize,
) -> ScoredQuery {
    score_query_with_scores(
        query,
        ranked,
        &[],
        returned,
        top_score,
        latency_ms,
        unresolved_keys,
    )
}

/// As [`score_query`], but with per-result scores so the highest
/// *relevant* score can be recorded for threshold calibration.
///
/// `scores` is positionally aligned with `ranked`; an empty slice means
/// scores are unavailable and `top_relevant_score` falls back to
/// `top_score` when the query has any relevant hit at all.
///
/// Repeated keys in `ranked` are collapsed to their first occurrence
/// before anything is measured. A ranked list is a list of *documents*,
/// and the same document twice is an artefact of how results were
/// projected onto keys, not a ranking signal. Left in, it corrupts two
/// metrics in opposite directions: NDCG@K counts the repeat's gain again
/// (a system returning one right answer three times could outscore one
/// returning three different right answers), while R@K lets the repeat
/// consume a top-K slot that a second answer would otherwise occupy.
/// Collapsing makes both read as if the system had returned the document
/// once, which is what it found.
#[must_use]
pub fn score_query_with_scores(
    query: &RecallQuery,
    ranked: &[String],
    scores: &[f64],
    returned: usize,
    top_score: f64,
    latency_ms: f64,
    unresolved_keys: usize,
) -> ScoredQuery {
    let (distinct, distinct_scores) = dedupe_ranked(ranked, scores);
    let (ranked, scores) = (distinct.as_slice(), distinct_scores.as_slice());

    let judgments: Vec<Judgment> = query
        .relevant
        .iter()
        .map(|g| Judgment {
            uri: g.key.clone(),
            relevance: g.relevance,
        })
        .collect();

    let relevant: std::collections::BTreeSet<&str> = query
        .relevant
        .iter()
        .filter(|g| g.relevance > 0)
        .map(|g| g.key.as_str())
        .collect();
    let top_relevant_score = if scores.len() == ranked.len() {
        ranked
            .iter()
            .zip(scores.iter())
            .filter(|(key, _)| relevant.contains(key.as_str()))
            .map(|(_, score)| *score)
            .reduce(f64::max)
    } else if ranked.iter().any(|k| relevant.contains(k.as_str())) {
        Some(top_score)
    } else {
        None
    };

    ScoredQuery {
        query_id: query.id.clone(),
        category: query.category.clone(),
        group_id: query.group_id.clone(),
        judgment_source: query.judgment_source,
        expect_abstention: query.expect_abstention,
        r_at_5: r_at_k(ranked, &judgments, 5),
        r_at_10: r_at_k(ranked, &judgments, 10),
        capacity_r_at_5: capacity_r_at_k(ranked, &judgments, 5),
        capacity_r_at_10: capacity_r_at_k(ranked, &judgments, 10),
        capacity_waived: query.capacity_waiver.is_some(),
        judgments_complete: query.judgments_complete,
        reciprocal_rank: mrr(ranked, &judgments),
        ndcg_at_5: ndcg_at_k(ranked, &judgments, 5),
        ndcg_at_10: ndcg_at_k(ranked, &judgments, 10),
        returned,
        top_score,
        top_relevant_score,
        latency_ms,
        unresolved_keys,
        // Set by the runner, which is the only caller that knows what it
        // removed or how hard it had to look; a directly-constructed row
        // excluded nothing and fetched nothing.
        excluded_hits: 0,
        physical_returned: returned,
        unresolved_hits: 0,
        fetch_rounds: 0,
        corpus_exhausted: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall_set::GradedKey;

    fn query(id: &str, category: &str, keys: &[&str], abstain: bool) -> RecallQuery {
        RecallQuery {
            id: id.into(),
            text: "t".into(),
            category: category.into(),
            relevant: keys
                .iter()
                .map(|k| GradedKey {
                    key: (*k).to_string(),
                    relevance: 1,
                })
                .collect(),
            exclude: Vec::new(),
            capacity_waiver: None,
            expect_abstention: abstain,
            judgment_source: JudgmentSource::Generated,
            group_id: None,
            note: None,
            judgments_complete: true,
        }
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let s = LatencyStats::from_samples(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        assert!((s.p50_ms - 5.0).abs() < 1e-9, "{s:?}");
        assert!((s.p95_ms - 10.0).abs() < 1e-9, "{s:?}");
        assert!((s.max_ms - 10.0).abs() < 1e-9);
        assert!((s.mean_ms - 5.5).abs() < 1e-9);
    }

    #[test]
    fn percentiles_handle_single_and_empty_samples() {
        assert!(LatencyStats::from_samples(&[]).p95_ms.abs() < f64::EPSILON);
        let one = LatencyStats::from_samples(&[4.0]);
        assert!((one.p50_ms - 4.0).abs() < 1e-9);
        assert!((one.p99_ms - 4.0).abs() < 1e-9);
    }

    #[test]
    fn perfect_ranking_scores_one() {
        let q = query("q", "direct", &["a"], false);
        let s = score_query(&q, &["a".into(), "b".into()], 2, 0.9, 1.0, 0);
        assert!((s.r_at_5 - 1.0).abs() < 1e-9);
        assert!((s.reciprocal_rank - 1.0).abs() < 1e-9);
        assert!((s.ndcg_at_5 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn missing_answer_scores_zero() {
        let q = query("q", "direct", &["a"], false);
        let s = score_query(&q, &["x".into(), "y".into()], 2, 0.5, 1.0, 0);
        assert!(s.r_at_5.abs() < f64::EPSILON);
        assert!(s.reciprocal_rank.abs() < f64::EPSILON);
    }

    #[test]
    fn abstention_queries_are_excluded_from_quality_but_scored_separately() {
        let good = score_query(&query("a1", "abstention", &[], true), &[], 0, 0.0, 1.0, 0);
        let bad = score_query(&query("a2", "abstention", &[], true), &[], 3, 0.8, 1.0, 0);
        let ranked = score_query(
            &query("d1", "direct", &["k"], false),
            &["k".into()],
            1,
            0.9,
            1.0,
            0,
        );

        let report = RecallReport::assemble("s", "baseline", vec![good, bad, ranked]);

        // Quality aggregates only over ranked queries.
        assert_eq!(report.overall.n_queries, 1);
        assert!((report.overall.r_at_5 - 1.0).abs() < 1e-9);

        assert_eq!(report.abstention.n_queries, 2);
        assert!((report.abstention.abstained - 0.5).abs() < 1e-9);
        assert!((report.abstention.mean_false_results - 1.5).abs() < 1e-9);
        assert!((report.abstention.max_false_score - 0.8).abs() < 1e-9);
    }

    #[test]
    fn a_waived_query_leaves_raw_recall_and_keeps_everything_else() {
        // The declared-exception path. `history` has 20 right answers at
        // K=10, so its raw R@10 tops out at 0.5 by construction and
        // averaging it in would drag the aggregate down by an amount
        // that has nothing to do with retrieval. It still contributes to
        // MRR, NDCG and the capacity-normalised recall, all of which it
        // can attain.
        let keys: Vec<String> = (0..20).map(|i| format!("v{i}")).collect();
        let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let mut history = query("history", "temporal-history", &key_refs, false);
        history.capacity_waiver = Some(crate::recall_set::CapacityWaiver {
            reason: "one right answer per version".into(),
        });

        // A perfect ten: every slot is a right answer.
        let top_ten: Vec<String> = (0..10).map(|i| format!("v{i}")).collect();
        let waived = score_query(&history, &top_ten, 10, 0.9, 1.0, 0);
        assert!((waived.r_at_10 - 0.5).abs() < 1e-12, "{waived:?}");
        assert!(
            (waived.capacity_r_at_10 - 1.0).abs() < 1e-12,
            "a full top ten of right answers is the most a ten-long list can do: {waived:?}"
        );
        assert!(waived.capacity_waived);

        let ordinary = score_query(
            &query("plain", "direct", &["k"], false),
            &["k".into()],
            1,
            0.9,
            1.0,
            0,
        );
        let report = RecallReport::assemble("s", "baseline", vec![waived, ordinary]);

        assert_eq!(report.overall.n_queries, 2);
        assert_eq!(report.overall.n_recall_queries, 1);
        assert!(
            (report.overall.r_at_10 - 1.0).abs() < 1e-12,
            "raw recall must average only the query where it is answerable: {:?}",
            report.overall
        );
        assert!(
            (report.overall.capacity_r_at_10 - 1.0).abs() < 1e-12,
            "capacity recall covers both: {:?}",
            report.overall
        );
        assert!((report.overall.mrr - 1.0).abs() < 1e-12);
        // And the exception is scoped to its own category, not the set.
        assert_eq!(report.by_category["direct"].n_recall_queries, 1);
        assert_eq!(report.by_category["temporal-history"].n_recall_queries, 0);
    }

    #[test]
    fn categories_and_sources_roll_up_separately() {
        let mut human = query("h", "multi-hop", &["k"], false);
        human.judgment_source = JudgmentSource::HumanReviewed;

        let scored = vec![
            score_query(
                &query("d", "direct", &["k"], false),
                &["k".into()],
                1,
                0.9,
                2.0,
                0,
            ),
            score_query(&human, &["miss".into()], 1, 0.5, 4.0, 0),
        ];
        let report = RecallReport::assemble("s", "baseline", scored);

        assert_eq!(report.by_category.len(), 2);
        assert!((report.by_category["direct"].r_at_5 - 1.0).abs() < 1e-9);
        assert!((report.by_category["multi-hop"].r_at_5).abs() < 1e-9);
        assert!((report.by_judgment_source["human-reviewed"].r_at_5).abs() < 1e-9);
        assert!((report.by_judgment_source["generated"].r_at_5 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn risk_coverage_curve_shows_the_tradeoff() {
        // Separable case: no-answer queries all score below answerable
        // ones, so some threshold silences everything while keeping
        // every answer.
        let abstain: Vec<ScoredQuery> = [0.1_f64, 0.2, 0.3]
            .iter()
            .enumerate()
            .map(|(i, s)| {
                score_query(
                    &query(&format!("a{i}"), "abstention", &[], true),
                    &[],
                    1,
                    *s,
                    1.0,
                    0,
                )
            })
            .collect();
        let answer: Vec<ScoredQuery> = [0.8_f64, 0.9]
            .iter()
            .enumerate()
            .map(|(i, s)| {
                score_query(
                    &query(&format!("d{i}"), "direct", &["k"], false),
                    &["k".into()],
                    1,
                    *s,
                    1.0,
                    0,
                )
            })
            .collect();

        let mut all = abstain.clone();
        all.extend(answer.clone());
        let report = RecallReport::assemble("s", "baseline", all);
        let curve = &report.abstention.risk_coverage;

        assert!(!curve.is_empty());
        // Somewhere in the curve a threshold silences every negative
        // while keeping every positive, because the scores separate.
        assert!(
            curve
                .iter()
                .any(|p| p.abstained >= 1.0 && p.answer_retained >= 1.0),
            "expected a clean operating point: {curve:?}"
        );
        // The curve is monotonic in abstention as the threshold rises.
        for pair in curve.windows(2) {
            assert!(pair[1].abstained >= pair[0].abstained - 1e-9);
        }
    }

    #[test]
    fn retention_tracks_the_relevant_hit_not_the_top_hit() {
        // The case that motivates `top_relevant_score`: an irrelevant
        // result scores 0.9 while the actual answer scores 0.2. A
        // threshold of 0.5 destroys this query's answer, and the curve
        // must say so rather than crediting the 0.9.
        let q = query("q", "direct", &["answer"], false);
        let scored = score_query_with_scores(
            &q,
            &["noise".into(), "answer".into()],
            &[0.9, 0.2],
            2,
            0.9,
            1.0,
            0,
        );
        assert!((scored.top_score - 0.9).abs() < 1e-9);
        assert!(
            scored
                .top_relevant_score
                .is_some_and(|v| (v - 0.2).abs() < 1e-9),
            "expected the relevant hit's score, got {:?}",
            scored.top_relevant_score
        );

        let negative = score_query_with_scores(
            &query("n", "abstention", &[], true),
            &["junk".into()],
            &[0.6],
            1,
            0.6,
            1.0,
            0,
        );
        let report = RecallReport::assemble("s", "b", vec![scored, negative]);
        let at_half = report
            .abstention
            .risk_coverage
            .iter()
            .find(|p| p.threshold >= 0.5 && p.threshold < 0.9);
        if let Some(point) = at_half {
            assert!(
                point.answer_retained.abs() < 1e-9,
                "a threshold above the relevant hit must not count it retained: {point:?}"
            );
        }
    }

    #[test]
    fn repeated_keys_score_as_if_the_document_appeared_once() {
        // The case file-granularity judging creates: three chunks of one
        // judged file come back at ranks 1-3 and a second judged file at
        // rank 4. All three chunks resolve to the same key, so the
        // ranked list the scorer sees must behave exactly as [A, B].
        let q = query("q", "commit-to-file", &["fileA", "fileB"], false);
        let collapsed = score_query_with_scores(
            &q,
            &[
                "fileA".into(),
                "fileA".into(),
                "fileA".into(),
                "fileB".into(),
            ],
            &[0.9, 0.8, 0.7, 0.6],
            4,
            0.9,
            1.0,
            0,
        );
        let ideal = score_query_with_scores(
            &q,
            &["fileA".into(), "fileB".into()],
            &[0.9, 0.6],
            2,
            0.9,
            1.0,
            0,
        );

        for (name, got, want) in [
            ("R@5", collapsed.r_at_5, ideal.r_at_5),
            ("R@10", collapsed.r_at_10, ideal.r_at_10),
            ("MRR", collapsed.reciprocal_rank, ideal.reciprocal_rank),
            ("NDCG@5", collapsed.ndcg_at_5, ideal.ndcg_at_5),
            ("NDCG@10", collapsed.ndcg_at_10, ideal.ndcg_at_10),
        ] {
            assert!(
                (got - want).abs() < 1e-12,
                "{name}: {got} != {want} — duplicates must not change the score"
            );
        }
        // Both judged files are found, so this is a perfect result.
        assert!((collapsed.ndcg_at_10 - 1.0).abs() < 1e-12);
        assert!((collapsed.r_at_10 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn duplicates_do_not_evict_a_later_answer_from_the_top_ten() {
        // Without collapsing, the eight repeats of `a` would push the
        // second answer out of the top 10 and halve R@10.
        let q = query("q", "c", &["a", "late"], false);
        let mut ranked: Vec<String> = std::iter::repeat_n("a".to_string(), 9).collect();
        ranked.push("noise".into());
        ranked.push("late".into());
        let scored = score_query(&q, &ranked, ranked.len(), 0.9, 1.0, 0);
        assert!((scored.r_at_10 - 1.0).abs() < 1e-12, "{scored:?}");
    }

    #[test]
    fn duplicates_keep_the_first_occurrence_score() {
        let q = query("q", "c", &["a"], false);
        let scored =
            score_query_with_scores(&q, &["a".into(), "a".into()], &[0.9, 0.1], 2, 0.9, 1.0, 0);
        assert!(scored
            .top_relevant_score
            .is_some_and(|v| (v - 0.9).abs() < 1e-12));
    }

    #[test]
    fn queries_with_no_relevant_hit_are_never_retained() {
        let q = query("q", "direct", &["missing"], false);
        let scored = score_query_with_scores(&q, &["noise".into()], &[0.9], 1, 0.9, 1.0, 0);
        assert!(scored.top_relevant_score.is_none());
    }

    #[test]
    fn risk_coverage_curve_is_empty_without_negatives() {
        let scored = vec![score_query(
            &query("d", "direct", &["k"], false),
            &["k".into()],
            1,
            0.9,
            1.0,
            0,
        )];
        let report = RecallReport::assemble("s", "baseline", scored);
        assert!(report.abstention.risk_coverage.is_empty());
    }

    #[test]
    fn unresolved_keys_are_totalled() {
        let scored = vec![
            score_query(
                &query("a", "direct", &["k"], false),
                &["k".into()],
                1,
                0.9,
                1.0,
                2,
            ),
            score_query(
                &query("b", "direct", &["j"], false),
                &["j".into()],
                1,
                0.9,
                1.0,
                3,
            ),
        ];
        let report = RecallReport::assemble("s", "baseline", scored);
        assert_eq!(report.unresolved_keys, 5);
    }
}
