//! Content-addressed embedding cache backed by SQLite.
//!
//! Avoids re-embedding identical text by keying every entry on the
//! BLAKE3 hash of the input string. Persistent across restarts;
//! per-session hit/miss counters are kept in atomics for cheap
//! observability.

use crate::error::{EmbedError, EmbedResult};
use openmemory_core::error::OmError;
use openmemory_core::migrations::Migrator;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const SCHEMA_VERSION: u32 = 1;
const META_TABLE: &str = "embed_meta";
const MIGRATION_V1: &str = "
    CREATE TABLE cache (
        hash BLOB PRIMARY KEY,
        vector BLOB NOT NULL
    );
";

impl From<rusqlite::Error> for EmbedError {
    fn from(e: rusqlite::Error) -> Self {
        EmbedError::Cache(e.to_string())
    }
}

impl From<OmError> for EmbedError {
    fn from(e: OmError) -> Self {
        EmbedError::Cache(e.to_string())
    }
}

/// SQLite-backed embedding cache.
pub struct EmbeddingCache {
    conn: Mutex<Connection>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl EmbeddingCache {
    /// Open a persistent cache at `path`. Creates parent directories
    /// and applies schema migrations as needed.
    pub fn open(path: &Path) -> EmbedResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open an ephemeral in-memory cache. Useful for tests.
    pub fn in_memory() -> EmbedResult<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> EmbedResult<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )?;
        Migrator::new(&conn, META_TABLE).apply(SCHEMA_VERSION, &[(1, MIGRATION_V1)])?;

        Ok(Self {
            conn: Mutex::new(conn),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        })
    }

    /// Look up a cached embedding by content. Returns `None` on miss.
    pub fn get(&self, text: &str) -> Option<Vec<f32>> {
        let hash = blake3_hash(text);
        let conn = self.conn.lock().ok()?;
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT vector FROM cache WHERE hash = ?1",
                params![hash.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .ok()?;

        if let Some(b) = blob {
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(bytes_to_f32(&b))
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Look up many cache keys while holding the SQLite connection once.
    /// Results preserve input order. Cache failures degrade individual
    /// entries to misses because embedding remains the authoritative path.
    pub fn get_batch(&self, texts: &[&str]) -> Vec<Option<Vec<f32>>> {
        if texts.is_empty() {
            return Vec::new();
        }
        let Ok(conn) = self.conn.lock() else {
            self.misses.fetch_add(texts.len() as u64, Ordering::Relaxed);
            return vec![None; texts.len()];
        };
        let Ok(mut statement) = conn.prepare_cached("SELECT vector FROM cache WHERE hash = ?1")
        else {
            self.misses.fetch_add(texts.len() as u64, Ordering::Relaxed);
            return vec![None; texts.len()];
        };

        let mut hits = 0;
        let mut output = Vec::with_capacity(texts.len());
        for text in texts {
            let hash = blake3_hash(text);
            let vector = statement
                .query_row(params![hash.as_slice()], |row| row.get::<_, Vec<u8>>(0))
                .optional()
                .ok()
                .flatten()
                .map(|bytes| bytes_to_f32(&bytes));
            hits += u64::from(vector.is_some());
            output.push(vector);
        }
        self.hits.fetch_add(hits, Ordering::Relaxed);
        self.misses
            .fetch_add(texts.len() as u64 - hits, Ordering::Relaxed);
        output
    }

    /// Store an embedding. Overwrites any existing entry with the
    /// same content hash.
    pub fn put(&self, text: &str, vector: &[f32]) {
        let hash = blake3_hash(text);
        let blob = f32_to_bytes(vector);
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO cache (hash, vector) VALUES (?1, ?2)",
                params![hash.as_slice(), blob],
            );
        }
    }

    /// Store a batch of embeddings inside a single transaction.
    /// Re-uses one serialization buffer when every vector has the
    /// same length (the common case).
    pub fn put_batch(&self, entries: &[(&str, &[f32])]) {
        if entries.is_empty() {
            return;
        }
        let Ok(conn) = self.conn.lock() else {
            return;
        };

        let vector_len = entries[0].1.len();
        let uniform = entries.iter().all(|(_, v)| v.len() == vector_len);
        let mut buf = if uniform {
            vec![0u8; vector_len * 4]
        } else {
            Vec::new()
        };

        let _ = conn.execute_batch("BEGIN");
        for (text, vector) in entries {
            let hash = blake3_hash(text);
            let blob: &[u8] = if uniform {
                f32_write_le_bytes(vector, &mut buf);
                buf.as_slice()
            } else {
                buf.clear();
                buf.resize(vector.len() * 4, 0);
                f32_write_le_bytes(vector, &mut buf);
                buf.as_slice()
            };
            let _ = conn.execute(
                "INSERT OR REPLACE INTO cache (hash, vector) VALUES (?1, ?2)",
                params![hash.as_slice(), blob],
            );
        }
        let _ = conn.execute_batch("COMMIT");
    }

    /// Per-session (hits, misses).
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.conn
            .lock()
            .ok()
            .and_then(|conn| {
                conn.query_row("SELECT COUNT(*) FROM cache", [], |row| row.get::<_, i64>(0))
                    .ok()
            })
            .unwrap_or(0) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn blake3_hash(text: &str) -> [u8; 32] {
    *blake3::hash(text.as_bytes()).as_bytes()
}

fn f32_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; v.len() * 4];
    f32_write_le_bytes(v, &mut out);
    out
}

fn f32_write_le_bytes(v: &[f32], out: &mut [u8]) {
    for (i, &x) in v.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&x.to_le_bytes());
    }
}

fn bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn miss_returns_none_and_increments_miss_counter() {
        let cache = EmbeddingCache::in_memory().unwrap();
        assert!(cache.get("nope").is_none());
        assert_eq!(cache.stats(), (0, 1));
    }

    #[test]
    fn put_then_get_returns_exact_vector() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put("hello", &[1.0, 2.0, 3.0]);
        assert_eq!(cache.get("hello").unwrap(), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn hit_increments_hit_counter() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put("x", &[1.0]);
        cache.get("x");
        cache.get("x");
        assert_eq!(cache.stats(), (2, 0));
    }

    #[test]
    fn distinct_keys_are_independent() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put("alpha", &[1.0]);
        cache.put("beta", &[2.0]);
        assert_eq!(cache.get("alpha").unwrap(), vec![1.0]);
        assert_eq!(cache.get("beta").unwrap(), vec![2.0]);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn put_same_key_twice_keeps_latest_value() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put("key", &[1.0, 0.0]);
        cache.put("key", &[0.0, 1.0]);
        assert_eq!(cache.get("key").unwrap(), vec![0.0, 1.0]);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn batch_put_inserts_all_entries() {
        let cache = EmbeddingCache::in_memory().unwrap();
        let vecs: Vec<Vec<f32>> = (0..50).map(|i| vec![i as f32; 8]).collect();
        let entries: Vec<(&str, &[f32])> = (0..50)
            .map(|i| {
                let s: &'static str = Box::leak(format!("text_{i}").into_boxed_str());
                (s, vecs[i].as_slice())
            })
            .collect();
        cache.put_batch(&entries);
        assert_eq!(cache.len(), 50);
        assert_eq!(cache.get("text_42").unwrap(), vec![42.0; 8]);
    }

    #[test]
    fn batch_put_empty_is_noop() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put_batch(&[]);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn batch_get_preserves_order_duplicates_and_stats() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put("alpha", &[1.0, 2.0]);
        cache.put("beta", &[3.0, 4.0]);
        let values = cache.get_batch(&["beta", "missing", "alpha", "beta"]);
        assert_eq!(values[0], Some(vec![3.0, 4.0]));
        assert_eq!(values[1], None);
        assert_eq!(values[2], Some(vec![1.0, 2.0]));
        assert_eq!(values[3], Some(vec![3.0, 4.0]));
        assert_eq!(cache.stats(), (3, 1));
    }

    #[test]
    fn batch_put_handles_mixed_dimensions() {
        let cache = EmbeddingCache::in_memory().unwrap();
        let v_a = vec![1.0, 2.0];
        let v_b = vec![3.0, 4.0, 5.0, 6.0];
        cache.put_batch(&[("a", v_a.as_slice()), ("b", v_b.as_slice())]);
        assert_eq!(cache.get("a").unwrap(), vec![1.0, 2.0]);
        assert_eq!(cache.get("b").unwrap(), vec![3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn empty_string_is_a_valid_key() {
        let cache = EmbeddingCache::in_memory().unwrap();
        cache.put("", &[9.0]);
        assert_eq!(cache.get("").unwrap(), vec![9.0]);
    }

    #[test]
    fn high_dim_vector_roundtrips_exactly() {
        let cache = EmbeddingCache::in_memory().unwrap();
        let v: Vec<f32> = (0..768).map(|i| (i as f32) * 0.001 - 0.384).collect();
        cache.put("doc", &v);
        assert_eq!(cache.get("doc").unwrap(), v);
    }

    #[test]
    fn special_float_values_preserved() {
        let cache = EmbeddingCache::in_memory().unwrap();
        let special = vec![0.0f32, -0.0, f32::MIN, f32::MAX, f32::EPSILON, 1e-38];
        cache.put("special", &special);
        let got = cache.get("special").unwrap();
        for (a, b) in got.iter().zip(special.iter()) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn cache_survives_close_and_reopen() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cache.db");

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
    fn stats_reset_per_session_but_data_persists() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cache.db");

        {
            let cache = EmbeddingCache::open(&path).unwrap();
            cache.put("x", &[1.0]);
            cache.get("x");
            assert_eq!(cache.stats(), (1, 0));
        }
        {
            let cache = EmbeddingCache::open(&path).unwrap();
            assert_eq!(cache.stats(), (0, 0));
            assert_eq!(cache.len(), 1);
        }
    }

    #[test]
    fn hit_ratio_after_warm_pass() {
        let cache = EmbeddingCache::in_memory().unwrap();
        let texts = ["alpha", "beta", "gamma", "delta"];
        let vectors: Vec<Vec<f32>> = texts.iter().map(|t| vec![t.len() as f32; 4]).collect();

        for (t, v) in texts.iter().zip(vectors.iter()) {
            cache.put(t, v);
        }
        for t in &texts {
            assert!(cache.get(t).is_some());
        }
        for t in &texts {
            assert!(cache.get(t).is_some());
        }

        let (hits, misses) = cache.stats();
        assert_eq!(misses, 0);
        assert_eq!(hits, (texts.len() * 2) as u64);
    }

    #[test]
    fn f32_bytes_roundtrip() {
        let original = vec![1.0f32, -2.5, 0.0, f32::MAX, f32::MIN_POSITIVE];
        let bytes = f32_to_bytes(&original);
        assert_eq!(bytes.len(), original.len() * 4);
        let decoded = bytes_to_f32(&bytes);
        assert_eq!(decoded, original);
    }

    #[test]
    fn empty_vector_roundtrips() {
        let bytes = f32_to_bytes(&[]);
        assert!(bytes.is_empty());
        let decoded = bytes_to_f32(&bytes);
        assert!(decoded.is_empty());
    }

    #[test]
    fn refusing_to_open_db_with_future_schema() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("future.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE embed_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO embed_meta (key, value) VALUES ('schema_version', '99');",
            )
            .unwrap();
        }

        let Err(err) = EmbeddingCache::open(&path) else {
            panic!("expected open to refuse a future-version database");
        };
        assert!(matches!(err, EmbedError::Cache(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("99"),
            "error did not mention version 99: {msg}"
        );
    }
}
