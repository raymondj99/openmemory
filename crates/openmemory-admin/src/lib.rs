//! Typed contracts for the local OpenMemory admin API.
//!
//! Existing v1alpha1 DTOs remain re-exported from the crate root. New memory
//! spaces surfaces have focused module owners so they can grow without
//! returning this crate to a single DTO monolith.

#![forbid(unsafe_code)]

mod existing;

pub mod changesets;
pub mod identity;
pub mod merges;
pub mod spaces;

pub use existing::*;
