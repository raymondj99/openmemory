//! Evaluate retrieval quality through `MemoryStore::recall`.
//!
//! This is the difference that matters between this runner and
//! [`crate::runner::EvalRunner`]. The older runner calls
//! `engine.search()` directly against a freshly built in-memory index,
//! which measures the index layer alone. Everything the memory system
//! adds on top of the index lives in `recall()`:
//!
//! - Ebbinghaus decay on `observed_at`;
//! - the retrieval-frequency boost derived from `access_count`;
//! - the correction-source boost;
//! - confidence and importance priors;
//! - temporal validity filtering;
//! - spreading activation across relations.
//!
//! None of those are observable through the index path, so the
//! experiments that exist to interrogate them (access-count ablation,
//! graph diffusion, temporal retrieval) need this runner.
//!
//! Two safety properties are deliberate:
//!
//! - `record_access` defaults to **false**. `recall()` otherwise calls
//!   `bump_access_counts`, which mutates `access_count` — an input to
//!   the very ranking term an ablation is trying to measure. Running an
//!   evaluation must not change the corpus it is evaluating.
//! - The runner never opens a store itself. The caller supplies an
//!   already-open handle, so decisions about *which* store (and whether
//!   it is a disposable copy) stay with the caller.

use std::time::Instant;

use anyhow::{Context, Result};
use openmemory_graph::recall::RecallFilters;
use openmemory_graph::MemoryStore;
use openmemory_index::SearchMode;

use crate::recall_report::{score_query_with_scores, RecallReport, ScoredQuery};
use crate::recall_set::RecallQuerySet;
use crate::resolver::KeyResolver;

/// Knobs for one recall-path run.
#[derive(Debug, Clone)]
pub struct RecallRunnerConfig {
    /// Results requested per query.
    pub top_k: usize,
    /// Search mode. `None` leaves the store default (hybrid).
    pub mode: Option<SearchMode>,
    /// Whether relation spreading may supplement sparse direct hits.
    pub spreading_activation: bool,
    /// Whether recall records retrieval-frequency feedback. Kept false
    /// for evaluation so a run cannot mutate its own inputs.
    pub record_access: bool,
    /// Results scoring at or below this are treated as not returned.
    /// Abstention is judged against this floor.
    pub score_floor: f64,
    /// Evaluate validity as of this instant, when set.
    pub valid_at: Option<i64>,
    /// Label for the strategy under test, recorded in the report.
    pub strategy: String,
    /// Corpus identity, recorded in the evaluation contract so a
    /// baseline from one corpus cannot be compared against a run on
    /// another.
    pub corpus: String,
    /// Ranking-prior configuration in force, when the caller pinned it.
    pub decay_rate: Option<f64>,
    /// Digest of the corpus's verification receipt, when it has one.
    /// The caller computes it, because the receipt format belongs to
    /// whoever built the corpus rather than to the evaluator.
    pub corpus_receipt: Option<String>,
}

impl Default for RecallRunnerConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            mode: None,
            spreading_activation: true,
            record_access: false,
            score_floor: 0.0,
            valid_at: None,
            strategy: "baseline".to_string(),
            corpus: "unspecified".to_string(),
            decay_rate: None,
            corpus_receipt: None,
        }
    }
}

impl RecallRunnerConfig {
    /// Build the filters this configuration implies.
    ///
    /// Built from `RecallFilters::default()` and set explicitly, because
    /// `default()` and `new()` disagree about `spreading_activation` and
    /// `record_access`; relying on either implicitly would make an
    /// experiment's configuration depend on which constructor was used.
    #[must_use]
    pub fn filters(&self) -> RecallFilters {
        RecallFilters {
            spreading_activation: self.spreading_activation,
            record_access: self.record_access,
            mode: self.mode,
            valid_at: self.valid_at,
            ..RecallFilters::default()
        }
    }
}

/// How far a widened fetch may grow before the runner gives up and
/// reports what it has. A query whose eligible top ten needs more than
/// this many physical rows is pathological, and silently fetching
/// unboundedly would turn one bad query into a hung evaluation.
const MAX_FETCH: usize = 512;

/// Multiplier per widening round. Geometric so a query needing a deep
/// tail converges in a few rounds rather than crawling one row at a time.
const FETCH_GROWTH: usize = 4;

/// One query's retrieval outcome, with the physical and logical views
/// kept apart.
///
/// They were previously one number, which is how a report came to say
/// `returned = 10` for queries scored over nine candidates, and to quote
/// an excluded document's score as the top score returned.
struct Eligible {
    /// Surviving candidates, truncated to the target.
    candidates: Vec<(Vec<String>, f64)>,
    /// Top score among candidates that were actually scored — not among
    /// rows that came back.
    top_score: f64,
    /// Rows the store returned on the final fetch.
    physical_returned: usize,
    /// Rows dropped as the query's declared source.
    excluded_hits: usize,
    /// Rows whose key did not resolve, so they could not be scored.
    unresolved_hits: usize,
    /// Fetch rounds needed. More than one means the first fetch did not
    /// yield enough eligible documents.
    rounds: usize,
    /// Whether the corpus ran out before the target was met.
    exhausted: bool,
}

/// Runs a query set against an open store.
pub struct RecallRunner<'a, R: KeyResolver> {
    store: &'a MemoryStore,
    resolver: R,
    config: RecallRunnerConfig,
}

impl<'a, R: KeyResolver> RecallRunner<'a, R> {
    #[must_use]
    pub fn new(store: &'a MemoryStore, resolver: R, config: RecallRunnerConfig) -> Self {
        Self {
            store,
            resolver,
            config,
        }
    }

    /// Run every query and assemble a report.
    ///
    /// Fetches until **`GATED_K` eligible documents survive**, not until
    /// `GATED_K` rows come back. Those differ whenever anything is
    /// dropped between retrieval and scoring — an excluded source
    /// document, a hit whose key does not resolve, several chunks of one
    /// file collapsing to one logical answer — and the difference is not
    /// cosmetic: fetching exactly ten and then removing one scores the
    /// query over nine candidates while withholding the document that
    /// would have taken the vacant slot.
    ///
    /// That defect made `R@10` look immovable under the source-exclusion
    /// repair, which was read as evidence the repair was well-behaved. It
    /// was an artifact of the tail never being fetched.
    pub fn run(&self, set: &RecallQuerySet) -> Result<RecallReport> {
        let filters = self.config.filters();
        let target = self.config.top_k.max(crate::recall_report::GATED_K);
        let mut scored: Vec<ScoredQuery> = Vec::with_capacity(set.queries.len());

        for query in &set.queries {
            let started = Instant::now();
            let outcome = self.retrieve_eligible(query, &filters, target)?;
            // Latency covers every fetch the eligible set required, not
            // just the first: under-reporting it would make a widened
            // fetch look free.
            let latency_ms = started.elapsed().as_secs_f64() * 1000.0;

            let (ranked, scores) = project_to_judged_granularity(query, &outcome.candidates);

            // Drift between the query set and the corpus is detected
            // once up front by `unknown_judgment_keys`, not per query:
            // deciding it here would need a corpus-wide scan inside the
            // timed loop and would distort the latency it is measuring.
            let mut row = score_query_with_scores(
                query,
                &ranked,
                &scores,
                outcome.candidates.len(),
                outcome.top_score,
                latency_ms,
                0,
            );
            row.excluded_hits = outcome.excluded_hits;
            row.physical_returned = outcome.physical_returned;
            row.unresolved_hits = outcome.unresolved_hits;
            row.fetch_rounds = outcome.rounds;
            row.corpus_exhausted = outcome.exhausted;
            scored.push(row);
        }

        let mut report = RecallReport::assemble(&set.name, &self.config.strategy, scored);
        // Measured at 10, not at `top_k`: R@10 is the metric the gates
        // compare, so 10 is the cutoff a ceiling has to be stated
        // against. Fetching more results does not raise it.
        report.recall_ceiling = Some(crate::recall_report::RecallCeiling::measure(
            set,
            crate::recall_report::GATED_K,
        ));
        report.contract = Some(crate::contract::EvaluationContract {
            metric_version: crate::contract::METRIC_VERSION,
            query_set_digest: crate::contract::EvaluationContract::digest_query_set(set),
            query_count: set.queries.len(),
            corpus: self.config.corpus.clone(),
            corpus_receipt: self.config.corpus_receipt.clone(),
            mode: format!("{:?}", self.config.mode),
            top_k: self.config.top_k,
            spreading_activation: self.config.spreading_activation,
            record_access: self.config.record_access,
            decay_rate: self.config.decay_rate,
        });
        Ok(report)
    }

    /// Retrieve until `target` eligible candidates survive every filter,
    /// or the corpus is exhausted.
    ///
    /// `k + exclude.len()` would not be enough. One exclusion can match
    /// several physical rows, an unresolved hit consumes a slot without
    /// producing a candidate, and several chunks of one file can collapse
    /// into a single logical answer. So the fetch widens geometrically
    /// and re-runs from the top each round, which keeps the result a pure
    /// function of the query and the corpus — `recall` exposes no cursor,
    /// and a stateful pagination would make the outcome depend on how
    /// many rounds happened to run.
    fn retrieve_eligible(
        &self,
        query: &crate::recall_set::RecallQuery,
        filters: &RecallFilters,
        target: usize,
    ) -> Result<Eligible> {
        let mut fetch = target;
        let mut rounds = 0_usize;

        loop {
            rounds += 1;
            let hits = self
                .store
                .recall(&query.text, fetch, filters)
                .with_context(|| format!("recall failed for query {:?}", query.id))?;
            let physical_returned = hits.len();

            let above_floor: Vec<_> = hits
                .iter()
                .filter(|h| f64::from(h.score) > self.config.score_floor)
                .collect();

            let mut unresolved_hits = 0_usize;
            let candidates: Vec<(Vec<String>, f64)> = above_floor
                .iter()
                .filter_map(|h| {
                    let Some(canonical) = self.resolver.key_for(h) else {
                        // A hit that resolves to no key is not a
                        // candidate, but it did occupy a fetched slot.
                        // Counting it is what distinguishes "the corpus
                        // has nothing more" from "we did not look".
                        unresolved_hits += 1;
                        return None;
                    };
                    let mut keys = vec![canonical];
                    keys.extend(self.resolver.alias_keys(h));
                    Some((keys, f64::from(h.score)))
                })
                .collect();

            let (candidates, excluded_hits) = drop_source_documents(query, candidates);

            // Whether the corpus can offer more, independent of how many
            // survived: `recall` returning fewer rows than asked for is
            // the only honest exhaustion signal available.
            let exhausted = physical_returned < fetch;

            if candidates.len() >= target || exhausted || fetch >= MAX_FETCH {
                let top_score = candidates.first().map_or(0.0, |(_, score)| *score);
                let mut candidates = candidates;
                candidates.truncate(target);
                return Ok(Eligible {
                    candidates,
                    top_score,
                    physical_returned,
                    excluded_hits,
                    unresolved_hits,
                    rounds,
                    exhausted,
                });
            }

            fetch = (fetch * FETCH_GROWTH).min(MAX_FETCH);
        }
    }
}

/// Remove the documents a query declares as its own source, returning
/// the surviving candidates and how many were dropped.
///
/// A result matches when *any* of its keys is excluded, so an exclusion
/// written at either granularity works. The removal compacts the ranking:
/// the source document does not hold a top-K slot it was never eligible
/// for, which is the whole point of taking it out.
fn drop_source_documents(
    query: &crate::recall_set::RecallQuery,
    candidates: Vec<(Vec<String>, f64)>,
) -> (Vec<(Vec<String>, f64)>, usize) {
    if query.exclude.is_empty() {
        return (candidates, 0);
    }
    let excluded: std::collections::BTreeSet<&str> =
        query.exclude.iter().map(String::as_str).collect();
    let before = candidates.len();
    let kept: Vec<(Vec<String>, f64)> = candidates
        .into_iter()
        .filter(|(keys, _)| !keys.iter().any(|k| excluded.contains(k.as_str())))
        .collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

/// Choose, per result, the key granularity this query is judged at.
///
/// A result arrives as its canonical key followed by any coarser aliases
/// (`omem://s/code/x.rs#7`, then `omem://s/code/x.rs`). The query decides
/// which of them counts: whichever key it actually lists is the one used,
/// preferring the most specific. When it lists none, the canonical key is
/// used, so unjudged results stay distinct from one another and still
/// occupy their rank positions.
///
/// This is what makes mixed-granularity query sets scoreable in one run.
/// A `doc-heading` query judged against chunk keys is untouched by an
/// alias it never mentions, while a `commit-to-file` query judged against
/// file keys credits any chunk of a judged file. Collapsing several
/// chunks of one file onto one key produces repeats in the ranked list;
/// [`crate::recall_report::score_query_with_scores`] removes them.
fn project_to_judged_granularity(
    query: &crate::recall_set::RecallQuery,
    candidates: &[(Vec<String>, f64)],
) -> (Vec<String>, Vec<f64>) {
    let judged: std::collections::BTreeSet<&str> =
        query.relevant.iter().map(|g| g.key.as_str()).collect();

    let mut ranked = Vec::with_capacity(candidates.len());
    let mut scores = Vec::with_capacity(candidates.len());
    for (keys, score) in candidates {
        let Some(canonical) = keys.first() else {
            continue;
        };
        let chosen = keys
            .iter()
            .find(|k| judged.contains(k.as_str()))
            .unwrap_or(canonical);
        ranked.push(chosen.clone());
        scores.push(*score);
    }
    (ranked, scores)
}

/// Verify every judged key in `set` exists in `known`, returning the
/// keys that do not.
///
/// Run this once before evaluating. A missing key otherwise looks
/// exactly like a retrieval failure, which would make a stale query set
/// indistinguishable from a genuine quality regression.
#[must_use]
pub fn unknown_judgment_keys<'a>(
    set: &'a RecallQuerySet,
    known: &std::collections::BTreeSet<&str>,
) -> Vec<&'a str> {
    let mut missing = Vec::new();
    for query in &set.queries {
        for graded in &query.relevant {
            if !known.contains(graded.key.as_str()) {
                missing.push(graded.key.as_str());
            }
        }
        // Exclusions are checked too. An exclusion that no longer names
        // anything in the corpus fails silently — the source document
        // stays in the ranking and the report still says it was removed
        // — so a stale one has to be as loud as a stale judgment.
        for key in &query.exclude {
            if !known.contains(key.as_str()) {
                missing.push(key.as_str());
            }
        }
    }
    missing.sort_unstable();
    missing.dedup();
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall_set::RecallQuerySet;

    #[test]
    fn config_defaults_do_not_record_access() {
        let config = RecallRunnerConfig::default();
        assert!(
            !config.record_access,
            "evaluation must not mutate access counts"
        );
        let filters = config.filters();
        assert!(!filters.record_access);
        assert!(filters.spreading_activation);
    }

    #[test]
    fn config_filters_are_explicit_not_inherited() {
        let config = RecallRunnerConfig {
            spreading_activation: false,
            record_access: false,
            valid_at: Some(1_700_000_000),
            ..RecallRunnerConfig::default()
        };
        let filters = config.filters();
        assert!(!filters.spreading_activation);
        assert_eq!(filters.valid_at, Some(1_700_000_000));
    }

    fn candidates(rows: &[(&[&str], f64)]) -> Vec<(Vec<String>, f64)> {
        rows.iter()
            .map(|(keys, score)| (keys.iter().map(|k| (*k).to_string()).collect(), *score))
            .collect()
    }

    fn one_query(json: &str) -> crate::recall_set::RecallQuery {
        RecallQuerySet::from_str(json, "s")
            .unwrap()
            .queries
            .remove(0)
    }

    #[test]
    fn a_file_judged_query_credits_any_chunk_of_that_file() {
        let query = one_query(
            r#"{"id":"q","text":"t","category":"commit-to-file","relevant":[{"key":"omem://s/code/x.rs"}]}"#,
        );
        // Chunk 7 is retrieved; the judgment names the file. Before
        // aliases this scored zero although it is a correct answer.
        let (ranked, scores) = project_to_judged_granularity(
            &query,
            &candidates(&[(&["omem://s/code/x.rs#7", "omem://s/code/x.rs"], 0.8)]),
        );
        assert_eq!(ranked, vec!["omem://s/code/x.rs".to_string()]);
        assert_eq!(scores, vec![0.8]);
    }

    #[test]
    fn a_chunk_judged_query_ignores_the_alias() {
        // Same corpus, same aliases, but this query judges chunks. The
        // alias must not change what it measures.
        let query = one_query(
            r#"{"id":"q","text":"t","category":"doc-heading","relevant":[{"key":"omem://s/doc/a.md#2"}]}"#,
        );
        let (ranked, _) = project_to_judged_granularity(
            &query,
            &candidates(&[
                (&["omem://s/doc/a.md#1", "omem://s/doc/a.md"], 0.9),
                (&["omem://s/doc/a.md#2", "omem://s/doc/a.md"], 0.7),
            ]),
        );
        assert_eq!(
            ranked,
            vec![
                "omem://s/doc/a.md#1".to_string(),
                "omem://s/doc/a.md#2".to_string()
            ],
            "chunk-level judgments must keep chunk-level identity"
        );
    }

    #[test]
    fn unjudged_results_keep_their_canonical_identity() {
        let query = one_query(
            r#"{"id":"q","text":"t","category":"commit-to-file","relevant":[{"key":"omem://s/code/x.rs"}]}"#,
        );
        let (ranked, _) = project_to_judged_granularity(
            &query,
            &candidates(&[
                (&["omem://s/code/y.rs#0", "omem://s/code/y.rs"], 0.9),
                (&["omem://s/code/y.rs#1", "omem://s/code/y.rs"], 0.8),
            ]),
        );
        // Two distinct wrong answers, not one collapsed wrong answer:
        // they occupy two rank positions, as they should.
        assert_eq!(
            ranked,
            vec![
                "omem://s/code/y.rs#0".to_string(),
                "omem://s/code/y.rs#1".to_string()
            ]
        );
    }

    #[test]
    fn a_declared_source_document_leaves_the_ranking() {
        let query = one_query(
            r#"{"id":"q","text":"t","category":"commit-to-file","relevant":[{"key":"omem://s/code/x.rs"}],"exclude":["omem://s/commit/git/log#4"]}"#,
        );
        let (kept, dropped) = drop_source_documents(
            &query,
            candidates(&[
                (&["omem://s/commit/git/log#4"], 0.99),
                (&["omem://s/code/x.rs#7", "omem://s/code/x.rs"], 0.5),
            ]),
        );
        assert_eq!(dropped, 1);
        // The removal compacts: the answer is now rank 1, because the
        // document it was behind was never a candidate for this question.
        let (ranked, _) = project_to_judged_granularity(&query, &kept);
        assert_eq!(ranked, vec!["omem://s/code/x.rs".to_string()]);
    }

    #[test]
    fn an_exclusion_that_matches_nothing_reports_zero_rather_than_pretending() {
        // The failure mode worth catching: a rebuilt corpus renames the
        // source document, the exclusion silently stops applying, and the
        // confound is back in the ranking. The count is the only witness.
        let query = one_query(
            r#"{"id":"q","text":"t","category":"commit-to-file","relevant":[{"key":"omem://s/code/x.rs"}],"exclude":["omem://s/commit/git/log#stale"]}"#,
        );
        let (kept, dropped) = drop_source_documents(
            &query,
            candidates(&[(&["omem://s/commit/git/log#4"], 0.99)]),
        );
        assert_eq!(dropped, 0);
        assert_eq!(kept.len(), 1, "nothing was removed, and the report says so");
    }

    #[test]
    fn queries_without_exclusions_are_untouched() {
        let query =
            one_query(r#"{"id":"q","text":"t","category":"doc-heading","relevant":[{"key":"k"}]}"#);
        let input = candidates(&[(&["a"], 0.9), (&["b"], 0.8)]);
        let (kept, dropped) = drop_source_documents(&query, input.clone());
        assert_eq!(dropped, 0);
        assert_eq!(kept.len(), input.len());
    }

    #[test]
    fn detects_judgment_keys_absent_from_the_corpus() {
        let body = concat!(
            r#"{"id":"a","text":"t","category":"direct","relevant":[{"key":"present"}]}"#,
            "\n",
            r#"{"id":"b","text":"t","category":"direct","relevant":[{"key":"absent"}]}"#,
        );
        let set = RecallQuerySet::from_str(body, "s").unwrap();
        let known: std::collections::BTreeSet<&str> = ["present"].into_iter().collect();

        let missing = unknown_judgment_keys(&set, &known);
        assert_eq!(missing, vec!["absent"]);
    }

    #[test]
    fn a_stale_exclusion_key_is_reported_like_a_stale_judgment() {
        let body = r#"{"id":"a","text":"t","category":"c","relevant":[{"key":"present"}],"exclude":["gone"]}"#;
        let set = RecallQuerySet::from_str(body, "s").unwrap();
        let known: std::collections::BTreeSet<&str> = ["present"].into_iter().collect();
        assert_eq!(unknown_judgment_keys(&set, &known), vec!["gone"]);
    }

    #[test]
    fn no_missing_keys_when_corpus_matches() {
        let body = r#"{"id":"a","text":"t","category":"direct","relevant":[{"key":"k"}]}"#;
        let set = RecallQuerySet::from_str(body, "s").unwrap();
        let known: std::collections::BTreeSet<&str> = ["k"].into_iter().collect();
        assert!(unknown_judgment_keys(&set, &known).is_empty());
    }
}
