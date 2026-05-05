//! Persistent knowledge-graph memory for AI agents.
//!
//! The heart of the project: SQLite plus the [`open_memory_index`] hybrid
//! search engine, kept in lockstep by a single [`MemoryStore`]. Entities
//! have stable names; observations are temporal facts about entities
//! ([`valid_from`](Observation::valid_from),
//! [`valid_until`](Observation::valid_until)); relations are directed
//! edges. Recall is hybrid search over observation text, filtered by
//! temporal validity, re-scored with Ebbinghaus decay and access-frequency
//! boosts.
//!
//! # Quick start
//!
//! ```
//! use std::sync::Arc;
//! use open_memory_core::config::Config;
//! use open_memory_graph::{MemoryStore, EntityType, ObservationInput, RecallFilters};
//!
//! let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
//! store.remember(
//!     "Raymond",
//!     EntityType::Person,
//!     &[ObservationInput::new("prefers Rust over Python")],
//!     &[],
//!     "test",
//! ).unwrap();
//!
//! let mut filters = RecallFilters::new();
//! filters.mode = Some(open_memory_index::SearchMode::KeywordOnly);
//! let hits = store.recall("Rust", 5, &filters).unwrap();
//! assert!(!hits.is_empty());
//! # let _ = Arc::new(store);
//! ```
//!
//! # Concurrency
//!
//! `MemoryStore` is `Send + Sync`. Internally it wraps a
//! `Mutex<rusqlite::Connection>` (SQLite is serial anyway) and an
//! `RwLock<()>` rebuild barrier. Recall takes the read lock; writes and
//! consolidation take the write lock — concurrent recall therefore never
//! observes a half-rebuilt vector index.
//!
//! # Features
//!
//! - `default = ["fts5"]` — SQLite FTS5 backend.
//! - `hnsw` — usearch-backed approximate-nearest-neighbour vector index.
//! - `embeddings` — pull in [`open_memory_embed`] for ONNX vectors.
//! - `testing` — re-export the [`open_memory_core::testing`] doubles.

#![forbid(unsafe_code)]

pub mod consolidate;
pub mod error;
pub mod forget;
pub mod recall;
pub mod remember;
pub mod schema;
pub mod store;
pub mod types;

pub use consolidate::{ConsolidateConfig, ConsolidateReport};
pub use error::{MemoryError, MemoryResult};
pub use forget::{PruneReport, DEFAULT_TOMBSTONE_TTL_SECS};
pub use open_memory_index::SearchMode;
pub use recall::{
    RecallFilters, RecallResult, CORRECTION_RETRIEVAL_BOOST, RECALL_MIN_SCORE,
    SPREADING_DISTANCE_DECAY,
};
pub use remember::{ObservationInput, RelationInput, RememberOutcome};
pub use schema::MEMORY_SCHEMA_VERSION;
pub use store::{EntityListRow, MemoryStatus, MemoryStore, MEMORY_DB_FILE};
pub use types::{new_id, Entity, EntityType, MemoryTier, Observation, Relation};
