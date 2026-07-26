use openmemory_memory_model_poc::audit::MemorySnapshot;
use openmemory_memory_model_poc::merge::{plan_merge, GraphSnapshot};
use proptest::prelude::*;

fn memory(id: &str, content: String, deleted: bool, version: u64) -> MemorySnapshot {
    MemorySnapshot {
        logical_id: id.to_string(),
        revision_id: format!("revision-{id}-{version}"),
        content,
        row_version: version,
        deleted,
        previous_revision_id: None,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn conflict_classification_is_symmetric(
        base_content in "[a-z]{1,24}",
        source_content in "[a-z]{1,24}",
        target_content in "[a-z]{1,24}",
        base_deleted in any::<bool>(),
        source_deleted in any::<bool>(),
        target_deleted in any::<bool>(),
    ) {
        let base = GraphSnapshot::from_memories([memory(
            "m", base_content, base_deleted, 1,
        )]).expect("base");
        let source = GraphSnapshot::from_memories([memory(
            "m", source_content, source_deleted, 2,
        )]).expect("source");
        let target = GraphSnapshot::from_memories([memory(
            "m", target_content, target_deleted, 3,
        )]).expect("target");
        let forward = plan_merge(&base, &source, &target).expect("forward");
        let reverse = plan_merge(&base, &target, &source).expect("reverse");
        prop_assert_eq!(forward.conflicts.len(), reverse.conflicts.len());
    }

    #[test]
    fn planning_is_deterministic_except_for_plan_identity(
        values in prop::collection::vec(("[a-z]{1,12}", any::<bool>()), 0..64),
    ) {
        let base = GraphSnapshot::from_memories(
            values.iter().enumerate().map(|(index, (content, deleted))| {
                memory(&format!("m-{index}"), content.clone(), *deleted, 1)
            }),
        ).expect("base");
        let source = base.clone();
        let target = base.clone();
        let first = plan_merge(&base, &source, &target).expect("first");
        let second = plan_merge(&base, &source, &target).expect("second");
        prop_assert_eq!(&first.actions, &second.actions);
        prop_assert_eq!(&first.conflicts, &second.conflicts);
        prop_assert_eq!(first.result_hash, second.result_hash);
        prop_assert!(first.actions.is_empty());
        prop_assert!(first.conflicts.is_empty());
    }

    #[test]
    fn disjoint_source_and_target_additions_never_conflict(
        source_content in "[a-z]{1,24}",
        target_content in "[a-z]{1,24}",
    ) {
        let base = GraphSnapshot::from_memories([]).expect("base");
        let source = GraphSnapshot::from_memories([
            memory("source", source_content, false, 1),
        ]).expect("source");
        let target = GraphSnapshot::from_memories([
            memory("target", target_content, false, 1),
        ]).expect("target");
        let plan = plan_merge(&base, &source, &target).expect("plan");
        prop_assert!(plan.conflicts.is_empty());
        prop_assert_eq!(plan.actions.len(), 1);
    }
}
