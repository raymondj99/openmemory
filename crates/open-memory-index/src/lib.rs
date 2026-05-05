//! Hybrid (vector + keyword) search backend for `open-memory`.
//!
//! Provides the storage backends that the knowledge graph and the
//! free-text URI index share:
//!
//! - [`FlatVectorIndex`] — brute-force cosine similarity (default)
//! - `HnswIndex` — approximate nearest-neighbour (feature `hnsw`)
//! - [`Fts5Store`] — SQLite FTS5 BM25 keyword search (feature `fts5`)
//! - [`Bm25Store`] — pure-Rust BM25 (when `fts5` is off)
//! - [`MetadataStore`] — SQLite metadata table (feature `sqlite`)
//! - `HybridSearchEngine` — RRF fusion of vector + keyword results
//!
//! Implementation lands in subsequent commits — see the project
//! roadmap under `docs/03-roadmap.md`.

#![forbid(unsafe_code)]

#[cfg(not(feature = "fts5"))]
pub mod bm25;
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
