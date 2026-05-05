//! SQLite-backed metadata store.
//!
//! Tracks every URI we have indexed: hash, size, type, chunk count, status,
//! timestamps, and a `kind` discriminator separating graph observations from
//! `index_text` rows. Uses `core::Migrator` for schema versioning.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use open_memory_core::migrations::Migrator;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::{IndexError, IndexResult};

pub const METADATA_SCHEMA_VERSION: u32 = 1;

/// Discriminator for which subsystem owns a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Backed by an `open_memory_remember` graph observation.
    Observation,
    /// Backed by an `open_memory_index_text` row.
    IndexText,
}

impl SourceKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Observation => "observation",
            SourceKind::IndexText => "index_text",
        }
    }

    pub fn parse(s: &str) -> IndexResult<Self> {
        match s {
            "observation" => Ok(SourceKind::Observation),
            "index_text" => Ok(SourceKind::IndexText),
            other => Err(IndexError::InvalidInput(format!(
                "unknown source kind: {other}"
            ))),
        }
    }
}

/// One row of the `sources` table.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRecord {
    pub uri: String,
    pub kind: SourceKind,
    pub content_hash: Vec<u8>,
    pub size_bytes: u64,
    pub source_type: String,
    pub chunk_count: u32,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Aggregate counts over `sources`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataStats {
    pub total_sources: u64,
    pub total_chunks: u64,
    pub by_kind: HashMap<String, u64>,
}

/// SQLite-backed metadata store. Single connection guarded by a [`Mutex`]
/// (rusqlite's connection is `Send` but not `Sync`).
#[derive(Debug)]
pub struct MetadataStore {
    conn: Mutex<Connection>,
}

impl MetadataStore {
    /// Open or create a metadata database at `path`.
    pub fn open(path: &Path) -> IndexResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory metadata store (for tests).
    pub fn open_in_memory() -> IndexResult<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn configure(conn: &Connection) -> IndexResult<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;
             PRAGMA cache_size=-8000;",
        )?;
        Ok(())
    }

    fn migrate(conn: &Connection) -> IndexResult<()> {
        let m = Migrator::new(conn, "index_meta");
        m.apply(
            METADATA_SCHEMA_VERSION,
            &[(
                1,
                "CREATE TABLE IF NOT EXISTS sources (
                    uri          TEXT PRIMARY KEY,
                    kind         TEXT NOT NULL,
                    content_hash BLOB NOT NULL,
                    size_bytes   INTEGER NOT NULL DEFAULT 0,
                    source_type  TEXT NOT NULL DEFAULT '',
                    chunk_count  INTEGER NOT NULL DEFAULT 0,
                    status       TEXT NOT NULL DEFAULT 'indexed',
                    created_at   INTEGER NOT NULL,
                    updated_at   INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_sources_kind ON sources(kind);
                CREATE INDEX IF NOT EXISTS idx_sources_updated_at ON sources(updated_at);",
            )],
        )?;
        Ok(())
    }

    fn lock(&self) -> IndexResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| IndexError::Lock(e.to_string()))
    }

    /// Run a WAL checkpoint, flushing the WAL into the main database file.
    /// Best-effort: errors are swallowed because checkpoints are advisory.
    pub fn checkpoint(&self) -> IndexResult<()> {
        let conn = self.lock()?;
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    }

    /// Insert or replace a `sources` row at the given timestamp.
    pub fn upsert(&self, record: &SourceRecord) -> IndexResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sources (
                uri, kind, content_hash, size_bytes, source_type,
                chunk_count, status, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(uri) DO UPDATE SET
                kind         = excluded.kind,
                content_hash = excluded.content_hash,
                size_bytes   = excluded.size_bytes,
                source_type  = excluded.source_type,
                chunk_count  = excluded.chunk_count,
                status       = excluded.status,
                updated_at   = excluded.updated_at",
            params![
                record.uri,
                record.kind.as_str(),
                record.content_hash,
                record.size_bytes as i64,
                record.source_type,
                i64::from(record.chunk_count),
                record.status,
                record.created_at,
                record.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Insert/replace a batch of records in a single transaction.
    pub fn upsert_batch(&self, records: &[SourceRecord]) -> IndexResult<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO sources (
                    uri, kind, content_hash, size_bytes, source_type,
                    chunk_count, status, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(uri) DO UPDATE SET
                    kind         = excluded.kind,
                    content_hash = excluded.content_hash,
                    size_bytes   = excluded.size_bytes,
                    source_type  = excluded.source_type,
                    chunk_count  = excluded.chunk_count,
                    status       = excluded.status,
                    updated_at   = excluded.updated_at",
            )?;
            for r in records {
                stmt.execute(params![
                    r.uri,
                    r.kind.as_str(),
                    r.content_hash,
                    r.size_bytes as i64,
                    r.source_type,
                    i64::from(r.chunk_count),
                    r.status,
                    r.created_at,
                    r.updated_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Look up a single record by URI.
    pub fn get(&self, uri: &str) -> IndexResult<Option<SourceRecord>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT uri, kind, content_hash, size_bytes, source_type,
                    chunk_count, status, created_at, updated_at
             FROM sources WHERE uri = ?1",
        )?;
        let mut rows = stmt.query(params![uri])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_record(row)?))
        } else {
            Ok(None)
        }
    }

    /// Delete a single row. Returns true if it existed.
    pub fn delete(&self, uri: &str) -> IndexResult<bool> {
        let conn = self.lock()?;
        let n = conn.execute("DELETE FROM sources WHERE uri = ?1", params![uri])?;
        Ok(n > 0)
    }

    /// List every record, optionally filtered by `kind`. Sorted by URI.
    pub fn list(&self, kind: Option<SourceKind>) -> IndexResult<Vec<SourceRecord>> {
        let conn = self.lock()?;
        let mut stmt;
        let mut rows = if let Some(k) = kind {
            stmt = conn.prepare(
                "SELECT uri, kind, content_hash, size_bytes, source_type,
                        chunk_count, status, created_at, updated_at
                 FROM sources WHERE kind = ?1 ORDER BY uri",
            )?;
            stmt.query(params![k.as_str()])?
        } else {
            stmt = conn.prepare(
                "SELECT uri, kind, content_hash, size_bytes, source_type,
                        chunk_count, status, created_at, updated_at
                 FROM sources ORDER BY uri",
            )?;
            stmt.query([])?
        };
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_record(row)?);
        }
        Ok(out)
    }

    /// Aggregate counts over the table.
    pub fn stats(&self) -> IndexResult<MetadataStats> {
        let conn = self.lock()?;
        let total_sources: i64 =
            conn.query_row("SELECT COUNT(*) FROM sources", [], |row| row.get(0))?;
        let total_chunks: i64 = conn.query_row(
            "SELECT COALESCE(SUM(chunk_count), 0) FROM sources",
            [],
            |row| row.get(0),
        )?;

        let mut by_kind: HashMap<String, u64> = HashMap::new();
        let mut stmt = conn.prepare("SELECT kind, COUNT(*) FROM sources GROUP BY kind")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let kind: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            by_kind.insert(kind, count as u64);
        }
        Ok(MetadataStats {
            total_sources: total_sources as u64,
            total_chunks: total_chunks as u64,
            by_kind,
        })
    }

    /// Schema version recorded on disk.
    pub fn schema_version(&self) -> IndexResult<u32> {
        let conn = self.lock()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .ok();
        match raw {
            None => Ok(0),
            Some(s) => s
                .parse::<u32>()
                .map_err(|e| IndexError::Schema(format!("invalid schema_version: {e}"))),
        }
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> IndexResult<SourceRecord> {
    let kind_str: String = row.get(1)?;
    Ok(SourceRecord {
        uri: row.get(0)?,
        kind: SourceKind::parse(&kind_str)?,
        content_hash: row.get(2)?,
        size_bytes: row.get::<_, i64>(3)? as u64,
        source_type: row.get(4)?,
        chunk_count: row.get::<_, i64>(5)? as u32,
        status: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(uri: &str, kind: SourceKind, chunk_count: u32) -> SourceRecord {
        SourceRecord {
            uri: uri.into(),
            kind,
            content_hash: vec![0u8; 32],
            size_bytes: 100,
            source_type: "txt".into(),
            chunk_count,
            status: "indexed".into(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
        }
    }

    #[test]
    fn open_records_schema_version() {
        let store = MetadataStore::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), METADATA_SCHEMA_VERSION);
    }

    #[test]
    fn open_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/path/meta.sqlite");
        let store = MetadataStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), METADATA_SCHEMA_VERSION);
        assert!(path.exists());
    }

    #[test]
    fn open_rejects_future_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE index_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO index_meta (key, value) VALUES ('schema_version', '99');",
            )
            .unwrap();
        }
        let err = MetadataStore::open(&path).unwrap_err();
        match err {
            IndexError::Schema(msg) => assert!(msg.contains("99")),
            other => panic!("expected Schema, got {other:?}"),
        }
    }

    #[test]
    fn upsert_then_get() {
        let store = MetadataStore::open_in_memory().unwrap();
        let r = record("u://a", SourceKind::IndexText, 3);
        store.upsert(&r).unwrap();
        let got = store.get("u://a").unwrap().unwrap();
        assert_eq!(got, r);
    }

    #[test]
    fn upsert_replaces_existing_row() {
        let store = MetadataStore::open_in_memory().unwrap();
        store
            .upsert(&record("u://a", SourceKind::IndexText, 3))
            .unwrap();
        let mut updated = record("u://a", SourceKind::IndexText, 7);
        updated.status = "stale".into();
        updated.updated_at = 1_800_000_000;
        store.upsert(&updated).unwrap();
        let got = store.get("u://a").unwrap().unwrap();
        assert_eq!(got.chunk_count, 7);
        assert_eq!(got.status, "stale");
        assert_eq!(got.updated_at, 1_800_000_000);
    }

    #[test]
    fn get_returns_none_for_missing() {
        let store = MetadataStore::open_in_memory().unwrap();
        assert!(store.get("u://nope").unwrap().is_none());
    }

    #[test]
    fn delete_returns_true_when_existed() {
        let store = MetadataStore::open_in_memory().unwrap();
        store
            .upsert(&record("u://a", SourceKind::IndexText, 1))
            .unwrap();
        assert!(store.delete("u://a").unwrap());
        assert!(!store.delete("u://a").unwrap());
    }

    #[test]
    fn list_filters_by_kind_and_sorts() {
        let store = MetadataStore::open_in_memory().unwrap();
        store
            .upsert(&record("u://b", SourceKind::IndexText, 1))
            .unwrap();
        store
            .upsert(&record("u://a", SourceKind::Observation, 1))
            .unwrap();
        store
            .upsert(&record("u://c", SourceKind::IndexText, 1))
            .unwrap();
        let all = store.list(None).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].uri, "u://a");
        assert_eq!(all[1].uri, "u://b");
        assert_eq!(all[2].uri, "u://c");
        let txt = store.list(Some(SourceKind::IndexText)).unwrap();
        assert_eq!(txt.len(), 2);
        let obs = store.list(Some(SourceKind::Observation)).unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].uri, "u://a");
    }

    #[test]
    fn stats_aggregates_correctly() {
        let store = MetadataStore::open_in_memory().unwrap();
        store
            .upsert(&record("u://a", SourceKind::IndexText, 4))
            .unwrap();
        store
            .upsert(&record("u://b", SourceKind::IndexText, 1))
            .unwrap();
        store
            .upsert(&record("u://c", SourceKind::Observation, 2))
            .unwrap();
        let s = store.stats().unwrap();
        assert_eq!(s.total_sources, 3);
        assert_eq!(s.total_chunks, 7);
        assert_eq!(s.by_kind.get("index_text").copied(), Some(2));
        assert_eq!(s.by_kind.get("observation").copied(), Some(1));
    }

    #[test]
    fn upsert_batch_in_single_transaction() {
        let store = MetadataStore::open_in_memory().unwrap();
        let batch: Vec<SourceRecord> = (0..50)
            .map(|i| record(&format!("u://{i}"), SourceKind::IndexText, 1))
            .collect();
        store.upsert_batch(&batch).unwrap();
        assert_eq!(store.stats().unwrap().total_sources, 50);
    }

    #[test]
    fn upsert_batch_empty_is_noop() {
        let store = MetadataStore::open_in_memory().unwrap();
        store.upsert_batch(&[]).unwrap();
        assert_eq!(store.stats().unwrap().total_sources, 0);
    }

    #[test]
    fn source_kind_parse_round_trip() {
        assert_eq!(
            SourceKind::parse(SourceKind::IndexText.as_str()).unwrap(),
            SourceKind::IndexText
        );
        assert_eq!(
            SourceKind::parse(SourceKind::Observation.as_str()).unwrap(),
            SourceKind::Observation
        );
        assert!(SourceKind::parse("nope").is_err());
    }

    #[test]
    fn checkpoint_does_not_error() {
        let store = MetadataStore::open_in_memory().unwrap();
        store
            .upsert(&record("u://a", SourceKind::IndexText, 1))
            .unwrap();
        store.checkpoint().unwrap();
    }

    #[test]
    fn concurrent_readers_see_committed_writes() {
        // WAL mode lets independent connections read while a writer is open.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.sqlite");
        let writer = MetadataStore::open(&path).unwrap();
        for i in 0..20 {
            writer
                .upsert(&record(&format!("u://{i}"), SourceKind::IndexText, 1))
                .unwrap();
        }

        let path1 = path.clone();
        let path2 = path.clone();
        let h1 = std::thread::spawn(move || {
            let r = MetadataStore::open(&path1).unwrap();
            r.stats().unwrap().total_sources
        });
        let h2 = std::thread::spawn(move || {
            let r = MetadataStore::open(&path2).unwrap();
            r.list(None).unwrap().len()
        });

        assert_eq!(h1.join().unwrap(), 20);
        assert_eq!(h2.join().unwrap(), 20);
    }
}
