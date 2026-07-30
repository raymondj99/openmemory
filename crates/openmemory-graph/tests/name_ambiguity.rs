//! Name ambiguity, built from the shape found in a real store.
//!
//! # Provenance of this fixture
//!
//! This is not a hypothetical. The live profile store at
//! `~/.openmemory/data/default` (schema v2, 59 entities under 58 distinct
//! names) carries **`ProjectAlpha` twice — once as a `concept` and once
//! as a `project`**. That was found by T11 while prototyping the entity
//! identity migration on a *copy* of that store, and it is why these
//! tests use that name and those two types rather than `foo`/`bar`.
//!
//! `idx_entities_name_type` is UNIQUE over `(name, entity_type)`, so
//! today a name can only collide across types — which is exactly the
//! case below. `MemoryStore::get_entity(name)` used to be
//! `SELECT … WHERE name = ?1` through `query_row`: it returned whichever
//! row SQLite reached first and discarded the rest, so one of the two
//! `ProjectAlpha` entities was unreachable by name and nothing said so.
//!
//! T4a demonstrated the *within-type* version of this by constructing it
//! (two unrelated people both named "Alex Chen" collapsing into one
//! row); the cross-type version was already present in real user data.
//! See `plan/16-production-memory-spaces/11-entity-identity-migration.md`.
//!
//! The tests below pin three things:
//!
//! 1. the old single-row answer really was arbitrary — both entities
//!    exist, and a single-row query can only return one of them;
//! 2. the resolver reports both;
//! 3. destructive by-name operations refuse rather than guess.

use std::sync::Arc;

use openmemory_core::clock::{Clock, FixedClock};
use openmemory_core::config::Config;
use openmemory_graph::{EntityResolution, EntityType, MemoryError, MemoryStore, ObservationInput};

/// The name that is genuinely duplicated in the live profile store.
const SHARED_NAME: &str = "ProjectAlpha";

fn open(now_secs: i64) -> MemoryStore {
    let clock = Arc::new(FixedClock::new(now_secs));
    MemoryStore::open_in_memory(&Config::default())
        .unwrap()
        .with_clock(clock as Arc<dyn Clock>)
}

/// Reproduce the live store's shape: one name, two entity types, two
/// unrelated sets of observations.
fn store_with_the_real_collision() -> MemoryStore {
    let store = open(1_000);
    store
        .remember(
            SHARED_NAME,
            EntityType::Concept,
            &[ObservationInput::new(
                "The architectural pattern the team refers to as ProjectAlpha.",
            )],
            &[],
            "test-concept",
        )
        .unwrap();
    store
        .remember(
            SHARED_NAME,
            EntityType::Project,
            &[ObservationInput::new(
                "The shipping codebase called ProjectAlpha; unrelated to the pattern.",
            )],
            &[],
            "test-project",
        )
        .unwrap();
    store
}

#[test]
fn the_collision_the_live_store_contains_is_representable_today() {
    // Nothing here needs the schema change: the UNIQUE index is over
    // (name, entity_type), so two types under one name already coexist.
    let store = store_with_the_real_collision();

    let concept = store
        .get_entity_by_name_and_type(SHARED_NAME, EntityType::Concept)
        .unwrap()
        .expect("the concept exists");
    let project = store
        .get_entity_by_name_and_type(SHARED_NAME, EntityType::Project)
        .unwrap()
        .expect("the project exists");

    assert_ne!(
        concept.id, project.id,
        "these are two different entities that happen to share a label"
    );
    assert_eq!(store.get_entity_observations(&concept.id).unwrap().len(), 1);
    assert_eq!(store.get_entity_observations(&project.id).unwrap().len(), 1);
}

#[test]
fn the_old_single_row_answer_could_only_ever_be_one_of_the_two() {
    // The defect, stated as a test: a lookup that returns at most one row
    // cannot describe this store. Whichever entity `get_entity` returned,
    // the other was unreachable by name and no signal said so.
    let store = store_with_the_real_collision();
    let resolution = store.resolve_entity(SHARED_NAME).unwrap();

    assert_eq!(
        resolution.candidate_count(),
        2,
        "two entities carry this name"
    );
    assert!(
        resolution.clone().unique().is_none(),
        "a single-entity answer must not be invented from a two-entity store"
    );
}

#[test]
fn the_resolver_reports_both_with_enough_to_disambiguate() {
    let store = store_with_the_real_collision();

    let EntityResolution::Ambiguous(candidates) = store.resolve_entity(SHARED_NAME).unwrap() else {
        panic!("a name carried by two entities must resolve as ambiguous");
    };

    assert_eq!(candidates.name(), SHARED_NAME);
    assert!(!candidates.truncated(), "only two entities exist");

    let types: Vec<&str> = candidates
        .entities()
        .iter()
        .map(|entity| entity.entity_type.as_str())
        .collect();
    assert!(types.contains(&"concept"), "got {types:?}");
    assert!(types.contains(&"project"), "got {types:?}");

    // A caller must be able to act on the answer, which means ids.
    for entity in candidates.entities() {
        assert!(!entity.id.is_empty());
    }
    let described = candidates.describe();
    assert!(described.contains("2 entities are named"), "{described}");
    assert!(described.contains(SHARED_NAME), "{described}");
}

#[test]
fn resolution_is_deterministic_not_sqlite_row_order() {
    // Two stores built in opposite insertion orders must resolve to the
    // same ordering, or "the first candidate" means nothing.
    let forward = store_with_the_real_collision();

    let reverse = open(1_000);
    reverse
        .remember(
            SHARED_NAME,
            EntityType::Project,
            &[ObservationInput::new("project first this time")],
            &[],
            "test-project",
        )
        .unwrap();
    reverse
        .remember(
            SHARED_NAME,
            EntityType::Concept,
            &[ObservationInput::new("concept second this time")],
            &[],
            "test-concept",
        )
        .unwrap();

    // Both stores use the same FixedClock, so `created_at` ties and the
    // documented tie-break — ascending id — decides. The point is that
    // *some* stated rule decides, and it is stable.
    let ordered = |store: &MemoryStore| -> Vec<String> {
        store
            .resolve_entity(SHARED_NAME)
            .unwrap()
            .candidates()
            .iter()
            .map(|entity| entity.id.clone())
            .collect()
    };
    let forward_ids = ordered(&forward);
    let mut sorted = forward_ids.clone();
    sorted.sort();
    assert_eq!(
        forward_ids, sorted,
        "candidates must come back in the documented (created_at, id) order"
    );
    let reverse_ids = ordered(&reverse);
    let mut reverse_sorted = reverse_ids.clone();
    reverse_sorted.sort();
    assert_eq!(reverse_ids, reverse_sorted);
}

#[test]
fn a_unique_name_still_resolves_to_exactly_one_entity() {
    // The compatibility half: nothing changes for names that are not
    // shared, which is every other name in the live store.
    let store = store_with_the_real_collision();
    store
        .remember(
            "UniquelyNamedThing",
            EntityType::Tool,
            &[ObservationInput::new("only one of these")],
            &[],
            "test",
        )
        .unwrap();

    let resolution = store.resolve_entity("UniquelyNamedThing").unwrap();
    assert!(!resolution.is_ambiguous());
    let entity = resolution.unique().expect("exactly one match");
    assert_eq!(entity.name, "UniquelyNamedThing");
    assert_eq!(entity.entity_type, EntityType::Tool);
}

#[test]
fn an_absent_name_resolves_to_not_found() {
    let store = store_with_the_real_collision();
    let resolution = store.resolve_entity("NoSuchEntity").unwrap();
    assert_eq!(resolution, EntityResolution::NotFound);
    assert_eq!(resolution.candidate_count(), 0);
}

#[test]
fn hard_delete_by_an_ambiguous_name_refuses_instead_of_guessing() {
    // The most important one. `forget_entity` used to delete "the first
    // match". On the live store, `forget-entity ProjectAlpha` would have
    // hard-deleted one of two unrelated entities, cascading its
    // observations and relations, with no way to tell which and no way
    // back.
    let store = store_with_the_real_collision();

    let error = store.forget_entity(SHARED_NAME).unwrap_err();
    let MemoryError::AmbiguousEntityName { name, candidates } = error else {
        panic!("a destructive call on an ambiguous name must fail closed");
    };
    assert_eq!(name, SHARED_NAME);
    assert_eq!(candidates.len(), 2);

    // Nothing was deleted.
    assert_eq!(
        store.resolve_entity(SHARED_NAME).unwrap().candidate_count(),
        2
    );

    // And the caller can act once it has picked an id.
    let concept = store
        .get_entity_by_name_and_type(SHARED_NAME, EntityType::Concept)
        .unwrap()
        .unwrap();
    let purged = store.forget_entity_by_id(&concept.id).unwrap();
    assert_eq!(purged, 1, "one observation cascaded");

    let remaining = store.resolve_entity(SHARED_NAME).unwrap();
    let survivor = remaining.unique().expect("one entity left");
    assert_eq!(survivor.entity_type, EntityType::Project);
}

#[test]
fn retiring_observations_by_an_ambiguous_name_refuses_instead_of_guessing() {
    let store = store_with_the_real_collision();

    let error = store.retire_entity_observations(SHARED_NAME).unwrap_err();
    let MemoryError::AmbiguousEntityName { name, candidates } = error else {
        panic!("retiring every observation of a guessed entity must fail closed");
    };
    assert_eq!(name, SHARED_NAME);
    assert_eq!(candidates.len(), 2);

    // Both entities keep their observations.
    for entity_type in [EntityType::Concept, EntityType::Project] {
        let entity = store
            .get_entity_by_name_and_type(SHARED_NAME, entity_type)
            .unwrap()
            .unwrap();
        assert_eq!(store.get_entity_observations(&entity.id).unwrap().len(), 1);
    }

    // By id it works and touches only the named entity.
    let concept = store
        .get_entity_by_name_and_type(SHARED_NAME, EntityType::Concept)
        .unwrap()
        .unwrap();
    let project = store
        .get_entity_by_name_and_type(SHARED_NAME, EntityType::Project)
        .unwrap()
        .unwrap();
    assert_eq!(
        store.retire_entity_observations_by_id(&concept.id).unwrap(),
        1
    );
    assert!(store
        .get_entity_observations(&concept.id)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.get_entity_observations(&project.id).unwrap().len(),
        1,
        "the other ProjectAlpha must be untouched"
    );
}

#[test]
fn destructive_calls_on_an_unambiguous_name_are_unchanged() {
    // Fail-closed must not become fail-always: the overwhelmingly common
    // case is a name carried by exactly one entity.
    let store = open(1_000);
    store
        .remember(
            "OnlyOne",
            EntityType::Project,
            &[ObservationInput::new("sole observation")],
            &[],
            "test",
        )
        .unwrap();
    assert_eq!(store.retire_entity_observations("OnlyOne").unwrap(), 1);
    assert_eq!(store.forget_entity("OnlyOne").unwrap(), 1);
    assert_eq!(
        store.resolve_entity("OnlyOne").unwrap(),
        EntityResolution::NotFound
    );
}

#[test]
fn a_missing_name_still_reports_not_found_not_ambiguous() {
    let store = store_with_the_real_collision();
    assert!(matches!(
        store.forget_entity("NoSuchEntity").unwrap_err(),
        MemoryError::EntityNotFound(_)
    ));
    assert!(matches!(
        store
            .retire_entity_observations("NoSuchEntity")
            .unwrap_err(),
        MemoryError::EntityNotFound(_)
    ));
}
