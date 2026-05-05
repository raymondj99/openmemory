//! Shared foundations for the [`open-memory`] workspace.
//!
//! The thinnest possible foundation: trait abstractions ([`clock::Clock`],
//! `Embedder` re-exported from [`testing`] when the feature is on),
//! the workspace [`error::OmError`] / [`error::OmResult`] type, the
//! [`config::Config`] loader/saver, the [`migrations::Migrator`] schema-
//! versioning helper used by every `*_meta` table, and a [`retry::with_retry`]
//! helper used by anything that talks to the network.
//!
//! Nothing pipeline-shaped lives here — there is no parser stage, no
//! chunker stage, no source stage. The crate exists to give the graph,
//! index, and embed crates a shared clock, error, and schema-migration
//! vocabulary.
//!
//! # Stability
//!
//! v0.1.x is **pre-stable**. Breaking changes are allowed; bumping the
//! minor version (0.1 → 0.2) is the signal.
//!
//! [`open-memory`]: https://github.com/raymondj99/open-memory

#![forbid(unsafe_code)]

pub mod clock;
pub mod config;
pub mod error;
pub mod migrations;
pub mod retry;

#[cfg(feature = "testing")]
pub mod testing;
