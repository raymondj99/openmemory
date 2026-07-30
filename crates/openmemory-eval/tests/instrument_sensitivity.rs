//! Proves the retrieval-quality instrument still works.
//!
//! A quality gate that has silently stopped detecting regressions is
//! worse than no gate: it reports green and everyone believes it. These
//! tests run the *real* recall path against a real store and check two
//! properties that make a green result meaningful:
//!
//! - **sensitivity** — a deliberately degraded retrieval configuration
//!   is caught;
//! - **specificity** — an unchanged configuration is not flagged, and
//!   repeated runs agree exactly.
//!
//! They use an in-memory store and a deterministic embedder rather than
//! a fixture corpus, so they run in ordinary CI with no personal data
//! and nothing to keep in sync.
//!
//! The degradation used is not synthetic bookkeeping: it changes the
//! actual search mode the store runs, so the instrument has to notice a
//! difference produced by the production retrieval path.

#![cfg(all(feature = "fts5", feature = "testing"))]

use std::sync::Arc;

use openmemory_core::config::Config;
use openmemory_core::testing::{Embedder, FakeEmbedder};
use openmemory_eval::gates::{evaluate, QualityGates};
use openmemory_eval::recall_report::RecallReport;
use openmemory_eval::recall_runner::{RecallRunner, RecallRunnerConfig};
use openmemory_eval::recall_set::RecallQuerySet;
use openmemory_eval::resolver::MapResolver;
use openmemory_graph::types::EntityType;
use openmemory_graph::{MemoryStore, ObservationInput};
use openmemory_index::SearchMode;

/// A store with several distinguishable topics, so retrieval has
/// something to get right or wrong.
fn store_with_content() -> (MemoryStore, MapResolver) {
    ingest(false)
}

/// The same source topics ingested by a system whose extractor silently
/// stored a placeholder instead of the body for part of the corpus.
///
/// This is the degradation the instrument has to catch, and it is
/// deliberately *not* a configuration change: the query set, the search
/// mode, K, the spreading flag, and the corpus identity are all
/// identical, so the two runs are legitimately comparable and the drop
/// is attributable to the system rather than to the experiment being
/// redefined. A regression in ingest is exactly the failure that reaches
/// users while every latency budget stays green.
fn store_with_degraded_ingest() -> (MemoryStore, MapResolver) {
    ingest(true)
}

/// `(entity, query text, body)` for each topic.
///
/// The query is a phrase from the body rather than the entity name, so
/// retrieval has to work through the indexed content. Querying by name
/// would let a topic be found by its title alone, and a corpus-side
/// regression would leave the score untouched.
const TOPICS: [(&str, &str, &str); 6] = [
    (
        "photosynthesis",
        "chloroplasts converting light into chemical energy",
        "chloroplasts convert light into chemical energy in plants",
    ),
    (
        "volcano",
        "magma rising through the crust erupting as lava",
        "magma rises through the crust and erupts as lava flows",
    ),
    (
        "harpsichord",
        "strings plucked by quills instead of struck by hammers",
        "strings are plucked by quills rather than struck by hammers",
    ),
    (
        "glacier",
        "compacted snow carving valleys over centuries",
        "compacted snow flows downhill carving valleys over centuries",
    ),
    (
        "antibiotic",
        "compounds inhibiting bacterial cell wall synthesis",
        "compounds that inhibit bacterial cell wall synthesis",
    ),
    (
        "cartography",
        "projections trading area fidelity for angular fidelity",
        "projections trade area fidelity against angular fidelity",
    ),
];

/// Which topics the degraded ingest damages. Two of six, so the drop is
/// large enough to be unambiguous and partial enough to be realistic.
const DAMAGED: [&str; 2] = ["harpsichord", "cartography"];

fn ingest(truncate: bool) -> (MemoryStore, MapResolver) {
    let store = MemoryStore::open_in_memory(&Config::default())
        .expect("in-memory store")
        .with_embedder(Arc::new(FakeEmbedder::new(32)) as Arc<dyn Embedder>);

    let mut pairs = Vec::new();
    for (name, _, body) in TOPICS {
        let text = if truncate && DAMAGED.contains(&name) {
            // The defect: for some records the extractor returned
            // nothing and a placeholder was stored in place of the body.
            // The entity, its name, the relation graph, the source text,
            // and the row count are all unchanged, so nothing but
            // retrieval quality moves — and only for part of the corpus,
            // which is the realistic shape. A total collapse would prove
            // the gate fires on catastrophe without showing it can see a
            // partial regression.
            "record unavailable"
        } else {
            body
        };
        let outcome = store
            .remember(
                name,
                EntityType::Concept,
                &[ObservationInput::new(text)],
                &[],
                "instrument-test",
            )
            .expect("write");
        // Stable key, so one query set judges both stores. Observation
        // IDs are per-ingest and would make the two runs incomparable
        // for a reason that has nothing to do with quality.
        pairs.push((outcome.observation_ids[0].clone(), format!("topic:{name}")));
    }
    (store, MapResolver::new(pairs))
}

/// One query per topic, judged against that topic's stable key.
fn query_set() -> RecallQuerySet {
    let rows: Vec<String> = TOPICS
        .iter()
        .enumerate()
        .map(|(i, (name, text, _))| {
            format!(
                r#"{{"id":"q{i}","text":"{text}","category":"direct","relevant":[{{"key":"topic:{name}","relevance":1}}]}}"#
            )
        })
        .collect();
    RecallQuerySet::from_str(&rows.join("\n"), "instrument").expect("query set")
}

fn run(
    store: &MemoryStore,
    resolver: &MapResolver,
    set: &RecallQuerySet,
    mode: SearchMode,
    label: &str,
) -> RecallReport {
    RecallRunner::new(
        store,
        resolver.clone(),
        RecallRunnerConfig {
            mode: Some(mode),
            strategy: label.to_string(),
            corpus: "instrument-topics".to_string(),
            ..RecallRunnerConfig::default()
        },
    )
    .run(set)
    .expect("run")
}

#[test]
fn the_instrument_reports_repeatable_results() {
    // Everything downstream assumes a run is reproducible. If it is not,
    // every comparison is noise and no gate means anything.
    let (store, resolver) = store_with_content();
    let set = query_set();

    let first = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");
    let second = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");

    assert!(
        (first.overall.ndcg_at_10 - second.overall.ndcg_at_10).abs() < f64::EPSILON,
        "repeated runs disagreed: {:.6} vs {:.6}",
        first.overall.ndcg_at_10,
        second.overall.ndcg_at_10
    );
    assert_eq!(first.per_query.len(), second.per_query.len());
}

#[test]
fn the_instrument_measures_something_real() {
    // A gate comparing two identical zeros would also "pass". The
    // baseline has to actually retrieve, or sensitivity is untestable.
    let (store, resolver) = store_with_content();
    let set = query_set();
    let report = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");

    assert!(
        report.overall.r_at_10 > 0.5,
        "baseline retrieval is too weak to test a gate against: R@10 {:.4}",
        report.overall.r_at_10
    );
    assert_eq!(report.overall.n_queries, TOPICS.len());
}

#[test]
fn an_unchanged_configuration_passes_the_gates() {
    // Specificity. A gate that fires on an unchanged run gets disabled
    // by the first person it annoys, and then protects nothing.
    let (store, resolver) = store_with_content();
    let set = query_set();

    let baseline = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");
    let current = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");

    let outcome = evaluate(&current, Some(&baseline), &QualityGates::default());
    assert!(
        outcome.passed(),
        "unchanged run was flagged: {:?}",
        outcome.failures()
    );
}

#[test]
fn a_degraded_system_is_caught_under_an_identical_experiment() {
    // Sensitivity, driven through the production recall path. Both arms
    // declare the same query set, mode, K, spreading flag, and corpus,
    // so the comparison is legitimate and the whole drop is
    // attributable to the system. The degradation is an ingest defect
    // that stores less of each observation — the failure class that
    // reaches users while every latency budget stays green.
    let (good, good_keys) = store_with_content();
    let (broken, broken_keys) = store_with_degraded_ingest();
    let set = query_set();

    let baseline = run(&good, &good_keys, &set, SearchMode::KeywordOnly, "baseline");
    let degraded = run(
        &broken,
        &broken_keys,
        &set,
        SearchMode::KeywordOnly,
        "baseline",
    );

    // The comparison must be admissible, or this proves only that the
    // contract check works.
    let contracts = (
        baseline.contract.as_ref().expect("baseline contract"),
        degraded.contract.as_ref().expect("degraded contract"),
    );
    assert!(
        contracts.0.comparable_to(contracts.1),
        "the two arms are not comparable, so a failure below would not be \
         evidence of sensitivity: {:?}",
        contracts.0.differences(contracts.1)
    );

    assert!(
        degraded.overall.ndcg_at_10 < baseline.overall.ndcg_at_10,
        "the degraded arm was not actually worse ({:.4} vs {:.4}); this test \
         cannot prove sensitivity without a real degradation",
        degraded.overall.ndcg_at_10,
        baseline.overall.ndcg_at_10
    );

    let outcome = evaluate(&degraded, Some(&baseline), &QualityGates::default());
    assert!(
        !outcome.passed(),
        "a real retrieval regression passed every gate: {:.4} -> {:.4}",
        baseline.overall.ndcg_at_10,
        degraded.overall.ndcg_at_10
    );

    // And it must say what moved, not merely that something did.
    let detail: String = outcome
        .failures()
        .iter()
        .filter_map(|o| o.detail.clone())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        detail.contains("NDCG@10") || detail.contains("R@10"),
        "failure gave no actionable detail: {detail}"
    );
}

#[test]
fn a_redefined_experiment_is_refused_rather_than_scored() {
    // The other half of the instrument's job. Changing the search mode
    // changes what the numbers mean, so the drop it produces is not a
    // regression measurement. The gate must say the comparison is
    // inadmissible instead of reporting a delta that reads like one.
    let (store, resolver) = store_with_content();
    let set = query_set();

    let baseline = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");
    let other = run(&store, &resolver, &set, SearchMode::VectorOnly, "baseline");

    let outcome = evaluate(&other, Some(&baseline), &QualityGates::default());
    assert!(!outcome.passed());
    let failure = outcome
        .failures()
        .iter()
        .find(|o| o.name == "comparable_experiment")
        .expect("comparability must fire")
        .detail
        .clone()
        .unwrap_or_default();
    assert!(failure.contains("mode"), "{failure}");
    assert_eq!(
        outcome.outcomes.len(),
        1,
        "scoring continued past an inadmissible comparison"
    );
}

#[test]
fn evaluation_does_not_mutate_the_corpus_it_measures() {
    // `recall` bumps `access_count`, which feeds the ranking. An
    // evaluation that records access changes the thing it is measuring
    // and makes the second run of any comparison invalid.
    let config = RecallRunnerConfig::default();
    assert!(!config.record_access);
    assert!(!config.filters().record_access);

    let (store, resolver) = store_with_content();
    let set = query_set();

    let first = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");
    for _ in 0..5 {
        let _ = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");
    }
    let last = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");

    assert!(
        (first.overall.ndcg_at_10 - last.overall.ndcg_at_10).abs() < f64::EPSILON,
        "repeated evaluation drifted, so a run is changing the corpus: \
         {:.6} -> {:.6}",
        first.overall.ndcg_at_10,
        last.overall.ndcg_at_10
    );
}

#[test]
fn a_baseline_survives_being_written_and_read_back() {
    // Baselines live on disk between CI runs. A report that cannot
    // round-trip makes gating impossible, and this failed in practice.
    let (store, resolver) = store_with_content();
    let set = query_set();
    let baseline = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");

    let json = serde_json::to_string(&baseline).expect("serialise");
    let restored: RecallReport = serde_json::from_str(&json).expect("deserialise");

    let direct = evaluate(&baseline, Some(&baseline), &QualityGates::default());
    let round_tripped = evaluate(&restored, Some(&restored), &QualityGates::default());
    assert_eq!(direct.passed(), round_tripped.passed());
    assert!((restored.overall.ndcg_at_10 - baseline.overall.ndcg_at_10).abs() < 1e-12);
}

/// A corpus whose documents are *chunks of files*, which is the shape
/// every real ingest produces and the shape the topic fixture above does
/// not have.
///
/// Returns a resolver whose canonical keys are chunk keys and whose
/// aliases are the file the chunk came from.
fn store_of_chunked_files() -> (MemoryStore, MapResolver) {
    const CHUNKS: [(&str, usize, &str); 4] = [
        ("atlas.md", 0, "introduction to the mapping of coastlines"),
        ("atlas.md", 1, "tables of tidal ranges by month"),
        (
            "atlas.md",
            2,
            "the isopleth notation used for magnetic declination",
        ),
        (
            "almanac.md",
            0,
            "sunrise and sunset tables for each latitude",
        ),
    ];

    let store = MemoryStore::open_in_memory(&Config::default())
        .expect("in-memory store")
        .with_embedder(Arc::new(FakeEmbedder::new(32)) as Arc<dyn Embedder>);

    let mut pairs = Vec::new();
    let mut aliases = Vec::new();
    for (file, ordinal, body) in CHUNKS {
        let outcome = store
            .remember(
                file,
                EntityType::Concept,
                &[ObservationInput::new(body)],
                &[],
                "instrument-test",
            )
            .expect("write");
        let id = outcome.observation_ids[0].clone();
        pairs.push((id.clone(), format!("omem://t/doc/{file}#{ordinal}")));
        aliases.push((id, format!("omem://t/doc/{file}")));
    }
    (store, MapResolver::new(pairs).with_aliases(aliases))
}

#[test]
fn a_file_granularity_judgment_credits_any_chunk_of_that_file() {
    // The defect and its repair, end to end through `recall()`.
    //
    // The question is "which file covers declination notation", and the
    // answer lives in chunk 2. A judgment list naming only chunk 0 —
    // which is what a per-file chunk cap produces for any file longer
    // than the cap — scores this a total miss although retrieval
    // returned a chunk of exactly the right file. Judged at file
    // granularity, the same run is a hit.
    let (store, resolver) = store_of_chunked_files();
    let text = "the isopleth notation used for magnetic declination";

    let partial = RecallQuerySet::from_str(
        &format!(
            r#"{{"id":"q","text":"{text}","category":"commit-to-file","relevant":[{{"key":"omem://t/doc/atlas.md#0"}}]}}"#
        ),
        "partial-chunk-listing",
    )
    .expect("query set");
    let by_file = RecallQuerySet::from_str(
        &format!(
            r#"{{"id":"q","text":"{text}","category":"commit-to-file","relevant":[{{"key":"omem://t/doc/atlas.md"}}]}}"#
        ),
        "file-granularity",
    )
    .expect("query set");

    let missed = run(
        &store,
        &resolver,
        &partial,
        SearchMode::KeywordOnly,
        "baseline",
    );
    let found = run(
        &store,
        &resolver,
        &by_file,
        SearchMode::KeywordOnly,
        "baseline",
    );

    assert!(
        missed.overall.r_at_10.abs() < 1e-12,
        "expected the partial chunk listing to miss: {:.4}",
        missed.overall.r_at_10
    );
    assert!(
        (found.overall.r_at_10 - 1.0).abs() < 1e-12,
        "any chunk of the judged file must count: R@10 {:.4}",
        found.overall.r_at_10
    );
    // Retrieval returned the same rows in both runs — only the judgment
    // granularity differs, so this is a measurement repair and not a
    // retrieval improvement.
    assert_eq!(
        missed.per_query[0].returned, found.per_query[0].returned,
        "the two runs must differ only in how their results were judged"
    );
}

#[test]
fn several_chunks_of_one_judged_file_do_not_inflate_the_score() {
    // All three atlas chunks are retrievable for this query, so the
    // ranked list holds the same file key three times. NDCG@10 counts a
    // repeated key's gain again unless the list is collapsed, which
    // would let one right answer score better than one right answer.
    let (store, resolver) = store_of_chunked_files();
    let set = RecallQuerySet::from_str(
        r#"{"id":"q","text":"tables coastlines isopleth","category":"commit-to-file","relevant":[{"key":"omem://t/doc/atlas.md"}]}"#,
        "file-granularity",
    )
    .expect("query set");

    let report = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");
    let scored = &report.per_query[0];

    assert!(
        scored.returned >= 2,
        "this query is only interesting if several chunks come back: {}",
        scored.returned
    );
    assert!(
        (scored.r_at_10 - 1.0).abs() < 1e-12 && (scored.ndcg_at_10 - 1.0).abs() < 1e-12,
        "one judged file found once is a perfect result, no more and no \
         less: R@10 {:.4} NDCG@10 {:.4}",
        scored.r_at_10,
        scored.ndcg_at_10
    );
}

#[test]
fn a_query_is_not_scored_against_the_document_it_was_written_from() {
    // The generated-query confound, end to end. The query text is lifted
    // from one document ("source"), so retrieval ranks that document
    // first; the answer the query is *about* sits behind it. Declaring
    // the source removes it from the ranking, and the answer's rank
    // improves by exactly the slot the source was holding.
    let store = MemoryStore::open_in_memory(&Config::default())
        .expect("in-memory store")
        .with_embedder(Arc::new(FakeEmbedder::new(32)) as Arc<dyn Embedder>);

    let text = "the isopleth notation used for magnetic declination";
    let docs: Vec<(String, String)> = vec![
        // Contains the query verbatim, as a commit record contains its
        // own subject.
        ("source".into(), text.into()),
        (
            "answer".into(),
            "declination is drawn with isopleth contours here".into(),
        ),
    ];
    let mut pairs = Vec::new();
    for (name, body) in &docs {
        let outcome = store
            .remember(
                name,
                EntityType::Concept,
                &[ObservationInput::new(body.clone())],
                &[],
                "instrument-test",
            )
            .expect("write");
        pairs.push((outcome.observation_ids[0].clone(), format!("doc:{name}")));
    }
    let resolver = MapResolver::new(pairs);

    let scored_with = |exclude: &str| {
        let json = if exclude.is_empty() {
            format!(
                r#"{{"id":"q","text":"{text}","category":"c","relevant":[{{"key":"doc:answer"}}]}}"#
            )
        } else {
            format!(
                r#"{{"id":"q","text":"{text}","category":"c","relevant":[{{"key":"doc:answer"}}],"exclude":["{exclude}"]}}"#
            )
        };
        let set = RecallQuerySet::from_str(&json, "source-doc").expect("query set");
        let report = run(&store, &resolver, &set, SearchMode::KeywordOnly, "baseline");
        report.per_query[0].clone()
    };

    let kept = scored_with("");
    let removed = scored_with("doc:source");

    assert_eq!(kept.excluded_hits, 0);
    assert_eq!(
        removed.excluded_hits, 1,
        "the source document must actually have been in the ranking, \
         or this test proves nothing"
    );
    assert!(
        removed.reciprocal_rank > kept.reciprocal_rank,
        "removing the source should promote the answer: {} -> {}",
        kept.reciprocal_rank,
        removed.reciprocal_rank
    );
    assert!(
        (removed.reciprocal_rank - 1.0).abs() < 1e-12,
        "with the source gone the answer is rank 1, got {}",
        removed.reciprocal_rank
    );
    // The slot-refill invariant this fixture cannot test: with two
    // documents in the store, excluding one leaves nothing to refill
    // with and `corpus_exhausted` is the honest answer. The invariant is
    // tested where it can bite, against a corpus with a tail — see
    // `a_relevant_document_at_eligible_rank_ten_is_found_after_an_exclusion`.
    assert!(
        removed.corpus_exhausted,
        "this fixture holds two documents, so excluding one must exhaust it; \
         if that changed, this test is no longer measuring what it says"
    );
}

#[test]
fn a_broad_question_is_excused_by_declaration_and_not_by_a_lowered_threshold() {
    // End to end through the production recall path: a question with
    // more right answers than K reaches the runner, the ceiling, and the
    // gate. Undeclared it is refused; declared it passes and the verdict
    // names the exception. Nothing about the query or the corpus differs
    // between the two runs — only whether the exception was written down.
    let store = MemoryStore::open_in_memory(&Config::default())
        .expect("in-memory store")
        .with_embedder(Arc::new(FakeEmbedder::new(32)) as Arc<dyn Embedder>);

    // Twelve versions of one setting, the shape a change-history
    // question has: every version is a right answer.
    let mut pairs = Vec::new();
    for i in 0..12 {
        let outcome = store
            .remember(
                &format!("version-{i}"),
                EntityType::Concept,
                &[ObservationInput::new(format!(
                    "the packaging manifest pinned the toolchain to release {i}"
                ))],
                &[],
                "instrument-test",
            )
            .expect("write");
        pairs.push((outcome.observation_ids[0].clone(), format!("v{i}")));
    }
    let resolver = MapResolver::new(pairs);

    let judged: String = (0..12)
        .map(|i| format!(r#"{{"key":"v{i}","relevance":2}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let build = |waiver: &str| {
        let row = format!(
            r#"{{"id":"history","text":"every release the packaging manifest pinned the toolchain to","category":"temporal-history","relevant":[{judged}]{waiver}}}"#
        );
        RecallQuerySet::from_str(&row, "history").expect("query set")
    };

    let undeclared = run(
        &store,
        &resolver,
        &build(""),
        SearchMode::KeywordOnly,
        "baseline",
    );
    let declared = run(
        &store,
        &resolver,
        &build(r#","capacity_waiver":{"reason":"one right answer per recorded version"}"#),
        SearchMode::KeywordOnly,
        "baseline",
    );

    // Undeclared: the ceiling sees an unreachable query and the gate
    // refuses it, naming the query rather than quoting only a mean.
    let ceiling = undeclared
        .recall_ceiling
        .as_ref()
        .expect("the runner must measure a ceiling");
    assert_eq!(ceiling.capped_queries, 1);
    assert!(ceiling
        .min_ceiling
        .is_some_and(|v| (v - 10.0 / 12.0).abs() < 1e-9));
    let refusal = evaluate(&undeclared, None, &QualityGates::default());
    let detail = refusal
        .failures()
        .iter()
        .find(|o| o.name == "recall_ceiling")
        .and_then(|o| o.detail.clone())
        .expect("an undeclared over-K query must be refused");
    assert!(detail.contains("history"), "{detail}");

    // Declared: excused, and the verdict carries the justification.
    let ceiling = declared.recall_ceiling.as_ref().expect("ceiling");
    assert_eq!(ceiling.capped_queries, 0);
    assert_eq!(ceiling.waived.len(), 1);
    let verdict = evaluate(&declared, None, &QualityGates::default());
    assert!(verdict.passed(), "{:?}", verdict.failures());
    assert!(
        verdict.capacity_waivers[0]
            .reason
            .contains("per recorded version"),
        "{:?}",
        verdict.capacity_waivers
    );

    // What the exception costs and what it does not: raw recall drops
    // the query, the capacity-normalised measure keeps it, and NDCG —
    // attainable either way — is identical in both runs.
    assert_eq!(declared.overall.n_queries, 1);
    assert_eq!(declared.overall.n_recall_queries, 0);
    assert!(
        (declared.overall.ndcg_at_10 - undeclared.overall.ndcg_at_10).abs() < 1e-12,
        "declaring an exception must not change what was retrieved"
    );
    assert!(
        declared.overall.capacity_r_at_10 >= declared.per_query[0].r_at_10,
        "capacity recall cannot be below raw recall: {:?}",
        declared.overall
    );
}

/// The tail must be fetched, not left behind by a compacted list.
///
/// This is the defect the unit tests could not see: they handed
/// `drop_source_documents` a pre-built vector, so they exercised the
/// filter and never the fetch. The runner fetched exactly K rows, removed
/// the excluded source, and scored the query over K-1 candidates — with
/// the document that should have taken the vacant slot never retrieved.
///
/// The construction is deliberately exact. Eleven documents; the query's
/// declared source ranks first; the only relevant document ranks
/// **eleventh physically**, which is **tenth among eligible documents**.
/// Fetching ten rows and filtering afterwards can never see it, so `R@10`
/// is 0. Retrieving until ten eligible documents survive finds it at the
/// last eligible slot, and `R@10` is 1. The two behaviours are therefore
/// distinguishable by the metric itself, not merely by a bookkeeping
/// field whose meaning changed with the fix.
#[test]
fn a_relevant_document_at_eligible_rank_ten_is_found_after_an_exclusion() {
    let store = MemoryStore::open_in_memory(&Config::default())
        .expect("in-memory store")
        .with_embedder(Arc::new(FakeEmbedder::new(32)) as Arc<dyn Embedder>);

    // One shared term makes every document a hit. BM25 prefers shorter
    // documents, so padding length fixes the order: doc00 first,
    // doc10 last.
    let mut ids = Vec::new();
    for i in 0..11 {
        let outcome = store
            .remember(
                &format!("doc{i:02}"),
                EntityType::Concept,
                &[ObservationInput::new(format!(
                    "quarklight {}",
                    "filler ".repeat(i + 1)
                ))],
                &[],
                "tail-test",
            )
            .expect("write");
        ids.push(outcome.observation_ids[0].clone());
    }
    let resolver = MapResolver::new(
        ids.iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), format!("doc:{i:02}"))),
    );

    let run = |body: &str| {
        RecallRunner::new(
            &store,
            resolver.clone(),
            RecallRunnerConfig {
                mode: Some(SearchMode::KeywordOnly),
                strategy: "tail".into(),
                corpus: "tail-test".into(),
                ..RecallRunnerConfig::default()
            },
        )
        .run(&RecallQuerySet::from_str(body, "tail").expect("query set"))
        .expect("run")
    };

    // Establish the premise rather than assuming it: without an
    // exclusion, the eleventh document is outside the top ten and scores
    // zero. If this ever stops holding, the test below proves nothing and
    // says so here instead of passing quietly.
    let baseline = run(
        r#"{"id":"q","text":"quarklight","category":"direct","relevant":[{"key":"doc:10","relevance":1}]}"#,
    );
    assert!(
        baseline.per_query[0].r_at_10.abs() < f64::EPSILON,
        "premise broken: the target was already inside the top ten (R@10 {})",
        baseline.per_query[0].r_at_10
    );

    // Now exclude the document ranked first. That frees exactly one
    // eligible slot, and the target should occupy it.
    let repaired = run(
        r#"{"id":"q","text":"quarklight","category":"direct","relevant":[{"key":"doc:10","relevance":1}],"exclude":["doc:00"]}"#,
    );
    let row = &repaired.per_query[0];

    assert_eq!(
        row.excluded_hits, 1,
        "the exclusion did not fire, so nothing was freed"
    );
    assert!(
        (row.r_at_10 - 1.0).abs() < f64::EPSILON,
        "R@10 is {} after excluding the top-ranked document: the freed slot \
         was left empty and the eleventh row was never fetched",
        row.r_at_10
    );
    assert_eq!(
        row.returned, 10,
        "ten eligible documents should have been scored, not {}",
        row.returned
    );
    assert!(
        row.physical_returned > 10,
        "scoring ten eligible documents after dropping one requires fetching \
         more than ten rows; only {} were fetched",
        row.physical_returned
    );
    assert!(
        row.fetch_rounds > 1,
        "the fetch should have widened; it ran {} round(s)",
        row.fetch_rounds
    );
}

/// The reported top score must be a score that was actually scored.
#[test]
fn an_excluded_document_does_not_supply_the_reported_top_score() {
    let store = MemoryStore::open_in_memory(&Config::default())
        .expect("in-memory store")
        .with_embedder(Arc::new(FakeEmbedder::new(32)) as Arc<dyn Embedder>);

    let mut ids = Vec::new();
    for i in 0..4 {
        let outcome = store
            .remember(
                &format!("t{i}"),
                EntityType::Concept,
                &[ObservationInput::new(format!(
                    "zephyrine {}",
                    "pad ".repeat(i + 1)
                ))],
                &[],
                "top-score-test",
            )
            .expect("write");
        ids.push(outcome.observation_ids[0].clone());
    }
    let resolver = MapResolver::new(
        ids.iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), format!("t:{i}"))),
    );

    let run = |body: &str| {
        RecallRunner::new(
            &store,
            resolver.clone(),
            RecallRunnerConfig {
                mode: Some(SearchMode::KeywordOnly),
                strategy: "s".into(),
                corpus: "top-score-test".into(),
                ..RecallRunnerConfig::default()
            },
        )
        .run(&RecallQuerySet::from_str(body, "s").expect("set"))
        .expect("run")
    };

    let plain = run(
        r#"{"id":"q","text":"zephyrine","category":"direct","relevant":[{"key":"t:3","relevance":1}]}"#,
    );
    let top_key = plain.per_query[0].top_score;

    // Exclude whatever ranked first, then the reported top score must
    // belong to something else. Quoting the excluded document's score
    // would describe a ranking the query was never scored against.
    let excluded = run(
        r#"{"id":"q","text":"zephyrine","category":"direct","relevant":[{"key":"t:3","relevance":1}],"exclude":["t:0"]}"#,
    );
    let row = &excluded.per_query[0];
    if row.excluded_hits > 0 {
        assert!(
            (row.top_score - top_key).abs() > f64::EPSILON
                || row.returned == plain.per_query[0].returned,
            "the excluded document's score was reported as the top score"
        );
    }
}
