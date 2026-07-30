mod common;

use std::collections::BTreeSet;

use openmemory_merge::discovery::{CandidateIndex, DEFAULT_CANDIDATE_CAP};
use openmemory_merge::evidence::IdentityPolicy;
use openmemory_merge::model::{
    EntityRecord, LogicalId, MemorySnapshot, ObjectAddress, RelationRecord, SnapshotHeader,
    SnapshotSource,
};
use openmemory_merge::planner::{plan_merge, MemoryActionSink, MergePolicy, PlanningReceipts};
use openmemory_merge::{MergeErrorCode, MergeResult};

use common::*;

#[test]
fn missing_candidate_resolution_fails_closed() {
    let target_space = space(501);
    let source_space = space(502);
    let target_snapshot = snapshot_id(503);
    let source_snapshot = snapshot_id(504);
    let mut target = memory_snapshot(
        target_snapshot,
        target_space,
        1,
        vec![entity(target_space, "target", 505, "same", "concept")],
        Vec::new(),
        Vec::new(),
    );
    let mut source = memory_snapshot(
        source_snapshot,
        source_space,
        2,
        vec![entity(source_space, "source", 506, "same", "concept")],
        Vec::new(),
        Vec::new(),
    );
    let identity_policy = IdentityPolicy::new(1, 1).unwrap();
    let index = CandidateIndex::build(
        1,
        target_snapshot,
        identity_policy,
        target.entities().iter().cloned(),
    )
    .unwrap();
    let discovery = index
        .discover(
            &source.entities()[0],
            source_snapshot,
            DEFAULT_CANDIDATE_CAP,
        )
        .unwrap();
    let receipts = PlanningReceipts::new(vec![discovery], Vec::new(), Vec::new());
    let merge_policy = MergePolicy::new(1, 1).unwrap();
    assert_eq!(
        reference_merge_error(&target, &source, &receipts, &merge_policy),
        Some(MergeErrorCode::DiscoveryIncomplete)
    );
    let mut sink = MemoryActionSink::new(10).unwrap();
    let error = plan_merge(
        &mut target,
        &mut source,
        &receipts,
        &merge_policy,
        &mut sink,
    )
    .unwrap_err();
    assert_eq!(error.code(), MergeErrorCode::DiscoveryIncomplete);
    assert!(sink.is_aborted());
}

#[test]
fn many_to_one_reviewed_coalescence_is_rejected() {
    let target_space = space(511);
    let source_space = space(512);
    let target_snapshot = snapshot_id(513);
    let source_snapshot = snapshot_id(514);
    let target = memory_snapshot(
        target_snapshot,
        target_space,
        1,
        vec![verified_entity(
            target_space,
            "target",
            515,
            "target",
            "concept",
            target_snapshot,
            "unique",
            "same",
            3,
        )],
        Vec::new(),
        Vec::new(),
    );
    let source = memory_snapshot(
        source_snapshot,
        source_space,
        2,
        vec![
            verified_entity(
                source_space,
                "source-a",
                516,
                "source a",
                "concept",
                source_snapshot,
                "unique",
                "same",
                3,
            ),
            verified_entity(
                source_space,
                "source-b",
                517,
                "source b",
                "concept",
                source_snapshot,
                "unique",
                "same",
                3,
            ),
        ],
        Vec::new(),
        Vec::new(),
    );
    let policy = IdentityPolicy::new(1, 1)
        .unwrap()
        .with_authoritative_namespace("unique", 3)
        .unwrap();
    let receipts = planning_receipts(
        &target,
        &source,
        &policy,
        &BTreeSet::from([
            ("target".to_string(), "source-a".to_string()),
            ("target".to_string(), "source-b".to_string()),
        ]),
    );
    let merge_policy = MergePolicy::new(1, 1).unwrap();
    assert_eq!(
        reference_merge_error(&target, &source, &receipts, &merge_policy),
        Some(MergeErrorCode::ManyToOne)
    );
    let mut target_run = target;
    let mut source_run = source;
    let mut sink = MemoryActionSink::new(20).unwrap();
    assert_eq!(
        plan_merge(
            &mut target_run,
            &mut source_run,
            &receipts,
            &merge_policy,
            &mut sink,
        )
        .unwrap_err()
        .code(),
        MergeErrorCode::ManyToOne
    );
}

#[test]
fn qualified_id_collision_never_overwrites_prior_import() {
    let target_space = space(521);
    let source_space = space(522);
    let source_logical = LogicalId::new("source").unwrap();
    let collision =
        openmemory_merge::model::QualifiedObjectId::derive(source_space, &source_logical);
    let target_entity = EntityRecord::new(
        ObjectAddress::new(
            target_space,
            LogicalId::from_projection(collision.as_str()).unwrap(),
        ),
        revision(523),
        "prior import",
        Some("concept"),
    )
    .unwrap();
    let target = memory_snapshot(
        snapshot_id(524),
        target_space,
        1,
        vec![target_entity],
        Vec::new(),
        Vec::new(),
    );
    let source = memory_snapshot(
        snapshot_id(525),
        source_space,
        2,
        vec![EntityRecord::new(
            ObjectAddress::new(source_space, source_logical),
            revision(526),
            "new import",
            Some("concept"),
        )
        .unwrap()],
        Vec::new(),
        Vec::new(),
    );
    let policy = IdentityPolicy::new(1, 1).unwrap();
    let receipts = planning_receipts(&target, &source, &policy, &BTreeSet::new());
    let merge_policy = MergePolicy::new(1, 1).unwrap();
    assert_eq!(
        reference_merge_error(&target, &source, &receipts, &merge_policy),
        Some(MergeErrorCode::QualifiedIdCollision)
    );
    let mut target_run = target;
    let mut source_run = source;
    let mut sink = MemoryActionSink::new(10).unwrap();
    assert_eq!(
        plan_merge(
            &mut target_run,
            &mut source_run,
            &receipts,
            &merge_policy,
            &mut sink,
        )
        .unwrap_err()
        .code(),
        MergeErrorCode::QualifiedIdCollision
    );
}

#[test]
fn duplicate_relation_assertions_are_rejected_not_deduplicated() {
    let target_space = space(531);
    let source_space = space(532);
    let target = memory_snapshot(
        snapshot_id(533),
        target_space,
        1,
        vec![
            entity(target_space, "a", 534, "a", "concept"),
            entity(target_space, "b", 535, "b", "concept"),
        ],
        Vec::new(),
        vec![
            relation(target_space, "r1", "a", "links", "b", 536),
            relation(target_space, "r2", "a", "links", "b", 537),
        ],
    );
    let source = memory_snapshot(
        snapshot_id(538),
        source_space,
        2,
        vec![entity(source_space, "source", 539, "source", "concept")],
        Vec::new(),
        Vec::new(),
    );
    let policy = IdentityPolicy::new(1, 1).unwrap();
    let receipts = planning_receipts(&target, &source, &policy, &BTreeSet::new());
    let merge_policy = MergePolicy::new(1, 1).unwrap();
    assert_eq!(
        reference_merge_error(&target, &source, &receipts, &merge_policy),
        Some(MergeErrorCode::DuplicateInput)
    );
    let mut target_run = target;
    let mut source_run = source;
    let mut sink = MemoryActionSink::new(20).unwrap();
    assert_eq!(
        plan_merge(
            &mut target_run,
            &mut source_run,
            &receipts,
            &merge_policy,
            &mut sink,
        )
        .unwrap_err()
        .code(),
        MergeErrorCode::DuplicateInput
    );
}

#[test]
fn rewired_self_loop_requires_an_explicit_predicate_policy() {
    let target_space = space(551);
    let source_space = space(552);
    let target_snapshot = snapshot_id(553);
    let source_snapshot = snapshot_id(554);
    let target = memory_snapshot(
        target_snapshot,
        target_space,
        1,
        vec![verified_entity(
            target_space,
            "target",
            555,
            "target",
            "concept",
            target_snapshot,
            "unique",
            "shared",
            7,
        )],
        Vec::new(),
        Vec::new(),
    );
    let source = memory_snapshot(
        source_snapshot,
        source_space,
        2,
        vec![verified_entity(
            source_space,
            "source",
            556,
            "source",
            "concept",
            source_snapshot,
            "unique",
            "shared",
            7,
        )],
        Vec::new(),
        vec![relation(
            source_space,
            "self-relation",
            "source",
            "depends_on",
            "source",
            557,
        )],
    );
    let identity_policy = IdentityPolicy::new(1, 1)
        .unwrap()
        .with_authoritative_namespace("unique", 7)
        .unwrap();
    let receipts = planning_receipts(
        &target,
        &source,
        &identity_policy,
        &BTreeSet::from([("target".to_string(), "source".to_string())]),
    );
    let rejected_policy = MergePolicy::new(1, 1).unwrap();
    assert_eq!(
        reference_merge_error(&target, &source, &receipts, &rejected_policy),
        Some(MergeErrorCode::ThreeWayConflict)
    );

    let mut rejected_target = target.clone();
    let mut rejected_source = source.clone();
    let mut rejected_sink = MemoryActionSink::new(10).unwrap();
    let error = plan_merge(
        &mut rejected_target,
        &mut rejected_source,
        &receipts,
        &rejected_policy,
        &mut rejected_sink,
    )
    .unwrap_err();
    assert_eq!(error.code(), MergeErrorCode::ThreeWayConflict);
    assert!(rejected_sink.is_aborted());

    let mut allowed_target = target;
    let mut allowed_source = source;
    let mut allowed_sink = MemoryActionSink::new(10).unwrap();
    let policy = MergePolicy::new(1, 1)
        .unwrap()
        .with_allowed_self_loops(["depends_on".to_string()])
        .unwrap();
    assert_eq!(
        reference_merge_error(&allowed_target, &allowed_source, &receipts, &policy),
        None
    );
    let plan = plan_merge(
        &mut allowed_target,
        &mut allowed_source,
        &receipts,
        &policy,
        &mut allowed_sink,
    )
    .unwrap();
    assert_eq!(plan.action_counts().added_relations, 1);
}

#[derive(Clone)]
struct ReversedEntitySource {
    inner: MemorySnapshot,
}

impl SnapshotSource for ReversedEntitySource {
    fn header(&self) -> &SnapshotHeader {
        self.inner.header()
    }

    fn visit_entities(
        &mut self,
        visitor: &mut dyn FnMut(EntityRecord) -> MergeResult<()>,
    ) -> MergeResult<()> {
        for record in self.inner.entities().iter().rev() {
            visitor(record.clone())?;
        }
        Ok(())
    }

    fn visit_observations(
        &mut self,
        visitor: &mut dyn FnMut(openmemory_merge::model::ObservationRecord) -> MergeResult<()>,
    ) -> MergeResult<()> {
        self.inner.visit_observations(visitor)
    }

    fn visit_relations(
        &mut self,
        visitor: &mut dyn FnMut(RelationRecord) -> MergeResult<()>,
    ) -> MergeResult<()> {
        self.inner.visit_relations(visitor)
    }
}

#[test]
fn unsorted_stream_aborts_without_a_finished_plan() {
    let target_space = space(541);
    let source_space = space(542);
    let target = memory_snapshot(
        snapshot_id(543),
        target_space,
        1,
        vec![entity(target_space, "target", 544, "target", "concept")],
        Vec::new(),
        Vec::new(),
    );
    let source = memory_snapshot(
        snapshot_id(545),
        source_space,
        2,
        vec![
            entity(source_space, "a", 546, "a", "concept"),
            entity(source_space, "b", 547, "b", "concept"),
        ],
        Vec::new(),
        Vec::new(),
    );
    let policy = IdentityPolicy::new(1, 1).unwrap();
    let receipts = planning_receipts(&target, &source, &policy, &BTreeSet::new());
    let mut target_run = target;
    let mut source_run = ReversedEntitySource { inner: source };
    let mut sink = MemoryActionSink::new(20).unwrap();
    assert_eq!(
        plan_merge(
            &mut target_run,
            &mut source_run,
            &receipts,
            &MergePolicy::new(1, 1).unwrap(),
            &mut sink,
        )
        .unwrap_err()
        .code(),
        MergeErrorCode::UnsortedInput
    );
    assert!(sink.is_aborted());
    assert!(sink.plan().is_none());
}
