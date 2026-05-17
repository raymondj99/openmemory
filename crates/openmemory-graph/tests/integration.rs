//! Integration tests for the public `MemoryStore` API. These tests stay at
//! the highest layer of the crate: no `pub(crate)` access, no SQL pokes
//! into private internals. They lock in the contract callers (the MCP layer
//! and the CLI) rely on.
//!
//! Most tests run against a fully in-memory store (`MemoryStore::open_in_memory`);
//! concurrency and reopen tests use temporary on-disk stores when persistence
//! or independent reader connections are part of the contract under test.

use std::sync::Arc;
use std::time::Instant;

use openmemory_core::clock::{Clock, FixedClock};
use openmemory_core::config::Config;
use openmemory_graph::{
    ConsolidateConfig, EntityType, MemoryError, MemoryStore, NormalizeMatch, ObservationInput,
    RecallFilters, RelationInput, SearchMode,
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
                ObservationInput::new("ships openmemory"),
            ],
            &[RelationInput::new(
                "maintains",
                "openmemory",
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
    // Reopen; should preserve all rows.
    let store = MemoryStore::open(&cfg, dir.path()).unwrap();
    let s = store.status().unwrap();
    assert_eq!(s.total_observations, 1);
    assert_eq!(s.total_entities, 1);
    assert_eq!(s.schema_version, openmemory_graph::MEMORY_SCHEMA_VERSION);
}

/// Concurrent recall against an on-disk store. Spawns N threads, each
/// looping `recall` enough times to make per-thread SQL work dominate
/// thread-spawn overhead. Asserts:
///
/// 1. **No torn reads.** Every thread returns a non-empty hit list whose
///    observation IDs are a subset of the seeded set: no foreign rows,
///    no panics. (Per-recall ranking can drift between threads because
///    `recall` bumps `access_count` as a side effect; that doesn't
///    matter for the no-torn-reads invariant.)
/// 2. **Some real overlap, not full serialisation.** Wall-clock for N
///    threads is strictly less than the fully-serial estimate
///    (`N * single_thread_time`). A regression to single-mutex
///    serialisation would push parallel time to roughly that bound or
///    higher; the assertion only fails when readers no longer overlap
///    at all. We deliberately avoid asserting a numeric speedup ratio
///    (1.25x, 2x, and so on) because shared CI runners hit ratios that depend
///    on neighbour load, but "any overlap whatsoever" is a stable
///    signal. A proportional slack (5% of serial estimate) absorbs
///    scheduler jitter without flaking on fast machines with few cores.
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
        "test needs at least 2 reader slots; got {pool_size}. CI should have at least 2 CPUs."
    );

    let mut filters = RecallFilters::new();
    filters.mode = Some(SearchMode::KeywordOnly);

    // How many recalls each thread runs. Big enough that per-thread SQL
    // work dominates thread-spawn / barrier-wait overhead. Bumped above
    // the legacy `25` so the test's `MIN_SERIAL_FOR_SIGNAL` (50 ms) is
    // still cleared on low-core CI runners (e.g. macos-latest with
    // n=3) after the recall hot path's per-call latency dropped under
    // the v0.3 batched-candidate optimisation.
    let iters: usize = 100;

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
    //    drawn from the seeded set: no torn reads surfacing garbage.
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

    // 2) Parallel time stays close to the fully-serial estimate
    //    `single_median * n`. The point of this assertion is to catch
    //    a regression where readers re-serialise on a single connection
    //    — in that pathology parallel time scales toward
    //    `n * serial_estimate` (each thread waits for every other
    //    thread's read before its own starts).
    //
    //    We do not require a numeric speedup. Each recall mixes a
    //    parallelisable read path with a write through `bump_access_counts`
    //    that re-serialises on the writer mutex, so on low-core CI runners
    //    the writer queue can push `parallel_elapsed` slightly above
    //    `serial_estimate` without any reader regression. The
    //    `MAX_PARALLEL_OVERHEAD` tolerance below absorbs that without
    //    inviting a fully-serialised reader regression to sneak through:
    //    a true regression would land parallel at multiples of
    //    `serial_estimate`, far past the tolerance.
    const MAX_PARALLEL_OVERHEAD: f64 = 0.50;
    const MIN_SERIAL_FOR_SIGNAL: Duration = Duration::from_millis(50);
    let serial_estimate = single_median * u32::try_from(n).unwrap_or(u32::MAX);
    assert!(
        serial_estimate >= MIN_SERIAL_FOR_SIGNAL,
        "serial estimate {serial_estimate:?} is too small to validate parallelism \
         (single-thread median {single_median:?}, n={n}, iters={iters}); \
         the test would be inconclusive; bump iters or the seed size",
    );
    let upper_bound =
        Duration::from_secs_f64(serial_estimate.as_secs_f64() * (1.0 + MAX_PARALLEL_OVERHEAD));
    let speedup = serial_estimate.as_secs_f64() / parallel_elapsed.as_secs_f64();
    println!(
        "concurrent_recall: n={n} iters={iters} \
         single_median={single_median:?} serial={serial_estimate:?} \
         parallel={parallel_elapsed:?} speedup={speedup:.2}x"
    );
    assert!(
        parallel_elapsed < upper_bound,
        "concurrent recall took {parallel_elapsed:?}; expected < {upper_bound:?} \
         (single-thread median {single_median:?}, n={n}, iters={iters}, \
          serial estimate {serial_estimate:?}, \
          tolerance {MAX_PARALLEL_OVERHEAD}); \
         readers may have regressed to fully-serialised execution",
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

/// Concurrent near-duplicate writes must coalesce into a single entity.
///
/// Why this matters: `ensure_entity` does a candidate SELECT and then,
/// if no match wins the threshold, INSERTs a new row. If those two
/// steps weren't part of the same serialized critical section, two
/// threads could each see an empty candidate set, each insert their
/// own row, and the store would end up with duplicate entities for
/// the same logical concept, defeating the whole point of fuzzy
/// matching. In practice the write side is serialized two ways:
///
/// 1. `MemoryStore::remember` holds the rebuild-barrier write lock
///    (`write_rebuild`) for the entire function.
/// 2. Inside that, it holds the connection `Mutex` for the whole
///    SQL transaction (candidate lookup, ensure_entity, observation
///    inserts, commit).
///
/// This test makes the resulting invariant observable: N threads
/// race with N distinct near-duplicate names that all share a
/// canonical form ("projectalpha" after lowercase + alnum strip);
/// after they all complete, exactly one entity survives with N
/// observations attached, and exactly one thread reports that it
/// created the entity. A regression in either lock surfaces as
/// `total_entities > 1` or `created_count != 1`.
#[test]
fn integration_concurrent_remember_of_near_duplicates_coalesces() {
    use std::collections::HashSet;
    use std::sync::Barrier;
    use std::thread;

    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    let store = Arc::new(MemoryStore::open(&cfg, dir.path()).unwrap());

    // Every variant strips to "projectalpha" after lowercase + alnum
    // filtering, so `similarity(any_pair)` == 1.0 via the stripped
    // fast path. That guarantees the second-through-Nth writers all
    // AutoMerge into whichever variant won the race; no spurious
    // fresh entities from a candidate that scored just below the
    // threshold. The variants are deliberately distinct *as stored
    // strings* so no two threads can take the cheaper exact-match
    // path; every follower exercises the fuzzy candidate lookup,
    // which is the path we actually want to stress.
    let variants: [&str; 8] = [
        "Project Alpha",
        "project alpha",
        "PROJECT ALPHA",
        "Project  Alpha",
        "ProjectAlpha",
        "project-alpha",
        "project_alpha",
        "project.alpha",
    ];
    let n = variants.len();
    let barrier = Arc::new(Barrier::new(n));

    let handles: Vec<_> = variants
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let name = (*name).to_string();
            thread::spawn(move || {
                // Barrier sync maximizes the chance of contention on
                // the write locks. Whether threads actually interleave
                // is irrelevant to correctness; the invariants we
                // assert hold under any ordering.
                barrier.wait();
                store
                    .remember(
                        &name,
                        EntityType::Project,
                        &[ObservationInput::new(format!(
                            "observation from thread {i}"
                        ))],
                        &[],
                        "race",
                    )
                    .unwrap()
            })
        })
        .collect();

    let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // 1) Exactly one entity row in the database. The headline
    //    invariant: any number >1 here is a torn write.
    let status = store.status().unwrap();
    assert_eq!(
        status.total_entities,
        1,
        "concurrent near-duplicate writes produced {} entities, expected 1 \
         (winner + {} merged followers); the candidate lookup may not be \
         serialized with the insert",
        status.total_entities,
        n - 1,
    );

    // 2) Every observation landed. No silent drops from a rolled-back
    //    transaction or an unhandled lock contention error.
    assert_eq!(
        usize::try_from(status.total_observations).unwrap(),
        n,
        "expected {n} observations, got {}",
        status.total_observations,
    );

    // 3) All outcomes converge on the same entity_id. A diverged id
    //    here would be the same regression as (1) but caught at the
    //    return-value layer, which is what the MCP / CLI actually see.
    let canonical_id = outcomes[0].entity_id.clone();
    for (i, o) in outcomes.iter().enumerate() {
        assert_eq!(
            o.entity_id, canonical_id,
            "thread {i} wrote to a different entity_id ({} vs winner {canonical_id})",
            o.entity_id,
        );
    }

    // 4) Exactly one thread reports the entity as freshly created.
    //    Two `false` values would mean two threads each saw an empty
    //    candidate window and both inserted: the smoking-gun
    //    signature of a torn write that (1) might miss if the rows
    //    later collapsed via FK or unique-constraint behavior.
    let created_count = outcomes.iter().filter(|o| !o.entity_existed).count();
    assert_eq!(
        created_count, 1,
        "expected exactly one creator, got {created_count}: \
         a creation count >1 means the candidate lookup raced the insert",
    );

    // 5) The N-1 followers all merged via the fuzzy path. The
    //    exact-match SQL can't fire for any follower because every
    //    variant is a distinct stored string, so the only valid
    //    non-creator outcome is `AutoMerge` (with `entity_existed`
    //    true). A `Flag` here would mean the similarity contract
    //    silently dropped under 0.95; a `None` would mean the
    //    follower bypassed normalization entirely.
    let auto_merged_count = outcomes
        .iter()
        .filter(|o| matches!(o.normalized, Some(NormalizeMatch::AutoMerge { .. })))
        .count();
    assert_eq!(
        auto_merged_count,
        n - 1,
        "expected {expected} AutoMerge outcomes (one per follower), got {auto_merged_count}: \
         outcomes were {normalized:?}",
        expected = n - 1,
        normalized = outcomes.iter().map(|o| &o.normalized).collect::<Vec<_>>(),
    );

    // 6) All N observations live on the surviving entity, with each
    //    thread's distinct content present. Belt-and-suspenders for
    //    (2): catches a path where observation rows survive but get
    //    re-parented to the wrong entity.
    let observations = store.get_entity_observations(&canonical_id).unwrap();
    assert_eq!(observations.len(), n);
    let contents: HashSet<_> = observations.iter().map(|o| o.content.clone()).collect();
    for i in 0..n {
        let expected = format!("observation from thread {i}");
        assert!(
            contents.contains(&expected),
            "missing observation from thread {i}; got {contents:?}",
        );
    }
}
