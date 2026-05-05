//! Subcommand implementations.
//!
//! Each submodule is a single subcommand. The split keeps the CLI surface
//! in `cli.rs` argument-shape-only and lets each command's tests live next
//! to its `run`.

pub mod consolidate;
pub mod init;
pub mod integrate;
pub mod mcp;
pub mod status;
