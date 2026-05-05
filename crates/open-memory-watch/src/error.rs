//! Error types for `open-memory-watch`.
//!
//! Wraps every fallible call the watcher makes — filesystem I/O, the
//! `notify` family, the underlying graph store — into one `WatchError`
//! enum. Lets the CLI render uniform messages without leaking internal
//! types and keeps `?` ergonomic across the watcher modules.

use std::io;

use thiserror::Error;

/// Anything that can go wrong inside [`crate::Watcher`].
#[derive(Debug, Error)]
pub enum WatchError {
    /// Filesystem read / stat / canonicalise / etc.
    #[error("filesystem error: {0}")]
    Io(#[from] io::Error),

    /// Underlying `notify` watcher refused a path or surfaced an event
    /// error. Re-thrown so the caller can decide whether to give up or
    /// rebuild the watcher.
    #[error("file-watcher error: {0}")]
    Notify(#[from] notify::Error),

    /// `ignore` failed to parse a `.gitignore` / `.open-memory-ignore`
    /// file or hit a permission error during the walk.
    #[error("ignore-walk error: {0}")]
    Ignore(#[from] ignore::Error),

    /// Memory store / index error from the underlying `open-memory-graph`
    /// crate.
    #[error("memory-store error: {0}")]
    Memory(#[from] open_memory_graph::MemoryError),

    /// Hybrid search-engine error. The watcher inserts directly into the
    /// engine, so its errors travel up here rather than through the
    /// graph-store wrapper.
    #[error("index-engine error: {0}")]
    Index(#[from] open_memory_index::IndexError),

    /// Caller passed a bad argument (e.g. relative root path that
    /// canonicalisation can't resolve, or a non-directory).
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// Convenience alias so call sites read `WatchResult<T>` instead of
/// `Result<T, WatchError>`.
pub type WatchResult<T> = Result<T, WatchError>;
