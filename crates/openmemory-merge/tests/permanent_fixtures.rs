mod common;

use std::collections::BTreeSet;

use openmemory_merge::evidence::IdentityPolicy;
use openmemory_merge::model::{EntityRecord, IdentifierAssertion, MemorySnapshot};
use openmemory_merge::planner::{plan_merge, verify_plan, MemoryActionSink, MergePolicy};
use serde::Deserialize;

use common::*;

const CODEX_HOMEBREW: &str = include_str!("fixtures/codex-homebrew-tools.json");
const AXUM_ACTIX: &str = include_str!("fixtures/axum-actix-web.json");
const MATHLIB_LEAN: &str = include_str!("fixtures/mathlib-lean4.json");

#[derive(Deserialize)]
struct Fixture {
    schema_version: u16,
    name: String,
    retrieved_at: String,
    provenance: Vec<Provenance>,
    target: FixtureSpace,
    source: FixtureSpace,
    same_pairs: Vec<[String; 2]>,
    expected: Expected,
}

#[derive(Deserialize)]
struct Provenance {
    repository: String,
    commit: String,
    license: String,
}

#[derive(Deserialize)]
struct FixtureSpace {
    space_sequence: u64,
    snapshot_sequence: u64,
    entities: Vec<FixtureEntity>,
    relations: Vec<FixtureRelation>,
}

#[derive(Deserialize)]
struct FixtureEntity {
    id: String,
    label: String,
    kind: String,
    identifier: Option<FixtureIdentifier>,
    description: String,
}

#[derive(Deserialize)]
struct FixtureIdentifier {
    namespace: String,
    value: String,
}

#[derive(Deserialize)]
struct FixtureRelation {
    id: String,
    subject: String,
    predicate: String,
    object: String,
}

#[derive(Deserialize)]
struct Expected {
    coalesced_entities: u64,
    added_entities: u64,
    result_relations: u64,
    plan_hash: String,
    predicted_result_hash: String,
}

fn load_space(raw: &FixtureSpace, revision_base: u64) -> MemorySnapshot {
    let fixture_space = space(raw.space_sequence);
    let fixture_snapshot = snapshot_id(raw.snapshot_sequence);
    let entities = raw
        .entities
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let revision_sequence = revision_base + u64::try_from(index).unwrap();
            let mut record = entity(
                fixture_space,
                &raw.id,
                revision_sequence,
                &raw.label,
                &raw.kind,
            )
            .with_properties([("description".to_string(), raw.description.clone())])
            .unwrap();
            if let Some(identifier) = &raw.identifier {
                record = record
                    .with_identifiers([IdentifierAssertion::source_verified(
                        &identifier.namespace,
                        &identifier.value,
                        fixture_snapshot,
                        "pinned-repository-fixture-v1",
                        5,
                    )
                    .unwrap()])
                    .unwrap();
            }
            record
        })
        .collect::<Vec<EntityRecord>>();
    let observations = raw
        .entities
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            observation(
                fixture_space,
                &format!("observation-{}", raw.id),
                &raw.id,
                revision_base + 100 + u64::try_from(index).unwrap(),
                &raw.description,
            )
        })
        .collect();
    let relations = raw
        .relations
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            relation(
                fixture_space,
                &raw.id,
                &raw.subject,
                &raw.predicate,
                &raw.object,
                revision_base + 200 + u64::try_from(index).unwrap(),
            )
        })
        .collect();
    memory_snapshot(
        fixture_snapshot,
        fixture_space,
        u8::try_from(raw.space_sequence % 200 + 1).unwrap(),
        entities,
        observations,
        relations,
    )
}

fn evaluate(raw: &str) {
    let fixture: Fixture = serde_json::from_str(raw).unwrap();
    assert_eq!(fixture.schema_version, 1);
    assert!(!fixture.name.trim().is_empty());
    assert_eq!(fixture.retrieved_at, "2026-07-18");
    assert_eq!(fixture.provenance.len(), 2);
    for provenance in &fixture.provenance {
        assert!(!provenance.repository.trim().is_empty());
        assert_eq!(provenance.commit.len(), 40);
        assert!(provenance
            .commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert!(matches!(provenance.license.as_str(), "Apache-2.0" | "MIT"));
    }

    let mut target = load_space(&fixture.target, 1_000);
    let mut source = load_space(&fixture.source, 2_000);
    let namespaces = fixture
        .target
        .entities
        .iter()
        .chain(&fixture.source.entities)
        .filter_map(|entity| entity.identifier.as_ref())
        .map(|identifier| identifier.namespace.clone())
        .collect::<BTreeSet<_>>();
    let mut identity_policy = IdentityPolicy::new(13, 17).unwrap();
    for namespace in namespaces {
        identity_policy = identity_policy
            .with_authoritative_namespace(namespace, 5)
            .unwrap();
    }
    let same_pairs = fixture
        .same_pairs
        .into_iter()
        .map(|[target, source]| (target, source))
        .collect::<BTreeSet<_>>();
    let receipts = planning_receipts(&target, &source, &identity_policy, &same_pairs);
    let expected_actions = reference_actions(&target, &source, &receipts);
    let expected_counts = reference_action_counts(&expected_actions);
    let expected_accounting = reference_accounting(&expected_actions, &receipts);
    let mut sink = MemoryActionSink::new(1_000).unwrap();
    let plan = plan_merge(
        &mut target,
        &mut source,
        &receipts,
        &MergePolicy::new(1, identity_policy.policy_generation()).unwrap(),
        &mut sink,
    )
    .unwrap();
    assert_eq!(sink.actions(), expected_actions);
    assert_eq!(plan.action_counts(), &expected_counts);
    assert_eq!(plan.accounting(), &expected_accounting);
    verify_plan(sink.actions(), &plan).unwrap();
    assert_eq!(
        plan.predicted_result_hash(),
        reference_predicted_hash(sink.actions())
    );
    assert_eq!(
        plan.action_counts().coalesced_entities,
        fixture.expected.coalesced_entities
    );
    assert_eq!(
        plan.action_counts().added_entities,
        fixture.expected.added_entities
    );
    assert_eq!(
        plan.accounting().result_relations,
        fixture.expected.result_relations
    );
    assert_eq!(
        plan.plan_hash().to_string(),
        fixture.expected.plan_hash,
        "{} plan hash",
        fixture.name
    );
    assert_eq!(
        plan.predicted_result_hash().to_string(),
        fixture.expected.predicted_result_hash,
        "{} predicted hash",
        fixture.name
    );
}

#[test]
fn codex_homebrew_tools_fixture() {
    evaluate(CODEX_HOMEBREW);
}

#[test]
fn axum_actix_web_fixture() {
    evaluate(AXUM_ACTIX);
}

#[test]
fn mathlib_lean4_fixture() {
    evaluate(MATHLIB_LEAN);
}
