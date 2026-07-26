//! Pure, deterministic identity and directional memory-space merge planning.
//!
//! This crate performs no filesystem, SQLite, network, model, authorization,
//! or clock work. Callers provide validated immutable snapshots and current
//! reviewed receipts; the crate returns a fully accounted plan and predicted
//! result hash. Persistence and promotion belong to `openmemory-engine`.

#![forbid(unsafe_code)]

pub mod candidate;
pub mod canonical;
pub mod error;
pub mod hash;
pub mod identity;
pub mod planner;
pub mod three_way;

pub use candidate::{
    discover_candidates, CandidateDiscovery, CandidatePage, CandidateRecord, NamespacePolicy,
};

pub use error::{MergeError, MergeResult};
