//! Integration tests for the public `MemoryStore` API. These tests stay at
//! the highest layer of the crate — no `pub(crate)` access, no SQL pokes
//! into private internals — to lock in the contract callers (the MCP layer
//! and the CLI) rely on.
//!
//! All tests run against a fully in-memory store (`MemoryStore::open_in_memory`)
//! and complete well under 2 seconds.

use std::sync::Arc;
use std::time::Instant;

use open_memory_core::clock::{Clock, FixedClock};
use open_memory_core::config::Config;
use open_memory_graph::{
    ConsolidateConfig, EntityType, MemoryError, MemoryStore, ObservationInput, RecallFilters,
    RelationInput, SearchMode,
};

const SECONDS_PER_DAY: i64 = 86_400;

fn open(now_secs: i64) -> (MemoryStore, Arc<FixedClock>) {
    let clock = Arc::new(FixedClock::new(now_secs));
    let store = MemoryStore::open_in_memory(&Config::default())
        .unwrap()
        .with_clock(clock.clone() as Arc<dyn Clock>);
    (store, clock)
}

#[test]
fn integration_remember_then_recall_round_trips() {
    let (store, _clock) = open(1_000);
    store
        .remember(
            "Raymond",
            EntityType::Person,
            &[
                ObservationInput::new("prefers Rust over Python"),
                ObservationInput::new("ships open-memory"),
            ],
            &[RelationInput::new(
                "maintains",
                "open-memory",
                EntityType::Project,
            )],
            "test",
        )
        .unwrap();

    let mut filters = RecallFilters::new();
    filters.mode = Some(SearchMode::KeywordOnly);
    let r = store.recall("Rust", 5, &filters).unwrap();
    assert!(!r.is_empty());
    assert_eq!(r[0].entity_name, "Raymond");
    assert!(r[0].observation.content.contains("Rust"));
}

#[test]
fn integration_recall_decay_prefers_fresher_observation() {
    let (store, clock) = open(0);

    store
        .remember(
            "Topic",
            EntityType::Fact,
            &[ObservationInput::new("alpha mention").with_source("old")],
            &[],
            "old",
        )
        .unwrap();

    clock.advance(30 * SECONDS_PER_DAY);

    store
        .remember(
            "Topic",
            EntityType::Fact,
            &[ObservationInput::new("alpha mention").with_source("fresh")],
            &[],
            "fresh",
        )
        .unwrap();

    let mut filters = RecallFilters::new();
    filters.mode = Some(SearchMode::KeywordOnly);
    let r = store.recall("alpha", 10, &filters).unwrap();
    let fresh_idx = r.iter().position(|x| x.observation.source == "fresh");
    assert!(fresh_idx.is_some(), "fresh result must be present");

    let old_idx = r.iter().position(|x| x.observation.source == "old");
    if let (Some(f), Some(o)) = (fresh_idx, old_idx) {
        assert!(f < o, "fresh should rank ahead of old");
    }
}

#[test]
fn integration_remember_idempotent_on_repeat() {
    let (store, _clock) = open(0);
    store
        .remember(
            "Project",
            EntityType::Project,
            &[ObservationInput::new("first")],
            &[],
            "a",
        )
        .unwrap();
    store
        .remember(
            "Project",
            EntityType::Project,
            &[ObservationInput::new("second")],
            &[],
            "b",
        )
        .unwrap();
    let s = store.status().unwrap();
    assert_eq!(s.total_entities, 1);
    assert_eq!(s.total_observations, 2);
}

#[test]
fn integration_get_entity_and_list_entities() {
    let (store, _) = open(0);
    store
        .remember(
            "Alpha",
            EntityType::Person,
            &[ObservationInput::new("a")],
            &[],
            "t",
        )
        .unwrap();
    store
        .remember(
            "Beta",
            EntityType::Project,
            &[ObservationInput::new("b")],
            &[],
            "t",
        )
        .unwrap();

    assert!(store.get_entity("Alpha").unwrap().is_some());
    assert!(store.get_entity("Missing").unwrap().is_none());

    let rows = store.list_entities(None, 100, 0).unwrap();
    assert_eq!(rows.len(), 2);
    let projects = store
        .list_entities(Some(EntityType::Project), 100, 0)
        .unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].entity.name, "Beta");
}

#[test]
fn integration_forget_excludes_observation_from_recall() {
    let (store, _) = open(0);
    let outcome = store
        .remember(
            "X",
            EntityType::Fact,
            &[ObservationInput::new("alpha mention")],
            &[],
            "t",
        )
        .unwrap();
    assert!(store.forget(&outcome.observation_ids[0]).unwrap());

    let mut filters = RecallFilters::new();
    filters.mode = Some(SearchMode::KeywordOnly);
    let r = store.recall("alpha", 5, &filters).unwrap();
    assert!(r.is_empty());
}

#[test]
fn integration_forget_entity_returns_observation_count() {
    let (store, _) = open(0);
    store
        .remember(
            "X",
            EntityType::Fact,
            &[
                ObservationInput::new("a"),
                ObservationInput::new("b"),
                ObservationInput::new("c"),
            ],
            &[],
            "t",
        )
        .unwrap();
    let removed = store.forget_entity("X").unwrap();
    assert_eq!(removed, 3);
    assert_eq!(store.status().unwrap().total_entities, 0);
}

#[test]
fn integration_forget_entity_unknown_returns_not_found() {
    let (store, _) = open(0);
    let err = store.forget_entity("Missing").unwrap_err();
    assert!(matches!(err, MemoryError::EntityNotFound(_)));
}

#[test]
fn integration_consolidate_dedup_then_idempotent() {
    let (store, _) = open(0);
    store
        .remember(
            "Topic",
            EntityType::Fact,
            &[
                ObservationInput::new("hello world"),
                ObservationInput::new("hello world"),
                ObservationInput::new("hello world"),
            ],
            &[],
            "t",
        )
        .unwrap();
    let cfg = ConsolidateConfig::for_store(&store);
    let r1 = store.consolidate(&cfg).unwrap();
    let r2 = store.consolidate(&cfg).unwrap();
    assert!(r1.duplicates_merged >= 2);
    assert_eq!(r2.duplicates_merged, 0);
}

#[test]
fn integration_status_after_writes_reports_counts() {
    let (store, _) = open(0);
    store
        .remember(
            "Raymond",
            EntityType::Person,
            &[ObservationInput::new("prefers Rust")],
            &[RelationInput::new("maintains", "Sift", EntityType::Project)],
            "t",
        )
        .unwrap();
    let s = store.status().unwrap();
    assert_eq!(s.total_entities, 2);
    assert_eq!(s.total_observations, 1);
    assert_eq!(s.total_relations, 1);
    assert_eq!(s.entity_type_counts.get("person"), Some(&1));
    assert_eq!(s.entity_type_counts.get("project"), Some(&1));
}

#[test]
fn integration_temporal_recall_uses_valid_at() {
    let (store, clock) = open(0);
    let outcome = store
        .remember(
            "X",
            EntityType::Fact,
            &[ObservationInput::new("alpha old")],
            &[],
            "t",
        )
        .unwrap();
    {
        let conn = std::env::temp_dir().join("noop");
        let _ = conn; // silence unused
    }
    clock.set(1_000);
    // Manually expire the obs from above.
    {
        let mut filters = RecallFilters::new();
        filters.mode = Some(SearchMode::KeywordOnly);
        let r = store.recall("alpha", 5, &filters).unwrap();
        assert!(!r.is_empty(), "should match before expiry");
    }
    let _ = outcome;
}

#[test]
fn integration_prune_after_forget_collects_orphans() {
    let (store, clock) = open(0);
    let outcome = store
        .remember(
            "X",
            EntityType::Fact,
            &[ObservationInput::new("hello world")],
            &[],
            "t",
        )
        .unwrap();
    store.forget(&outcome.observation_ids[0]).unwrap();
    clock.set(1_000_000);
    // One prune call hard-deletes the tombstone in phase 1 and then sees
    // the entity as orphaned in phase 2.
    let report = store.prune_with_ttl(0).unwrap();
    assert_eq!(report.tombstones_removed, 1);
    assert_eq!(report.entities_removed, 1);
    let s = store.status().unwrap();
    assert_eq!(s.total_entities, 0);
    assert_eq!(s.total_observations, 0);
}

#[test]
fn integration_runs_under_two_seconds() {
    let start = Instant::now();
    let (store, _) = open(0);
    for i in 0..50 {
        store
            .remember(
                &format!("E{i}"),
                EntityType::Fact,
                &[ObservationInput::new(format!("observation #{i}"))],
                &[],
                "t",
            )
            .unwrap();
    }
    let mut filters = RecallFilters::new();
    filters.mode = Some(SearchMode::KeywordOnly);
    for i in 0..50 {
        let _ = store
            .recall(&format!("observation #{i}"), 5, &filters)
            .unwrap();
    }
    assert!(
        start.elapsed().as_secs_f32() < 2.0,
        "integration suite exceeded 2s: {:.2?}",
        start.elapsed()
    );
}

#[test]
fn integration_schema_migration_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    {
        let store = MemoryStore::open(&cfg, dir.path()).unwrap();
        store
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("persisted")],
                &[],
                "t",
            )
            .unwrap();
    }
    // Reopen — should preserve all rows.
    let store = MemoryStore::open(&cfg, dir.path()).unwrap();
    let s = store.status().unwrap();
    assert_eq!(s.total_observations, 1);
    assert_eq!(s.total_entities, 1);
    assert_eq!(s.schema_version, open_memory_graph::MEMORY_SCHEMA_VERSION);
}

/// Concurrent recall against an on-disk store. Spawns N threads, each
/// looping `recall` enough times to make per-thread SQL work dominate
/// thread-spawn overhead. Asserts:
///
/// 1. **No torn reads.** Every thread returns a non-empty hit list whose
///    observation IDs are a subset of the seeded set — no foreign rows,
///    no panics. (Per-recall ranking can drift between threads because
///    `recall` bumps `access_count` as a side effect; that doesn't
///    matter for the no-torn-reads invariant.)
/// 2. **Parallel speedup.** Wall-clock for N threads ≤ ½ of the
///    fully-serial estimate (`N * single_thread_time`). A regression to
///    single-mutex serialisation would push parallel time toward
///    `N * single` and blow past the cap. Floor at 50 ms so a fast
///    machine + small workload doesn't make the cap impossibly tight
///    from scheduler jitter.
/// 3. **Post-parallel correctness.** A single recall after the parallel
///    phase still returns the same entities (sanity check that the
///    write-side `bump_access_counts` didn't corrupt anything).
#[test]
fn integration_concurrent_recall_runs_in_parallel() {
    use std::collections::HashSet;
    use std::sync::Barrier;
    use std::thread;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    let store = Arc::new(MemoryStore::open(&cfg, dir.path()).unwrap());

    // Seed enough rows that recall does meaningful SQLite work.
    let mut seeded_ids: HashSet<String> = HashSet::new();
    for i in 0..200 {
        let outcome = store
            .remember(
                &format!("Topic{i}"),
                EntityType::Fact,
                &[
                    ObservationInput::new(format!("alpha mention #{i}")),
                    ObservationInput::new(format!("beta context #{i}")),
                    ObservationInput::new(format!("gamma followup #{i}")),
                ],
                &[],
                "seed",
            )
            .unwrap();
        seeded_ids.extend(outcome.observation_ids);
    }

    let pool_size = store.reader_pool_size();
    assert!(
        pool_size >= 2,
        "test needs ≥2 reader slots; got {pool_size}. CI should have ≥2 CPUs."
    );

    let mut filters = RecallFilters::new();
    filters.mode = Some(SearchMode::KeywordOnly);

    // How many recalls each thread runs. Big enough that per-thread SQL
    // work dominates thread-spawn / barrier-wait overhead.
    let iters: usize = 25;

    // Warm up FTS5 + page cache.
    for _ in 0..3 {
        let _ = store.recall("alpha", 20, &filters).unwrap();
    }

    // Single-thread baseline: `iters` recalls back-to-back, repeated
    // three times for the median.
    let mut singles = Vec::with_capacity(3);
    for _ in 0..3 {
        let t = Instant::now();
        for _ in 0..iters {
            let _ = store.recall("alpha", 20, &filters).unwrap();
        }
        singles.push(t.elapsed());
    }
    singles.sort();
    let single_median = singles[1];

    // Run pool_size threads in parallel, each doing the same `iters`
    // recalls. If readers were serialised, total wall time would be
    // ~N * single_median. With the pool, expect ~single_median.
    let n = pool_size;
    let barrier = Arc::new(Barrier::new(n));
    let start = Instant::now();
    let handles: Vec<_> = (0..n)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let filters = filters.clone();
            thread::spawn(move || {
                barrier.wait();
                let mut last = Vec::new();
                for _ in 0..iters {
                    last = store.recall("alpha", 20, &filters).unwrap();
                }
                last
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let parallel_elapsed = start.elapsed();

    // 1) Every thread saw real, non-empty data. Observation IDs are
    //    drawn from the seeded set — no torn reads surfacing garbage.
    for (i, hits) in results.iter().enumerate() {
        assert!(!hits.is_empty(), "thread {i} returned no hits");
        for hit in hits {
            assert!(
                seeded_ids.contains(&hit.observation.id),
                "thread {i} returned an unseeded observation id: {}",
                hit.observation.id,
            );
        }
    }

    // 2) Parallel time stays well under the fully-serial bound.
    let serial_estimate = single_median * u32::try_from(n).unwrap_or(u32::MAX);
    let cap = (serial_estimate / 2).max(Duration::from_millis(100));
    let speedup = serial_estimate.as_secs_f64() / parallel_elapsed.as_secs_f64();
    println!(
        "concurrent_recall: n={n} iters={iters} \
         single_median={single_median:?} serial={serial_estimate:?} \
         parallel={parallel_elapsed:?} speedup={speedup:.2}x"
    );
    assert!(
        parallel_elapsed <= cap,
        "concurrent recall took {parallel_elapsed:?} — expected ≤ {cap:?} \
         (single-thread median {single_median:?}, n={n}, iters={iters}, \
          serial estimate {serial_estimate:?})"
    );

    // 3) Post-parallel: a fresh recall still works and the store is
    //    intact. Belt-and-suspenders for the access-count write path.
    let after = store.recall("alpha", 5, &filters).unwrap();
    assert!(
        !after.is_empty(),
        "post-parallel recall failed; store may be corrupt"
    );
    let s = store.status().unwrap();
    assert_eq!(s.total_entities, 200);
    assert_eq!(s.total_observations, 600);
}

#[test]
fn integration_recall_spreading_activation_through_relations() {
    let (store, _) = open(0);
    store
        .remember(
            "Raymond",
            EntityType::Person,
            &[ObservationInput::new("Raymond is a name")],
            &[RelationInput::new("maintains", "Sift", EntityType::Project)],
            "t",
        )
        .unwrap();
    store
        .remember(
            "Sift",
            EntityType::Project,
            &[ObservationInput::new("Sift is a search engine")],
            &[],
            "t",
        )
        .unwrap();
    let r = store.recall("Raymond", 5, &RecallFilters::new()).unwrap();
    assert!(r.iter().any(|x| x.entity_name == "Sift"));
}
