//! JSON-file-backed embedding cache.
//!
//! Used when the `sqlite` feature is disabled. The on-disk format is
//! a single JSON object mapping hex-encoded BLAKE3 hashes to vectors;
//! the whole thing is read into memory at `open` and rewritten on
//! every `put` / `put_batch`. Suitable for tests and tiny caches —
//! prefer [`crate::cache::EmbeddingCache`] for anything serious.

use crate::error::{EmbedError, EmbedResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Default, Serialize, Deserialize)]
struct CacheData {
    entries: HashMap<String, Vec<f32>>,
}

struct Inner {
    data: CacheData,
    path: Option<PathBuf>,
}

impl Inner {
    fn flush(&self) {
        if let Some(p) = &self.path {
            if let Ok(json) = serde_json::to_string(&self.data) {
                let _ = std::fs::write(p, json);
            }
        }
    }
}

pub struct EmbeddingCache {
    inner: Mutex<Inner>,
    hits: Mutex<u64>,
    misses: Mutex<u64>,
}

impl EmbeddingCache {
    pub fn open(path: &Path) -> EmbedResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let data = if path.exists() {
            let json = std::fs::read_to_string(path)?;
            serde_json::from_str(&json)
                .map_err(|e| EmbedError::Cache(format!("failed to parse cache JSON: {e}")))?
        } else {
            CacheData::default()
        };

        Ok(Self {
            inner: Mutex::new(Inner {
                data,
                path: Some(path.to_path_buf()),
            }),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
        })
    }

    pub fn in_memory() -> EmbedResult<Self> {
        Ok(Self {
            inner: Mutex::new(Inner {
                data: CacheData::default(),
                path: None,
            }),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
        })
    }

    pub fn get(&self, text: &str) -> Option<Vec<f32>> {
        let key = hex_hash(text);
        let inner = self.inner.lock().ok()?;
        if let Some(v) = inner.data.entries.get(&key) {
            if let Ok(mut h) = self.hits.lock() {
                *h += 1;
            }
            Some(v.clone())
        } else {
            if let Ok(mut m) = self.misses.lock() {
                *m += 1;
            }
            None
        }
    }

    pub fn get_batch(&self, texts: &[&str]) -> Vec<Option<Vec<f32>>> {
        let Ok(inner) = self.inner.lock() else {
            if let Ok(mut misses) = self.misses.lock() {
                *misses += texts.len() as u64;
            }
            return vec![None; texts.len()];
        };
        let mut hit_count = 0;
        let output: Vec<_> = texts
            .iter()
            .map(|text| {
                let value = inner.data.entries.get(&hex_hash(text)).cloned();
                hit_count += u64::from(value.is_some());
                value
            })
            .collect();
        drop(inner);
        if let Ok(mut hits) = self.hits.lock() {
            *hits += hit_count;
        }
        if let Ok(mut misses) = self.misses.lock() {
            *misses += texts.len() as u64 - hit_count;
        }
        output
    }

    pub fn put(&self, text: &str, vector: &[f32]) {
        let key = hex_hash(text);
        if let Ok(mut inner) = self.inner.lock() {
            inner.data.entries.insert(key, vector.to_vec());
            inner.flush();
        }
    }

    pub fn put_batch(&self, entries: &[(&str, &[f32])]) {
        if entries.is_empty() {
            return;
        }
        if let Ok(mut inner) = self.inner.lock() {
            for (text, vector) in entries {
                let key = hex_hash(text);
                inner.data.entries.insert(key, vector.to_vec());
            }
            inner.flush();
        }
    }

    pub fn stats(&self) -> (u64, u64) {
        let h = self.hits.lock().map(|h| *h).unwrap_or(0);
        let m = self.misses.lock().map(|m| *m).unwrap_or(0);
        (h, m)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|i| i.data.entries.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn hex_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn miss_returns_none() {
        let cache = EmbeddingCache::in_memory().unwrap();
        assert!(cache.get("nope").is_none());
        assert_eq!(cache.stats(), (0, 1));
    }

    #[test]
    fn put_then_get() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put("hello", &[1.0, 2.0, 3.0]);
        assert_eq!(cache.get("hello").unwrap(), vec![1.0, 2.0, 3.0]);
        assert_eq!(cache.stats(), (1, 0));
    }

    #[test]
    fn batch_put() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put_batch(&[("a", &[1.0]), ("b", &[2.0])]);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("a").unwrap(), vec![1.0]);
        assert_eq!(cache.get("b").unwrap(), vec![2.0]);
    }

    #[test]
    fn batch_get_preserves_order_and_stats() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put_batch(&[("a", &[1.0]), ("b", &[2.0])]);
        assert_eq!(
            cache.get_batch(&["b", "missing", "a"]),
            vec![Some(vec![2.0]), None, Some(vec![1.0])]
        );
        assert_eq!(cache.stats(), (2, 1));
    }

    #[test]
    fn overwrite_keeps_latest_value() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put("key", &[1.0]);
        cache.put("key", &[2.0]);
        assert_eq!(cache.get("key").unwrap(), vec![2.0]);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn persistence() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cache.json");

        {
            let cache = EmbeddingCache::open(&path).unwrap();
            cache.put("persistent", &[3.125, 2.875]);
            assert_eq!(cache.len(), 1);
        }
        {
            let cache = EmbeddingCache::open(&path).unwrap();
            assert_eq!(cache.len(), 1);
            assert_eq!(cache.get("persistent").unwrap(), vec![3.125, 2.875]);
        }
    }

    #[test]
    fn high_dim_vector_roundtrips() {
        let cache = EmbeddingCache::in_memory().unwrap();
        let v: Vec<f32> = (0..768).map(|i| (i as f32) * 0.001 - 0.384).collect();
        cache.put("doc", &v);
        assert_eq!(cache.get("doc").unwrap(), v);
    }

    #[test]
    fn hit_ratio_after_warm_pass() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put("x", &[1.0]);
        for _ in 0..3 {
            cache.get("x");
        }
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 3);
        assert_eq!(misses, 0);
    }

    #[test]
    fn corrupt_json_reports_cache_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"not json").unwrap();

        let Err(err) = EmbeddingCache::open(&path) else {
            panic!("expected open to fail");
        };
        assert!(matches!(err, EmbedError::Cache(_)));
    }
}
