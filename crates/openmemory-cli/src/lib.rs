//! Library entry point for `openmemory-cli`.
//!
//! The crate publishes both a library (this file) and a binary
//! (`bin/openmemory`). The binary is a thin shell that forwards to
//! [`cli::run`]; the library exists so integration tests under
//! `tests/` (notably `tui_snapshot.rs`) can import the screen
//! renderers and exercise them against `ratatui::backend::TestBackend`
//! without spawning the binary.

#![forbid(unsafe_code)]

pub mod cli;
pub mod commands;
pub mod ui;
