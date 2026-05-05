//! Shared foundations for the `open-memory` workspace.
//!
//! This crate defines the smallest set of abstractions every other
//! crate consumes: a [`Clock`] trait, configuration loader, error
//! type, retry helper, and the SQLite schema-migration helper.
//!
//! Implementation lands in subsequent commits — see the project
//! roadmap under `docs/03-roadmap.md`.

#![forbid(unsafe_code)]
