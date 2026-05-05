//! Hybrid (vector + keyword) search backend for `open-memory`.
//!
//! Provides the storage backends that the knowledge graph and the
//! free-text URI index share:
//!
//! - `FlatVectorIndex` — brute-force cosine similarity (default)
//! - `HnswIndex` — approximate nearest-neighbour (feature `hnsw`)
//! - `Fts5Store` — SQLite FTS5 BM25 keyword search (default)
//! - `Bm25Store` — pure-Rust BM25 (when `fts5` is off)
//! - `MetadataStore` — SQLite metadata table
//! - `HybridSearchEngine` — RRF fusion of vector + keyword results
//!
//! Implementation lands in subsequent commits — see the project
//! roadmap under `docs/03-roadmap.md`.

#![forbid(unsafe_code)]

pub mod error;
pub mod flat;
pub mod traits;

pub use error::{IndexError, IndexResult};
pub use flat::FlatVectorIndex;
pub use traits::{
    ExportEntry, FullTextStore, IndexEntry, SearchMode, SearchResult, VectorIndex, VectorStore,
};
