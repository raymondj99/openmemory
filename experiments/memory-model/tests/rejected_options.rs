use openmemory_memory_model_poc::audit::{AuditStore, ChangeSetDraft, DraftOp, SubmitMode};

fn add(store: &AuditStore, key: &str, id: &str) {
    store
        .submit(
            &ChangeSetDraft::new(
                key,
                "test",
                "in-place control",
                vec![DraftOp::Add {
                    logical_id: id.to_string(),
                    content: id.to_string(),
                }],
            ),
            SubmitMode::Apply,
        )
        .expect("add");
}

#[test]
fn independent_in_place_commits_expose_partial_material_merge() {
    let directory = tempfile::tempdir().expect("tempdir");
    let first_partition = AuditStore::open(&directory.path().join("first")).expect("first");
    let second_partition = AuditStore::open(&directory.path().join("second")).expect("second");

    // This is the failure mode of a naive material merge across independent
    // SQLite files: commit partition one, then lose the process before two.
    add(&first_partition, "merge-part-1", "copied-to-first");
    let injected_failure_before_second_commit = true;
    if !injected_failure_before_second_commit {
        add(&second_partition, "merge-part-2", "copied-to-second");
    }

    assert_eq!(first_partition.visible().expect("first visible").len(), 1);
    assert!(second_partition
        .visible()
        .expect("second visible")
        .is_empty());
}
