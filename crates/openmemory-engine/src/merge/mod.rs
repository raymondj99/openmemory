//! Staged material-merge coordination.
//!
//! Phase 0 establishes private module ownership only. Materialization,
//! verification, promotion, and recovery behavior is introduced in later
//! phases after the pure planner and durable space services exist.

#![forbid(unsafe_code)]

mod materialize;
mod promotion;
mod recovery;
mod verify;
