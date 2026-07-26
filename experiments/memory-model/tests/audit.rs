use std::sync::{Arc, Barrier};

use openmemory_memory_model_poc::audit::{
    AuditStore, ChangeSetDraft, ChangeSetState, DraftOp, SubmitMode,
};
use openmemory_memory_model_poc::PocError;

fn add(store: &AuditStore, key: &str, id: &str, content: &str) -> String {
    store
        .submit(
            &ChangeSetDraft::new(
                key,
                "test",
                "seed",
                vec![DraftOp::Add {
                    logical_id: id.to_string(),
                    content: content.to_string(),
                }],
            ),
            SubmitMode::Apply,
        )
        .expect("seed memory")
        .id
}

#[test]
fn proposal_is_invisible_until_approved_and_captures_full_diff() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = AuditStore::open(directory.path()).expect("open");
    let receipt = store
        .submit(
            &ChangeSetDraft::new(
                "proposal-1",
                "alice",
                "new team convention",
                vec![DraftOp::Add {
                    logical_id: "convention".to_string(),
                    content: "format with rustfmt".to_string(),
                }],
            ),
            SubmitMode::Propose,
        )
        .expect("propose");

    assert_eq!(receipt.state, ChangeSetState::Proposed);
    assert!(store.visible().expect("visible before").is_empty());
    let pending = store.audit_ops(&receipt.id).expect("pending audit");
    assert_eq!(pending.len(), 1);
    assert!(pending[0].before.is_none());
    assert!(pending[0].after.is_none());

    let approved = store.approve(&receipt.id, "reviewer-1").expect("approve");
    assert_eq!(approved.state, ChangeSetState::Applied);
    assert_eq!(store.visible().expect("visible after").len(), 1);
    let audit = store.audit_ops(&receipt.id).expect("applied audit");
    assert!(audit[0].before.is_none());
    assert_eq!(
        audit[0].after.as_ref().map(|value| value.content.as_str()),
        Some("format with rustfmt")
    );
    assert!(audit[0].before_hash.is_none());
    assert!(audit[0].after_hash.is_some());
}

#[test]
fn patch_retains_revision_while_supersede_creates_a_lineage() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = AuditStore::open(directory.path()).expect("open");
    add(&store, "seed", "m1", "Rust 1.84 is required");
    let original = store.get("m1").expect("get").expect("exists");

    store
        .submit(
            &ChangeSetDraft::new(
                "patch",
                "alice",
                "correct typo",
                vec![DraftOp::Patch {
                    logical_id: "m1".to_string(),
                    expected_version: 1,
                    content: "Rust 1.85 is required".to_string(),
                }],
            ),
            SubmitMode::Apply,
        )
        .expect("patch");
    let patched = store.get("m1").expect("get").expect("exists");
    assert_eq!(patched.revision_id, original.revision_id);
    assert_eq!(patched.row_version, 2);

    store
        .submit(
            &ChangeSetDraft::new(
                "supersede",
                "alice",
                "requirements changed",
                vec![DraftOp::Supersede {
                    logical_id: "m1".to_string(),
                    expected_version: 2,
                    content: "Rust 1.88 is required".to_string(),
                }],
            ),
            SubmitMode::Apply,
        )
        .expect("supersede");
    let superseded = store.get("m1").expect("get").expect("exists");
    assert_ne!(superseded.revision_id, patched.revision_id);
    assert_eq!(
        superseded.previous_revision_id.as_deref(),
        Some(patched.revision_id.as_str())
    );
    assert_eq!(superseded.row_version, 3);
}

#[test]
fn stale_multi_operation_approval_rolls_back_everything() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = AuditStore::open(directory.path()).expect("open");
    add(&store, "seed-a", "a", "a0");
    add(&store, "seed-b", "b", "b0");
    let proposal = store
        .submit(
            &ChangeSetDraft::new(
                "multi",
                "alice",
                "atomic pair",
                vec![
                    DraftOp::Patch {
                        logical_id: "a".to_string(),
                        expected_version: 1,
                        content: "a1".to_string(),
                    },
                    DraftOp::Patch {
                        logical_id: "b".to_string(),
                        expected_version: 1,
                        content: "b1".to_string(),
                    },
                ],
            ),
            SubmitMode::Propose,
        )
        .expect("proposal");
    store
        .submit(
            &ChangeSetDraft::new(
                "move-b",
                "bob",
                "concurrent update",
                vec![DraftOp::Patch {
                    logical_id: "b".to_string(),
                    expected_version: 1,
                    content: "b-concurrent".to_string(),
                }],
            ),
            SubmitMode::Apply,
        )
        .expect("move b");
    let generation_before = store.generation().expect("generation");

    assert!(matches!(
        store.approve(&proposal.id, "reviewer"),
        Err(PocError::Conflict(_))
    ));
    assert_eq!(
        store.get("a").expect("get a").expect("a exists").content,
        "a0"
    );
    assert_eq!(
        store.get("b").expect("get b").expect("b exists").content,
        "b-concurrent"
    );
    assert_eq!(store.generation().expect("generation"), generation_before);
    assert_eq!(
        store.changeset_state(&proposal.id).expect("state"),
        ChangeSetState::Proposed
    );
}

#[test]
fn concurrent_reviewers_cannot_lose_an_update() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = AuditStore::open(directory.path()).expect("open");
    add(&store, "seed", "m1", "base");
    let first = store
        .submit(
            &ChangeSetDraft::new(
                "candidate-a",
                "alice",
                "candidate a",
                vec![DraftOp::Patch {
                    logical_id: "m1".to_string(),
                    expected_version: 1,
                    content: "candidate a".to_string(),
                }],
            ),
            SubmitMode::Propose,
        )
        .expect("first proposal");
    let second = store
        .submit(
            &ChangeSetDraft::new(
                "candidate-b",
                "bob",
                "candidate b",
                vec![DraftOp::Patch {
                    logical_id: "m1".to_string(),
                    expected_version: 1,
                    content: "candidate b".to_string(),
                }],
            ),
            SubmitMode::Propose,
        )
        .expect("second proposal");

    let barrier = Arc::new(Barrier::new(3));
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (receipt, reviewer) in [(first, "reviewer-a"), (second, "reviewer-b")] {
            let barrier = Arc::clone(&barrier);
            let store = store.clone();
            handles.push(scope.spawn(move || {
                barrier.wait();
                store.approve(&receipt.id, reviewer)
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("reviewer did not panic"))
            .collect::<Vec<_>>()
    });

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(PocError::Conflict(_))))
            .count(),
        1
    );
    assert_eq!(
        store.get("m1").expect("get").expect("exists").row_version,
        2
    );
}

#[test]
fn idempotency_retry_is_exact_and_key_reuse_is_rejected() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = AuditStore::open(directory.path()).expect("open");
    let draft = ChangeSetDraft::new(
        "stable-key",
        "alice",
        "retryable",
        vec![DraftOp::Add {
            logical_id: "m1".to_string(),
            content: "once".to_string(),
        }],
    );
    let first = store.submit(&draft, SubmitMode::Apply).expect("first");
    let second = store.submit(&draft, SubmitMode::Apply).expect("retry");
    assert_eq!(first, second);
    assert_eq!(store.snapshot().expect("snapshot").len(), 1);
    assert!(matches!(
        store.submit(
            &ChangeSetDraft::new(
                "stable-key",
                "alice",
                "retryable",
                vec![DraftOp::Add {
                    logical_id: "different".to_string(),
                    content: "different".to_string(),
                }],
            ),
            SubmitMode::Apply
        ),
        Err(PocError::Conflict(_))
    ));
}

#[test]
fn delete_restore_and_reject_have_explicit_visibility() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = AuditStore::open(directory.path()).expect("open");
    add(&store, "seed", "m1", "keep lineage");
    store
        .submit(
            &ChangeSetDraft::new(
                "delete",
                "alice",
                "manual removal",
                vec![DraftOp::Delete {
                    logical_id: "m1".to_string(),
                    expected_version: 1,
                }],
            ),
            SubmitMode::Apply,
        )
        .expect("delete");
    assert!(store.visible().expect("visible").is_empty());
    let deleted = store.get("m1").expect("get").expect("tombstone");
    assert!(deleted.deleted);

    let restore = store
        .submit(
            &ChangeSetDraft::new(
                "restore",
                "alice",
                "candidate restore",
                vec![DraftOp::Restore {
                    logical_id: "m1".to_string(),
                    expected_version: 2,
                }],
            ),
            SubmitMode::Propose,
        )
        .expect("propose restore");
    store.reject(&restore.id, "reviewer").expect("reject");
    assert!(store.visible().expect("still hidden").is_empty());
    assert_eq!(
        store.changeset_state(&restore.id).expect("state"),
        ChangeSetState::Rejected
    );
}
