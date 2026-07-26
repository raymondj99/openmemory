use std::path::Path;

use openmemory_memory_model_poc::audit::{ApplyCrashPoint, AuditStore, ChangeSetDraft, DraftOp};
use openmemory_memory_model_poc::identity::{
    analyze_pair, EntityRecord, IdentityDecision, IdentityLedger, IdentityReviewCrashPoint,
};
use openmemory_memory_model_poc::merge::{
    materialize_plan, plan_merge, GraphSnapshot, SwapCrashPoint,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("crash worker failed before injection: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    match args.get(1).map(String::as_str) {
        Some("audit") if args.len() == 4 => {
            let store = AuditStore::open(Path::new(&args[2]))?;
            let point = match args[3].as_str() {
                "ledger" => ApplyCrashPoint::AfterLedgerInsert,
                "mutation" => ApplyCrashPoint::AfterCanonicalMutation,
                "commit" => ApplyCrashPoint::AfterCommit,
                other => return Err(format!("unknown audit crash point {other:?}").into()),
            };
            let draft = ChangeSetDraft::new(
                "crash-patch",
                "crash-worker",
                "durability injection",
                vec![DraftOp::Patch {
                    logical_id: "memory-1".to_string(),
                    expected_version: 1,
                    content: "after crash boundary".to_string(),
                }],
            );
            store.submit_with_crash_point(&draft, point)?;
        }
        Some("merge") if args.len() == 6 => {
            let base = AuditStore::open(Path::new(&args[2]))?;
            let source = AuditStore::open(Path::new(&args[3]))?;
            let target = AuditStore::open(Path::new(&args[4]))?;
            let point = match args[5].as_str() {
                "intent" => SwapCrashPoint::AfterIntent,
                "backup" => SwapCrashPoint::AfterBackupRename,
                "promotion" => SwapCrashPoint::AfterPromotion,
                other => return Err(format!("unknown merge crash point {other:?}").into()),
            };
            let plan = plan_merge(
                &GraphSnapshot::read(&base)?,
                &GraphSnapshot::read(&source)?,
                &GraphSnapshot::read(&target)?,
            )?;
            materialize_plan(&target, &plan, "crash-worker", Some(point))?;
        }
        Some("identity") if args.len() == 5 => {
            let ledger = IdentityLedger::open(Path::new(&args[2]))?;
            let point = match args[4].as_str() {
                "decision" => IdentityReviewCrashPoint::AfterDecisionInsert,
                "state" => IdentityReviewCrashPoint::AfterProposalStateUpdate,
                "commit" => IdentityReviewCrashPoint::AfterCommit,
                other => return Err(format!("unknown identity crash point {other:?}").into()),
            };
            let left = EntityRecord::new("project-a", "cerpheus-a", "a-r1", "Cerpheus");
            let right = EntityRecord::new("project-b", "cerpheus-b", "b-r1", "Cerpheus");
            let packet = analyze_pair(&left, &right)?;
            ledger.review_with_crash_point(
                &args[3],
                &packet,
                "crash-worker",
                IdentityDecision::Same,
                "durability injection",
                point,
            )?;
        }
        _ => {
            return Err(
                "usage: poc-crash-worker audit ROOT POINT | merge BASE SOURCE TARGET POINT | identity ROOT PROPOSAL POINT".into(),
            );
        }
    }
    Ok(())
}
