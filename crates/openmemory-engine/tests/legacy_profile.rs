//! Permanent production-path legacy-profile compatibility scenario.

use openmemory_core::config::Config;
use openmemory_engine::migrate::{migrate_domains, MIGRATE_BACKUP_DIR};
use openmemory_engine::partition::{DomainStore, DOMAIN_MANIFEST_FILE};
use openmemory_graph::recall::RecallFilters;
use openmemory_graph::{EntityType, ObservationInput};
use serde::Deserialize;

const FIXTURE: &str = include_str!("fixtures/legacy-profile-v2.json");
const FIXTURE_SOURCE: &str = "fixture:legacy-profile-v2";

#[derive(Debug, Deserialize)]
struct LegacyFixture {
    fixture_version: u32,
    source_revision: String,
    entities: Vec<FixtureEntity>,
    relations: Vec<FixtureRelation>,
    expected: ExpectedCounts,
}

#[derive(Debug, Deserialize)]
struct FixtureEntity {
    name: String,
    entity_type: EntityType,
    observations: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FixtureRelation {
    from: String,
    to: String,
    relation_type: String,
}

#[derive(Debug, Deserialize)]
struct ExpectedCounts {
    entities: u64,
    observations: u64,
    relations: u64,
}

fn assert_fixture_state(store: &DomainStore, fixture: &LegacyFixture) {
    let status = store.status().expect("read fixture status");
    assert_eq!(status.total_entities, fixture.expected.entities);
    assert_eq!(status.total_observations, fixture.expected.observations);
    assert_eq!(status.total_relations, fixture.expected.relations);

    for expected in &fixture.entities {
        let entity = store
            .resolve_entity(&expected.name)
            .expect("look up fixture entity")
            .unique()
            .expect("fixture entity exists and is unambiguous");
        assert_eq!(entity.entity_type, expected.entity_type);
        let observations = store
            .get_entity_observations(&entity.id)
            .expect("read fixture observations");
        assert_eq!(observations.len(), expected.observations.len());
    }

    let hits = store
        .recall("verified replacement layout", 8, &RecallFilters::default())
        .expect("recall fixture content");
    assert!(
        hits.iter().any(|hit| hit.entity_name == "Domain Migration"),
        "production recall must find the migrated fixture observation"
    );
}

#[test]
fn populated_legacy_profile_survives_reopen_and_domain_round_trip() {
    let fixture: LegacyFixture = serde_json::from_str(FIXTURE).expect("parse fixture");
    assert_eq!(fixture.fixture_version, 1);
    assert_eq!(
        fixture.source_revision,
        "bcd10fd3f736290c536ac94daf5a9014ef711828"
    );

    let dir = tempfile::tempdir().expect("create fixture root");
    let config = Config::default();
    let store = DomainStore::open(&config, dir.path(), 1).expect("open legacy profile");

    for entity in &fixture.entities {
        let observations = entity
            .observations
            .iter()
            .map(|content| ObservationInput::new(content).with_source(FIXTURE_SOURCE))
            .collect::<Vec<_>>();
        store
            .remember(
                &entity.name,
                entity.entity_type,
                &observations,
                &[],
                FIXTURE_SOURCE,
            )
            .expect("populate legacy entity");
    }

    for relation in &fixture.relations {
        let from = store
            .resolve_entity(&relation.from)
            .expect("look up relation source")
            .unique()
            .expect("relation source exists and is unambiguous");
        let to = store
            .resolve_entity(&relation.to)
            .expect("look up relation target")
            .unique()
            .expect("relation target exists and is unambiguous");
        store
            .add_relation(
                &from.id,
                &to.id,
                &relation.relation_type,
                None,
                FIXTURE_SOURCE,
            )
            .expect("populate legacy relation");
    }
    assert_fixture_state(&store, &fixture);
    drop(store);

    assert!(dir.path().join("memory.sqlite").is_file());
    assert!(!dir.path().join(DOMAIN_MANIFEST_FILE).exists());

    let reopened = DomainStore::open_existing(&config, dir.path()).expect("reopen legacy profile");
    assert_eq!(reopened.domains(), 1);
    assert_fixture_state(&reopened, &fixture);
    drop(reopened);

    let to_four = migrate_domains(&config, dir.path(), 4).expect("migrate fixture to four domains");
    assert_eq!(to_four.from_domains, 1);
    assert_eq!(to_four.to_domains, 4);
    let partitioned =
        DomainStore::open_existing(&config, dir.path()).expect("open partitioned fixture");
    assert_eq!(partitioned.domains(), 4);
    assert_fixture_state(&partitioned, &fixture);
    drop(partitioned);

    std::fs::remove_dir_all(dir.path().join(MIGRATE_BACKUP_DIR))
        .expect("remove retained test backup before reverse migration");
    let to_one = migrate_domains(&config, dir.path(), 1).expect("migrate fixture back to legacy");
    assert_eq!(to_one.from_domains, 4);
    assert_eq!(to_one.to_domains, 1);
    let legacy_again =
        DomainStore::open_existing(&config, dir.path()).expect("reopen round-tripped fixture");
    assert_eq!(legacy_again.domains(), 1);
    assert_fixture_state(&legacy_again, &fixture);
}
