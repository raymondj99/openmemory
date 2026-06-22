//! Brute-force flat vector index. O(n) cosine similarity scan.
//!
//! Sufficient for graph-sized datasets (<10⁶ vectors). For larger working
//! sets, enable the `hnsw` feature for the approximate-nearest-neighbour
//! backend.
//!
//! # On-disk format
//!
//! ```text
//! magic       4 bytes  "OMV1"
//! count       u64 LE   number of entries
//! dimensions  u32 LE   vector length (0 if no entries)
//! entries     repeated:
//!   uri_len   u32 LE
//!   uri       bytes
//!   text_len  u32 LE
//!   text      bytes
//!   chunk_idx u32 LE
//!   vector    f32 × dimensions, LE
//! ```
//!
//! Loading rejects unknown magic bytes and short reads.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::error::{IndexError, IndexResult};
use crate::traits::{ExportEntry, IndexEntry, SearchResult, VectorIndex, VectorStore};

const MAGIC: &[u8; 4] = b"OMV1";

#[derive(Debug)]
struct StoredEntry {
    uri: String,
    text: String,
    chunk_index: u32,
    vector: Vec<f32>,
}

/// In-memory flat vector index using brute-force cosine similarity.
#[derive(Debug)]
pub struct FlatVectorIndex {
    entries: Mutex<Vec<StoredEntry>>,
    /// Set by mutations, cleared by [`VectorIndex::save`]. Only ever
    /// touched while `entries` is locked, so `Relaxed` ordering is
    /// sufficient — the mutex provides the happens-before edges.
    dirty: AtomicBool,
}

impl FlatVectorIndex {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            dirty: AtomicBool::new(false),
        }
    }

    /// Load an index from a binary file.
    pub fn load(path: &Path) -> IndexResult<Self> {
        let data = std::fs::read(path)?;
        let mut cursor = &data[..];

        let mut magic = [0u8; 4];
        read_exact(&mut cursor, &mut magic, path, "magic")?;
        if &magic != MAGIC {
            return Err(IndexError::Corrupt {
                path: path.to_path_buf(),
                detail: format!("bad magic: {magic:?}"),
            });
        }

        let count = read_u64(&mut cursor, path, "count")? as usize;
        let dim = read_u32(&mut cursor, path, "dim")? as usize;
        if dim == 0 {
            // Older keyword-only runs persisted empty vectors. Treat that
            // legacy vector file as an empty vector index; the keyword store
            // remains authoritative for those rows.
            return Ok(Self::new());
        }

        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let uri = read_string(&mut cursor, path, &format!("uri[{i}]"))?;
            let text = read_string(&mut cursor, path, &format!("text[{i}]"))?;
            let chunk_index = read_u32(&mut cursor, path, &format!("chunk_index[{i}]"))?;

            let mut vector = Vec::with_capacity(dim);
            for j in 0..dim {
                let mut buf = [0u8; 4];
                read_exact(&mut cursor, &mut buf, path, &format!("vector[{i}][{j}]"))?;
                vector.push(f32::from_le_bytes(buf));
            }

            entries.push(StoredEntry {
                uri,
                text,
                chunk_index,
                vector,
            });
        }

        Ok(Self {
            entries: Mutex::new(entries),
            dirty: AtomicBool::new(false),
        })
    }

    /// Open the index from `path` if it exists, otherwise return an empty one.
    pub fn open(path: &Path) -> IndexResult<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::new())
        }
    }

    fn lock(&self) -> IndexResult<std::sync::MutexGuard<'_, Vec<StoredEntry>>> {
        self.entries
            .lock()
            .map_err(|e| IndexError::Lock(e.to_string()))
    }
}

impl Default for FlatVectorIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorStore for FlatVectorIndex {
    fn insert(&self, entries: &[IndexEntry]) -> IndexResult<()> {
        let mut guard = self.lock()?;
        let entries: Vec<&IndexEntry> = entries
            .iter()
            .filter(|entry| !entry.vector.is_empty())
            .collect();
        if entries.is_empty() {
            return Ok(());
        }

        if let Some(first) = entries.first() {
            let new_dim = first.vector.len();
            if let Some(existing) = guard.first() {
                if existing.vector.len() != new_dim {
                    return Err(IndexError::DimensionMismatch {
                        expected: existing.vector.len(),
                        actual: new_dim,
                    });
                }
            }
            for entry in &entries[1..] {
                if entry.vector.len() != new_dim {
                    return Err(IndexError::DimensionMismatch {
                        expected: new_dim,
                        actual: entry.vector.len(),
                    });
                }
            }
        }

        for entry in entries {
            guard.push(StoredEntry {
                uri: entry.uri.clone(),
                text: entry.text.clone(),
                chunk_index: entry.chunk_index,
                vector: entry.vector.clone(),
            });
        }
        self.dirty.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn search(&self, query_vector: &[f32], top_k: usize) -> IndexResult<Vec<SearchResult>> {
        let guard = self.lock()?;

        let mut scored: Vec<(f32, &StoredEntry)> = guard
            .iter()
            .map(|e| (cosine_similarity(query_vector, &e.vector), e))
            .collect();

        if scored.len() > top_k && top_k > 0 {
            scored.select_nth_unstable_by(top_k - 1, |a, b| {
                b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(top_k);
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(scored
            .into_iter()
            .map(|(score, e)| SearchResult {
                uri: e.uri.clone(),
                text: e.text.clone(),
                chunk_index: e.chunk_index,
                score,
            })
            .collect())
    }

    fn delete_by_uri(&self, uri: &str) -> IndexResult<u64> {
        let mut guard = self.lock()?;
        let before = guard.len();
        guard.retain(|e| e.uri != uri);
        let removed = (before - guard.len()) as u64;
        if removed > 0 {
            self.dirty.store(true, Ordering::Relaxed);
        }
        Ok(removed)
    }

    fn count(&self) -> IndexResult<u64> {
        Ok(self.lock()?.len() as u64)
    }
}

impl VectorIndex for FlatVectorIndex {
    fn save(&self, path: &Path) -> IndexResult<()> {
        let guard = self.lock()?;
        let dim = guard.first().map_or(0, |e| e.vector.len()) as u32;

        let mut buf: Vec<u8> = Vec::new();
        buf.write_all(MAGIC)?;
        buf.write_all(&(guard.len() as u64).to_le_bytes())?;
        buf.write_all(&dim.to_le_bytes())?;

        for entry in guard.iter() {
            let uri = entry.uri.as_bytes();
            buf.write_all(&(uri.len() as u32).to_le_bytes())?;
            buf.write_all(uri)?;

            let text = entry.text.as_bytes();
            buf.write_all(&(text.len() as u32).to_le_bytes())?;
            buf.write_all(text)?;

            buf.write_all(&entry.chunk_index.to_le_bytes())?;

            for &v in &entry.vector {
                buf.write_all(&v.to_le_bytes())?;
            }
        }

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        atomic_write(path, &buf)?;
        // `guard` is still held: no mutation can interleave between the
        // write above and clearing the flag.
        self.dirty.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    fn export_all(&self) -> IndexResult<Vec<ExportEntry>> {
        let guard = self.lock()?;
        Ok(guard
            .iter()
            .map(|e| ExportEntry {
                uri: e.uri.clone(),
                text: e.text.clone(),
                chunk_index: e.chunk_index,
                vector: e.vector.clone(),
            })
            .collect())
    }
}

fn read_exact(cursor: &mut &[u8], buf: &mut [u8], path: &Path, label: &str) -> IndexResult<()> {
    cursor.read_exact(buf).map_err(|e| IndexError::Corrupt {
        path: path.to_path_buf(),
        detail: format!("read {label}: {e}"),
    })
}

fn read_u32(cursor: &mut &[u8], path: &Path, label: &str) -> IndexResult<u32> {
    let mut buf = [0u8; 4];
    read_exact(cursor, &mut buf, path, label)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(cursor: &mut &[u8], path: &Path, label: &str) -> IndexResult<u64> {
    let mut buf = [0u8; 8];
    read_exact(cursor, &mut buf, path, label)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_string(cursor: &mut &[u8], path: &Path, label: &str) -> IndexResult<String> {
    let len = read_u32(cursor, path, label)? as usize;
    let mut bytes = vec![0u8; len];
    read_exact(cursor, &mut bytes, path, label)?;
    String::from_utf8(bytes).map_err(|e| IndexError::Corrupt {
        path: path.to_path_buf(),
        detail: format!("invalid UTF-8 in {label}: {e}"),
    })
}

/// Atomically write `bytes` to `path` by writing to a sibling temp file
/// first and renaming. Avoids partial-write corruption on crash.
fn atomic_write(path: &Path, bytes: &[u8]) -> IndexResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| IndexError::InvalidInput(format!("path has no file name: {path:?}")))?;
    let tmp: PathBuf = parent.join(format!(".{}.tmp", file_name.to_string_lossy()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    let chunks_a = a.chunks_exact(8);
    let chunks_b = b.chunks_exact(8);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();
    for (ca, cb) in chunks_a.zip(chunks_b) {
        for i in 0..8 {
            let ai = ca[i];
            let bi = cb[i];
            dot += ai * bi;
            na += ai * ai;
            nb += bi * bi;
        }
    }
    for (ai, bi) in rem_a.iter().zip(rem_b.iter()) {
        dot += ai * bi;
        na += ai * ai;
        nb += bi * bi;
    }
    let denom = (na * nb).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(uri: &str, text: &str, chunk_index: u32, vector: Vec<f32>) -> IndexEntry {
        IndexEntry::new(uri, text)
            .with_chunk_index(chunk_index)
            .with_vector(vector)
    }

    #[test]
    fn insert_and_count() {
        let store = FlatVectorIndex::new();
        store
            .insert(&[entry("u://a", "hello", 0, vec![1.0, 0.0])])
            .unwrap();
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn search_returns_highest_first() {
        let store = FlatVectorIndex::new();
        store
            .insert(&[
                entry("u://a", "hello", 0, vec![1.0, 0.0, 0.0]),
                entry("u://b", "world", 0, vec![0.0, 1.0, 0.0]),
            ])
            .unwrap();
        let results = store.search(&[1.0, 0.0, 0.0], 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].uri, "u://a");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn search_top_k_truncates() {
        let store = FlatVectorIndex::new();
        let v: Vec<IndexEntry> = (0..10u32)
            .map(|i| {
                let mut vec = vec![0.0; 3];
                vec[(i as usize) % 3] = 1.0;
                entry(&format!("u://{i}"), "t", i, vec)
            })
            .collect();
        store.insert(&v).unwrap();
        let r = store.search(&[1.0, 0.0, 0.0], 3).unwrap();
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn search_top_k_zero_returns_all() {
        let store = FlatVectorIndex::new();
        store.insert(&[entry("u", "t", 0, vec![1.0, 0.0])]).unwrap();
        let r = store.search(&[1.0, 0.0], 0).unwrap();
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn delete_by_uri_removes_all_chunks() {
        let store = FlatVectorIndex::new();
        store
            .insert(&[
                entry("u://a", "1", 0, vec![1.0, 0.0]),
                entry("u://a", "2", 1, vec![0.0, 1.0]),
                entry("u://b", "3", 0, vec![1.0, 1.0]),
            ])
            .unwrap();
        let removed = store.delete_by_uri("u://a").unwrap();
        assert_eq!(removed, 2);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn delete_unknown_uri_is_zero() {
        let store = FlatVectorIndex::new();
        store
            .insert(&[entry("u://a", "x", 0, vec![1.0, 0.0])])
            .unwrap();
        assert_eq!(store.delete_by_uri("u://nope").unwrap(), 0);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn empty_search_returns_empty() {
        let store = FlatVectorIndex::new();
        let r = store.search(&[1.0, 0.0], 5).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn empty_vectors_are_not_indexed() {
        let store = FlatVectorIndex::new();
        store.insert(&[entry("u://a", "x", 0, vec![])]).unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn dimension_mismatch_on_second_insert() {
        let store = FlatVectorIndex::new();
        store
            .insert(&[entry("u://a", "x", 0, vec![1.0, 0.0])])
            .unwrap();
        let err = store
            .insert(&[entry("u://b", "x", 0, vec![1.0, 0.0, 0.0])])
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
        let store = FlatVectorIndex::new();
        let err = store
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
    fn dirty_tracks_mutations_and_clears_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.bin");

        let store = FlatVectorIndex::new();
        assert!(!store.is_dirty(), "fresh index is clean");

        store
            .insert(&[entry("u://a", "x", 0, vec![1.0, 0.0])])
            .unwrap();
        assert!(store.is_dirty(), "insert dirties");

        store.save(&path).unwrap();
        assert!(!store.is_dirty(), "save cleans");

        // No-op mutations stay clean.
        store.insert(&[entry("u://b", "y", 0, vec![])]).unwrap();
        assert!(!store.is_dirty(), "empty-vector insert is a no-op");
        assert_eq!(store.delete_by_uri("u://nope").unwrap(), 0);
        assert!(!store.is_dirty(), "no-op delete stays clean");

        store.delete_by_uri("u://a").unwrap();
        assert!(store.is_dirty(), "delete dirties");
    }

    #[test]
    fn load_starts_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.bin");
        let store = FlatVectorIndex::new();
        store
            .insert(&[entry("u://a", "x", 0, vec![1.0, 0.0])])
            .unwrap();
        store.save(&path).unwrap();

        let loaded = FlatVectorIndex::load(&path).unwrap();
        assert!(!loaded.is_dirty(), "freshly loaded index matches disk");
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.bin");

        let store = FlatVectorIndex::new();
        store
            .insert(&[
                entry("u://a", "hello", 0, vec![1.0, 2.0, 3.0]),
                entry("u://b", "world!", 7, vec![0.5, -1.0, 2.5]),
            ])
            .unwrap();
        store.save(&path).unwrap();

        let loaded = FlatVectorIndex::load(&path).unwrap();
        assert_eq!(loaded.count().unwrap(), 2);

        let exported = loaded.export_all().unwrap();
        let by_uri: std::collections::HashMap<_, _> =
            exported.iter().map(|e| (e.uri.as_str(), e)).collect();
        let a = by_uri["u://a"];
        assert_eq!(a.text, "hello");
        assert_eq!(a.chunk_index, 0);
        assert_eq!(a.vector, vec![1.0, 2.0, 3.0]);
        let b = by_uri["u://b"];
        assert_eq!(b.text, "world!");
        assert_eq!(b.chunk_index, 7);
        assert_eq!(b.vector, vec![0.5, -1.0, 2.5]);
    }

    #[test]
    fn save_empty_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.bin");
        FlatVectorIndex::new().save(&path).unwrap();
        let loaded = FlatVectorIndex::load(&path).unwrap();
        assert_eq!(loaded.count().unwrap(), 0);
    }

    #[test]
    fn open_creates_empty_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FlatVectorIndex::open(&dir.path().join("missing.bin")).unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn load_rejects_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.bin");
        std::fs::write(&path, b"BADMAGIC0000000000000").unwrap();
        let err = FlatVectorIndex::load(&path).unwrap_err();
        match err {
            IndexError::Corrupt { detail, .. } => {
                assert!(detail.contains("bad magic"));
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn load_rejects_truncated_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trunc.bin");
        std::fs::write(&path, b"OMV1").unwrap();
        assert!(FlatVectorIndex::load(&path).is_err());
    }

    #[test]
    fn export_all_after_delete() {
        let store = FlatVectorIndex::new();
        store
            .insert(&[
                entry("u://a", "1", 0, vec![1.0]),
                entry("u://b", "2", 0, vec![0.5]),
            ])
            .unwrap();
        store.delete_by_uri("u://a").unwrap();
        let exported = store.export_all().unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].uri, "u://b");
    }

    #[test]
    fn cosine_basic() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_mismatched_lengths() {
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
        }
    }

    #[test]
    fn cosine_empty() {
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(cosine_similarity(&[], &[]), 0.0);
        }
    }

    #[test]
    fn cosine_zero_vectors() {
        let z = vec![0.0f32; 8];
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(cosine_similarity(&z, &z), 0.0);
        }
    }

    #[test]
    fn cosine_768d_smoke() {
        let a: Vec<f32> = (0..768).map(|i| (i as f32).sin()).collect();
        let b: Vec<f32> = (0..768).map(|i| (i as f32).cos()).collect();
        let s = cosine_similarity(&a, &b);
        assert!(s > -1.0 && s < 1.0);
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn vec_strategy(dim: usize) -> impl Strategy<Value = Vec<f32>> {
            prop::collection::vec(-10.0f32..10.0f32, dim)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn cosine_symmetric(a in vec_strategy(64), b in vec_strategy(64)) {
                let ab = cosine_similarity(&a, &b);
                let ba = cosine_similarity(&b, &a);
                prop_assert!((ab - ba).abs() < 1e-5);
            }

            #[test]
            fn cosine_bounded(a in vec_strategy(64), b in vec_strategy(64)) {
                let s = cosine_similarity(&a, &b);
                prop_assert!((-1.0 - 1e-5..=1.0 + 1e-5).contains(&s));
            }

            #[test]
            fn cosine_self_is_one(a in vec_strategy(64)) {
                if a.iter().any(|&x| x != 0.0) {
                    let s = cosine_similarity(&a, &a);
                    prop_assert!((s - 1.0).abs() < 1e-4);
                }
            }

            #[test]
            fn cosine_negation_is_minus_one(a in vec_strategy(64)) {
                if a.iter().any(|&x| x != 0.0) {
                    let neg: Vec<f32> = a.iter().map(|x| -x).collect();
                    let s = cosine_similarity(&a, &neg);
                    prop_assert!((s + 1.0).abs() < 1e-4);
                }
            }

            #[test]
            fn round_trip_save_load(
                n in 0usize..32,
                dim in 1usize..32,
            ) {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("v.bin");

                let store = FlatVectorIndex::new();
                let entries: Vec<IndexEntry> = (0..n)
                    .map(|i| {
                        let v: Vec<f32> = (0..dim).map(|j| (i * dim + j) as f32).collect();
                        IndexEntry::new(format!("u://{i}"), format!("t{i}"))
                            .with_vector(v)
                            .with_chunk_index(i as u32)
                    })
                    .collect();
                store.insert(&entries).unwrap();
                store.save(&path).unwrap();

                let loaded = FlatVectorIndex::load(&path).unwrap();
                prop_assert_eq!(loaded.count().unwrap(), n as u64);
                let exported = loaded.export_all().unwrap();
                prop_assert_eq!(exported.len(), n);
            }
        }
    }
}
