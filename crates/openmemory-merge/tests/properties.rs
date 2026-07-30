mod common;

use std::collections::BTreeSet;

use openmemory_merge::discovery::{CandidateIndex, MAX_CANDIDATE_CAP};
use openmemory_merge::evidence::{
    analyze_pair, normalize_label, DeterministicResolution, IdentityPolicy,
};
use openmemory_merge::model::IdentifierAssertion;
use openmemory_merge::planner::{plan_merge, MemoryActionSink, MergePolicy};
use proptest::prelude::*;

use common::*;

fn property_config() -> ProptestConfig {
    ProptestConfig {
        cases: 256,
        max_shrink_iters: 2_048,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(property_config())]

    #[test]
    fn every_source_entity_is_accounted_once_and_order_independent(
        ids in prop::collection::btree_set(0_u16..4_000, 1..48)
    ) {
        let target_space = space(401);
        let source_space = space(402);
        let target_snapshot = snapshot_id(403);
        let source_snapshot = snapshot_id(404);
        let target = memory_snapshot(
            target_snapshot,
            target_space,
            1,
            vec![entity(target_space, "root", 405, "target root", "concept")],
            Vec::new(),
            Vec::new(),
        );
        let forward = ids
            .iter()
            .enumerate()
            .map(|(index, value)| {
                entity(
                    source_space,
                    &format!("source-{value}"),
                    10_000 + u64::try_from(index).unwrap(),
                    &format!("source label {value}"),
                    "concept",
                )
            })
            .collect::<Vec<_>>();
        let mut reverse = forward.clone();
        reverse.reverse();
        let source_forward = memory_snapshot(
            source_snapshot,
            source_space,
            2,
            forward,
            Vec::new(),
            Vec::new(),
        );
        let source_reverse = memory_snapshot(
            source_snapshot,
            source_space,
            2,
            reverse,
            Vec::new(),
            Vec::new(),
        );
        let identity_policy = IdentityPolicy::new(1, 1).unwrap();
        let receipts_forward =
            planning_receipts(&target, &source_forward, &identity_policy, &BTreeSet::new());
        let receipts_reverse =
            planning_receipts(&target, &source_reverse, &identity_policy, &BTreeSet::new());
        let expected_actions =
            reference_actions(&target, &source_forward, &receipts_forward);
        let expected_counts = reference_action_counts(&expected_actions);
        let expected_accounting =
            reference_accounting(&expected_actions, &receipts_forward);

        let mut first_target = target.clone();
        let mut first_source = source_forward;
        let mut first_sink = MemoryActionSink::new(500).unwrap();
        let first = plan_merge(
            &mut first_target,
            &mut first_source,
            &receipts_forward,
            &MergePolicy::new(1, 1).unwrap(),
            &mut first_sink,
        )
        .unwrap();

        let mut second_target = target;
        let mut second_source = source_reverse;
        let mut second_sink = MemoryActionSink::new(500).unwrap();
        let second = plan_merge(
            &mut second_target,
            &mut second_source,
            &receipts_reverse,
            &MergePolicy::new(1, 1).unwrap(),
            &mut second_sink,
        )
        .unwrap();

        prop_assert_eq!(first.plan_hash(), second.plan_hash());
        prop_assert_eq!(first_sink.actions(), expected_actions);
        prop_assert_eq!(first.action_counts(), &expected_counts);
        prop_assert_eq!(first.accounting(), &expected_accounting);
        prop_assert_eq!(first.action_counts(), second.action_counts());
        prop_assert_eq!(first.accounting(), second.accounting());
        prop_assert_eq!(first_sink.actions(), second_sink.actions());
        prop_assert_eq!(
            first.predicted_result_hash(),
            reference_predicted_hash(first_sink.actions())
        );
    }

    #[test]
    fn context_and_claimed_identifiers_never_prove_same(
        label in "[A-Za-z0-9 ]{1,40}",
        identifier in "[a-z0-9:/._-]{1,40}"
    ) {
        let left_space = space(411);
        let right_space = space(412);
        let left_snapshot = snapshot_id(413);
        let right_snapshot = snapshot_id(414);
        let left = entity(left_space, "left", 415, &label, "concept")
            .with_identifiers([
                IdentifierAssertion::claimed("external", &identifier).unwrap()
            ])
            .unwrap();
        let right = entity(right_space, "right", 416, &label, "concept")
            .with_identifiers([
                IdentifierAssertion::claimed("external", &identifier).unwrap()
            ])
            .unwrap();
        let packet = analyze_pair(
            &left,
            left_snapshot,
            &right,
            right_snapshot,
            &IdentityPolicy::new(1, 1)
                .unwrap()
                .with_authoritative_namespace("external", 7)
                .unwrap(),
        )
        .unwrap();
        prop_assert_eq!(
            packet.resolution(),
            DeterministicResolution::ReviewOrSeparate
        );
    }

    #[test]
    fn indexed_discovery_matches_bounded_bruteforce_reference(
        labels in prop::collection::vec("[a-z]{1,8}", 1..40),
        source_label in "[a-z]{1,8}"
    ) {
        let target_space = space(421);
        let source_space = space(422);
        let target_snapshot = snapshot_id(423);
        let source_snapshot = snapshot_id(424);
        let targets = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                entity(
                    target_space,
                    &format!("target-{index}"),
                    20_000 + u64::try_from(index).unwrap(),
                    label,
                    "concept",
                )
            })
            .collect::<Vec<_>>();
        let source = entity(source_space, "source", 425, &source_label, "concept");
        let policy = IdentityPolicy::new(1, 1).unwrap();
        let index = CandidateIndex::build(
            1,
            target_snapshot,
            policy,
            targets.clone(),
        )
        .unwrap();
        let receipt = index
            .discover(&source, source_snapshot, MAX_CANDIDATE_CAP)
            .unwrap();
        let indexed = receipt
            .candidates()
            .iter()
            .map(|candidate| candidate.target().address().clone())
            .collect::<BTreeSet<_>>();
        let brute = targets
            .iter()
            .filter(|target| normalize_label(target.label()) == normalize_label(&source_label))
            .map(|target| target.address().clone())
            .collect::<BTreeSet<_>>();
        prop_assert_eq!(indexed, brute);
        prop_assert!(!receipt.truncated());
    }

    #[test]
    fn deterministic_resolution_is_symmetric_even_when_packet_direction_is_not(
        value in "[a-z0-9._/-]{1,32}"
    ) {
        let left_space = space(431);
        let right_space = space(432);
        let left_snapshot = snapshot_id(433);
        let right_snapshot = snapshot_id(434);
        let left = verified_entity(
            left_space,
            "left",
            435,
            "left",
            "project",
            left_snapshot,
            "repo",
            &value,
            9,
        );
        let right = verified_entity(
            right_space,
            "right",
            436,
            "right",
            "project",
            right_snapshot,
            "repo",
            &value,
            9,
        );
        let policy = IdentityPolicy::new(1, 1)
            .unwrap()
            .with_authoritative_namespace("repo", 9)
            .unwrap();
        let forward = analyze_pair(&left, left_snapshot, &right, right_snapshot, &policy).unwrap();
        let reverse = analyze_pair(&right, right_snapshot, &left, left_snapshot, &policy).unwrap();
        prop_assert_eq!(forward.resolution(), reverse.resolution());
        prop_assert_eq!(forward.resolution(), DeterministicResolution::ProofSame);
        prop_assert_ne!(forward.hash(), reverse.hash());
    }
}
