//! Runtime-adaptive vector backend.
//!
//! Exact flat search wins on small personal-memory corpora; HNSW wins once
//! the scan becomes cache- and CPU-heavy. This backend starts flat and
//! migrates once, preserving low-latency small-corpus behavior without
//! sacrificing large-corpus scale.

use std::path::Path;
use std::sync::RwLock;

use crate::error::{IndexError, IndexResult};
use crate::flat::FlatVectorIndex;
use crate::hnsw::HnswIndex;
use crate::traits::{ExportEntry, IndexEntry, SearchResult, VectorIndex, VectorStore};

/// Measured crossover is between 1K and 10K 256-dimensional vectors on
/// Apple Silicon. Migrating at 4K keeps the exact backend through the range
/// where it is clearly faster while bounding linear-scan growth.
pub const DEFAULT_HNSW_MIGRATION_THRESHOLD: usize = 4_096;

enum Backend {
    Flat(FlatVectorIndex),
    Hnsw(HnswIndex),
}

/// Flat-to-HNSW vector index that migrates at a measured corpus threshold.
pub struct AdaptiveVectorIndex {
    backend: RwLock<Backend>,
    migration_threshold: usize,
}

impl AdaptiveVectorIndex {
    /// Create an empty adaptive index.
    #[must_use]
    pub fn new() -> Self {
        Self::with_threshold(DEFAULT_HNSW_MIGRATION_THRESHOLD)
    }

    fn with_threshold(migration_threshold: usize) -> Self {
        Self {
            backend: RwLock::new(Backend::Flat(FlatVectorIndex::new())),
            migration_threshold: migration_threshold.max(1),
        }
    }

    /// Load the persisted backend, or open a flat index and migrate it when
    /// an older corpus already exceeds the crossover threshold.
    pub fn load_or_create(dir: &Path) -> IndexResult<Self> {
        let backend = if HnswIndex::has_persisted_files(dir) {
            Backend::Hnsw(HnswIndex::load_or_create(dir)?)
        } else {
            let flat = FlatVectorIndex::open(&dir.join("vectors.bin"))?;
            if flat.count()? as usize >= DEFAULT_HNSW_MIGRATION_THRESHOLD {
                Backend::Hnsw(HnswIndex::migrate_from_flat(&flat)?)
            } else {
                Backend::Flat(flat)
            }
        };
        Ok(Self {
            backend: RwLock::new(backend),
            migration_threshold: DEFAULT_HNSW_MIGRATION_THRESHOLD,
        })
    }

    /// Whether this instance has crossed over to HNSW.
    #[must_use]
    pub fn is_hnsw(&self) -> bool {
        matches!(
            &*self
                .backend
                .read()
                .unwrap_or_else(|error| error.into_inner()),
            Backend::Hnsw(_)
        )
    }

    fn read(&self) -> IndexResult<std::sync::RwLockReadGuard<'_, Backend>> {
        self.backend
            .read()
            .map_err(|error| IndexError::Lock(error.to_string()))
    }

    fn write(&self) -> IndexResult<std::sync::RwLockWriteGuard<'_, Backend>> {
        self.backend
            .write()
            .map_err(|error| IndexError::Lock(error.to_string()))
    }
}

impl Default for AdaptiveVectorIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorStore for AdaptiveVectorIndex {
    fn insert(&self, entries: &[IndexEntry]) -> IndexResult<()> {
        let mut backend = self.write()?;
        match &mut *backend {
            Backend::Hnsw(index) => index.insert(entries),
            Backend::Flat(index) => {
                index.insert(entries)?;
                if index.count()? as usize >= self.migration_threshold {
                    match HnswIndex::migrate_from_flat(index) {
                        Ok(hnsw) => *backend = Backend::Hnsw(hnsw),
                        Err(error) => tracing::warn!(
                            error = %error,
                            "adaptive vector migration failed; retaining exact flat index"
                        ),
                    }
                }
                Ok(())
            }
        }
    }

    fn search(&self, query_vector: &[f32], top_k: usize) -> IndexResult<Vec<SearchResult>> {
        match &*self.read()? {
            Backend::Flat(index) => index.search(query_vector, top_k),
            Backend::Hnsw(index) => index.search(query_vector, top_k),
        }
    }

    fn delete_by_uri(&self, uri: &str) -> IndexResult<u64> {
        match &mut *self.write()? {
            Backend::Flat(index) => index.delete_by_uri(uri),
            Backend::Hnsw(index) => index.delete_by_uri(uri),
        }
    }

    fn count(&self) -> IndexResult<u64> {
        match &*self.read()? {
            Backend::Flat(index) => index.count(),
            Backend::Hnsw(index) => index.count(),
        }
    }
}

impl VectorIndex for AdaptiveVectorIndex {
    fn save(&self, path: &Path) -> IndexResult<()> {
        match &*self.read()? {
            Backend::Flat(index) => index.save(path),
            Backend::Hnsw(index) => index.save(path),
        }
    }

    fn is_dirty(&self) -> bool {
        match self.backend.read() {
            Ok(backend) => match &*backend {
                Backend::Flat(index) => index.is_dirty(),
                Backend::Hnsw(index) => index.is_dirty(),
            },
            Err(_) => true,
        }
    }

    fn export_all(&self) -> IndexResult<Vec<ExportEntry>> {
        match &*self.read()? {
            Backend::Flat(index) => index.export_all(),
            Backend::Hnsw(index) => index.export_all(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(index: usize) -> IndexEntry {
        let mut vector = vec![0.0; 8];
        vector[index % 8] = 1.0;
        IndexEntry::new(format!("u://{index}"), "payload").with_vector(vector)
    }

    #[test]
    fn stays_flat_below_threshold_and_migrates_without_losing_entries() {
        let index = AdaptiveVectorIndex::with_threshold(8);
        index
            .insert(&(0..7).map(entry).collect::<Vec<_>>())
            .unwrap();
        assert!(!index.is_hnsw());
        index.insert(&[entry(7)]).unwrap();
        assert!(index.is_hnsw());
        assert_eq!(index.count().unwrap(), 8);
        let results = index.search(&entry(0).vector, 1).unwrap();
        assert_eq!(results[0].uri, "u://0");
    }

    #[test]
    fn small_index_persists_and_reopens_as_flat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.bin");
        let index = AdaptiveVectorIndex::with_threshold(8);
        index.insert(&[entry(0), entry(1)]).unwrap();
        index.save(&path).unwrap();

        let reopened = AdaptiveVectorIndex::load_or_create(dir.path()).unwrap();
        assert!(!reopened.is_hnsw());
        assert_eq!(reopened.count().unwrap(), 2);
    }
}
