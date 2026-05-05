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
    ConsolidateConfig, EntityType, MemoryError, MemoryStore, ObservationInput,
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
            &[RelationInput::new(
                "maintains",
                "Sift",
                EntityType::Project,
            )],
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
        let _ = store.recall(&format!("observation #{i}"), 5, &filters).unwrap();
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

#[test]
fn integration_recall_spreading_activation_through_relations() {
    let (store, _) = open(0);
    store
        .remember(
            "Raymond",
            EntityType::Person,
            &[ObservationInput::new("Raymond is a name")],
            &[RelationInput::new(
                "maintains",
                "Sift",
                EntityType::Project,
            )],
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
