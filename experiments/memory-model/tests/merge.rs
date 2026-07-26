use openmemory_memory_model_poc::audit::{AuditStore, ChangeSetDraft, DraftOp, SubmitMode};
use openmemory_memory_model_poc::merge::{apply_plan, materialize_plan, plan_merge, GraphSnapshot};
use openmemory_memory_model_poc::PocError;

fn apply(store: &AuditStore, key: &str, ops: Vec<DraftOp>) {
    store
        .submit(
            &ChangeSetDraft::new(key, "test", key, ops),
            SubmitMode::Apply,
        )
        .expect("apply change");
}

fn copy(source: &AuditStore, destination: &std::path::Path) -> AuditStore {
    source.snapshot_to(destination).expect("copy store")
}

#[test]
fn three_way_merge_combines_independent_changes_and_preserves_source() {
    let directory = tempfile::tempdir().expect("tempdir");
    let base = AuditStore::open(&directory.path().join("base")).expect("base");
    apply(
        &base,
        "seed",
        vec![DraftOp::Add {
            logical_id: "shared".to_string(),
            content: "base".to_string(),
        }],
    );
    let source = copy(&base, &directory.path().join("source"));
    let target = copy(&base, &directory.path().join("target"));
    apply(
        &source,
        "source-change",
        vec![DraftOp::Add {
            logical_id: "from-source".to_string(),
            content: "source knowledge".to_string(),
        }],
    );
    apply(
        &target,
        "target-change",
        vec![DraftOp::Add {
            logical_id: "from-target".to_string(),
            content: "target knowledge".to_string(),
        }],
    );
    let source_before = GraphSnapshot::read(&source).expect("source before");
    let plan = plan_merge(
        &GraphSnapshot::read(&base).expect("base snapshot"),
        &source_before,
        &GraphSnapshot::read(&target).expect("target snapshot"),
    )
    .expect("plan");
    assert!(plan.conflicts.is_empty());
    assert_eq!(plan.actions.len(), 1);

    apply_plan(&target, &plan, "merger").expect("merge");
    let visible = target.visible().expect("visible");
    assert_eq!(visible.len(), 3);
    assert_eq!(
        GraphSnapshot::read(&target).expect("target").hash,
        plan.result_hash
    );
    assert_eq!(GraphSnapshot::read(&source).expect("source"), source_before);
}

#[test]
fn divergent_edits_conflict_instead_of_using_last_writer_wins() {
    let directory = tempfile::tempdir().expect("tempdir");
    let base = AuditStore::open(&directory.path().join("base")).expect("base");
    apply(
        &base,
        "seed",
        vec![DraftOp::Add {
            logical_id: "shared".to_string(),
            content: "base".to_string(),
        }],
    );
    let source = copy(&base, &directory.path().join("source"));
    let target = copy(&base, &directory.path().join("target"));
    apply(
        &source,
        "source-edit",
        vec![DraftOp::Supersede {
            logical_id: "shared".to_string(),
            expected_version: 1,
            content: "source answer".to_string(),
        }],
    );
    apply(
        &target,
        "target-edit",
        vec![DraftOp::Supersede {
            logical_id: "shared".to_string(),
            expected_version: 1,
            content: "target answer".to_string(),
        }],
    );
    let plan = plan_merge(
        &GraphSnapshot::read(&base).expect("base snapshot"),
        &GraphSnapshot::read(&source).expect("source snapshot"),
        &GraphSnapshot::read(&target).expect("target snapshot"),
    )
    .expect("plan");
    assert_eq!(plan.conflicts.len(), 1);
    assert!(plan.actions.is_empty());
    assert!(matches!(
        apply_plan(&target, &plan, "merger"),
        Err(PocError::Conflict(_))
    ));
    assert_eq!(
        target.get("shared").expect("get").expect("exists").content,
        "target answer"
    );
}

#[test]
fn stale_target_invalidates_the_entire_plan() {
    let directory = tempfile::tempdir().expect("tempdir");
    let base = AuditStore::open(&directory.path().join("base")).expect("base");
    let source = copy(&base, &directory.path().join("source"));
    let target = copy(&base, &directory.path().join("target"));
    apply(
        &source,
        "source-add",
        vec![DraftOp::Add {
            logical_id: "source".to_string(),
            content: "source".to_string(),
        }],
    );
    let plan = plan_merge(
        &GraphSnapshot::read(&base).expect("base snapshot"),
        &GraphSnapshot::read(&source).expect("source snapshot"),
        &GraphSnapshot::read(&target).expect("target snapshot"),
    )
    .expect("plan");
    apply(
        &target,
        "target-moved",
        vec![DraftOp::Add {
            logical_id: "late".to_string(),
            content: "late".to_string(),
        }],
    );
    let before = GraphSnapshot::read(&target).expect("before rejected merge");
    assert!(matches!(
        apply_plan(&target, &plan, "merger"),
        Err(PocError::Conflict(_))
    ));
    assert_eq!(
        GraphSnapshot::read(&target).expect("after rejection"),
        before
    );
}

#[test]
fn delete_and_restore_semantics_merge_without_erasing_lineage() {
    let directory = tempfile::tempdir().expect("tempdir");
    let base = AuditStore::open(&directory.path().join("base")).expect("base");
    apply(
        &base,
        "seed",
        vec![DraftOp::Add {
            logical_id: "memory".to_string(),
            content: "obsolete".to_string(),
        }],
    );
    let source = copy(&base, &directory.path().join("source"));
    let target = copy(&base, &directory.path().join("target"));
    apply(
        &source,
        "delete",
        vec![DraftOp::Delete {
            logical_id: "memory".to_string(),
            expected_version: 1,
        }],
    );
    let plan = plan_merge(
        &GraphSnapshot::read(&base).expect("base snapshot"),
        &GraphSnapshot::read(&source).expect("source snapshot"),
        &GraphSnapshot::read(&target).expect("target snapshot"),
    )
    .expect("plan");
    apply_plan(&target, &plan, "merger").expect("merge deletion");
    assert!(target.visible().expect("visible").is_empty());
    assert!(
        target
            .get("memory")
            .expect("get")
            .expect("tombstone")
            .deleted
    );
}

#[test]
fn materialization_promotes_verified_copy_and_retains_exact_backup() {
    let directory = tempfile::tempdir().expect("tempdir");
    let base = AuditStore::open(&directory.path().join("base")).expect("base");
    let source = copy(&base, &directory.path().join("source"));
    let target = copy(&base, &directory.path().join("target"));
    apply(
        &source,
        "source-add",
        vec![DraftOp::Add {
            logical_id: "new".to_string(),
            content: "new knowledge".to_string(),
        }],
    );
    let before = GraphSnapshot::read(&target).expect("target before");
    let plan = plan_merge(
        &GraphSnapshot::read(&base).expect("base snapshot"),
        &GraphSnapshot::read(&source).expect("source snapshot"),
        &before,
    )
    .expect("plan");
    let report = materialize_plan(&target, &plan, "merger", None).expect("materialize");

    assert_eq!(
        GraphSnapshot::read(&AuditStore::open(target.root()).expect("promoted"))
            .expect("snapshot")
            .hash,
        plan.result_hash
    );
    assert_eq!(
        GraphSnapshot::read(&AuditStore::open(&report.backup_root).expect("backup"))
            .expect("backup snapshot"),
        before
    );
}

#[test]
fn identical_source_and_base_is_a_no_op_for_any_target() {
    let directory = tempfile::tempdir().expect("tempdir");
    let base = AuditStore::open(&directory.path().join("base")).expect("base");
    let source = copy(&base, &directory.path().join("source"));
    let target = copy(&base, &directory.path().join("target"));
    apply(
        &target,
        "target-add",
        vec![DraftOp::Add {
            logical_id: "target-only".to_string(),
            content: "target".to_string(),
        }],
    );
    let target_snapshot = GraphSnapshot::read(&target).expect("target snapshot");
    let plan = plan_merge(
        &GraphSnapshot::read(&base).expect("base snapshot"),
        &GraphSnapshot::read(&source).expect("source snapshot"),
        &target_snapshot,
    )
    .expect("plan");
    assert!(plan.actions.is_empty());
    assert!(plan.conflicts.is_empty());
    assert_eq!(plan.result_hash, target_snapshot.hash);
}
