//! Durable derived-index outbox contracts.
//!
//! The graph transaction owns outbox insertion; repair publishes derived
//! state and advances readiness only after persistence succeeds.

use serde::{Deserialize, Serialize};

/// Bounded index repair result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IndexRepairReport {
    pub applied: u64,
    pub remaining: u64,
    pub generation: u64,
}
