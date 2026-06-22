//! Single-call factory for the hybrid search engine + metadata store.
//!
//! Both `openmemory-graph` and `openmemory-mcp` need the same "open or
//! create the index" ceremony. Centralising the cfg-gate ladder here
//! means a new backend or a renamed on-disk file changes one place.
//!
//! On-disk layout under `data_dir`:
//!
//! ```text
//! <data_dir>/
//!   metadata.sqlite
//!   fulltext.sqlite       (or bm25.json with --no-default-features)
//!   vectors.bin
//! ```

use std::path::{Path, PathBuf};

use openmemory_core::config::Config;

use crate::cache::CachedSearchEngine;
use crate::error::IndexResult;
#[cfg(not(feature = "hnsw"))]
use crate::flat::FlatVectorIndex;
#[cfg(feature = "hnsw")]
use crate::hnsw::HnswIndex;
use crate::hybrid::HybridSearchEngine;
#[cfg(feature = "sqlite")]
use crate::metadata::MetadataStore;

#[cfg(not(feature = "fts5"))]
use crate::bm25::Bm25Store;
#[cfg(feature = "fts5")]
use crate::fts5::Fts5Store;

#[cfg(feature = "fts5")]
type DefaultFts = Fts5Store;
#[cfg(not(feature = "fts5"))]
type DefaultFts = Bm25Store;

#[cfg(feature = "hnsw")]
type DefaultVec = HnswIndex;
#[cfg(not(feature = "hnsw"))]
type DefaultVec = FlatVectorIndex;

/// File names under `data_dir`.
pub const METADATA_FILE: &str = "metadata.sqlite";
pub const VECTORS_FILE: &str = "vectors.bin";
#[cfg(feature = "fts5")]
pub const FULLTEXT_FILE: &str = "fulltext.sqlite";
#[cfg(not(feature = "fts5"))]
pub const FULLTEXT_FILE: &str = "bm25.json";

/// Bundle of stores returned by [`open_engine`]. Keep them together so
/// callers don't have to thread three independent handles.
pub struct OpenEngine {
    pub engine: CachedSearchEngine<DefaultVec, DefaultFts>,
    #[cfg(feature = "sqlite")]
    pub metadata: MetadataStore,
    pub data_dir: PathBuf,
}

/// Open or create every store that lives under `data_dir`. Wires them into a
/// `HybridSearchEngine` and an LRU cache; reads `alpha` and the cache TTL
/// (when present) from `config`.
pub fn open_engine(config: &Config, data_dir: &Path) -> IndexResult<OpenEngine> {
    if !data_dir.as_os_str().is_empty() {
        std::fs::create_dir_all(data_dir)?;
    }

    #[cfg(feature = "hnsw")]
    let vector_store = HnswIndex::load_or_create(data_dir)?;
    #[cfg(not(feature = "hnsw"))]
    let vector_store = FlatVectorIndex::open(&data_dir.join(VECTORS_FILE))?;

    #[cfg(feature = "fts5")]
    let fulltext_store = Fts5Store::open_with_field_weights(
        &data_dir.join(FULLTEXT_FILE),
        config.search.field_weights.as_array(),
    )?;
    #[cfg(not(feature = "fts5"))]
    let fulltext_store = Bm25Store::open(&data_dir.join(FULLTEXT_FILE))?;

    #[cfg(feature = "sqlite")]
    let metadata = MetadataStore::open(&data_dir.join(METADATA_FILE))?;

    let hybrid = HybridSearchEngine::with_rrf_k(
        vector_store,
        fulltext_store,
        config.search.hybrid_alpha,
        config.search.rrf_k as f32,
    );
    let cached = CachedSearchEngine::new(hybrid);

    Ok(OpenEngine {
        engine: cached,
        #[cfg(feature = "sqlite")]
        metadata,
        data_dir: data_dir.to_path_buf(),
    })
}

/// Persist the current vector index (and BM25 store, when used) to disk.
/// FTS5 / SQLite stores commit on every mutation and need no flushing.
///
/// The vector save is O(corpus) plus an fsync, so it is skipped when the
/// index is unchanged since the last save
/// ([`is_dirty`](crate::traits::VectorIndex::is_dirty)) — every
/// keyword-only write hits that fast path, and a vector-bearing write
/// pays it at most once per flush rather than once per call.
pub fn flush(open: &OpenEngine) -> IndexResult<()> {
    use crate::traits::VectorIndex;
    let vectors = open.engine.inner().vector_store();
    if vectors.is_dirty() {
        vectors.save(&open.data_dir.join(VECTORS_FILE))?;
    }
    #[cfg(not(feature = "fts5"))]
    {
        use crate::traits::FullTextStore;
        open.engine.inner().fulltext_store().flush()?;
    }
    #[cfg(feature = "sqlite")]
    open.metadata.checkpoint()?;
    Ok(())
}

/// Run a PASSIVE WAL checkpoint on the engine's SQLite-backed stores
/// (fulltext.sqlite, metadata.sqlite). Companion to [`flush`] for
/// callers that disable SQLite's auto-checkpoint and drive
/// checkpointing on a background cadence instead of paying it inside
/// commits.
pub fn checkpoint(open: &OpenEngine) -> IndexResult<()> {
    #[cfg(feature = "fts5")]
    open.engine.inner().fulltext_store().checkpoint()?;
    #[cfg(feature = "sqlite")]
    open.metadata.checkpoint()?;
    #[cfg(not(any(feature = "fts5", feature = "sqlite")))]
    let _ = open;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::traits::{IndexEntry, SearchMode};

    fn entry(uri: &str, text: &str, vec: Vec<f32>) -> IndexEntry {
        IndexEntry::new(uri, text).with_vector(vec)
    }

    #[test]
    fn flush_skips_clean_vector_index() {
        use crate::traits::VectorIndex;

        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::default();
        let opened = open_engine(&cfg, dir.path()).unwrap();

        // Vector-bearing insert dirties; flush saves and cleans.
        opened
            .engine
            .insert(&[entry("u://a", "hello", vec![1.0, 0.0])])
            .unwrap();
        // The persisted file name depends on the vector backend: the
        // default flat index writes VECTORS_FILE; the hnsw backend writes
        // its multi-file layout next to it.
        #[cfg(feature = "hnsw")]
        let vector_file = dir.path().join("vectors.usearch");
        #[cfg(not(feature = "hnsw"))]
        let vector_file = dir.path().join(VECTORS_FILE);

        assert!(opened.engine.inner().vector_store().is_dirty());
        flush(&opened).unwrap();
        assert!(!opened.engine.inner().vector_store().is_dirty());
        let saved_at = std::fs::metadata(&vector_file).unwrap().modified().unwrap();

        // Keyword-only insert (no vector) leaves the index clean, and the
        // next flush must not rewrite the file.
        opened
            .engine
            .insert(&[entry("u://b", "keyword only", vec![])])
            .unwrap();
        assert!(!opened.engine.inner().vector_store().is_dirty());
        flush(&opened).unwrap();
        let after = std::fs::metadata(&vector_file).unwrap().modified().unwrap();
        assert_eq!(saved_at, after, "clean flush must not rewrite the index");
    }

    #[test]
    fn open_creates_data_dir_and_recovers_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("nested/profile");
        let cfg = Config::default();

        // First open: empty.
        {
            let opened = open_engine(&cfg, &data_dir).unwrap();
            assert_eq!(opened.engine.count().unwrap(), 0);

            opened
                .engine
                .insert(&[
                    entry("u://a", "hello world", vec![1.0, 0.0, 0.0]),
                    entry("u://b", "rust programming", vec![0.0, 1.0, 0.0]),
                ])
                .unwrap();

            let r = opened
                .engine
                .search(&[1.0, 0.0, 0.0], "hello", 10, SearchMode::Hybrid, 0)
                .unwrap();
            assert_eq!(r[0].uri, "u://a");

            flush(&opened).unwrap();
        }

        // Second open: persisted data is back.
        {
            let opened = open_engine(&cfg, &data_dir).unwrap();
            assert_eq!(opened.engine.count().unwrap(), 2);
            let r = opened
                .engine
                .search(&[0.0, 1.0, 0.0], "rust", 10, SearchMode::Hybrid, 0)
                .unwrap();
            assert_eq!(r[0].uri, "u://b");
        }
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn metadata_store_is_initialised() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("p");
        let cfg = Config::default();
        let opened = open_engine(&cfg, &data_dir).unwrap();
        assert_eq!(
            opened.metadata.schema_version().unwrap(),
            crate::metadata::METADATA_SCHEMA_VERSION
        );
        // Side-effect: file exists on disk.
        assert!(data_dir.join(METADATA_FILE).exists());
    }

    #[test]
    fn alpha_passes_through_from_config() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.search.hybrid_alpha = 0.3;
        let opened = open_engine(&cfg, dir.path()).unwrap();
        assert!((opened.engine.inner().alpha() - 0.3).abs() < f32::EPSILON);
    }
}
