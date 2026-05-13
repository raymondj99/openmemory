//! [`MemoryStore`] — the public entry point for the knowledge graph.
//!
//! This module owns construction (`open` / `open_in_memory`) and the
//! read-only graph methods (`get_entity`, `list_entities`, `status`,
//! `get_entity_observations`, `get_entity_relations`). The write paths
//! (`remember`, `forget*`, `consolidate`) live in sibling modules but
//! funnel through the same store handle.
//!
//! Internally the store wraps:
//!
//! - **One writer** — `Arc<Mutex<rusqlite::Connection>>`. Every mutation
//!   serialises on this. The `Arc` exists so the read pool's degenerate
//!   in-memory variant can borrow the same handle.
//! - **A pool of readers** — [`crate::pool::ReadPool`], opened with
//!   `OPEN_READ_ONLY | OPEN_NO_MUTEX` against the same database file.
//!   Pool size defaults to `Config::num_jobs()` (CPU count). For
//!   `open_in_memory` the pool degrades to a single slot proxying to the
//!   writer.
//! - **The hybrid search engine** from [`openmemory_index`].
//! - **An `RwLock<()>` rebuild barrier**, so a vector-index rebuild after a
//!   bulk write can't race with concurrent recall calls. This is *not*
//!   the SQLite reader/writer barrier — that's WAL's job. The rebuild
//!   barrier guards the *vector* index visibility.
//! - **An optional `Embedder`** (the trait in `core::testing` —
//!   `openmemory-embed` re-exports the same trait, but pulling that
//!   crate in here would require the heavyweight ONNX deps).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use openmemory_core::clock::{Clock, SystemClock};
use openmemory_core::config::Config;
use openmemory_index::engine::{open_engine, OpenEngine};
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{MemoryError, MemoryResult};
use crate::pool::ReadPool;
use crate::schema::{configure, migrate, MEMORY_SCHEMA_VERSION};
use crate::types::{Entity, EntityType, MemoryTier, Observation, Relation};

#[cfg(any(feature = "testing", feature = "embeddings"))]
use openmemory_core::testing::Embedder;

/// SQLite filename for the knowledge-graph database, under `data_dir`.
pub const MEMORY_DB_FILE: &str = "memory.sqlite";

/// Aggregate counts and timestamps for a [`MemoryStore`]. Returned by
/// [`MemoryStore::status`] and surfaced through the MCP `status` tool.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryStatus {
    pub total_entities: u64,
    pub total_observations: u64,
    pub total_relations: u64,
    pub tombstoned_observations: u64,
    pub schema_version: u32,
    /// Earliest `observed_at` over live observations, if any.
    pub oldest_observation: Option<i64>,
    /// Latest `observed_at` over live observations, if any.
    pub newest_observation: Option<i64>,
    /// Per-`entity_type` count of entities.
    pub entity_type_counts: HashMap<String, u64>,
    /// Per-`memory_tier` count of live observations.
    pub tier_counts: HashMap<String, u64>,
    /// Vector store entry count, as reported by the search engine.
    pub vector_count: u64,
    /// Number of read-only `Connection` slots in the recall pool. `1`
    /// for the in-memory store; `Config::num_jobs()` for the on-disk
    /// store.
    pub reader_pool_size: usize,
}

/// One row of [`MemoryStore::list_entities`]. Pairs the entity record with
/// its live observation count so callers can render a one-shot summary
/// without an extra query.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityListRow {
    pub entity: Entity,
    pub observation_count: u64,
}

/// Persistent knowledge-graph memory store.
pub struct MemoryStore {
    db: Arc<Mutex<Connection>>,
    readers: ReadPool,
    engine: OpenEngine,
    rebuild_lock: RwLock<()>,
    data_dir: PathBuf,
    decay_rate: f64,
    pub(crate) normalization_enabled: bool,
    pub(crate) auto_merge_threshold: f64,
    pub(crate) flag_threshold: f64,
    pub(crate) max_candidates: usize,
    clock: Arc<dyn Clock>,
    #[cfg(any(feature = "testing", feature = "embeddings"))]
    embedder: Option<Arc<dyn Embedder>>,
    /// Holds an owning tempdir handle when [`Self::open_in_memory`] was
    /// used, so the engine's on-disk files are cleaned up when the store
    /// drops. `None` for the regular `open` path.
    _temp_dir: Option<tempfile::TempDir>,
}

impl MemoryStore {
    /// Open or create the memory store rooted at `data_dir`. Creates the
    /// directory if it does not exist; runs the schema migration; opens the
    /// hybrid search engine in the same directory; spins up a pool of
    /// read-only Connections sized to `config.num_jobs()` (CPU count by
    /// default).
    pub fn open(config: &Config, data_dir: &Path) -> MemoryResult<Self> {
        if !data_dir.as_os_str().is_empty() {
            std::fs::create_dir_all(data_dir)?;
        }

        let db_path = data_dir.join(MEMORY_DB_FILE);
        let conn = Connection::open(&db_path)?;
        configure(&conn)?;
        migrate(&conn)?;

        let engine = open_engine(config, data_dir)?;
        let readers = ReadPool::open(&db_path, config.num_jobs())?;

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            readers,
            engine,
            rebuild_lock: RwLock::new(()),
            data_dir: data_dir.to_path_buf(),
            decay_rate: config.memory.decay_rate,
            normalization_enabled: config.normalization.enabled,
            auto_merge_threshold: config.normalization.auto_merge_threshold,
            flag_threshold: config.normalization.flag_threshold,
            max_candidates: config.normalization.max_candidates,
            clock: Arc::new(SystemClock),
            #[cfg(any(feature = "testing", feature = "embeddings"))]
            embedder: None,
            _temp_dir: None,
        })
    }

    /// Open a fully in-memory store. SQLite is `:memory:` and the search
    /// engine's files live in a fresh tempdir that is cleaned up when the
    /// store drops. Sized for tests; production callers should use
    /// [`Self::open`].
    ///
    /// The read pool degrades to a single slot proxying the writer's
    /// mutex: a `:memory:` database is private to the handle that opened
    /// it, so a separate read-only connection would see an empty,
    /// unrelated database. Concurrent read tests should exercise
    /// [`Self::open`] against a tempdir.
    pub fn open_in_memory(config: &Config) -> MemoryResult<Self> {
        let conn = Connection::open_in_memory()?;
        // PRAGMA journal_mode=WAL is silently ignored on :memory: databases —
        // applying the rest is still correct.
        configure(&conn)?;
        migrate(&conn)?;

        // The hybrid engine needs an on-disk home for its FTS5/vector files.
        // tempfile cleans the directory up when the returned `_temp_dir`
        // drops alongside the store.
        let temp_dir = tempfile::tempdir().map_err(MemoryError::Io)?;
        let engine = open_engine(config, temp_dir.path())?;

        let db = Arc::new(Mutex::new(conn));
        let readers = ReadPool::shared_with_writer(Arc::clone(&db));

        Ok(Self {
            db,
            readers,
            engine,
            rebuild_lock: RwLock::new(()),
            data_dir: temp_dir.path().to_path_buf(),
            decay_rate: config.memory.decay_rate,
            normalization_enabled: config.normalization.enabled,
            auto_merge_threshold: config.normalization.auto_merge_threshold,
            flag_threshold: config.normalization.flag_threshold,
            max_candidates: config.normalization.max_candidates,
            clock: Arc::new(SystemClock),
            #[cfg(any(feature = "testing", feature = "embeddings"))]
            embedder: None,
            _temp_dir: Some(temp_dir),
        })
    }

    /// Override the clock. Tests inject a `FixedClock` for deterministic
    /// timestamps.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Override the decay rate (Ebbinghaus lambda). Mostly a hook for
    /// tests; production callers configure this via [`Config`].
    #[must_use]
    pub fn with_decay_rate(mut self, rate: f64) -> Self {
        self.decay_rate = rate;
        self
    }

    /// Attach an embedder for vector search. Production callers pass an
    /// `OnnxEmbedder`; tests use `FakeEmbedder` via the `testing` feature.
    #[cfg(any(feature = "testing", feature = "embeddings"))]
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Borrow the active clock.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Decay rate (Ebbinghaus lambda) currently in effect.
    pub fn decay_rate(&self) -> f64 {
        self.decay_rate
    }

    /// Path passed at open time. Empty for `open_in_memory`.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Borrow the open engine — useful for tests that want to peek at the
    /// search index without going through `recall`.
    pub fn engine(&self) -> &OpenEngine {
        &self.engine
    }

    /// Persist the in-memory vector index to disk. Called after writes
    /// so that short-lived CLI processes don't lose vectors on exit.
    /// Failures are logged but not propagated; the SQLite row is still
    /// authoritative and a future rebuild catches up.
    pub(crate) fn flush_engine(&self) {
        if let Err(e) = openmemory_index::engine::flush(&self.engine) {
            tracing::warn!(
                target: "openmemory_graph::store",
                error = %e,
                "vector index flush failed; vectors will be rebuilt on next full sync"
            );
        }
    }

    /// Borrow the optional embedder, if any. Used by the search-sync
    /// path to obtain a vector for newly-indexed observations.
    #[cfg(any(feature = "testing", feature = "embeddings"))]
    pub(crate) fn embedder_ref(&self) -> Option<Arc<dyn Embedder>> {
        self.embedder.clone()
    }

    /// Acquire the SQLite *writer* connection, recovering from a poisoned
    /// mutex. Mutex poisoning here means a previous holder panicked
    /// mid-write; the connection itself is still usable, so we recover
    /// the inner value rather than propagating the poison up to every
    /// caller.
    ///
    /// Read paths should prefer [`Self::with_reader`], which lets multiple
    /// recall calls run in parallel through the read-only pool.
    pub(crate) fn lock_db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run `f` against a read-only [`Connection`] from the pool. Blocks
    /// only when every reader slot is in use; SQLite's WAL mode keeps the
    /// readers from blocking the writer (and vice versa) so no recall
    /// call ever waits on an in-flight `remember`.
    pub(crate) fn with_reader<F, R>(&self, f: F) -> MemoryResult<R>
    where
        F: FnOnce(&Connection) -> MemoryResult<R>,
    {
        self.readers.with_reader(f)
    }

    /// Number of reader slots in the pool. Surfaced for tests and for
    /// the `status` snapshot.
    pub fn reader_pool_size(&self) -> usize {
        self.readers.size()
    }

    /// Acquire the rebuild-lock for read. Held by the recall path so a
    /// search never observes a half-rebuilt vector index.
    pub(crate) fn read_rebuild(&self) -> std::sync::RwLockReadGuard<'_, ()> {
        self.rebuild_lock.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Acquire the rebuild-lock for write. Held by `remember`, `forget`,
    /// and `consolidate` while they mutate the underlying indices.
    pub(crate) fn write_rebuild(&self) -> std::sync::RwLockWriteGuard<'_, ()> {
        self.rebuild_lock.write().unwrap_or_else(|e| e.into_inner())
    }

    // --------------------- read paths ---------------------

    /// Look up an entity by name. Names are case-sensitive in v0.1; pair
    /// with [`EntityType`] when uniqueness matters across types (the schema
    /// allows the same name with different types).
    pub fn get_entity(&self, name: &str) -> MemoryResult<Option<Entity>> {
        self.with_reader(|conn| {
            let row = conn
                .query_row(
                    "SELECT id, name, entity_type, created_at, updated_at, confidence, source
                     FROM entities WHERE name = ?1",
                    params![name],
                    row_to_entity,
                )
                .optional()?;
            Ok(row)
        })
    }

    /// Look up an entity by `(name, entity_type)`. Useful when two entities
    /// share a name across types.
    pub fn get_entity_by_name_and_type(
        &self,
        name: &str,
        entity_type: EntityType,
    ) -> MemoryResult<Option<Entity>> {
        self.with_reader(|conn| {
            let row = conn
                .query_row(
                    "SELECT id, name, entity_type, created_at, updated_at, confidence, source
                     FROM entities WHERE name = ?1 AND entity_type = ?2",
                    params![name, entity_type.as_str()],
                    row_to_entity,
                )
                .optional()?;
            Ok(row)
        })
    }

    /// Look up an entity by its UUID.
    pub fn get_entity_by_id(&self, id: &str) -> MemoryResult<Option<Entity>> {
        self.with_reader(|conn| {
            let row = conn
                .query_row(
                    "SELECT id, name, entity_type, created_at, updated_at, confidence, source
                     FROM entities WHERE id = ?1",
                    params![id],
                    row_to_entity,
                )
                .optional()?;
            Ok(row)
        })
    }

    /// List entities with optional `entity_type` filter and pagination.
    /// Sorted by `updated_at` descending.
    pub fn list_entities(
        &self,
        entity_type: Option<EntityType>,
        limit: usize,
        offset: usize,
    ) -> MemoryResult<Vec<EntityListRow>> {
        let limit_i = i64::try_from(limit).unwrap_or(i64::MAX);
        let offset_i = i64::try_from(offset).unwrap_or(0);
        let now = self.clock.now_secs();

        let base = "\
            SELECT e.id, e.name, e.entity_type, e.created_at, e.updated_at, \
                   e.confidence, e.source, \
                   COUNT(o.id) AS obs_count \
            FROM entities e \
            LEFT JOIN observations o \
                ON o.entity_id = e.id \
                AND o.tombstoned = 0 \
                AND (o.valid_until IS NULL OR o.valid_until > ?1)";

        self.with_reader(|conn| {
            if let Some(et) = entity_type {
                let sql = format!(
                    "{base} \
                     WHERE e.entity_type = ?2 \
                     GROUP BY e.id \
                     ORDER BY e.updated_at DESC \
                     LIMIT ?3 OFFSET ?4"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(
                    params![now, et.as_str(), limit_i, offset_i],
                    row_to_entity_row,
                )?;
                collect_rows(rows)
            } else {
                let sql = format!(
                    "{base} \
                     GROUP BY e.id \
                     ORDER BY e.updated_at DESC \
                     LIMIT ?2 OFFSET ?3"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(params![now, limit_i, offset_i], row_to_entity_row)?;
                collect_rows(rows)
            }
        })
    }

    /// Active observations for `entity_id`, sorted by `observed_at` DESC.
    /// Tombstoned observations are excluded; observations with a
    /// `valid_until` already in the past are excluded.
    pub fn get_entity_observations(&self, entity_id: &str) -> MemoryResult<Vec<Observation>> {
        let now = self.clock.now_secs();
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, entity_id, content, observed_at, valid_from, valid_until,
                        confidence, source, tombstoned, access_count
                 FROM observations
                 WHERE entity_id = ?1
                    AND tombstoned = 0
                    AND (valid_until IS NULL OR valid_until > ?2)
                 ORDER BY observed_at DESC",
            )?;
            let mut out = Vec::new();
            let mut rows = stmt.query(params![entity_id, now])?;
            while let Some(row) = rows.next()? {
                out.push(row_to_observation(row)?);
            }
            Ok(out)
        })
    }

    /// Active relations for `entity_id` (in either direction). Tombstoned
    /// relations and those with an expired `valid_until` are excluded.
    pub fn get_entity_relations(&self, entity_id: &str) -> MemoryResult<Vec<Relation>> {
        let now = self.clock.now_secs();
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, from_entity, to_entity, relation_type, weight, created_at,
                        valid_from, valid_until, source
                 FROM relations
                 WHERE (from_entity = ?1 OR to_entity = ?1)
                    AND (valid_until IS NULL OR valid_until > ?2)
                 ORDER BY created_at DESC",
            )?;
            let mut out = Vec::new();
            let mut rows = stmt.query(params![entity_id, now])?;
            while let Some(row) = rows.next()? {
                out.push(row_to_relation(row)?);
            }
            Ok(out)
        })
    }

    /// Aggregate counts + timestamps for the store.
    pub fn status(&self) -> MemoryResult<MemoryStatus> {
        let now = self.clock.now_secs();
        let pool_size = self.readers.size();
        self.with_reader(|conn| Self::status_from(conn, now, pool_size, &self.engine))
    }

    /// Inner status query — runs everything against a single read-only
    /// connection so the snapshot is consistent (every count comes from
    /// the same WAL end mark). Vector-store count comes from the engine
    /// regardless; that's its own concurrency story.
    fn status_from(
        conn: &Connection,
        now: i64,
        pool_size: usize,
        engine: &OpenEngine,
    ) -> MemoryResult<MemoryStatus> {
        let total_entities: u64 =
            conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))?;
        let total_observations: u64 = conn.query_row(
            "SELECT COUNT(*) FROM observations
             WHERE tombstoned = 0 AND (valid_until IS NULL OR valid_until > ?1)",
            params![now],
            |r| r.get(0),
        )?;
        let total_relations: u64 = conn.query_row(
            "SELECT COUNT(*) FROM relations
             WHERE valid_until IS NULL OR valid_until > ?1",
            params![now],
            |r| r.get(0),
        )?;
        let tombstoned_observations: u64 = conn.query_row(
            "SELECT COUNT(*) FROM observations WHERE tombstoned = 1",
            [],
            |r| r.get(0),
        )?;

        let oldest_observation: Option<i64> = conn
            .query_row(
                "SELECT MIN(observed_at) FROM observations WHERE tombstoned = 0",
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let newest_observation: Option<i64> = conn
            .query_row(
                "SELECT MAX(observed_at) FROM observations WHERE tombstoned = 0",
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten();

        let mut entity_type_counts: HashMap<String, u64> = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT entity_type, COUNT(*) FROM entities GROUP BY entity_type")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let k: String = row.get(0)?;
                let v: i64 = row.get(1)?;
                entity_type_counts.insert(k, v as u64);
            }
        }

        let mut tier_counts: HashMap<String, u64> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT memory_tier, COUNT(*) FROM observations
                 WHERE tombstoned = 0 GROUP BY memory_tier",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let k: String = row.get(0)?;
                let v: i64 = row.get(1)?;
                tier_counts.insert(k, v as u64);
            }
        }

        let schema_version: u32 = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(MEMORY_SCHEMA_VERSION);

        let vector_count = engine.engine.count().unwrap_or(0);

        Ok(MemoryStatus {
            total_entities,
            total_observations,
            total_relations,
            tombstoned_observations,
            schema_version,
            oldest_observation,
            newest_observation,
            entity_type_counts,
            tier_counts,
            vector_count,
            reader_pool_size: pool_size,
        })
    }
}

// --------------------- row mappers ---------------------

fn row_to_entity(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entity> {
    let entity_type_str: String = row.get(2)?;
    Ok(Entity {
        id: row.get(0)?,
        name: row.get(1)?,
        entity_type: EntityType::parse(&entity_type_str).unwrap_or(EntityType::Concept),
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        confidence: row.get(5)?,
        source: row.get(6)?,
    })
}

fn row_to_entity_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EntityListRow> {
    let entity = row_to_entity(row)?;
    let count: i64 = row.get(7)?;
    Ok(EntityListRow {
        entity,
        observation_count: count.max(0) as u64,
    })
}

pub(crate) fn row_to_observation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Observation> {
    let tombstoned: i64 = row.get(8)?;
    let access_count: i64 = row.get(9)?;
    Ok(Observation {
        id: row.get(0)?,
        entity_id: row.get(1)?,
        content: row.get(2)?,
        observed_at: row.get(3)?,
        valid_from: row.get(4)?,
        valid_until: row.get(5)?,
        confidence: row.get(6)?,
        source: row.get(7)?,
        tombstoned: tombstoned != 0,
        access_count: access_count.max(0) as u32,
    })
}

pub(crate) fn row_to_relation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Relation> {
    Ok(Relation {
        id: row.get(0)?,
        from_entity: row.get(1)?,
        to_entity: row.get(2)?,
        relation_type: row.get(3)?,
        weight: row.get(4)?,
        created_at: row.get(5)?,
        valid_from: row.get(6)?,
        valid_until: row.get(7)?,
        source: row.get(8)?,
    })
}

fn collect_rows<I, T>(rows: I) -> MemoryResult<Vec<T>>
where
    I: Iterator<Item = rusqlite::Result<T>>,
{
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[allow(dead_code)]
fn _impl_send_sync_check() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MemoryStore>();
}

// Marker so `MemoryTier` doesn't get pruned as unused when the writer-side
// commits land later. The tier column lives in the schema today.
#[allow(dead_code)]
const _MEMORY_TIER_DEFAULT: MemoryTier = MemoryTier::Episodic;

#[allow(dead_code)]
fn _ensure_error_module_used(_e: MemoryError) {}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_core::clock::FixedClock;

    fn cfg() -> Config {
        Config::default()
    }

    fn open_temp() -> (MemoryStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(&cfg(), dir.path()).unwrap();
        (store, dir)
    }

    #[test]
    fn open_creates_data_dir_and_db_file() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested/profile");
        let store = MemoryStore::open(&cfg(), &nested).unwrap();
        assert!(nested.join(MEMORY_DB_FILE).exists());
        assert_eq!(store.data_dir(), nested);
    }

    #[test]
    fn open_empty_store_status_is_zero() {
        let (store, _dir) = open_temp();
        let s = store.status().unwrap();
        assert_eq!(s.total_entities, 0);
        assert_eq!(s.total_observations, 0);
        assert_eq!(s.total_relations, 0);
        assert_eq!(s.tombstoned_observations, 0);
        assert_eq!(s.schema_version, MEMORY_SCHEMA_VERSION);
        assert_eq!(s.oldest_observation, None);
        assert_eq!(s.newest_observation, None);
        assert!(s.entity_type_counts.is_empty());
        assert!(s.tier_counts.is_empty());
    }

    #[test]
    fn list_entities_empty() {
        let (store, _dir) = open_temp();
        let rows = store.list_entities(None, 100, 0).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn get_entity_missing_returns_none() {
        let (store, _dir) = open_temp();
        assert!(store.get_entity("nope").unwrap().is_none());
        assert!(store.get_entity_by_id("missing").unwrap().is_none());
    }

    #[test]
    fn list_entities_returns_inserted_rows_ordered_by_updated_at() {
        let (store, _dir) = open_temp();
        // Hand-insert entities directly via SQL so this exercise targets the
        // store's `list_entities` query in isolation, independent of the
        // higher-level `remember` flow.
        {
            let conn = store.lock_db();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('a', 'Alpha', 'concept', 100, 100)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('b', 'Beta', 'concept', 50, 200)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('c', 'Gamma', 'project', 75, 75)",
                [],
            )
            .unwrap();
        }

        let rows = store.list_entities(None, 100, 0).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].entity.name, "Beta", "ordered by updated_at DESC");
        assert_eq!(rows[1].entity.name, "Alpha");
        assert_eq!(rows[2].entity.name, "Gamma");

        // Filter by entity_type.
        let rows = store
            .list_entities(Some(EntityType::Project), 100, 0)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.name, "Gamma");

        // Pagination.
        let rows = store.list_entities(None, 1, 0).unwrap();
        assert_eq!(rows.len(), 1);
        let rows = store.list_entities(None, 1, 1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.name, "Alpha");
    }

    #[test]
    fn list_entities_includes_observation_count() {
        let (store, _dir) = open_temp();
        {
            let conn = store.lock_db();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('e1', 'X', 'fact', 0, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO observations (id, entity_id, content, observed_at)
                 VALUES ('o1', 'e1', 'hello', 0), ('o2', 'e1', 'world', 0)",
                [],
            )
            .unwrap();
        }
        let rows = store.list_entities(None, 100, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].observation_count, 2);
    }

    #[test]
    fn list_entities_excludes_tombstoned_observations_from_count() {
        let (store, _dir) = open_temp();
        {
            let conn = store.lock_db();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('e1', 'X', 'fact', 0, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO observations
                 (id, entity_id, content, observed_at, tombstoned)
                 VALUES ('o1', 'e1', 'live', 0, 0),
                        ('o2', 'e1', 'dead', 0, 1)",
                [],
            )
            .unwrap();
        }
        let rows = store.list_entities(None, 100, 0).unwrap();
        assert_eq!(rows[0].observation_count, 1);
    }

    #[test]
    fn get_entity_by_id_round_trips() {
        let (store, _dir) = open_temp();
        {
            let conn = store.lock_db();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('e1', 'Raymond', 'person', 0, 0)",
                [],
            )
            .unwrap();
        }
        let e = store.get_entity_by_id("e1").unwrap().unwrap();
        assert_eq!(e.name, "Raymond");
        assert_eq!(e.entity_type, EntityType::Person);
    }

    #[test]
    fn get_entity_by_name_is_case_sensitive() {
        let (store, _dir) = open_temp();
        {
            let conn = store.lock_db();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('e1', 'Raymond', 'person', 0, 0)",
                [],
            )
            .unwrap();
        }
        assert!(store.get_entity("Raymond").unwrap().is_some());
        assert!(store.get_entity("raymond").unwrap().is_none());
    }

    #[test]
    fn status_reports_per_type_counts() {
        let (store, _dir) = open_temp();
        {
            let conn = store.lock_db();
            conn.execute_batch(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at) VALUES
                 ('1', 'A', 'person', 0, 0),
                 ('2', 'B', 'person', 0, 0),
                 ('3', 'C', 'project', 0, 0);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO observations (id, entity_id, content, observed_at)
                 VALUES ('o1', '1', 'hi', 100), ('o2', '2', 'hi', 200)",
                [],
            )
            .unwrap();
        }
        let s = store.status().unwrap();
        assert_eq!(s.total_entities, 3);
        assert_eq!(s.entity_type_counts.get("person"), Some(&2));
        assert_eq!(s.entity_type_counts.get("project"), Some(&1));
        assert_eq!(s.tier_counts.get("episodic"), Some(&2));
        assert_eq!(s.oldest_observation, Some(100));
        assert_eq!(s.newest_observation, Some(200));
    }

    #[test]
    fn status_excludes_expired_validity_window() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(FixedClock::new(1_000));
        let store = MemoryStore::open(&cfg(), dir.path())
            .unwrap()
            .with_clock(clock as Arc<dyn Clock>);
        {
            let conn = store.lock_db();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('e1', 'X', 'fact', 0, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO observations
                 (id, entity_id, content, observed_at, valid_until)
                 VALUES ('o1', 'e1', 'live', 100, NULL),
                        ('o2', 'e1', 'expired', 100, 500)",
                [],
            )
            .unwrap();
        }
        let s = store.status().unwrap();
        assert_eq!(s.total_observations, 1, "expired observation excluded");
    }

    #[test]
    fn open_in_memory_round_trip() {
        let store = MemoryStore::open_in_memory(&cfg()).unwrap();
        let s = store.status().unwrap();
        assert_eq!(s.total_entities, 0);
    }

    #[test]
    fn reopen_picks_up_existing_data() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = MemoryStore::open(&cfg(), dir.path()).unwrap();
            let conn = store.lock_db();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('1', 'A', 'fact', 0, 0)",
                [],
            )
            .unwrap();
        }
        let store = MemoryStore::open(&cfg(), dir.path()).unwrap();
        assert_eq!(store.status().unwrap().total_entities, 1);
        assert!(store.get_entity("A").unwrap().is_some());
    }

    #[test]
    fn get_entity_observations_filters_tombstoned_and_expired() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(FixedClock::new(1_000));
        let store = MemoryStore::open(&cfg(), dir.path())
            .unwrap()
            .with_clock(clock as Arc<dyn Clock>);
        {
            let conn = store.lock_db();
            conn.execute(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
                 VALUES ('e1', 'X', 'fact', 0, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO observations
                 (id, entity_id, content, observed_at, valid_until, tombstoned)
                 VALUES ('o1', 'e1', 'live',     100, NULL, 0),
                        ('o2', 'e1', 'tomb',     200, NULL, 1),
                        ('o3', 'e1', 'expired',  300, 500,  0),
                        ('o4', 'e1', 'live too', 400, NULL, 0)",
                [],
            )
            .unwrap();
        }
        let obs = store.get_entity_observations("e1").unwrap();
        let contents: Vec<_> = obs.iter().map(|o| o.content.clone()).collect();
        assert_eq!(contents, vec!["live too", "live"]);
    }

    #[test]
    fn get_entity_relations_returns_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(FixedClock::new(1_000));
        let store = MemoryStore::open(&cfg(), dir.path())
            .unwrap()
            .with_clock(clock as Arc<dyn Clock>);
        {
            let conn = store.lock_db();
            conn.execute_batch(
                "INSERT INTO entities (id, name, entity_type, created_at, updated_at) VALUES
                 ('a', 'A', 'person', 0, 0),
                 ('b', 'B', 'project', 0, 0),
                 ('c', 'C', 'concept', 0, 0);
                 INSERT INTO relations (id, from_entity, to_entity, relation_type, created_at) VALUES
                 ('r1', 'a', 'b', 'maintains', 100),
                 ('r2', 'c', 'a', 'mentions',  200),
                 ('r3', 'b', 'c', 'uses',      300);",
            )
            .unwrap();
        }
        let rels = store.get_entity_relations("a").unwrap();
        assert_eq!(rels.len(), 2);
        let kinds: Vec<_> = rels.iter().map(|r| r.relation_type.clone()).collect();
        assert!(kinds.contains(&"maintains".to_string()));
        assert!(kinds.contains(&"mentions".to_string()));
    }
}
