//! Hybrid (vector + keyword) search backend for `open-memory`.
//!
//! Open everything at once with [`open_engine`] — it returns a
//! [`CachedSearchEngine`] wrapping a [`HybridSearchEngine`] over the default
//! vector and full-text backends, plus a [`MetadataStore`] when the `sqlite`
//! feature is enabled (default).
//!
//! Backends, by feature:
//!
//! - [`FlatVectorIndex`] — brute-force cosine similarity (default)
//! - `HnswIndex` — approximate nearest-neighbour (feature `hnsw`)
//! - [`Fts5Store`] — SQLite FTS5 BM25 keyword search (feature `fts5`)
//! - [`Bm25Store`] — pure-Rust BM25 (when `fts5` is off)
//! - [`MetadataStore`] — SQLite metadata table (feature `sqlite`)
//! - [`HybridSearchEngine`] — RRF fusion of vector + keyword results
//! - [`CachedSearchEngine`] — LRU TTL cache over the hybrid engine

#![forbid(unsafe_code)]

#[cfg(not(feature = "fts5"))]
pub mod bm25;
pub mod cache;
pub mod engine;
pub mod error;
pub mod flat;
#[cfg(feature = "fts5")]
pub mod fts5;
pub mod hybrid;
#[cfg(feature = "sqlite")]
pub mod metadata;
pub mod traits;

#[cfg(not(feature = "fts5"))]
pub use bm25::Bm25Store;
pub use cache::{CachedSearchEngine, DEFAULT_CACHE_CAPACITY, DEFAULT_CACHE_TTL};
pub use engine::{flush, open_engine, OpenEngine, FULLTEXT_FILE, METADATA_FILE, VECTORS_FILE};
pub use error::{IndexError, IndexResult};
pub use flat::FlatVectorIndex;
#[cfg(feature = "fts5")]
pub use fts5::Fts5Store;
pub use hybrid::{HybridSearchEngine, DEFAULT_RRF_K};
#[cfg(feature = "sqlite")]
pub use metadata::{MetadataStats, MetadataStore, SourceKind, SourceRecord};
pub use traits::{
    ExportEntry, FullTextStore, IndexEntry, SearchMode, SearchResult, VectorIndex, VectorStore,
};

/// The default vector backend selected by feature flags. Currently always
/// [`FlatVectorIndex`] — when the `hnsw` feature lands, this becomes the
/// approximate index instead.
pub type DefaultVectorStore = FlatVectorIndex;

/// The default full-text backend. [`Fts5Store`] when the `fts5` feature is
/// enabled (default), [`Bm25Store`] otherwise.
#[cfg(feature = "fts5")]
pub type DefaultFullTextStore = Fts5Store;
#[cfg(not(feature = "fts5"))]
pub type DefaultFullTextStore = Bm25Store;
