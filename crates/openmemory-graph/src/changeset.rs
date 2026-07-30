//! Public typed changeset protocol.
//!
//! Construction validates bounds immediately; submission revalidates before
//! hashing or opening a transaction. Mutation mechanics remain private to the
//! audited graph owner.

pub use crate::audit::{
    ChangeOperation, ChangeSetDraft, ChangeSetReceipt, ChangeSetState, DestructionPreview,
    ObjectKind, ProposalRetentionReport, ReviewAuthorization, SubmitMode,
};
