//! Subcommand implementations.
//!
//! Each submodule is a single subcommand. The split keeps the CLI surface
//! in `cli.rs` argument-shape-only and lets each command's tests live next
//! to its `run`.

pub mod completions;
pub mod consolidate;
pub mod daemon;
#[cfg(feature = "eval")]
pub mod eval;
pub mod ingest;
pub mod init;
pub mod integrate;
pub mod mcp;
pub mod migrate;
#[cfg(feature = "embeddings")]
pub mod model;
pub mod scriptable;
pub mod setup;
pub mod status;
#[cfg(feature = "watch")]
pub mod watch;
