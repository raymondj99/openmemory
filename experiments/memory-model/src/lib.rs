//! Isolated validation harness for scoped memory, audit, and merge semantics.
//!
//! This crate is intentionally not a member of the production workspace. Its
//! job is to make design hypotheses executable before production APIs or
//! schemas commit to them.

#![forbid(unsafe_code)]

pub mod audit;
pub mod github_merge;
pub mod identity;
pub mod merge;
pub mod real_world;
pub mod semantic_merge;
pub mod spaces;

use std::io;

/// Errors surfaced by the proof-of-concept state machines.
#[derive(Debug, thiserror::Error)]
pub enum PocError {
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("injected failure: {0}")]
    Injected(&'static str),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Memory(#[from] openmemory_graph::MemoryError),
}

pub type PocResult<T> = Result<T, PocError>;
