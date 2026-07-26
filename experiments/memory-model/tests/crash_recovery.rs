use std::path::Path;
use std::process::Command;

use openmemory_memory_model_poc::audit::{AuditStore, ChangeSetDraft, DraftOp, SubmitMode};
use openmemory_memory_model_poc::identity::{
    analyze_pair, AgentProposal, EntityRecord, IdentityDecision, IdentityLedger, ProposalState,
};
use openmemory_memory_model_poc::merge::{
    plan_merge, recover_materialized_merge, GraphSnapshot, RecoveryStatus,
};

fn worker() -> &'static str {
    env!("CARGO_BIN_EXE_poc-crash-worker")
}

fn apply(store: &AuditStore, key: &str, ops: Vec<DraftOp>) {
    store
        .submit(
            &ChangeSetDraft::new(key, "test", key, ops),
            SubmitMode::Apply,
        )
        .expect("apply");
}

#[test]
fn audit_transaction_is_old_or_new_at_every_crash_boundary() {
    for point in ["ledger", "mutation", "commit"] {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("audit");
        let store = AuditStore::open(&root).expect("open");
        apply(
            &store,
            "seed",
            vec![DraftOp::Add {
                logical_id: "memory-1".to_string(),
                content: "before crash".to_string(),
            }],
        );
        drop(store);

        let status = Command::new(worker())
            .args(["audit", root.to_str().expect("utf8 path"), point])
            .status()
            .expect("run worker");
        assert!(!status.success(), "worker must abort at {point}");

        let reopened = AuditStore::open(&root).expect("reopen");
        let memory = reopened.get("memory-1").expect("get").expect("exists");
        if point == "commit" {
            assert_eq!(memory.content, "after crash boundary");
            assert_eq!(memory.row_version, 2);
        } else {
            assert_eq!(memory.content, "before crash");
            assert_eq!(memory.row_version, 1);
        }
    }
}

fn setup_merge(root: &Path) -> (AuditStore, AuditStore, AuditStore, String, String) {
    let base = AuditStore::open(&root.join("base")).expect("base");
    apply(
        &base,
        "seed",
        vec![DraftOp::Add {
            logical_id: "shared".to_string(),
            content: "base".to_string(),
        }],
    );
    let source = base.snapshot_to(&root.join("source")).expect("source copy");
    let target = base.snapshot_to(&root.join("target")).expect("target copy");
    apply(
        &source,
        "source-add",
        vec![DraftOp::Add {
            logical_id: "source-only".to_string(),
            content: "source knowledge".to_string(),
        }],
    );
    let before = GraphSnapshot::read(&target).expect("before").hash;
    let plan = plan_merge(
        &GraphSnapshot::read(&base).expect("base snapshot"),
        &GraphSnapshot::read(&source).expect("source snapshot"),
        &GraphSnapshot::read(&target).expect("target snapshot"),
    )
    .expect("plan");
    (base, source, target, before, plan.result_hash)
}

#[test]
fn directory_swap_rolls_forward_from_every_durable_boundary() {
    for point in ["intent", "backup", "promotion"] {
        let directory = tempfile::tempdir().expect("tempdir");
        let (base, source, target, before_hash, result_hash) = setup_merge(directory.path());
        let source_hash = GraphSnapshot::read(&source).expect("source before").hash;
        drop((base, source, target));

        let status = Command::new(worker())
            .arg("merge")
            .arg(directory.path().join("base"))
            .arg(directory.path().join("source"))
            .arg(directory.path().join("target"))
            .arg(point)
            .status()
            .expect("run merge worker");
        assert!(!status.success(), "worker must abort at {point}");

        let recovery = recover_materialized_merge(&directory.path().join("target"))
            .expect("recover materialization");
        let RecoveryStatus::Recovered(report) = recovery else {
            panic!("expected an interrupted merge");
        };
        assert_eq!(
            GraphSnapshot::read(
                &AuditStore::open(&directory.path().join("target")).expect("target")
            )
            .expect("target snapshot")
            .hash,
            result_hash
        );
        assert_eq!(
            GraphSnapshot::read(&AuditStore::open(&report.backup_root).expect("backup"))
                .expect("backup snapshot")
                .hash,
            before_hash
        );
        assert_eq!(
            GraphSnapshot::read(
                &AuditStore::open(&directory.path().join("source")).expect("source")
            )
            .expect("source snapshot")
            .hash,
            source_hash
        );
        assert_eq!(
            recover_materialized_merge(&directory.path().join("target")).expect("second recovery"),
            RecoveryStatus::NoIntent
        );
    }
}

#[test]
fn identity_review_is_old_or_new_at_every_crash_boundary() {
    for point in ["decision", "state", "commit"] {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("identity");
        let ledger = IdentityLedger::open(&root).expect("open");
        let left = EntityRecord::new("project-a", "cerpheus-a", "a-r1", "Cerpheus");
        let right = EntityRecord::new("project-b", "cerpheus-b", "b-r1", "Cerpheus");
        let packet = analyze_pair(&left, &right).expect("packet");
        let proposal = AgentProposal {
            pair: packet.pair.clone(),
            packet_hash: packet.binding_hash().expect("packet hash"),
            first_revision: packet.first.revision_id.clone(),
            second_revision: packet.second.revision_id.clone(),
            recommendation: IdentityDecision::Same,
            evidence_ids: packet.evidence.iter().map(|item| item.id).collect(),
            relation_suggestions: Vec::new(),
            rationale: "crash test recommendation".to_string(),
            model_id: "crash-fixture".to_string(),
            prompt_version: "v1".to_string(),
        };
        let receipt = ledger
            .submit("identity-crash", &packet, &proposal)
            .expect("submit");
        drop(ledger);

        let status = Command::new(worker())
            .arg("identity")
            .arg(&root)
            .arg(&receipt.id)
            .arg(point)
            .status()
            .expect("run worker");
        assert!(!status.success(), "worker must abort at {point}");

        let reopened = IdentityLedger::open(&root).expect("reopen");
        let cached = reopened
            .proposal_for_revisions(&packet)
            .expect("proposal")
            .expect("cached");
        if point == "commit" {
            assert_eq!(cached.state, ProposalState::Applied);
            assert_eq!(
                reopened
                    .current_decision(&packet.pair)
                    .expect("decision")
                    .expect("applied")
                    .decision,
                IdentityDecision::Same
            );
        } else {
            assert_eq!(cached.state, ProposalState::Proposed);
            assert!(reopened
                .current_decision(&packet.pair)
                .expect("decision")
                .is_none());
        }
    }
}
