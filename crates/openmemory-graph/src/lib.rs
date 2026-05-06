//! Persistent knowledge-graph memory for AI agents.
//!
//! The heart of the project: SQLite plus the [`openmemory_index`] hybrid
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
//! use openmemory_core::config::Config;
//! use openmemory_graph::{MemoryStore, EntityType, ObservationInput, RecallFilters};
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
//! filters.mode = Some(openmemory_index::SearchMode::KeywordOnly);
//! let hits = store.recall("Rust", 5, &filters).unwrap();
//! assert!(!hits.is_empty());
//! # let _ = Arc::new(store);
//! ```
//!
//! # Concurrency
//!
//! `MemoryStore` is `Send + Sync`. The internal layout is:
//!
//! - **One writer connection** behind a `Mutex<rusqlite::Connection>`.
//!   Every mutation (`remember`, `forget*`, `consolidate`, the
//!   post-recall `bump_access_counts`) serialises on this. SQLite is
//!   serial anyway; the mutex matches that contract.
//! - **A read-only connection pool** ([`ReadPool`], one of:
//!   `Config::num_jobs()` slots opened with
//!   `OPEN_READ_ONLY | OPEN_NO_MUTEX` for the on-disk store; a single
//!   slot proxying the writer mutex for the in-memory store). Every
//!   public read (`recall`, `get_entity*`, `list_entities`,
//!   `get_entity_observations`, `get_entity_relations`, `status`) runs
//!   through `with_reader`, so concurrent recall calls execute in
//!   parallel without serialising on the writer.
//! - **An `RwLock<()>` rebuild barrier**, separate from any SQLite
//!   handle. Recall takes the read lock around the *vector*-index
//!   search; remember / forget* / consolidate take the write lock
//!   while they sync the index. This prevents a recall from observing
//!   a half-rebuilt vector index. It is *not* the SQLite reader/writer
//!   barrier — that's WAL's job.
//!
//! WAL mode plus `OPEN_READ_ONLY | OPEN_NO_MUTEX` is what makes the
//! reader pool safe: SQLite arbitrates reader/writer interleaving via
//! WAL frames, never via per-connection locks. A `remember` mid-recall
//! does not block any reader, and committed writes are visible to the
//! next read transaction (each `with_reader` call starts a fresh
//! transaction). See `pool.rs` for the open-flags + pragma rationale.
//!
//! Across-process concurrency is also fine on the same on-disk
//! database: a second `openmemory mcp` process just opens its own
//! pool of handles; SQLite's WAL plus the configured `busy_timeout`
//! keeps the writer arbitration honest.
//!
//! # Features
//!
//! - `default = ["fts5"]` — SQLite FTS5 backend.
//! - `hnsw` — usearch-backed approximate-nearest-neighbour vector index.
//! - `embeddings` — pull in [`openmemory_embed`] for ONNX vectors.
//! - `testing` — re-export the [`openmemory_core::testing`] doubles.

#![forbid(unsafe_code)]

pub mod consolidate;
pub mod error;
pub mod forget;
pub mod pool;
pub mod recall;
pub mod remember;
pub mod schema;
pub mod store;
pub mod types;

pub use consolidate::{ConsolidateConfig, ConsolidateReport};
pub use error::{MemoryError, MemoryResult};
pub use forget::{PruneReport, DEFAULT_TOMBSTONE_TTL_SECS};
pub use openmemory_index::SearchMode;
pub use pool::ReadPool;
pub use recall::{
    RecallFilters, RecallResult, CORRECTION_RETRIEVAL_BOOST, RECALL_MIN_SCORE,
    SPREADING_DISTANCE_DECAY,
};
pub use remember::{ObservationInput, RelationInput, RememberOutcome};
pub use schema::MEMORY_SCHEMA_VERSION;
pub use store::{EntityListRow, MemoryStatus, MemoryStore, MEMORY_DB_FILE};
pub use types::{new_id, Entity, EntityType, MemoryTier, Observation, Relation};
