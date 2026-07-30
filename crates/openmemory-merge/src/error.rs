//! Stable pure-planner errors.

use thiserror::Error;

pub type MergeResult<T> = Result<T, MergeError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MergeErrorCode {
    InvalidInput,
    BoundExceeded,
    UnsortedInput,
    DuplicateInput,
    SnapshotMismatch,
    DiscoveryIncomplete,
    StaleReceipt,
    ConflictingResolution,
    ManyToOne,
    QualifiedIdCollision,
    DanglingEndpoint,
    ThreeWayConflict,
    AccountingMismatch,
    SinkFailure,
    ProtocolViolation,
}

impl MergeErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::BoundExceeded => "bound_exceeded",
            Self::UnsortedInput => "unsorted_input",
            Self::DuplicateInput => "duplicate_input",
            Self::SnapshotMismatch => "snapshot_mismatch",
            Self::DiscoveryIncomplete => "discovery_incomplete",
            Self::StaleReceipt => "stale_receipt",
            Self::ConflictingResolution => "conflicting_resolution",
            Self::ManyToOne => "many_to_one",
            Self::QualifiedIdCollision => "qualified_id_collision",
            Self::DanglingEndpoint => "dangling_endpoint",
            Self::ThreeWayConflict => "three_way_conflict",
            Self::AccountingMismatch => "accounting_mismatch",
            Self::SinkFailure => "sink_failure",
            Self::ProtocolViolation => "protocol_violation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}: {message}", code = .code.as_str())]
pub struct MergeError {
    code: MergeErrorCode,
    message: String,
}

impl MergeError {
    #[must_use]
    pub fn new(code: MergeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> MergeErrorCode {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}
