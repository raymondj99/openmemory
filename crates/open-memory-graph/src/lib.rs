//! Persistent knowledge-graph memory for AI agents.
//!
//! `open-memory-graph` is the heart of the project: SQLite plus the
//! [`open_memory_index`] hybrid search engine, kept in lockstep by a single
//! `MemoryStore` (lands in subsequent commits). Entities have stable
//! names; observations are temporal facts about entities (`valid_from`,
//! `valid_until`); relations are directed edges. Recall is hybrid search
//! over observation text, filtered by temporal validity, scored with
//! Ebbinghaus-style decay and access-frequency boosts.

#![forbid(unsafe_code)]

pub mod error;
pub mod recall;
pub mod remember;
pub mod schema;
pub mod store;
pub mod types;

pub use error::{MemoryError, MemoryResult};
pub use open_memory_index::SearchMode;
pub use recall::{
    RecallFilters, RecallResult, CORRECTION_RETRIEVAL_BOOST, RECALL_MIN_SCORE,
    SPREADING_DISTANCE_DECAY,
};
pub use remember::{ObservationInput, RelationInput, RememberOutcome};
pub use schema::MEMORY_SCHEMA_VERSION;
pub use store::{EntityListRow, MemoryStatus, MemoryStore, MEMORY_DB_FILE};
pub use types::{new_id, Entity, EntityType, MemoryTier, Observation, Relation};
