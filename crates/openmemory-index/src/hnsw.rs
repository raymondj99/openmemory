//! Approximate-nearest-neighbour vector index backed by usearch HNSW.
//!
//! Compiled only when the `hnsw` feature is enabled — usearch pulls in a C++
//! build dependency (CMake + a recent compiler), so it stays opt-in.
//!
//! O(log n) search instead of [`FlatVectorIndex`]'s O(n). The crossover
//! depends on the workload, but at >100K vectors the HNSW path is reliably
//! faster.
//!
//! On-disk layout under an index directory:
//!
//! ```text
//! <dir>/vectors.usearch              # usearch HNSW graph
//! <dir>/vectors.usearch.meta.json    # u64 label -> entry metadata
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::error::{IndexError, IndexResult};
use crate::flat::FlatVectorIndex;
use crate::traits::{ExportEntry, IndexEntry, SearchResult, VectorIndex, VectorStore};

const HNSW_FILE: &str = "vectors.usearch";
const META_FILE: &str = "vectors.usearch.meta.json";
const FLAT_BIN_FILE: &str = "vectors.bin";

const DEFAULT_M: usize = 16;
const DEFAULT_EF_CONSTRUCTION: usize = 128;
const DEFAULT_EF_SEARCH: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
struct EntryMeta {
    uri: String,
    text: String,
    chunk_index: u32,
}

struct Inner {
    index: Index,
    next_label: u64,
    /// label -> metadata
    meta: HashMap<u64, EntryMeta>,
    /// uri -> labels (for O(1) delete-by-uri)
    by_uri: HashMap<String, Vec<u64>>,
    dimensions: usize,
}

/// Approximate-nearest-neighbour vector index backed by usearch.
pub struct HnswIndex {
    inner: RwLock<Inner>,
    /// Set by mutations, cleared by [`VectorIndex::save`]. Only ever
    /// touched while `inner` is locked, so `Relaxed` ordering is
    /// sufficient — the read/write lock provides the happens-before edges.
    dirty: std::sync::atomic::AtomicBool,
}

impl HnswIndex {
    /// Whether either file in the persisted HNSW pair exists.
    pub(crate) fn has_persisted_files(dir: &Path) -> bool {
        dir.join(HNSW_FILE).exists() || dir.join(META_FILE).exists()
    }

    /// Create an empty index. Dimensionality is set on the first insert.
    pub fn new() -> Self {
        let opts = options(0);
        let index = Index::new(&opts).expect("usearch: empty index always constructible");
        Self {
            inner: RwLock::new(Inner {
                index,
                next_label: 0,
                meta: HashMap::new(),
                by_uri: HashMap::new(),
                dimensions: 0,
            }),
            dirty: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn with_dimensions(dim: usize) -> IndexResult<Self> {
        let opts = options(dim);
        let index =
            Index::new(&opts).map_err(|e| IndexError::InvalidInput(format!("usearch new: {e}")))?;
        Ok(Self {
            inner: RwLock::new(Inner {
                index,
                next_label: 0,
                meta: HashMap::new(),
                by_uri: HashMap::new(),
                dimensions: dim,
            }),
            dirty: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Open or create an HNSW index in `dir`. Migrates from a sibling
    /// `vectors.bin` if present.
    pub fn load_or_create(dir: &Path) -> IndexResult<Self> {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
        let hnsw_path = dir.join(HNSW_FILE);
        let meta_path = dir.join(META_FILE);
        if hnsw_path.exists() != meta_path.exists() {
            return Err(IndexError::Corrupt {
                path: dir.to_path_buf(),
                detail: "incomplete HNSW persistence pair".into(),
            });
        }
        if hnsw_path.exists() && meta_path.exists() {
            return Self::load_from(dir);
        }
        let flat_path = dir.join(FLAT_BIN_FILE);
        if flat_path.exists() {
            tracing::info!("migrating flat vector index -> HNSW");
            let flat = FlatVectorIndex::load(&flat_path)?;
            let hnsw = Self::migrate_from_flat(&flat)?;
            hnsw.save_to(dir)?;
            return Ok(hnsw);
        }
        Ok(Self::new())
    }

    /// Build an `HnswIndex` from every entry in a `FlatVectorIndex`.
    pub fn migrate_from_flat(flat: &FlatVectorIndex) -> IndexResult<Self> {
        let entries = flat.export_all()?;
        if entries.is_empty() {
            return Ok(Self::new());
        }
        let dim = entries[0].vector.len();
        let hnsw = Self::with_dimensions(dim)?;
        let pending: Vec<IndexEntry> = entries.into_iter().map(IndexEntry::from).collect();
        hnsw.insert(&pending)?;
        Ok(hnsw)
    }

    fn load_from(dir: &Path) -> IndexResult<Self> {
        let hnsw_path = dir.join(HNSW_FILE);
        let meta_path = dir.join(META_FILE);

        let meta_bytes = std::fs::read(&meta_path)?;
        let meta: HashMap<u64, EntryMeta> =
            serde_json::from_slice(&meta_bytes).map_err(|e| IndexError::Corrupt {
                path: meta_path.clone(),
                detail: format!("hnsw meta: {e}"),
            })?;
        let next_label = meta.keys().copied().max().map_or(0, |k| k + 1);

        let mut by_uri: HashMap<String, Vec<u64>> = HashMap::new();
        for (&label, entry) in &meta {
            by_uri.entry(entry.uri.clone()).or_default().push(label);
        }

        let opts = options(0);
        let index =
            Index::new(&opts).map_err(|e| IndexError::InvalidInput(format!("usearch new: {e}")))?;
        let path_str = hnsw_path
            .to_str()
            .ok_or_else(|| IndexError::InvalidInput("non-UTF-8 path".into()))?;
        index.load(path_str).map_err(|e| IndexError::Corrupt {
            path: hnsw_path.clone(),
            detail: format!("usearch load: {e}"),
        })?;
        let dimensions = index.dimensions();

        Ok(Self {
            inner: RwLock::new(Inner {
                index,
                next_label,
                meta,
                by_uri,
                dimensions,
            }),
            dirty: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn save_to(&self, dir: &Path) -> IndexResult<()> {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
        let inner = self.write()?;
        let hnsw_path = dir.join(HNSW_FILE);
        let path_str = hnsw_path
            .to_str()
            .ok_or_else(|| IndexError::InvalidInput("non-UTF-8 path".into()))?;
        inner
            .index
            .save(path_str)
            .map_err(|e| IndexError::InvalidInput(format!("usearch save: {e}")))?;
        let meta_bytes = serde_json::to_vec(&inner.meta)
            .map_err(|e| IndexError::InvalidInput(format!("serialize meta: {e}")))?;
        std::fs::write(dir.join(META_FILE), meta_bytes)?;
        // `inner` is still locked: no mutation can interleave between the
        // writes above and clearing the flag.
        self.dirty
            .store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn read(&self) -> IndexResult<std::sync::RwLockReadGuard<'_, Inner>> {
        self.inner
            .read()
            .map_err(|e| IndexError::Lock(e.to_string()))
    }

    fn write(&self) -> IndexResult<std::sync::RwLockWriteGuard<'_, Inner>> {
        self.inner
            .write()
            .map_err(|e| IndexError::Lock(e.to_string()))
    }

    /// Ensure the inner index is sized for `dim`. On first insert, recreate
    /// with the right dimensionality; on subsequent inserts, error on a
    /// mismatch.
    fn ensure_dimensions(inner: &mut Inner, dim: usize) -> IndexResult<()> {
        if inner.dimensions == 0 {
            let opts = options(dim);
            let index = Index::new(&opts)
                .map_err(|e| IndexError::InvalidInput(format!("usearch new: {e}")))?;
            inner.index = index;
            inner.dimensions = dim;
            Ok(())
        } else if inner.dimensions == dim {
            Ok(())
        } else {
            Err(IndexError::DimensionMismatch {
                expected: inner.dimensions,
                actual: dim,
            })
        }
    }
}

impl Default for HnswIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorStore for HnswIndex {
    fn insert(&self, entries: &[IndexEntry]) -> IndexResult<()> {
        let entries: Vec<&IndexEntry> = entries
            .iter()
            .filter(|entry| !entry.vector.is_empty())
            .collect();
        if entries.is_empty() {
            return Ok(());
        }
        let mut inner = self.write()?;
        let dim = entries[0].vector.len();
        Self::ensure_dimensions(&mut inner, dim)?;
        // Verify uniform dim within batch
        for e in &entries[1..] {
            if e.vector.len() != dim {
                return Err(IndexError::DimensionMismatch {
                    expected: dim,
                    actual: e.vector.len(),
                });
            }
        }

        let new_total = inner.index.size() + entries.len();
        inner
            .index
            .reserve(new_total)
            .map_err(|e| IndexError::InvalidInput(format!("usearch reserve: {e}")))?;

        for entry in entries {
            let label = inner.next_label;
            inner.next_label += 1;
            inner
                .index
                .add(label, &entry.vector)
                .map_err(|e| IndexError::InvalidInput(format!("usearch add: {e}")))?;
            inner
                .by_uri
                .entry(entry.uri.clone())
                .or_default()
                .push(label);
            inner.meta.insert(
                label,
                EntryMeta {
                    uri: entry.uri.clone(),
                    text: entry.text.clone(),
                    chunk_index: entry.chunk_index,
                },
            );
        }
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn search(&self, query_vector: &[f32], top_k: usize) -> IndexResult<Vec<SearchResult>> {
        let inner = self.read()?;
        if query_vector.is_empty() || inner.meta.is_empty() || inner.dimensions == 0 {
            return Ok(Vec::new());
        }
        let matches = inner
            .index
            .search(query_vector, top_k)
            .map_err(|e| IndexError::InvalidInput(format!("usearch search: {e}")))?;
        let mut out = Vec::with_capacity(matches.keys.len());
        for (k, d) in matches.keys.iter().zip(matches.distances.iter()) {
            if let Some(meta) = inner.meta.get(k) {
                // usearch cosine distance = 1 - cosine_similarity.
                let score = 1.0 - d;
                out.push(SearchResult {
                    uri: meta.uri.clone(),
                    text: meta.text.clone(),
                    chunk_index: meta.chunk_index,
                    score,
                });
            }
        }
        Ok(out)
    }

    fn delete_by_uri(&self, uri: &str) -> IndexResult<u64> {
        let mut inner = self.write()?;
        let labels = match inner.by_uri.remove(uri) {
            Some(v) => v,
            None => return Ok(0),
        };
        let count = labels.len() as u64;
        for label in labels {
            let _ = inner.index.remove(label);
            inner.meta.remove(&label);
        }
        if count > 0 {
            self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(count)
    }

    fn count(&self) -> IndexResult<u64> {
        Ok(self.read()?.meta.len() as u64)
    }
}

impl VectorIndex for HnswIndex {
    fn save(&self, path: &Path) -> IndexResult<()> {
        // `path` points to a file (e.g. vectors.bin) for trait parity with
        // FlatVectorIndex. We save into the file's parent directory using
        // the multi-file HNSW layout.
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        self.save_to(dir)
    }

    fn is_dirty(&self) -> bool {
        self.dirty.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn export_all(&self) -> IndexResult<Vec<ExportEntry>> {
        let inner = self.read()?;
        let dim = inner.dimensions;
        let mut out = Vec::with_capacity(inner.meta.len());
        for (&label, meta) in &inner.meta {
            let mut vector = vec![0.0f32; dim];
            let _ = inner.index.get(label, &mut vector);
            out.push(ExportEntry {
                uri: meta.uri.clone(),
                text: meta.text.clone(),
                chunk_index: meta.chunk_index,
                vector,
            });
        }
        Ok(out)
    }
}

fn options(dim: usize) -> IndexOptions {
    IndexOptions {
        dimensions: dim,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        connectivity: DEFAULT_M,
        expansion_add: DEFAULT_EF_CONSTRUCTION,
        expansion_search: DEFAULT_EF_SEARCH,
        multi: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(uri: &str, text: &str, chunk: u32, vec: Vec<f32>) -> IndexEntry {
        IndexEntry::new(uri, text)
            .with_chunk_index(chunk)
            .with_vector(vec)
    }

    #[test]
    fn insert_and_count() {
        let h = HnswIndex::new();
        h.insert(&[entry("u://a", "hello", 0, vec![1.0, 0.0, 0.0])])
            .unwrap();
        assert_eq!(h.count().unwrap(), 1);
    }

    #[test]
    fn empty_search_returns_empty() {
        let h = HnswIndex::new();
        let r = h.search(&[1.0, 0.0, 0.0], 5).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn search_returns_nearest() {
        let h = HnswIndex::new();
        h.insert(&[
            entry("u://a", "hello", 0, vec![1.0, 0.0, 0.0]),
            entry("u://b", "world", 0, vec![0.0, 1.0, 0.0]),
        ])
        .unwrap();
        let r = h.search(&[1.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(r[0].uri, "u://a");
        assert!(r[0].score >= r[1].score);
    }

    #[test]
    fn concurrent_searches_are_safe_and_stable() {
        let h = std::sync::Arc::new(HnswIndex::new());
        let entries: Vec<_> = (0..256)
            .map(|i| {
                let mut vector = vec![0.0; 16];
                let slot = i % vector.len();
                vector[slot] = 1.0;
                entry(&format!("u://{i}"), "payload", i as u32, vector)
            })
            .collect();
        h.insert(&entries).unwrap();

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let h = std::sync::Arc::clone(&h);
                std::thread::spawn(move || {
                    let mut query = vec![0.0; 16];
                    query[0] = 1.0;
                    for _ in 0..100 {
                        let results = h.search(&query, 10).unwrap();
                        assert_eq!(results.len(), 10);
                        assert!(results[0].score >= results[9].score);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[test]
    fn delete_by_uri_removes_all_chunks() {
        let h = HnswIndex::new();
        h.insert(&[
            entry("u://a", "1", 0, vec![1.0, 0.0]),
            entry("u://a", "2", 1, vec![0.0, 1.0]),
            entry("u://b", "3", 0, vec![1.0, 1.0]),
        ])
        .unwrap();
        let removed = h.delete_by_uri("u://a").unwrap();
        assert_eq!(removed, 2);
        assert_eq!(h.count().unwrap(), 1);
    }

    #[test]
    fn delete_unknown_uri_is_zero() {
        let h = HnswIndex::new();
        h.insert(&[entry("u://a", "x", 0, vec![1.0, 0.0])]).unwrap();
        assert_eq!(h.delete_by_uri("u://nope").unwrap(), 0);
    }

    #[test]
    fn dimension_mismatch_after_first_insert() {
        let h = HnswIndex::new();
        h.insert(&[entry("u://a", "x", 0, vec![1.0, 0.0])]).unwrap();
        let err = h
            .insert(&[entry("u://b", "y", 0, vec![1.0, 0.0, 0.0])])
            .unwrap_err();
        assert!(matches!(
            err,
            IndexError::DimensionMismatch {
                expected: 2,
                actual: 3
            }
        ));
    }

    #[test]
    fn dimension_mismatch_within_batch() {
        let h = HnswIndex::new();
        let err = h
            .insert(&[
                entry("u://a", "x", 0, vec![1.0, 0.0]),
                entry("u://b", "y", 0, vec![1.0, 0.0, 0.0]),
            ])
            .unwrap_err();
        assert!(matches!(
            err,
            IndexError::DimensionMismatch {
                expected: 2,
                actual: 3
            }
        ));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let h = HnswIndex::new();
        h.insert(&[
            entry("u://a", "hello", 0, vec![1.0, 2.0, 3.0]),
            entry("u://b", "world", 7, vec![0.5, -1.0, 2.5]),
        ])
        .unwrap();
        h.save_to(dir.path()).unwrap();
        assert!(dir.path().join(HNSW_FILE).exists());
        assert!(dir.path().join(META_FILE).exists());

        let loaded = HnswIndex::load_or_create(dir.path()).unwrap();
        assert_eq!(loaded.count().unwrap(), 2);
        let r = loaded.search(&[1.0, 2.0, 3.0], 1).unwrap();
        assert_eq!(r[0].uri, "u://a");
    }

    #[test]
    fn vector_index_save_uses_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.bin");
        let h = HnswIndex::new();
        h.insert(&[entry("u://a", "x", 0, vec![1.0, 0.0])]).unwrap();
        VectorIndex::save(&h, &path).unwrap();
        // Save lands the multi-file layout in the *parent* of `path`.
        assert!(dir.path().join(HNSW_FILE).exists());
        assert!(dir.path().join(META_FILE).exists());
    }

    #[test]
    fn migrate_from_flat() {
        let dir = tempfile::tempdir().unwrap();
        let flat = FlatVectorIndex::new();
        flat.insert(&[entry("u://a", "migrated", 0, vec![1.0, 2.0, 3.0])])
            .unwrap();
        flat.save(&dir.path().join(FLAT_BIN_FILE)).unwrap();

        let h = HnswIndex::load_or_create(dir.path()).unwrap();
        assert_eq!(h.count().unwrap(), 1);
        let r = h.search(&[1.0, 2.0, 3.0], 1).unwrap();
        assert_eq!(r[0].uri, "u://a");
        assert_eq!(r[0].text, "migrated");
        // After migration, HNSW files exist alongside the flat file.
        assert!(dir.path().join(HNSW_FILE).exists());
        assert!(dir.path().join(META_FILE).exists());
    }

    #[test]
    fn migrate_from_empty_flat() {
        let flat = FlatVectorIndex::new();
        let h = HnswIndex::migrate_from_flat(&flat).unwrap();
        assert_eq!(h.count().unwrap(), 0);
    }

    #[test]
    fn export_all_round_trips_metadata() {
        let h = HnswIndex::new();
        h.insert(&[entry("u://x", "the text", 5, vec![0.1, 0.2, 0.3])])
            .unwrap();
        let exported = h.export_all().unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].uri, "u://x");
        assert_eq!(exported[0].text, "the text");
        assert_eq!(exported[0].chunk_index, 5);
        assert_eq!(exported[0].vector.len(), 3);
    }

    #[test]
    fn corrupt_meta_rejected() {
        let dir = tempfile::tempdir().unwrap();
        // Plant an HNSW file (empty path) plus invalid meta JSON.
        let h = HnswIndex::new();
        h.insert(&[entry("u://a", "x", 0, vec![1.0, 0.0])]).unwrap();
        h.save_to(dir.path()).unwrap();
        std::fs::write(dir.path().join(META_FILE), b"not json").unwrap();
        match HnswIndex::load_or_create(dir.path()) {
            Err(IndexError::Corrupt { .. }) => {}
            Err(other) => panic!("expected Corrupt, got {other:?}"),
            Ok(_) => panic!("expected Corrupt error, got Ok"),
        }
    }

    #[test]
    fn batch_insert_then_search() {
        let h = HnswIndex::new();
        let chunks: Vec<IndexEntry> = (0..50u32)
            .map(|i| {
                let mut v = vec![0.0f32; 3];
                v[(i as usize) % 3] = 1.0;
                entry(&format!("u://{i}"), "x", 0, v)
            })
            .collect();
        h.insert(&chunks).unwrap();
        assert_eq!(h.count().unwrap(), 50);
        let r = h.search(&[1.0, 0.0, 0.0], 5).unwrap();
        assert_eq!(r.len(), 5);
    }
}
