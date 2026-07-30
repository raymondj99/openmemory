//! Pure, deterministic identity evidence and semantic merge planning.
//!
//! This crate has no storage, authorization, model, network, clock, or
//! filesystem dependency. Product policy is normalized by an owner above this
//! crate and passed in as immutable validated values.

#![forbid(unsafe_code)]

mod canonical;

pub mod discovery;
pub mod error;
pub mod evidence;
pub mod hash;
pub mod model;
pub mod planner;
pub mod receipt;
pub mod three_way;

pub use error::{MergeError, MergeErrorCode, MergeResult};
