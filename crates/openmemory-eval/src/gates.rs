//! Release gates for retrieval quality.
//!
//! Latency budgets alone cannot tell a faster system from a better one.
//! A change that halves p95 while returning the wrong memories is a
//! regression, and until these gates existed the project had no way to
//! say so: it specified latency, memory, disk, and crash budgets in
//! detail and nothing about whether retrieval got worse.
//!
//! ## What a gate has to survive to be worth having
//!
//! A gate that never fires is indistinguishable from no gate. So the
//! test suite here does not only check that the comparison logic is
//! correct on hand-built inputs; it checks **sensitivity** — that each
//! gate fires on the degradation it exists to catch — and
//! **specificity** — that it does not fire on noise or on an unchanged
//! run. Both properties are what makes a green gate mean something.
//!
//! ## Why per-category and not only aggregate
//!
//! An aggregate can improve while the category a change was meant to
//! fix gets worse. Measured on this project: a graph channel gained
//! 0.027 R@10 on 18 queries while losing 0.696 on 454, and the two
//! partially cancel in any single number. Categories are gated
//! individually for that reason.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::recall_report::RecallReport;

/// Thresholds a run must satisfy.
///
/// Defaults are deliberately loose enough to survive ordinary
/// measurement jitter and tight enough to catch the regressions this
/// project has actually seen, the smallest of which moved a category by
/// about 2 points.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGates {
    /// Largest tolerated drop in any single category's NDCG@10.
    pub max_category_drop: f64,
    /// Largest tolerated drop in an aggregate metric.
    pub max_aggregate_drop: f64,
    /// Whether every category present in the baseline must still be
    /// present. A category silently vanishing from a report reads as
    /// "no regression" while actually being total loss of coverage.
    pub require_all_categories: bool,
    /// Smallest tolerated value of `RecallCeiling::mean_ceiling`.
    ///
    /// 1.0 demands a query set on which perfect retrieval would score
    /// `R@10 = 1.0` on average. It is the weakest of the three ceiling
    /// thresholds and must never be the only one: a mean is a budget,
    /// and unrelated easy queries pay into it. Do not lower this to
    /// accommodate a question that genuinely has more than K right
    /// answers — declare a
    /// [`crate::recall_set::CapacityWaiver`] on that query instead, so
    /// the exception names itself rather than authorising every future
    /// qrel defect.
    pub min_recall_ceiling: f64,
    /// Smallest tolerated ceiling for any **single** query.
    ///
    /// This is the threshold padding cannot move. A query judged against
    /// 30 targets at K=10 has a ceiling of 0.33 whether it sits among 30
    /// queries or 30 000.
    #[serde(default = "one")]
    pub min_query_ceiling: f64,
    /// Most capped queries tolerated, as an absolute count.
    ///
    /// A count rather than a share, for the same reason: a share falls
    /// as the set grows, so it would let a large set carry more defects
    /// than a small one for no reason connected to the defects.
    #[serde(default)]
    pub max_capped_queries: usize,
    /// Largest tolerated drop in the fraction of no-answer queries the
    /// system correctly stays quiet on.
    ///
    /// Paired with [`Self::max_aggregate_drop`] this is what makes
    /// abstention gateable at all: silencing everything would ace this
    /// gate and fail the aggregate one, and answering everything does
    /// the reverse. Neither gate is safe alone.
    pub max_abstention_drop: f64,
    /// Whether both reports must carry a corpus-receipt digest.
    ///
    /// Defaults to **false**, which is the exception rather than the
    /// policy. The instrument's own sensitivity suite builds in-memory
    /// stores that have no source tree to fingerprint, and forcing a
    /// receipt there would mean inventing one — a fake receipt is worse
    /// than an absent one, because it certifies nothing while looking
    /// like it certifies something.
    ///
    /// The release path turns it on. `corpus gate` sets it, and the
    /// stored verdict records the value, so a run gated without receipt
    /// binding is visible rather than assumed.
    pub require_corpus_receipt: bool,
}

impl Default for QualityGates {
    fn default() -> Self {
        Self {
            max_category_drop: 0.01,
            max_aggregate_drop: 0.01,
            require_all_categories: true,
            min_recall_ceiling: 1.0,
            min_query_ceiling: 1.0,
            max_capped_queries: 0,
            max_abstention_drop: 0.05,
            require_corpus_receipt: false,
        }
    }
}

/// Serde default for thresholds whose neutral value is 1.0, so a
/// verdict written before they existed does not read as "no limit".
fn one() -> f64 {
    1.0
}

/// What a gate concluded.
///
/// Three-valued because two is a lie for a slice whose judgments are not
/// exhaustive. Over partial labels a metric movement has no known
/// direction — an arm that surfaces a correct-but-unjudged document
/// displaces a judged one and reads as a regression — so a gate over
/// such a slice can be *diagnostic* without being able to approve or
/// block a release. Encoding that only in the detail text left the
/// machine-readable verdict saying `PASS`/`FAIL` with confidence nobody
/// had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    Fail,
    /// The slice moved, but its labels are incomplete, so the movement
    /// cannot decide anything until the union of the arms is
    /// adjudicated.
    IndeterminateNeedsAdjudication,
}

impl Verdict {
    /// Whether this verdict permits a release on its own.
    ///
    /// `Indeterminate` does not: it is neither a pass nor a failure, and
    /// treating it as either is the error this enum exists to prevent.
    #[must_use]
    pub fn is_pass(self) -> bool {
        matches!(self, Self::Pass)
    }

    /// Whether this verdict blocks on its own.
    #[must_use]
    pub fn blocks(self) -> bool {
        matches!(self, Self::Fail)
    }
}

/// One gate's outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateOutcome {
    pub name: String,
    /// Kept for readers that only ask "did anything object?". False for
    /// both `Fail` and `Indeterminate`, because an indeterminate slice
    /// has not established that the run is fine.
    pub passed: bool,
    /// Why it failed, in terms a reader can act on.
    pub detail: Option<String>,
    /// The three-valued conclusion. Defaulted on deserialisation so
    /// verdicts written before this existed still load, as `Pass` when
    /// `passed` and `Fail` otherwise — which is what they meant.
    #[serde(default)]
    pub verdict: Option<Verdict>,
}

impl GateOutcome {
    /// The verdict, falling back to the boolean for older records.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        self.verdict.unwrap_or(if self.passed {
            Verdict::Pass
        } else {
            Verdict::Fail
        })
    }
}

/// Bumped when the set of gates or their meaning changes, so an old
/// stored verdict is not read as if it certified today's checks.
///
/// - v2: `recall_ceiling` gained per-query and per-count thresholds and
///   a typed capacity exception. A v1 verdict certifies only that the
///   *mean* ceiling cleared its threshold, which a large enough query
///   set could satisfy while carrying an unreachable query.
pub const GATE_SCHEMA_VERSION: u32 = 2;

/// Result of evaluating every gate — and a durable record of what was
/// checked, against what, under which thresholds.
///
/// A stored file containing only outcomes says "it passed" without
/// saying what passed. The provenance fields exist so a release verdict
/// can be re-read months later and still mean something: the same
/// argument that produced [`crate::contract::EvaluationContract`], one
/// level up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateReport {
    #[serde(default)]
    pub schema_version: u32,
    /// Thresholds in force. A verdict is only as strong as the limits it
    /// was measured against, and those are configurable.
    #[serde(default)]
    pub gates: Option<QualityGates>,
    #[serde(default)]
    pub dataset: String,
    #[serde(default)]
    pub strategy: String,
    /// Contract of the run under test.
    #[serde(default)]
    pub contract: Option<crate::contract::EvaluationContract>,
    /// Contract of the baseline it was compared against, if any.
    #[serde(default)]
    pub baseline_contract: Option<crate::contract::EvaluationContract>,
    /// Capacity exceptions in force for this run, with the reasons given.
    ///
    /// A verdict that says PASS while some queries were excused from the
    /// ceiling gate has to say which ones and why, or the exception is
    /// invisible to exactly the person auditing the PASS. Their ids and
    /// reasons are also part of the query-set digest, so a waiver cannot
    /// be moved to a different query without breaking comparability.
    #[serde(default)]
    pub capacity_waivers: Vec<crate::recall_report::WaivedQuery>,
    pub outcomes: Vec<GateOutcome>,
}

impl GateReport {
    /// Outcomes with no provenance. Used where the caller supplies the
    /// context itself; [`evaluate`] fills the rest in.
    #[must_use]
    fn bare(outcomes: Vec<GateOutcome>) -> Self {
        Self {
            schema_version: GATE_SCHEMA_VERSION,
            gates: None,
            dataset: String::new(),
            strategy: String::new(),
            contract: None,
            baseline_contract: None,
            capacity_waivers: Vec::new(),
            outcomes,
        }
    }

    #[must_use]
    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(|o| o.passed)
    }

    /// Outcomes that could not decide, because their slice's labels are
    /// incomplete.
    ///
    /// Separate from [`Self::failures`] on purpose. A failure says
    /// "this got worse"; an indeterminate says "this moved and the
    /// evidence cannot say which way". Collapsing them either blocks
    /// releases on unknowns or ships past them, and which one you get is
    /// an accident of how the boolean was written.
    #[must_use]
    pub fn indeterminate(&self) -> Vec<&GateOutcome> {
        self.outcomes
            .iter()
            .filter(|o| o.verdict() == Verdict::IndeterminateNeedsAdjudication)
            .collect()
    }

    /// Outcomes that block a release on their own.
    #[must_use]
    pub fn blocking(&self) -> Vec<&GateOutcome> {
        self.outcomes
            .iter()
            .filter(|o| o.verdict().blocks())
            .collect()
    }

    /// Failures only, for a build log that should be short when green.
    #[must_use]
    pub fn failures(&self) -> Vec<&GateOutcome> {
        self.outcomes.iter().filter(|o| !o.passed).collect()
    }

    /// Write the verdict where a release process can find it.
    ///
    /// Written to a sibling temporary file and renamed, so a reader
    /// never observes a half-written verdict and a crashed run leaves
    /// the previous one intact rather than a truncated file that parses
    /// as "no failures".
    pub fn write_atomic(&self, path: &std::path::Path) -> std::io::Result<()> {
        let body = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&tmp, &body)?;
        std::fs::rename(&tmp, path)
    }
}

/// Compare a run against a recorded baseline.
///
/// `None` for the baseline means this run *is* the baseline; only the
/// gates that do not need a comparison are evaluated, so a first run
/// still catches a query set whose R@10 could never reach 1.0 or whose
/// judgments no longer resolve.
#[must_use]
pub fn evaluate(
    current: &RecallReport,
    baseline: Option<&RecallReport>,
    gates: &QualityGates,
) -> GateReport {
    let mut outcomes = Vec::new();
    let provenance = |outcomes: Vec<GateOutcome>| GateReport {
        gates: Some(gates.clone()),
        dataset: current.dataset.clone(),
        strategy: current.strategy.clone(),
        contract: current.contract.clone(),
        baseline_contract: baseline.and_then(|b| b.contract.clone()),
        capacity_waivers: current
            .recall_ceiling
            .as_ref()
            .map(|c| c.waived.clone())
            .unwrap_or_default(),
        ..GateReport::bare(outcomes)
    };

    if let Some(base) = baseline {
        // Comparability first. Scores from two different experiments are
        // not a regression signal, and comparing them anyway is how a
        // baseline from another corpus or an easier query subset passes.
        let comparability = comparability_gate(current, base);
        let comparable = comparability.passed;
        outcomes.push(comparability);
        if !comparable {
            return provenance(outcomes);
        }
        if gates.require_corpus_receipt {
            let missing: Vec<&str> = [
                ("baseline", base.contract.as_ref()),
                ("current", current.contract.as_ref()),
            ]
            .into_iter()
            .filter(|(_, c)| c.is_none_or(|c| c.corpus_receipt.is_none()))
            .map(|(side, _)| side)
            .collect();
            outcomes.push(GateOutcome {
                name: "corpus_receipt".into(),
                passed: missing.is_empty(),
                detail: (!missing.is_empty()).then(|| {
                    format!(
                        "no corpus receipt on: {}. Without one the comparison is bound                          to a directory name, and two different corpora can sit at the                          same path on different days",
                        missing.join(", ")
                    )
                }),
                verdict: None,
            });
        }
        outcomes.push(aggregate_gate(current, base, gates));
        outcomes.extend(category_gates(current, base, gates));
        outcomes.extend(judgment_source_gates(current, base, gates));
        outcomes.push(abstention_gate(current, base, gates));
        if gates.require_all_categories {
            outcomes.push(coverage_gate(current, base));
        }
    }

    outcomes.push(unresolved_gate(current));
    outcomes.push(abstention_sanity_gate(current));
    outcomes.push(recall_ceiling_gate(current, gates));
    outcomes.push(wellformed_gate(current, baseline, gates));

    provenance(outcomes)
}

/// Refuse to compare reports that describe different experiments.
fn comparability_gate(current: &RecallReport, baseline: &RecallReport) -> GateOutcome {
    match (&baseline.contract, &current.contract) {
        (Some(base), Some(now)) => {
            let diffs = base.differences(now);
            GateOutcome {
                name: "comparable_experiment".into(),
                passed: diffs.is_empty(),
                detail: (!diffs.is_empty()).then(|| {
                    format!(
                        "baseline and current describe different experiments, so \
                         their scores are not a regression signal: {}",
                        diffs.join("; ")
                    )
                }),
                verdict: None,
            }
        }
        _ => GateOutcome {
            name: "comparable_experiment".into(),
            passed: false,
            detail: Some(
                "a report is missing its evaluation contract, so comparability \
                 cannot be established; re-run to produce one"
                    .into(),
            ),
            verdict: None,
        },
    }
}

fn aggregate_gate(
    current: &RecallReport,
    baseline: &RecallReport,
    gates: &QualityGates,
) -> GateOutcome {
    let metrics: [(&str, f64, f64); 3] = [
        ("R@10", current.overall.r_at_10, baseline.overall.r_at_10),
        ("MRR", current.overall.mrr, baseline.overall.mrr),
        (
            "NDCG@10",
            current.overall.ndcg_at_10,
            baseline.overall.ndcg_at_10,
        ),
    ];
    let regressions: Vec<String> = metrics
        .iter()
        .filter(|(_, now, before)| before - now > gates.max_aggregate_drop)
        .map(|(name, now, before)| format!("{name} {before:.4} -> {now:.4}"))
        .collect();

    GateOutcome {
        name: "aggregate_quality".into(),
        passed: regressions.is_empty(),
        detail: (!regressions.is_empty()).then(|| {
            format!(
                "aggregate metrics dropped more than {:.3}: {}",
                gates.max_aggregate_drop,
                regressions.join(", ")
            )
        }),
        verdict: None,
    }
}

fn category_gates(
    current: &RecallReport,
    baseline: &RecallReport,
    gates: &QualityGates,
) -> Vec<GateOutcome> {
    let mut out = Vec::new();
    for (category, base) in &baseline.by_category {
        if base.n_queries == 0 {
            continue;
        }
        let Some(now) = current.by_category.get(category) else {
            continue; // coverage gate owns this case
        };
        let drop = base.ndcg_at_10 - now.ndcg_at_10;
        // A slice whose judgments are not exhaustive fails in a way a
        // reader will otherwise misdiagnose: a change that surfaces a
        // *correct but unjudged* document scores it as a miss and pushes
        // a judged one down, which is indistinguishable here from
        // retrieval getting worse. Saying so changes what the reader
        // does next — look at the new top hits before reverting.
        let incomplete = !now.judgments_complete || !base.judgments_complete;
        let moved = drop > gates.max_category_drop;

        // Three-valued, because two is a lie here. Over partial labels a
        // drop has no known direction: an arm that surfaces a
        // correct-but-unjudged document displaces a judged one and reads
        // as a regression, while an arm that overfits the miner's known
        // subset reads as an improvement. Encoding that only in prose
        // left the machine-readable verdict saying FAIL with a
        // confidence nobody had, and a release process reads the field,
        // not the sentence.
        let verdict = match (moved, incomplete) {
            (false, _) => Verdict::Pass,
            (true, false) => Verdict::Fail,
            (true, true) => Verdict::IndeterminateNeedsAdjudication,
        };

        out.push(GateOutcome {
            name: format!("category:{category}"),
            // Indeterminate is not a pass: it has not established that
            // the run is fine, only that this slice cannot say.
            passed: verdict.is_pass(),
            detail: moved.then(|| {
                let caveat = if incomplete {
                    ". This category's judgments are not exhaustive, so this movement \
                     cannot decide anything: a correct-but-unjudged document displacing \
                     a judged one produces exactly this signal, and so does a real \
                     regression. Adjudicate the union of both arms' top results, freeze \
                     a new query-set version, and rerun before treating it either way"
                } else {
                    ""
                };
                format!(
                    "NDCG@10 {:.4} -> {:.4} (drop {:.4}, limit {:.3}); an aggregate \
                     can hide this{caveat}",
                    base.ndcg_at_10, now.ndcg_at_10, drop, gates.max_category_drop
                )
            }),
            verdict: Some(verdict),
        });
    }
    out
}

fn coverage_gate(current: &RecallReport, baseline: &RecallReport) -> GateOutcome {
    let missing: Vec<&str> = baseline
        .by_category
        .iter()
        .filter(|(name, base)| base.n_queries > 0 && !current.by_category.contains_key(*name))
        .map(|(name, _)| name.as_str())
        .collect();

    GateOutcome {
        name: "category_coverage".into(),
        passed: missing.is_empty(),
        detail: (!missing.is_empty()).then(|| {
            format!(
                "categories present in the baseline are absent now: {}. A vanished \
                 category reads as 'no regression' while being total loss of coverage",
                missing.join(", ")
            )
        }),
        verdict: None,
    }
}

fn unresolved_gate(current: &RecallReport) -> GateOutcome {
    GateOutcome {
        name: "judgments_resolve".into(),
        passed: current.unresolved_keys == 0,
        detail: (current.unresolved_keys > 0).then(|| {
            format!(
                "{} judged keys did not resolve to any store row; a stale query set \
                 is indistinguishable from a quality regression",
                current.unresolved_keys
            )
        }),
        verdict: None,
    }
}

/// Catches a report whose abstention slice is internally impossible.
///
/// Cheap, and it has caught a real defect class: a metric that cannot
/// reach its own maximum reads as a permanent failure nobody can fix.
fn abstention_sanity_gate(current: &RecallReport) -> GateOutcome {
    let a = &current.abstention;
    let impossible = a.n_queries > 0 && !(0.0..=1.0).contains(&a.abstained);
    GateOutcome {
        name: "abstention_wellformed".into(),
        passed: !impossible,
        detail: impossible.then(|| format!("abstention rate {:.4} is outside [0, 1]", a.abstained)),
        verdict: None,
    }
}

/// Quality per judgment provenance, so mined judgments cannot quietly
/// carry a decision that the hand-reviewed slice contradicts.
///
/// Deliberately does *not* require a human-reviewed slice to exist. The
/// query sets are currently all generated, and a gate that demands
/// something the corpus does not have is a gate somebody switches off.
/// What it does require is that a slice which existed in the baseline
/// still exists and has not regressed — so the moment hand-reviewed
/// judgments land, they are protected from being averaged away by
/// generated volume.
fn judgment_source_gates(
    current: &RecallReport,
    baseline: &RecallReport,
    gates: &QualityGates,
) -> Vec<GateOutcome> {
    let mut out = Vec::new();
    for (source, base) in &baseline.by_judgment_source {
        if base.n_queries == 0 {
            continue;
        }
        let name = format!("judgment_source:{source}");
        let Some(now) = current.by_judgment_source.get(source) else {
            out.push(GateOutcome {
                name,
                passed: false,
                detail: Some(format!(
                    "the {source} slice had {} queries in the baseline and is absent \
                     now; a slice that vanishes reads as 'no regression'",
                    base.n_queries
                )),
                verdict: None,
            });
            continue;
        };
        if now.n_queries == 0 {
            out.push(GateOutcome {
                name,
                passed: false,
                detail: Some(format!(
                    "the {source} slice dropped from {} queries to none",
                    base.n_queries
                )),
                verdict: None,
            });
            continue;
        }
        let drop = base.ndcg_at_10 - now.ndcg_at_10;
        out.push(GateOutcome {
            name,
            passed: drop <= gates.max_category_drop,
            detail: (drop > gates.max_category_drop).then(|| {
                format!(
                    "{source} NDCG@10 {:.4} -> {:.4} (drop {:.4}, limit {:.3}) over {} \
                     queries; the overall mean can hide this behind another slice",
                    base.ndcg_at_10, now.ndcg_at_10, drop, gates.max_category_drop, now.n_queries
                )
            }),
            verdict: None,
        });
    }
    out
}

/// Protect no-answer behaviour, which the ranking metrics cannot see.
///
/// Abstention queries are excluded from every `QualityMetrics` rollup by
/// construction, so their categories report `n_queries: 0` and the
/// per-category gates skip them entirely. A change that starts
/// confidently answering questions with no answer therefore moved
/// nothing the other gates look at.
///
/// Two failures are distinguished because they need different fixes: the
/// slice disappearing (a harness or query-set problem) and the rate
/// falling (a retrieval problem).
fn abstention_gate(
    current: &RecallReport,
    baseline: &RecallReport,
    gates: &QualityGates,
) -> GateOutcome {
    let base = &baseline.abstention;
    let now = &current.abstention;

    if base.n_queries == 0 {
        return GateOutcome {
            name: "abstention_quality".into(),
            passed: true,
            detail: None,
            verdict: None,
        };
    }
    if now.n_queries == 0 {
        return GateOutcome {
            name: "abstention_quality".into(),
            passed: false,
            detail: Some(format!(
                "the baseline scored {} no-answer queries and this run scored none; \
                 abstention is invisible to every ranking metric, so losing the slice \
                 silently removes the only check on it",
                base.n_queries
            )),
            verdict: None,
        };
    }

    let drop = base.abstained - now.abstained;
    // Clusters, not rows, are the sample. Negatives are generated as
    // many surface forms of one subject, so quoting the row count would
    // lend an interval it has not earned.
    let sample = |m: &crate::recall_report::AbstentionMetrics| {
        if m.n_groups > 0 && m.n_groups < m.n_queries {
            format!("{} queries from {} clusters", m.n_queries, m.n_groups)
        } else {
            format!("{} queries", m.n_queries)
        }
    };

    GateOutcome {
        name: "abstention_quality".into(),
        passed: drop <= gates.max_abstention_drop,
        detail: (drop > gates.max_abstention_drop).then(|| {
            format!(
                "abstention rate {:.4} -> {:.4} (drop {:.4}, limit {:.3}) over {}; \
                 the system started answering questions that have no answer",
                base.abstained,
                now.abstained,
                drop,
                gates.max_abstention_drop,
                sample(now)
            )
        }),
        verdict: None,
    }
}

/// Refuse numbers that make every comparison vacuous.
///
/// Every quality gate is a comparison of the form `before - now > limit`.
/// If `now` is NaN that expression is **false**, so a report full of NaN
/// passes every gate in this file while measuring nothing. The same is
/// true from the other side: a NaN limit makes its gate unfireable. Both
/// read as green, which is the one outcome a gate must never produce
/// without evidence.
///
/// Checked last but reported alongside the rest, because a caller that
/// sees only "aggregate_quality passed" should also see why that was not
/// worth believing.
fn wellformed_gate(
    current: &RecallReport,
    baseline: Option<&RecallReport>,
    gates: &QualityGates,
) -> GateOutcome {
    let mut problems = Vec::new();

    fn metric(problems: &mut Vec<String>, label: &str, m: &crate::recall_report::QualityMetrics) {
        for (name, value) in [
            ("R@5", m.r_at_5),
            ("R@10", m.r_at_10),
            ("MRR", m.mrr),
            ("NDCG@5", m.ndcg_at_5),
            ("NDCG@10", m.ndcg_at_10),
        ] {
            if !value.is_finite() {
                problems.push(format!("{label} {name} is {value}"));
            }
        }
    }
    // Both sides. A non-finite baseline fools the comparison exactly as
    // a non-finite current run does, and a stored baseline is the value
    // least likely to be looked at again.
    for (side, report) in [("current", Some(current)), ("baseline", baseline)] {
        let Some(report) = report else { continue };
        metric(&mut problems, side, &report.overall);
        for (name, m) in &report.by_category {
            metric(&mut problems, &format!("{side} category {name}"), m);
        }
        for (name, m) in &report.by_judgment_source {
            metric(&mut problems, &format!("{side} source {name}"), m);
        }
        if !report.abstention.abstained.is_finite() {
            problems.push(format!(
                "{side} abstention rate is {}",
                report.abstention.abstained
            ));
        }
    }

    for (name, limit) in [
        ("max_category_drop", gates.max_category_drop),
        ("max_aggregate_drop", gates.max_aggregate_drop),
        ("max_abstention_drop", gates.max_abstention_drop),
        ("min_recall_ceiling", gates.min_recall_ceiling),
        ("min_query_ceiling", gates.min_query_ceiling),
    ] {
        if !limit.is_finite() {
            problems.push(format!(
                "threshold {name} is {limit}, so its gate can never fire"
            ));
        } else if limit < 0.0 {
            // Not unsafe — a negative tolerance is stricter than zero —
            // but it is almost certainly a typo, and a gate nobody meant
            // to set that tight gets disabled rather than investigated.
            problems.push(format!("threshold {name} is negative ({limit})"));
        }
    }
    // The ceiling thresholds are compared against a ratio, so anything
    // above 1.0 demands the impossible and anything below 0.0 forbids
    // nothing. Both make the gate say something other than what its name
    // implies, and above 1.0 is the worse of the two: it fires on every
    // run, and a gate that always fires gets switched off.
    for (name, limit) in [
        ("min_recall_ceiling", gates.min_recall_ceiling),
        ("min_query_ceiling", gates.min_query_ceiling),
    ] {
        if limit.is_finite() && !(0.0..=1.0).contains(&limit) {
            problems.push(format!(
                "threshold {name} is {limit}, outside the 0..=1 range a ceiling ratio \
                 can take"
            ));
        }
    }

    GateOutcome {
        name: "wellformed".into(),
        passed: problems.is_empty(),
        detail: (!problems.is_empty()).then(|| {
            format!(
                "the comparison is vacuous: {}. Every quality gate here asks \
                 whether a drop exceeds a limit, and a non-finite value on \
                 either side makes that question answer 'no' regardless of \
                 what happened",
                problems.join("; ")
            )
        }),
        verdict: None,
    }
}

/// Refuse a query set on which perfect retrieval could not score 1.0.
///
/// Needs no baseline: a ceiling below 1.0 makes part of every reported
/// R@10 a property of the judgments rather than the system, and that is
/// worth knowing on the very first run.
fn recall_ceiling_gate(current: &RecallReport, gates: &QualityGates) -> GateOutcome {
    let Some(ceiling) = &current.recall_ceiling else {
        return GateOutcome {
            name: "recall_ceiling".into(),
            passed: false,
            detail: Some(
                "the report carries no recall ceiling, so it is not known whether its \
                 R@10 could reach 1.0; re-run to produce one"
                    .into(),
            ),
            verdict: None,
        };
    };
    let k = ceiling.k;
    let mut problems: Vec<String> = Vec::new();

    // Per-query first: it is the check that a large query set cannot
    // dilute, and therefore the one that actually binds.
    let Some(min_ceiling) = ceiling.min_ceiling else {
        return GateOutcome {
            name: "recall_ceiling".into(),
            passed: false,
            detail: Some(
                "this report predates the per-query ceiling statistic, so it can say                  what its query set scored on average but not whether any single query                  was unreachable — the exact question a mean cannot answer. Re-run to                  produce one"
                    .into(),
            ),
            verdict: None,
        };
    };
    if min_ceiling < gates.min_query_ceiling - 1e-9 {
        let worst: Vec<String> = ceiling
            .worst
            .iter()
            .map(|(id, n)| format!("{id} ({n} targets)"))
            .collect();
        problems.push(format!(
            "the worst single query admits R@{k} of only {:.4}, below the required \
             {:.4}. Worst: {}",
            min_ceiling,
            gates.min_query_ceiling,
            worst.join(", "),
        ));
    }
    if ceiling.capped_queries > gates.max_capped_queries {
        problems.push(format!(
            "{} of {} answerable queries are judged against more than {k} targets \
             ({:.1}% of the set), above the tolerated {}",
            ceiling.capped_queries,
            ceiling.n_queries,
            ceiling.capped_share * 100.0,
            gates.max_capped_queries,
        ));
    }
    if ceiling.mean_ceiling < gates.min_recall_ceiling - 1e-9 {
        problems.push(format!(
            "the mean attainable R@{k} is {:.4}, below the required {:.4}",
            ceiling.mean_ceiling, gates.min_recall_ceiling,
        ));
    }
    // A category can be wholly capped and still cost the set-wide mean
    // almost nothing, so it is named rather than left to be inferred.
    let hurt: Vec<String> = ceiling
        .by_category
        .iter()
        .filter(|(_, stats)| stats.capped_queries > 0)
        .map(|(name, stats)| {
            format!(
                "{name} ({}/{} capped, mean {:.4}, worst {:.4})",
                stats.capped_queries, stats.n_queries, stats.mean_ceiling, stats.min_ceiling
            )
        })
        .collect();
    if !problems.is_empty() && !hurt.is_empty() {
        problems.push(format!("by category: {}", hurt.join(", ")));
    }
    // A waiver granted for a query that no longer needs it is a standing
    // permission nobody remembers, and the next query to inherit that id
    // would inherit the exception with it.
    if !ceiling.unused_waivers.is_empty() {
        problems.push(format!(
            "{} capacity waiver(s) no longer apply — their queries now fit inside {k} \
             targets, so the declaration is stale and should be deleted: {}",
            ceiling.unused_waivers.len(),
            ceiling.unused_waivers.join(", "),
        ));
    }

    GateOutcome {
        name: "recall_ceiling".into(),
        passed: problems.is_empty(),
        detail: (!problems.is_empty()).then(|| {
            let waived = if ceiling.waived.is_empty() {
                "none".to_string()
            } else {
                ceiling
                    .waived
                    .iter()
                    .map(|w| format!("{} ({} targets: {})", w.query_id, w.judged, w.reason))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            format!(
                "{}. Only raw R@{k} is capped — NDCG@{k} computes its ideal DCG over \
                 the top {k} judgments and can still reach 1.0, and MRR is unaffected \
                 — so read this as 'R@{k} is a mixture', not 'the run is invalid'. If \
                 a question genuinely has more than {k} right answers, declare a \
                 capacity waiver on that query with a reason; do not lower a global \
                 threshold, which would also authorise every unrelated qrel defect. \
                 Waivers in force: {waived}",
                problems.join(". "),
            )
        }),
        verdict: None,
    }
}

/// Roll a set of per-category scores into the shape `evaluate` compares.
///
/// Exposed so a caller assembling a report from another source can reuse
/// the same gate logic rather than reimplementing the comparison.
#[must_use]
pub fn category_ndcg(report: &RecallReport) -> BTreeMap<String, f64> {
    report
        .by_category
        .iter()
        .map(|(name, m)| (name.clone(), m.ndcg_at_10))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall_report::{QualityMetrics, RecallCeiling, ScoredQuery};
    use crate::recall_set::{GradedKey, JudgmentSource, RecallQuery, RecallQuerySet};

    fn scored(id: &str, category: &str, ndcg: f64, abstain: bool) -> ScoredQuery {
        ScoredQuery {
            query_id: id.into(),
            category: category.into(),
            judgment_source: JudgmentSource::Generated,
            expect_abstention: abstain,
            r_at_5: ndcg,
            r_at_10: ndcg,
            capacity_r_at_5: ndcg,
            capacity_r_at_10: ndcg,
            capacity_waived: false,
            reciprocal_rank: ndcg,
            ndcg_at_5: ndcg,
            ndcg_at_10: ndcg,
            returned: 1,
            top_score: 0.9,
            top_relevant_score: Some(0.9),
            group_id: None,
            latency_ms: 1.0,
            unresolved_keys: 0,
            excluded_hits: 0,
            physical_returned: 1,
            unresolved_hits: 0,
            fetch_rounds: 1,
            corpus_exhausted: true,
            judgments_complete: true,
        }
    }

    /// The contract every fixture shares, so comparability passes and
    /// the quality gates are the thing under test.
    fn test_contract() -> crate::contract::EvaluationContract {
        crate::contract::EvaluationContract {
            metric_version: crate::contract::METRIC_VERSION,
            query_set_digest: "fixture-digest".into(),
            query_count: 4,
            corpus: "fixture".into(),
            corpus_receipt: Some("fixture-receipt".into()),
            mode: "KeywordOnly".into(),
            top_k: 10,
            spreading_activation: true,
            record_access: false,
            decay_rate: Some(0.01),
        }
    }

    /// A query set whose judgments all fit inside K, so the ceiling gate
    /// passes and the gate under test is the thing being measured.
    fn clean_ceiling() -> RecallCeiling {
        RecallCeiling {
            k: 10,
            n_queries: 4,
            mean_ceiling: 1.0,
            min_ceiling: Some(1.0),
            capped_queries: 0,
            capped_share: 0.0,
            worst: Vec::new(),
            by_category: BTreeMap::new(),
            waived: Vec::new(),
            unused_waivers: Vec::new(),
        }
    }

    /// A report with two categories at the given quality.
    fn report(direct: f64, multihop: f64) -> RecallReport {
        from_scored(vec![
            scored("d1", "direct", direct, false),
            scored("d2", "direct", direct, false),
            scored("m1", "multi-hop", multihop, false),
            scored("m2", "multi-hop", multihop, false),
        ])
    }

    /// Assemble a gate-ready report: contract and ceiling populated, so
    /// only the property under test can fail.
    fn from_scored(scored: Vec<ScoredQuery>) -> RecallReport {
        let mut report = RecallReport::assemble("set", "strategy", scored);
        report.contract = Some(test_contract());
        report.recall_ceiling = Some(clean_ceiling());
        report
    }

    // ---- specificity: a green gate must mean something ----

    #[test]
    fn an_unchanged_run_passes_every_gate() {
        let base = report(0.90, 0.60);
        let now = report(0.90, 0.60);
        let result = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(result.passed(), "{:?}", result.failures());
    }

    #[test]
    fn ordinary_jitter_does_not_trip_a_gate() {
        // Movement below the threshold must not fire, or the gate is
        // noise and will be disabled by whoever it annoys first.
        let base = report(0.90, 0.60);
        let now = report(0.895, 0.595);
        let result = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(result.passed(), "{:?}", result.failures());
    }

    #[test]
    fn an_improvement_passes() {
        let base = report(0.80, 0.50);
        let now = report(0.90, 0.70);
        assert!(evaluate(&now, Some(&base), &QualityGates::default()).passed());
    }

    // ---- sensitivity: each gate fires on what it exists to catch ----

    #[test]
    fn a_category_regression_fires_even_when_the_aggregate_improves() {
        // The exact shape this project measured: one category gains a
        // lot, another loses a little more, and the mean looks fine.
        let base = report(0.90, 0.60);
        let now = report(0.99, 0.45);

        let result = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(
            !result.passed(),
            "a hidden category regression slipped through"
        );
        let names: Vec<&str> = result.failures().iter().map(|o| o.name.as_str()).collect();
        assert!(
            names.contains(&"category:multi-hop"),
            "expected the losing category to fire: {names:?}"
        );
    }

    #[test]
    fn an_aggregate_regression_fires() {
        let base = report(0.90, 0.90);
        let now = report(0.60, 0.60);
        let result = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(result
            .failures()
            .iter()
            .any(|o| o.name == "aggregate_quality"));
    }

    #[test]
    fn a_vanished_category_fires() {
        // The realistic shape: the same query set ran, so the contract
        // matches and comparability passes, but the report lost a
        // category. That is a harness bug which would otherwise read as
        // "no regression" while being total loss of coverage.
        let base = report(0.90, 0.60);
        let now = from_scored(vec![
            scored("d1", "direct", 0.90, false),
            scored("d2", "direct", 0.90, false),
            scored("m1", "direct", 0.60, false),
            scored("m2", "direct", 0.60, false),
        ]);

        let result = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(
            result
                .failures()
                .iter()
                .any(|o| o.name == "category_coverage"),
            "a vanished category was not reported: {:?}",
            result.failures()
        );
    }

    #[test]
    fn unresolved_judgments_fire_without_a_baseline() {
        // A first run has nothing to compare against and must still
        // catch a query set that has drifted from its corpus.
        let mut now = report(0.90, 0.60);
        now.unresolved_keys = 7;
        let result = evaluate(&now, None, &QualityGates::default());
        assert!(!result.passed());
        assert!(result
            .failures()
            .iter()
            .any(|o| o.name == "judgments_resolve"));
    }

    #[test]
    fn the_size_of_the_detected_regression_is_reported() {
        // A gate that says only "failed" makes the next person re-derive
        // the number themselves.
        let base = report(0.90, 0.60);
        let now = report(0.90, 0.40);
        let result = evaluate(&now, Some(&base), &QualityGates::default());
        let detail = result
            .failures()
            .iter()
            .find(|o| o.name == "category:multi-hop")
            .and_then(|o| o.detail.clone())
            .unwrap_or_default();
        assert!(detail.contains("0.6000"), "{detail}");
        assert!(detail.contains("0.4000"), "{detail}");
    }

    // ---- recall ceiling ----

    fn judged(id: &str, n: usize, abstain: bool) -> RecallQuery {
        RecallQuery {
            id: id.into(),
            text: "t".into(),
            category: "c".into(),
            relevant: (0..n)
                .map(|i| GradedKey {
                    key: format!("k{i}"),
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
    fn a_query_set_that_cannot_reach_one_is_refused_without_a_baseline() {
        // The defect this replaces: the old check lived in a helper the
        // gate had no way to call, so it never ran on any real report.
        let set = RecallQuerySet {
            name: "s".into(),
            queries: vec![judged("ok", 5, false), judged("too-many", 40, false)],
        };
        let ceiling = RecallCeiling::measure(&set, 10);
        assert_eq!(ceiling.capped_queries, 1);
        assert_eq!(ceiling.worst[0], ("too-many".to_string(), 40));
        // (1.0 + 10/40) / 2
        assert!((ceiling.mean_ceiling - 0.625).abs() < 1e-9, "{ceiling:?}");

        let mut now = report(0.9, 0.6);
        now.recall_ceiling = Some(ceiling);
        let outcome = evaluate(&now, None, &QualityGates::default());
        let failure = outcome
            .failures()
            .iter()
            .find(|o| o.name == "recall_ceiling")
            .expect("the ceiling gate must fire")
            .detail
            .clone()
            .unwrap_or_default();
        assert!(failure.contains("too-many"), "{failure}");
        assert!(failure.contains("0.6250"), "{failure}");
    }

    #[test]
    fn easy_queries_cannot_buy_acceptance_for_an_exceptional_one() {
        // The dilution attack a mean cannot survive. One query judged
        // against 30 targets is either acceptable or it is not; whether
        // 30 or 999 unrelated one-answer queries sit beside it is a fact
        // about the rest of the set, not about that query.
        //
        // A mean threshold below 1.0 — which is exactly what a corpus
        // with a legitimately large answer set tempts you to set — makes
        // the verdict a function of the company the query keeps.
        let with_company = |easy: usize| {
            let mut queries = vec![judged("exceptional", 30, false)];
            queries.extend((0..easy).map(|i| judged(&format!("easy-{i}"), 1, false)));
            let set = RecallQuerySet {
                name: "s".into(),
                queries,
            };
            let mut now = report(0.9, 0.6);
            now.recall_ceiling = Some(RecallCeiling::measure(&set, 10));
            let gates = QualityGates {
                min_recall_ceiling: 0.99,
                ..QualityGates::default()
            };
            evaluate(&now, None, &gates)
                .outcomes
                .iter()
                .find(|o| o.name == "recall_ceiling")
                .expect("the ceiling gate must run")
                .passed
        };

        let small = with_company(30);
        let padded = with_company(999);
        assert_eq!(
            small,
            padded,
            "the same exceptional query was judged {} in a 31-query set and {} in a \
             1000-query set: padding with easy queries bought its acceptance",
            if small { "acceptable" } else { "unacceptable" },
            if padded { "acceptable" } else { "unacceptable" },
        );
        assert!(
            !padded,
            "a query judged against 30 targets at K=10 must not be accepted"
        );
    }

    /// As [`judged`], plus a declared capacity exception.
    fn waived(id: &str, n: usize, reason: &str) -> RecallQuery {
        RecallQuery {
            capacity_waiver: Some(crate::recall_set::CapacityWaiver {
                reason: reason.into(),
            }),
            ..judged(id, n, false)
        }
    }

    #[test]
    fn a_declared_exception_is_excused_and_named_in_the_verdict() {
        // The legitimate case: a change-history question has one right
        // answer per version, so twelve versions means twelve. It keeps
        // NDCG and MRR, leaves the ceiling statistics, and appears in the
        // verdict with its reason — an exception nobody can read is
        // indistinguishable from a lowered threshold.
        let set = RecallQuerySet {
            name: "s".into(),
            queries: vec![
                judged("ordinary", 3, false),
                waived(
                    "history",
                    12,
                    "a change-history question has one right answer per version",
                ),
            ],
        };
        let ceiling = RecallCeiling::measure(&set, 10);
        assert_eq!(
            ceiling.n_queries, 1,
            "the waived query leaves the population"
        );
        assert_eq!(ceiling.capped_queries, 0);
        assert!(ceiling.min_ceiling.is_some_and(|v| (v - 1.0).abs() < 1e-12));
        assert_eq!(ceiling.waived.len(), 1);
        assert_eq!(ceiling.waived[0].judged, 12);

        let mut now = report(0.9, 0.6);
        now.recall_ceiling = Some(ceiling);
        let verdict = evaluate(&now, None, &QualityGates::default());
        assert!(verdict.passed(), "{:?}", verdict.failures());
        assert_eq!(verdict.capacity_waivers.len(), 1);
        assert!(
            verdict.capacity_waivers[0].reason.contains("per version"),
            "the verdict must carry the justification: {:?}",
            verdict.capacity_waivers
        );
    }

    #[test]
    fn a_waiver_does_not_cover_a_second_query_that_becomes_capped() {
        // The inheritance risk: one waived query must not licence the
        // next one. `undeclared` is capped and has no exception, so the
        // gate fires and names it even though `history` is excused.
        let set = RecallQuerySet {
            name: "s".into(),
            queries: vec![
                waived("history", 12, "one right answer per version"),
                judged("undeclared", 30, false),
            ],
        };
        let mut now = report(0.9, 0.6);
        now.recall_ceiling = Some(RecallCeiling::measure(&set, 10));
        let detail = evaluate(&now, None, &QualityGates::default())
            .failures()
            .iter()
            .find(|o| o.name == "recall_ceiling")
            .and_then(|o| o.detail.clone())
            .expect("the ceiling gate must fire");
        assert!(detail.contains("undeclared"), "{detail}");
        // And the verdict still explains what *was* excused.
        assert!(detail.contains("one right answer per version"), "{detail}");
    }

    #[test]
    fn a_waiver_its_query_no_longer_needs_is_reported_as_stale() {
        // The query shrank below K; the standing permission did not.
        let set = RecallQuerySet {
            name: "s".into(),
            queries: vec![waived("shrunk", 4, "it used to have many versions")],
        };
        let ceiling = RecallCeiling::measure(&set, 10);
        assert_eq!(ceiling.unused_waivers, vec!["shrunk".to_string()]);
        assert_eq!(ceiling.n_queries, 1, "it is an ordinary query again");

        let mut now = report(0.9, 0.6);
        now.recall_ceiling = Some(ceiling);
        let detail = evaluate(&now, None, &QualityGates::default())
            .failures()
            .iter()
            .find(|o| o.name == "recall_ceiling")
            .and_then(|o| o.detail.clone())
            .expect("a stale waiver must be refused");
        assert!(detail.contains("stale"), "{detail}");
    }

    #[test]
    fn a_wholly_capped_category_is_named_even_though_the_mean_barely_moves() {
        // 3 capped queries among 300 move the set mean by 0.007 — noise
        // at any threshold anyone would set. The category rollup is what
        // makes it legible.
        let mut queries: Vec<RecallQuery> = (0..300)
            .map(|i| judged(&format!("easy-{i}"), 1, false))
            .collect();
        for i in 0..3 {
            let mut q = judged(&format!("c2f-{i}"), 30, false);
            q.category = "commit-to-file".into();
            queries.push(q);
        }
        let set = RecallQuerySet {
            name: "s".into(),
            queries,
        };
        let ceiling = RecallCeiling::measure(&set, 10);
        assert!(ceiling.mean_ceiling > 0.99, "{:?}", ceiling.mean_ceiling);
        let c2f = &ceiling.by_category["commit-to-file"];
        assert_eq!(c2f.capped_queries, 3);
        assert!((c2f.mean_ceiling - 1.0 / 3.0).abs() < 1e-9);

        // Loosen the mean past the point where it would fire on its own,
        // so what is left to catch this is the per-query threshold and
        // the count — and the category rollup that explains them.
        let gates = QualityGates {
            min_recall_ceiling: 0.99,
            ..QualityGates::default()
        };
        let mut now = report(0.9, 0.6);
        now.recall_ceiling = Some(ceiling);
        let detail = evaluate(&now, None, &gates)
            .failures()
            .iter()
            .find(|o| o.name == "recall_ceiling")
            .and_then(|o| o.detail.clone())
            .expect("the ceiling gate must fire");
        assert!(
            !detail.contains("mean attainable"),
            "the mean passed here; this must not be why it fired: {detail}"
        );
        assert!(detail.contains("commit-to-file"), "{detail}");
        assert!(detail.contains("3/3 capped"), "{detail}");
    }

    #[test]
    fn a_report_predating_the_per_query_statistic_is_refused() {
        // A verdict is only as strong as what the report can answer. An
        // older report knows its mean and its capped count but not its
        // worst query, and defaulting that to 1.0 would have it certify
        // the one thing it cannot see.
        let mut legacy = clean_ceiling();
        legacy.min_ceiling = None;
        let mut now = report(0.9, 0.6);
        now.recall_ceiling = Some(legacy);

        let detail = evaluate(&now, None, &QualityGates::default())
            .failures()
            .iter()
            .find(|o| o.name == "recall_ceiling")
            .and_then(|o| o.detail.clone())
            .expect("a report that cannot answer must not pass");
        assert!(detail.contains("predates"), "{detail}");
    }

    #[test]
    fn ceiling_thresholds_outside_zero_to_one_are_refused() {
        // Above 1.0 is the dangerous direction: it fires on every run,
        // and a gate that always fires is a gate somebody switches off.
        let mut now = report(0.9, 0.6);
        now.recall_ceiling = Some(clean_ceiling());
        for (label, gates) in [
            (
                "mean above one",
                QualityGates {
                    min_recall_ceiling: 1.5,
                    ..QualityGates::default()
                },
            ),
            (
                "per-query above one",
                QualityGates {
                    min_query_ceiling: 2.0,
                    ..QualityGates::default()
                },
            ),
            (
                "negative",
                QualityGates {
                    min_query_ceiling: -0.5,
                    ..QualityGates::default()
                },
            ),
        ] {
            let detail = evaluate(&now, None, &gates)
                .failures()
                .iter()
                .find(|o| o.name == "wellformed")
                .and_then(|o| o.detail.clone())
                .unwrap_or_else(|| panic!("{label}: wellformed must refuse this threshold"));
            assert!(
                detail.contains("min_recall_ceiling") || detail.contains("min_query_ceiling"),
                "{label}: {detail}"
            );
        }
    }

    #[test]
    fn abstention_and_unjudged_queries_do_not_lower_the_ceiling() {
        // A no-answer query has no targets to miss, and a query with no
        // graded target contributes no information. Counting either as
        // 1.0 would let them dilute a genuinely capped query.
        let set = RecallQuerySet {
            name: "s".into(),
            queries: vec![
                judged("silent", 0, true),
                judged("ungraded", 0, false),
                judged("capped", 20, false),
            ],
        };
        let ceiling = RecallCeiling::measure(&set, 10);
        assert_eq!(ceiling.n_queries, 1, "{ceiling:?}");
        assert!((ceiling.mean_ceiling - 0.5).abs() < 1e-9, "{ceiling:?}");
    }

    #[test]
    fn a_reachable_query_set_passes_the_ceiling_gate() {
        let set = RecallQuerySet {
            name: "s".into(),
            queries: vec![judged("a", 1, false), judged("b", 10, false)],
        };
        let ceiling = RecallCeiling::measure(&set, 10);
        assert_eq!(ceiling.capped_queries, 0);
        assert!((ceiling.mean_ceiling - 1.0).abs() < 1e-12);

        let mut now = report(0.9, 0.6);
        now.recall_ceiling = Some(ceiling);
        assert!(evaluate(&now, None, &QualityGates::default()).passed());
    }

    #[test]
    fn a_report_without_a_ceiling_is_refused() {
        let mut now = report(0.9, 0.6);
        now.recall_ceiling = None;
        let outcome = evaluate(&now, None, &QualityGates::default());
        assert!(outcome
            .failures()
            .iter()
            .any(|o| o.name == "recall_ceiling"));
    }

    // ---- abstention ----

    /// `n` no-answer queries of which `silent` correctly returned
    /// nothing, alongside two answerable ones so the report is realistic.
    fn with_abstention(n: usize, silent: usize, groups: usize) -> RecallReport {
        let mut rows = vec![
            scored("d1", "direct", 0.9, false),
            scored("d2", "direct", 0.9, false),
        ];
        for i in 0..n {
            let mut row = scored(&format!("a{i}"), "no-answer", 0.0, true);
            row.returned = usize::from(i >= silent);
            row.group_id = Some(format!("g{}", i % groups.max(1)));
            rows.push(row);
        }
        from_scored(rows)
    }

    #[test]
    fn losing_no_answer_behaviour_fires_even_though_ranking_is_unchanged() {
        // The gap this closes: abstention queries are excluded from every
        // QualityMetrics rollup, so their category reports n_queries: 0,
        // the per-category gates skip them, and a system that starts
        // confidently answering unanswerable questions moves nothing the
        // other gates look at.
        let base = with_abstention(10, 9, 5);
        let now = with_abstention(10, 2, 5);

        assert!(
            (base.overall.ndcg_at_10 - now.overall.ndcg_at_10).abs() < 1e-12,
            "the answerable side must be identical or this proves nothing"
        );
        assert_eq!(
            base.by_category.get("no-answer").map(|m| m.n_queries),
            Some(0),
            "the rollup is expected to be blind here; that is the point"
        );

        let outcome = evaluate(&now, Some(&base), &QualityGates::default());
        let failure = outcome
            .failures()
            .iter()
            .find(|o| o.name == "abstention_quality")
            .expect("abstention regression was invisible")
            .detail
            .clone()
            .unwrap_or_default();
        assert!(
            failure.contains("0.9000") && failure.contains("0.2000"),
            "{failure}"
        );
        // And it must quote clusters, not the inflated row count.
        assert!(failure.contains("5 clusters"), "{failure}");
    }

    #[test]
    fn an_unchanged_abstention_slice_does_not_fire() {
        let base = with_abstention(10, 9, 5);
        let now = with_abstention(10, 9, 5);
        assert!(evaluate(&now, Some(&base), &QualityGates::default()).passed());
    }

    #[test]
    fn a_vanished_abstention_slice_fires() {
        let base = with_abstention(10, 9, 5);
        let now = report(0.9, 0.9);
        let outcome = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(outcome
            .failures()
            .iter()
            .any(|o| o.name == "abstention_quality"));
    }

    #[test]
    fn abstaining_from_everything_does_not_buy_a_pass() {
        // The evasion the pair of gates exists for: silencing the system
        // maximises abstention and must still fail on the answerable side.
        let base = with_abstention(10, 5, 5);
        let mut rows = vec![
            scored("d1", "direct", 0.0, false),
            scored("d2", "direct", 0.0, false),
        ];
        for i in 0..10 {
            let mut row = scored(&format!("a{i}"), "no-answer", 0.0, true);
            row.returned = 0;
            row.group_id = Some(format!("g{}", i % 5));
            rows.push(row);
        }
        let silent_everywhere = from_scored(rows);

        let outcome = evaluate(&silent_everywhere, Some(&base), &QualityGates::default());
        assert!(!outcome.passed(), "silencing every query passed every gate");
        assert!(outcome
            .failures()
            .iter()
            .any(|o| o.name == "aggregate_quality"));
    }

    // ---- judgment provenance ----

    #[test]
    fn a_hand_reviewed_slice_cannot_be_averaged_away_by_generated_volume() {
        let build = |human: f64| {
            let mut rows = vec![{
                let mut row = scored("h1", "direct", human, false);
                row.judgment_source = JudgmentSource::HumanReviewed;
                row
            }];
            for i in 0..20 {
                rows.push(scored(&format!("g{i}"), "direct", 0.90, false));
            }
            from_scored(rows)
        };
        let base = build(0.90);
        let now = build(0.20);

        // The aggregate barely notices: one row in twenty-one.
        assert!(base.overall.ndcg_at_10 - now.overall.ndcg_at_10 < 0.04);

        let outcome = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(outcome
            .failures()
            .iter()
            .any(|o| o.name == "judgment_source:human-reviewed"));
    }

    #[test]
    fn an_all_generated_corpus_is_not_blocked_for_lacking_human_judgments() {
        // The query sets are currently all generated. A gate demanding a
        // slice the corpus does not have is a gate somebody switches off.
        let base = report(0.90, 0.60);
        let now = report(0.90, 0.60);
        let outcome = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(outcome.passed(), "{:?}", outcome.failures());
    }

    #[test]
    fn a_vanished_judgment_source_fires() {
        let mut rows = vec![scored("g1", "direct", 0.9, false)];
        let mut human = scored("h1", "direct", 0.9, false);
        human.judgment_source = JudgmentSource::HumanReviewed;
        rows.push(human);
        let base = from_scored(rows);

        let now = from_scored(vec![scored("g1", "direct", 0.9, false)]);
        let outcome = evaluate(&now, Some(&base), &QualityGates::default());
        let failure = outcome
            .failures()
            .iter()
            .find(|o| o.name == "judgment_source:human-reviewed")
            .expect("a vanished provenance slice must fire")
            .detail
            .clone()
            .unwrap_or_default();
        assert!(failure.contains("absent now"), "{failure}");
    }

    #[test]
    fn reports_from_different_experiments_are_refused_before_scoring() {
        // The defect this closes: a baseline from another corpus, or an
        // easier query subset, keeps the same dataset/strategy strings
        // and would otherwise be compared as if it were the same run.
        let baseline = report(0.90, 0.60);
        let mut other_corpus = report(0.90, 0.60);
        other_corpus.contract = Some(crate::contract::EvaluationContract {
            corpus: "a-different-corpus".into(),
            ..test_contract()
        });

        let outcome = evaluate(&other_corpus, Some(&baseline), &QualityGates::default());
        assert!(!outcome.passed());
        let failure = outcome
            .failures()
            .iter()
            .find(|o| o.name == "comparable_experiment")
            .expect("comparability must fire")
            .detail
            .clone()
            .unwrap_or_default();
        assert!(failure.contains("corpus"), "{failure}");

        // And it must stop there rather than reporting scores that do
        // not mean anything.
        assert_eq!(
            outcome.outcomes.len(),
            1,
            "scoring continued past an incomparable pair"
        );
    }

    #[test]
    fn a_missing_contract_is_refused() {
        let baseline = report(0.90, 0.60);
        let mut legacy = report(0.90, 0.60);
        legacy.contract = None;
        let outcome = evaluate(&legacy, Some(&baseline), &QualityGates::default());
        assert!(!outcome.passed());
    }

    #[test]
    fn a_report_survives_a_json_round_trip() {
        // Baselines are stored as JSON and read back on the next run, so
        // a report that cannot round-trip makes gating impossible. This
        // failed in practice: `top_relevant_score` held `NEG_INFINITY`
        // for a query with no relevant hit, serde wrote `null`, and
        // reading it back errored. Every unit test had built the struct
        // directly and so never touched the boundary.
        let mut with_miss = report(0.90, 0.60);
        with_miss.per_query[0].top_relevant_score = None;

        let json = serde_json::to_string(&with_miss).expect("serialise");
        let back: RecallReport = serde_json::from_str(&json).expect("round trip");

        assert_eq!(back.per_query.len(), with_miss.per_query.len());
        assert!(back.per_query[0].top_relevant_score.is_none());
        assert!((back.overall.ndcg_at_10 - with_miss.overall.ndcg_at_10).abs() < 1e-12);

        // And the round-tripped report must gate identically.
        let direct = evaluate(&with_miss, Some(&with_miss), &QualityGates::default());
        let via_json = evaluate(&back, Some(&back), &QualityGates::default());
        assert_eq!(direct.passed(), via_json.passed());
    }

    // ---- vacuous comparisons ----

    #[test]
    fn a_nan_metric_does_not_pass_every_gate() {
        // The failure this exists for: `before - now > limit` is false
        // when `now` is NaN, so a report of NaNs would clear every
        // quality gate in this file while measuring nothing at all.
        let base = report(0.90, 0.60);
        let mut now = report(0.90, 0.60);
        now.overall.ndcg_at_10 = f64::NAN;

        let outcome = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(
            !outcome
                .failures()
                .iter()
                .any(|o| o.name == "aggregate_quality"),
            "this test is only meaningful if the aggregate gate is fooled"
        );
        let failure = outcome
            .failures()
            .iter()
            .find(|o| o.name == "wellformed")
            .expect("a NaN metric passed every gate")
            .detail
            .clone()
            .unwrap_or_default();
        assert!(failure.contains("NDCG@10"), "{failure}");
    }

    #[test]
    fn a_nan_in_the_stored_baseline_is_caught_too() {
        // The baseline is the value least likely to be looked at again,
        // and it fools `before - now > limit` exactly as the current run
        // does.
        let mut base = report(0.90, 0.60);
        base.overall.r_at_10 = f64::NAN;
        let now = report(0.10, 0.10);

        let outcome = evaluate(&now, Some(&base), &QualityGates::default());
        let failure = outcome
            .failures()
            .iter()
            .find(|o| o.name == "wellformed")
            .expect("a NaN baseline was accepted")
            .detail
            .clone()
            .unwrap_or_default();
        assert!(failure.contains("baseline"), "{failure}");
    }

    #[test]
    fn an_unfireable_threshold_is_reported() {
        let base = report(0.90, 0.60);
        let now = report(0.10, 0.10);
        let outcome = evaluate(
            &now,
            Some(&base),
            &QualityGates {
                max_aggregate_drop: f64::NAN,
                ..QualityGates::default()
            },
        );
        assert!(outcome.failures().iter().any(|o| o.name == "wellformed"));
    }

    #[test]
    fn a_negative_threshold_is_reported() {
        let now = report(0.90, 0.60);
        let outcome = evaluate(
            &now,
            None,
            &QualityGates {
                max_category_drop: -0.5,
                ..QualityGates::default()
            },
        );
        let failure = outcome
            .failures()
            .iter()
            .find(|o| o.name == "wellformed")
            .expect("a negative tolerance was accepted silently")
            .detail
            .clone()
            .unwrap_or_default();
        assert!(failure.contains("max_category_drop"), "{failure}");
    }

    #[test]
    fn a_nan_in_a_category_or_source_slice_is_caught() {
        // The aggregate can be perfectly finite while a slice is not.
        let mut now = report(0.90, 0.60);
        now.by_category.get_mut("multi-hop").unwrap().r_at_10 = f64::INFINITY;
        let outcome = evaluate(&now, None, &QualityGates::default());
        let failure = outcome
            .failures()
            .iter()
            .find(|o| o.name == "wellformed")
            .expect("a non-finite slice metric was accepted")
            .detail
            .clone()
            .unwrap_or_default();
        assert!(failure.contains("current category multi-hop"), "{failure}");
    }

    // ---- corpus binding ----

    #[test]
    fn a_release_gate_refuses_an_unreceipted_comparison() {
        // A directory name is not corpus identity: two different corpora
        // can sit at the same path on different days, and the older gate
        // would have compared them without complaint.
        let policy = QualityGates {
            require_corpus_receipt: true,
            ..QualityGates::default()
        };

        let base = report(0.90, 0.60);
        let mut unreceipted = report(0.90, 0.60);
        unreceipted.contract = Some(crate::contract::EvaluationContract {
            corpus_receipt: None,
            ..test_contract()
        });

        // Comparability fires first — None and Some are different — so
        // the run is refused before any score is read.
        let outcome = evaluate(&unreceipted, Some(&base), &policy);
        assert!(!outcome.passed());
        assert!(outcome
            .failures()
            .iter()
            .any(|o| o.name == "comparable_experiment"));

        // And when *both* sides lack a receipt, the contracts match, so
        // comparability passes and the receipt gate is the thing that
        // has to catch it.
        let strip = |mut r: RecallReport| {
            r.contract = Some(crate::contract::EvaluationContract {
                corpus_receipt: None,
                ..test_contract()
            });
            r
        };
        let outcome = evaluate(&strip(report(0.90, 0.60)), Some(&strip(base)), &policy);
        let failure = outcome
            .failures()
            .iter()
            .find(|o| o.name == "corpus_receipt")
            .expect("an unreceipted release comparison was accepted")
            .detail
            .clone()
            .unwrap_or_default();
        assert!(
            failure.contains("baseline") && failure.contains("current"),
            "{failure}"
        );
    }

    #[test]
    fn receipted_runs_pass_the_release_gate() {
        let policy = QualityGates {
            require_corpus_receipt: true,
            ..QualityGates::default()
        };
        let base = report(0.90, 0.60);
        let now = report(0.90, 0.60);
        assert!(
            evaluate(&now, Some(&base), &policy).passed(),
            "receipted, unchanged runs must not be flagged"
        );
    }

    #[test]
    fn the_default_policy_does_not_demand_a_receipt() {
        // The instrument's own suite builds in-memory stores with no
        // source tree to fingerprint. Demanding a receipt there would
        // mean inventing one, and a fake receipt certifies nothing while
        // looking like it certifies something.
        assert!(!QualityGates::default().require_corpus_receipt);
    }

    // ---- the verdict as a release artifact ----

    #[test]
    fn a_stored_verdict_says_what_it_certified() {
        // A file containing only `passed: true` records a conclusion
        // with no argument. Months later nobody can tell which corpus,
        // which query set, or which thresholds produced it.
        let base = report(0.90, 0.60);
        let now = report(0.90, 0.60);
        let verdict = evaluate(&now, Some(&base), &QualityGates::default());

        assert_eq!(verdict.schema_version, GATE_SCHEMA_VERSION);
        assert_eq!(verdict.strategy, "strategy");
        assert_eq!(verdict.dataset, "set");
        assert_eq!(
            verdict.contract.as_ref().map(|c| c.corpus.as_str()),
            Some("fixture")
        );
        assert!(verdict.baseline_contract.is_some());
        let thresholds = verdict.gates.as_ref().expect("thresholds recorded");
        assert!((thresholds.max_category_drop - 0.01).abs() < 1e-12);
        assert!((thresholds.min_recall_ceiling - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_verdict_publishes_atomically_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("omem-gate-{}", std::process::id()));
        let path = dir.join("nested").join("verdict.json");

        let base = report(0.90, 0.60);
        let now = report(0.90, 0.40);
        let verdict = evaluate(&now, Some(&base), &QualityGates::default());
        assert!(
            !verdict.passed(),
            "this fixture must fail to be worth storing"
        );
        verdict.write_atomic(&path).expect("write");

        let body = std::fs::read_to_string(&path).expect("read");
        let back: GateReport = serde_json::from_str(&body).expect("round trip");
        assert_eq!(back.passed(), verdict.passed());
        assert_eq!(back.outcomes.len(), verdict.outcomes.len());
        assert!(back
            .failures()
            .iter()
            .any(|o| o.name == "category:multi-hop"));

        // No temporary file left where a reader could mistake it for the
        // verdict.
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn category_ndcg_projects_the_report() {
        let map = category_ndcg(&report(0.9, 0.6));
        assert!((map["direct"] - 0.9).abs() < 1e-9);
        assert!((map["multi-hop"] - 0.6).abs() < 1e-9);
    }

    #[test]
    fn an_empty_baseline_category_is_not_compared() {
        // A baseline category with no queries carries no information.
        let base = RecallReport {
            by_category: {
                let mut m = BTreeMap::new();
                m.insert("empty".to_string(), QualityMetrics::default());
                m
            },
            ..report(0.9, 0.6)
        };
        let now = report(0.9, 0.6);
        assert!(evaluate(&now, Some(&base), &QualityGates::default()).passed());
    }
}
