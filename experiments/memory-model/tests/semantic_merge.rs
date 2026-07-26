use std::collections::{BTreeMap, BTreeSet};

use openmemory_memory_model_poc::identity::IdentityDecision;
use openmemory_memory_model_poc::semantic_merge::{
    materialize_knowledge_merge, plan_knowledge_merge, CandidatePair, EntityAction,
    IdentityResolution, KnowledgeSnapshot, LocalEntity, LocalRelation, RelationKey,
};

fn entity(id: &str, revision: &str, label: &str, source: &str) -> LocalEntity {
    LocalEntity {
        logical_id: id.to_string(),
        revision_id: revision.to_string(),
        label: label.to_string(),
        aliases: BTreeSet::new(),
        kind: "concept".to_string(),
        identifiers: BTreeMap::new(),
        description: format!("description of {label}"),
        source_refs: BTreeSet::from([source.to_string()]),
    }
}

fn snapshot(
    space: &str,
    revision: &str,
    entities: Vec<LocalEntity>,
    relations: Vec<LocalRelation>,
) -> KnowledgeSnapshot {
    KnowledgeSnapshot::from_local(space, revision, entities, relations).expect("snapshot is valid")
}

fn reviewed(
    target: &KnowledgeSnapshot,
    target_id: &str,
    source: &KnowledgeSnapshot,
    source_id: &str,
    decision: IdentityDecision,
) -> IdentityResolution {
    IdentityResolution::reviewed(
        CandidatePair::new(target_id, source_id),
        decision,
        &target.entities()[target_id].revision_id,
        &source.entities()[source_id].revision_id,
        format!("packet-{target_id}-{source_id}"),
        "team-reviewer",
    )
    .expect("review receipt is valid")
}

#[test]
fn reviewed_coalescence_preserves_contributions_and_rewires_every_endpoint() {
    let target = snapshot(
        "project-a",
        "a1",
        vec![
            entity("a-project", "a-project-r1", "Project A", "a-project-src"),
            entity("a-cerpheus", "a-cerpheus-r1", "cerpheus", "a-concept-src"),
        ],
        vec![LocalRelation {
            subject: "a-project".to_string(),
            predicate: "defines".to_string(),
            object: "a-cerpheus".to_string(),
            source_ref: "a-project-src".to_string(),
        }],
    );
    let source = snapshot(
        "project-b",
        "b1",
        vec![
            entity("b-project", "b-project-r1", "Project B", "b-project-src"),
            entity("b-cerpheus", "b-cerpheus-r1", "cerpheus", "b-concept-src"),
            entity("b-runtime", "b-runtime-r1", "runtime", "b-runtime-src"),
        ],
        vec![
            LocalRelation {
                subject: "b-project".to_string(),
                predicate: "defines".to_string(),
                object: "b-cerpheus".to_string(),
                source_ref: "b-project-src".to_string(),
            },
            LocalRelation {
                subject: "b-cerpheus".to_string(),
                predicate: "implemented_by".to_string(),
                object: "b-runtime".to_string(),
                source_ref: "b-concept-src".to_string(),
            },
        ],
    );
    let target_hash = target.hash().to_string();
    let source_hash = source.hash().to_string();
    let pair = CandidatePair::new("a-cerpheus", "b-cerpheus");
    let candidates = BTreeSet::from([pair]);
    let resolutions = vec![reviewed(
        &target,
        "a-cerpheus",
        &source,
        "b-cerpheus",
        IdentityDecision::Same,
    )];

    let plan =
        plan_knowledge_merge(&target, &source, &candidates, &resolutions).expect("merge plans");
    let result = materialize_knowledge_merge(&target, &source, &plan).expect("merge materializes");

    assert_eq!(target.hash(), target_hash);
    assert_eq!(source.hash(), source_hash);
    assert_eq!(plan.merged_entities(), 1);
    assert_eq!(plan.added_entities(), 2);
    assert_eq!(plan.entity_actions.len(), source.entities().len());
    assert_eq!(
        plan.relation_actions.len(),
        source.relation_assertion_count()
    );
    assert_eq!(result.entities().len(), 4);
    assert_eq!(result.entities()["a-cerpheus"].contributions.len(), 2);
    assert!(result.entities().contains_key("project-b::b-project"));
    assert!(result.entities().contains_key("project-b::b-runtime"));
    assert!(result.relations().contains_key(&RelationKey {
        subject: "project-b::b-project".to_string(),
        predicate: "defines".to_string(),
        object: "a-cerpheus".to_string(),
    }));
    assert!(result.relations().contains_key(&RelationKey {
        subject: "a-cerpheus".to_string(),
        predicate: "implemented_by".to_string(),
        object: "project-b::b-runtime".to_string(),
    }));
}

#[test]
fn different_homonyms_stay_separate_and_source_is_added_once() {
    let target = snapshot(
        "project-a",
        "a1",
        vec![entity("cerpheus", "a-r1", "cerpheus", "a-src")],
        vec![],
    );
    let source = snapshot(
        "project-b",
        "b1",
        vec![entity("cerpheus", "b-r1", "cerpheus", "b-src")],
        vec![],
    );
    let pair = CandidatePair::new("cerpheus", "cerpheus");
    let resolution = IdentityResolution::policy(
        pair.clone(),
        IdentityDecision::Different,
        "a-r1",
        "b-r1",
        "conflicting-project-coordinates",
        "conflicting_authoritative_id",
    )
    .expect("different may be policy-resolved");

    let plan = plan_knowledge_merge(&target, &source, &BTreeSet::from([pair]), &[resolution])
        .expect("different identities plan");
    let result = materialize_knowledge_merge(&target, &source, &plan).expect("plan materializes");

    assert_eq!(plan.merged_entities(), 0);
    assert_eq!(plan.added_entities(), 1);
    assert_eq!(plan.separate_candidate_pairs, 1);
    assert_eq!(result.entities()["cerpheus"].contributions.len(), 1);
    assert_eq!(
        result.entities()["project-b::cerpheus"].contributions.len(),
        1
    );
}

#[test]
fn every_candidate_requires_exactly_one_resolution() {
    let target = snapshot(
        "a",
        "a1",
        vec![entity("shared", "a-r1", "shared", "a-src")],
        vec![],
    );
    let source = snapshot(
        "b",
        "b1",
        vec![entity("shared", "b-r1", "shared", "b-src")],
        vec![],
    );
    let pair = CandidatePair::new("shared", "shared");

    let missing = plan_knowledge_merge(&target, &source, &BTreeSet::from([pair.clone()]), &[])
        .expect_err("missing resolution fails closed");
    assert!(missing.to_string().contains("exactly one"));

    let resolution = reviewed(
        &target,
        "shared",
        &source,
        "shared",
        IdentityDecision::Different,
    );
    let extra = plan_knowledge_merge(&target, &source, &BTreeSet::new(), &[resolution])
        .expect_err("extra resolution fails closed");
    assert!(extra.to_string().contains("not in the candidate set"));
}

#[test]
fn stale_receipt_and_moved_snapshots_cannot_materialize() {
    let target = snapshot(
        "a",
        "a1",
        vec![entity("shared", "a-r1", "shared", "a-src")],
        vec![],
    );
    let source = snapshot(
        "b",
        "b1",
        vec![entity("shared", "b-r1", "shared", "b-src")],
        vec![],
    );
    let pair = CandidatePair::new("shared", "shared");
    let stale = IdentityResolution::reviewed(
        pair.clone(),
        IdentityDecision::Same,
        "old-target-revision",
        "b-r1",
        "packet",
        "reviewer",
    )
    .expect("receipt shape is valid");
    let error = plan_knowledge_merge(&target, &source, &BTreeSet::from([pair.clone()]), &[stale])
        .expect_err("stale receipt fails");
    assert!(error.to_string().contains("stale identity resolution"));

    let current = reviewed(&target, "shared", &source, "shared", IdentityDecision::Same);
    let plan = plan_knowledge_merge(&target, &source, &BTreeSet::from([pair]), &[current])
        .expect("current plan succeeds");
    let moved_source = snapshot(
        "b",
        "b2",
        vec![entity("shared", "b-r2", "shared changed", "b-src")],
        vec![],
    );
    let error =
        materialize_knowledge_merge(&target, &moved_source, &plan).expect_err("moved source fails");
    assert!(error.to_string().contains("input moved"));
}

#[test]
fn many_to_one_coalescence_requires_source_normalization_first() {
    let target = snapshot(
        "a",
        "a1",
        vec![entity("shared", "a-r1", "shared", "a-src")],
        vec![],
    );
    let source = snapshot(
        "b",
        "b1",
        vec![
            entity("first", "b-r1", "shared", "b1-src"),
            entity("second", "b-r2", "shared", "b2-src"),
        ],
        vec![],
    );
    let first = CandidatePair::new("shared", "first");
    let second = CandidatePair::new("shared", "second");
    let resolutions = vec![
        reviewed(&target, "shared", &source, "first", IdentityDecision::Same),
        reviewed(&target, "shared", &source, "second", IdentityDecision::Same),
    ];

    let error = plan_knowledge_merge(
        &target,
        &source,
        &BTreeSet::from([first, second]),
        &resolutions,
    )
    .expect_err("many-to-one merge fails closed");
    assert!(error.to_string().contains("many-to-one coalescence"));
}

#[test]
fn generated_import_id_can_never_overwrite_target_data() {
    let target = snapshot(
        "a",
        "a1",
        vec![entity("project-b::new", "a-r1", "existing target", "a-src")],
        vec![],
    );
    let source = snapshot(
        "project-b",
        "b1",
        vec![entity("new", "b-r1", "source entity", "b-src")],
        vec![],
    );

    let error = plan_knowledge_merge(&target, &source, &BTreeSet::new(), &[])
        .expect_err("generated ID collision fails");
    assert!(error.to_string().contains("already exists"));
}

#[test]
fn relation_source_must_be_bound_to_subject_evidence() {
    let error = KnowledgeSnapshot::from_local(
        "space",
        "r1",
        vec![
            entity("subject", "s1", "subject", "subject-source"),
            entity("object", "o1", "object", "object-source"),
        ],
        vec![LocalRelation {
            subject: "subject".to_string(),
            predicate: "uses".to_string(),
            object: "object".to_string(),
            source_ref: "unbound-source".to_string(),
        }],
    )
    .expect_err("unbound provenance fails");
    assert!(error.to_string().contains("not bound to its subject"));
}

#[test]
fn policy_cannot_silently_coalesce_entities() {
    let error = IdentityResolution::policy(
        CandidatePair::new("a", "b"),
        IdentityDecision::Same,
        "a-r1",
        "b-r1",
        "packet",
        "shared_identifier",
    )
    .expect_err("team same requires review");
    assert!(error.to_string().contains("must be different"));
}

#[test]
fn tampered_plan_is_detected_by_result_hash() {
    let target = snapshot(
        "a",
        "a1",
        vec![entity("root", "a-r1", "root", "a-src")],
        vec![],
    );
    let source = snapshot(
        "b",
        "b1",
        vec![entity("new", "b-r1", "new", "b-src")],
        vec![],
    );
    let mut plan =
        plan_knowledge_merge(&target, &source, &BTreeSet::new(), &[]).expect("plan succeeds");
    plan.result_hash = "tampered-result".to_string();

    let error = materialize_knowledge_merge(&target, &source, &plan)
        .expect_err("tampered plan fails verification");
    assert!(error.to_string().contains("plan hash mismatch"));
}

#[test]
fn planning_is_byte_deterministic_across_input_order() {
    let target = snapshot(
        "a",
        "a1",
        vec![entity("root", "a-r1", "root", "a-src")],
        vec![],
    );
    let forward = vec![
        entity("one", "b-r1", "one", "one-src"),
        entity("two", "b-r2", "two", "two-src"),
        entity("three", "b-r3", "three", "three-src"),
    ];
    let mut reverse = forward.clone();
    reverse.reverse();
    let first = snapshot("b", "b1", forward, vec![]);
    let second = snapshot("b", "b1", reverse, vec![]);

    let first_plan =
        plan_knowledge_merge(&target, &first, &BTreeSet::new(), &[]).expect("first plans");
    let second_plan =
        plan_knowledge_merge(&target, &second, &BTreeSet::new(), &[]).expect("second plans");

    assert_eq!(first.hash(), second.hash());
    assert_eq!(first_plan.plan_hash, second_plan.plan_hash);
    assert_eq!(
        serde_json::to_vec(&first_plan).expect("first serializes"),
        serde_json::to_vec(&second_plan).expect("second serializes")
    );
    assert!(first_plan
        .entity_actions
        .iter()
        .all(|action| matches!(action, EntityAction::Add { .. })));
}
