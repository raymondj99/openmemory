mod common;

use std::collections::BTreeSet;

use openmemory_merge::evidence::IdentityPolicy;
use openmemory_merge::model::{
    EntityRecord, MemorySnapshot, ObservationRecord, RelationRecord, SnapshotHeader, SnapshotSource,
};
use openmemory_merge::planner::{
    plan_merge, verify_plan, ActionSink, MemoryActionSink, MergeAction, MergePlan, MergePolicy,
    PlanPrelude,
};
use openmemory_merge::{MergeError, MergeErrorCode, MergeResult};

use common::*;

fn representative_graphs() -> (
    openmemory_merge::model::MemorySnapshot,
    openmemory_merge::model::MemorySnapshot,
    IdentityPolicy,
) {
    let target_space = space(1);
    let source_space = space(2);
    let target_snapshot = snapshot_id(3);
    let source_snapshot = snapshot_id(4);
    let policy = IdentityPolicy::new(7, 11)
        .unwrap()
        .with_authoritative_namespace("repo", 5)
        .unwrap();
    let target = memory_snapshot(
        target_snapshot,
        target_space,
        1,
        vec![
            verified_entity(
                target_space,
                "shared-target",
                10,
                "Shared",
                "concept",
                target_snapshot,
                "repo",
                "canonical/shared",
                5,
            ),
            entity(target_space, "target-only", 11, "Target only", "concept"),
        ],
        vec![observation(
            target_space,
            "target-observation",
            "shared-target",
            12,
            "same semantic claim",
        )],
        Vec::new(),
    );
    let source = memory_snapshot(
        source_snapshot,
        source_space,
        2,
        vec![
            verified_entity(
                source_space,
                "shared-source",
                20,
                "Shared renamed",
                "concept",
                source_snapshot,
                "repo",
                "canonical/shared",
                5,
            ),
            entity(source_space, "source-only", 21, "Source only", "concept"),
        ],
        vec![
            observation(
                source_space,
                "source-shared-observation",
                "shared-source",
                22,
                "same semantic claim",
            ),
            observation(
                source_space,
                "source-new-observation",
                "source-only",
                23,
                "new semantic claim",
            ),
        ],
        vec![relation(
            source_space,
            "source-relation",
            "shared-source",
            "references",
            "source-only",
            24,
        )],
    );
    (target, source, policy)
}

#[test]
fn streaming_plan_matches_bounded_materialized_reference() {
    let (mut target, mut source, identity_policy) = representative_graphs();
    let receipts = planning_receipts(
        &target,
        &source,
        &identity_policy,
        &BTreeSet::from([("shared-target".to_string(), "shared-source".to_string())]),
    );
    let expected_actions = reference_actions(&target, &source, &receipts);
    let expected_counts = reference_action_counts(&expected_actions);
    let expected_accounting = reference_accounting(&expected_actions, &receipts);
    let merge_policy = MergePolicy::new(1, identity_policy.policy_generation()).unwrap();
    let mut sink = MemoryActionSink::new(100).unwrap();
    let plan = plan_merge(
        &mut target,
        &mut source,
        &receipts,
        &merge_policy,
        &mut sink,
    )
    .unwrap();

    assert_eq!(sink.actions(), expected_actions);
    assert_eq!(plan.action_counts(), &expected_counts);
    assert_eq!(plan.accounting(), &expected_accounting);
    assert_eq!(plan.action_counts().coalesced_entities, 1);
    assert_eq!(plan.action_counts().added_entities, 1);
    assert_eq!(plan.action_counts().merged_observations, 1);
    assert_eq!(plan.action_counts().added_observations, 1);
    assert_eq!(plan.action_counts().added_relations, 1);
    assert_eq!(
        plan.predicted_result_hash(),
        reference_predicted_hash(sink.actions())
    );
    verify_plan(sink.actions(), &plan).unwrap();
    assert_eq!(sink.plan(), Some(&plan));
}

#[test]
fn same_label_without_review_never_coalesces() {
    let target_space = space(31);
    let source_space = space(32);
    let target_snapshot = snapshot_id(33);
    let source_snapshot = snapshot_id(34);
    let target = memory_snapshot(
        target_snapshot,
        target_space,
        1,
        vec![entity(
            target_space,
            "target-homonym",
            35,
            "cerpheus",
            "concept",
        )],
        Vec::new(),
        Vec::new(),
    );
    let source = memory_snapshot(
        source_snapshot,
        source_space,
        2,
        vec![entity(
            source_space,
            "source-homonym",
            36,
            "cerpheus",
            "concept",
        )],
        Vec::new(),
        Vec::new(),
    );
    let identity_policy = IdentityPolicy::new(2, 2).unwrap();
    let receipts = planning_receipts(&target, &source, &identity_policy, &BTreeSet::new());
    let mut target_run = target;
    let mut source_run = source;
    let mut sink = MemoryActionSink::new(10).unwrap();
    let plan = plan_merge(
        &mut target_run,
        &mut source_run,
        &receipts,
        &MergePolicy::new(1, 2).unwrap(),
        &mut sink,
    )
    .unwrap();
    assert_eq!(plan.action_counts().coalesced_entities, 0);
    assert_eq!(plan.action_counts().added_entities, 1);
}

struct FailingSink {
    fail_at: usize,
    fail_finish: bool,
    emitted: usize,
    begun: bool,
    aborted: usize,
    finished: bool,
}

impl ActionSink for FailingSink {
    fn begin(&mut self, _prelude: &PlanPrelude) -> MergeResult<()> {
        self.begun = true;
        Ok(())
    }

    fn emit(&mut self, _action: &MergeAction) -> MergeResult<()> {
        if self.emitted == self.fail_at {
            return Err(MergeError::new(
                MergeErrorCode::SinkFailure,
                "injected sink failure",
            ));
        }
        self.emitted += 1;
        Ok(())
    }

    fn finish(&mut self, _plan: &MergePlan) -> MergeResult<()> {
        if self.fail_finish {
            return Err(MergeError::new(
                MergeErrorCode::SinkFailure,
                "injected sink finish failure",
            ));
        }
        self.finished = true;
        Ok(())
    }

    fn abort(&mut self) {
        self.aborted += 1;
        self.finished = false;
    }
}

#[test]
fn every_sink_failure_aborts_once_and_never_finishes() {
    let (target, source, identity_policy) = representative_graphs();
    let receipts = planning_receipts(
        &target,
        &source,
        &identity_policy,
        &BTreeSet::from([("shared-target".to_string(), "shared-source".to_string())]),
    );
    for fail_at in 0..=8 {
        let mut target = target.clone();
        let mut source = source.clone();
        let mut sink = FailingSink {
            fail_at,
            fail_finish: fail_at == 8,
            emitted: 0,
            begun: false,
            aborted: 0,
            finished: false,
        };
        let result = plan_merge(
            &mut target,
            &mut source,
            &receipts,
            &MergePolicy::new(1, identity_policy.policy_generation()).unwrap(),
            &mut sink,
        );
        assert_eq!(result.unwrap_err().code(), MergeErrorCode::SinkFailure);
        assert!(sink.begun);
        assert_eq!(sink.aborted, 1);
        assert!(!sink.finished);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisitKind {
    Entities,
    Observations,
    Relations,
}

struct InjectedFailureSource {
    inner: MemorySnapshot,
    fail: Option<VisitKind>,
}

impl SnapshotSource for InjectedFailureSource {
    fn header(&self) -> &SnapshotHeader {
        self.inner.header()
    }

    fn visit_entities(
        &mut self,
        visitor: &mut dyn FnMut(EntityRecord) -> MergeResult<()>,
    ) -> MergeResult<()> {
        if self.fail == Some(VisitKind::Entities) {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "injected entity stream failure",
            ));
        }
        self.inner.visit_entities(visitor)
    }

    fn visit_observations(
        &mut self,
        visitor: &mut dyn FnMut(ObservationRecord) -> MergeResult<()>,
    ) -> MergeResult<()> {
        if self.fail == Some(VisitKind::Observations) {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "injected observation stream failure",
            ));
        }
        self.inner.visit_observations(visitor)
    }

    fn visit_relations(
        &mut self,
        visitor: &mut dyn FnMut(RelationRecord) -> MergeResult<()>,
    ) -> MergeResult<()> {
        if self.fail == Some(VisitKind::Relations) {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "injected relation stream failure",
            ));
        }
        self.inner.visit_relations(visitor)
    }
}

#[test]
fn every_injected_snapshot_stream_failure_aborts_without_a_plan() {
    let (target, source, identity_policy) = representative_graphs();
    let receipts = planning_receipts(
        &target,
        &source,
        &identity_policy,
        &BTreeSet::from([("shared-target".to_string(), "shared-source".to_string())]),
    );
    for fail in [
        VisitKind::Entities,
        VisitKind::Observations,
        VisitKind::Relations,
    ] {
        for fail_target in [true, false] {
            let mut target = InjectedFailureSource {
                inner: target.clone(),
                fail: fail_target.then_some(fail),
            };
            let mut source = InjectedFailureSource {
                inner: source.clone(),
                fail: (!fail_target).then_some(fail),
            };
            let mut sink = MemoryActionSink::new(100).unwrap();
            let error = plan_merge(
                &mut target,
                &mut source,
                &receipts,
                &MergePolicy::new(1, identity_policy.policy_generation()).unwrap(),
                &mut sink,
            )
            .unwrap_err();
            assert_eq!(error.code(), MergeErrorCode::InvalidInput);
            assert!(sink.is_aborted());
            assert!(sink.actions().is_empty());
            assert!(sink.plan().is_none());
        }
    }
}

#[test]
fn forged_action_structure_is_rejected_even_with_a_real_plan() {
    let (mut target, mut source, identity_policy) = representative_graphs();
    let receipts = planning_receipts(
        &target,
        &source,
        &identity_policy,
        &BTreeSet::from([("shared-target".to_string(), "shared-source".to_string())]),
    );
    let mut sink = MemoryActionSink::new(100).unwrap();
    let plan = plan_merge(
        &mut target,
        &mut source,
        &receipts,
        &MergePolicy::new(1, identity_policy.policy_generation()).unwrap(),
        &mut sink,
    )
    .unwrap();
    let mut forged = sink.actions().to_vec();
    forged.swap(0, 1);
    assert_eq!(
        verify_plan(&forged, &plan).unwrap_err().code(),
        MergeErrorCode::AccountingMismatch
    );
}
