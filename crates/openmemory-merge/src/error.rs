//! Exhaustive validation and planning failures.

use openmemory_core::space::SpaceId;
use thiserror::Error;

pub type MergeResult<T> = Result<T, MergeError>;

/// A content-safe pure validation/planning error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MergeError {
    #[error("unsupported canonical format version {0}")]
    UnsupportedVersion(u32),
    #[error("bounded field {field} is invalid")]
    InvalidField { field: &'static str },
    #[error("duplicate {kind} identifier")]
    Duplicate { kind: &'static str },
    #[error("relation endpoint is missing from the snapshot")]
    DanglingRelationEndpoint,
    #[error("semantic hash does not match canonical fields")]
    SemanticHashMismatch,
    #[error("snapshot hash does not match canonical records")]
    SnapshotHashMismatch,
    #[error("identity packet binding hash does not match its evidence")]
    PacketHashMismatch,
    #[error("identity receipt hash does not match its decision")]
    ReceiptHashMismatch,
    #[error("identity receipt is stale for the current revisions or policy")]
    StaleIdentityReceipt,
    #[error("identity evidence is invalid: {reason}")]
    InvalidEvidence { reason: &'static str },
    #[error("agent proposal is invalid: {reason}")]
    InvalidAgentProposal { reason: &'static str },
    #[error("candidate coverage is incomplete or contains extras")]
    CandidateCoverage,
    #[error("candidate page truncation was not acknowledged")]
    CandidatePageTruncated,
    #[error("one candidate does not have exactly one current resolution")]
    CandidateResolutionCount,
    #[error("one source entity resolves same to multiple targets")]
    SourceIdentityAmbiguous,
    #[error("multiple source entities cannot coalesce into one target")]
    ManyToOneCoalescence,
    #[error("generated source-qualified identifier collides with target")]
    GeneratedIdCollision,
    #[error("source and target must be distinct spaces ({0})")]
    SameSpace(SpaceId),
    #[error("merge accounting is incomplete")]
    IncompleteAccounting,
    #[error("merge has unresolved semantic conflicts")]
    UnresolvedConflicts,
    #[error("merge plan hash does not match its actions")]
    PlanHashMismatch,
    #[error("merge input moved after planning")]
    InputMoved,
    #[error("materialized result does not match the predicted result")]
    PredictedResultMismatch,
}
