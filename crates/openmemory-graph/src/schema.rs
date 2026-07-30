//! SQLite schema and migrations for the knowledge graph.
//!
//! Schema versioning is delegated to [`openmemory_core::migrations::Migrator`]:
//! every database carries its current version in the `memory_meta` table, the
//! migrator refuses to open a database whose version is higher than the binary
//! supports, and the v1 migration is idempotent so repeated `MemoryStore::open`
//! calls are safe.
//!
//! v1 lays down four tables: `entities`, `observations`, `relations`, and the
//! `memory_meta` key/value bag. Indexes target the queries the store hot path
//! actually runs — entity name lookup, observation listing by entity, temporal
//! validity scans, and relation traversal in either direction.

use openmemory_core::migrations::Migrator;
use rusqlite::Connection;

use crate::error::MemoryResult;

/// Current memory-store schema version. Bump when a new migration step lands.
pub const MEMORY_SCHEMA_VERSION: u32 = 7;

/// Apply the recommended SQLite pragmas for the memory database. WAL keeps
/// readers from blocking the write path; `foreign_keys=ON` makes the FK
/// declarations on `observations.entity_id` and `relations.from/to_entity`
/// actually enforce.
pub fn configure(conn: &Connection) -> MemoryResult<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA busy_timeout=5000;
         PRAGMA cache_size=-8000;",
    )?;
    Ok(())
}

/// Run forward migrations on `conn` up to [`MEMORY_SCHEMA_VERSION`].
pub fn migrate(conn: &Connection) -> MemoryResult<()> {
    let migrator = Migrator::new(conn, "memory_meta");
    migrator.apply(
        MEMORY_SCHEMA_VERSION,
        &[
            (1, V1_SQL),
            (2, V2_SQL),
            (3, V3_SQL),
            (4, V4_SQL),
            (5, V5_SQL),
            (6, V6_SQL),
            (7, V7_SQL),
        ],
    )?;
    Ok(())
}

/// v1 — initial schema. Three primary tables plus indexes.
const V1_SQL: &str = "
CREATE TABLE IF NOT EXISTS entities (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    entity_type  TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    confidence   REAL NOT NULL DEFAULT 1.0,
    source       TEXT NOT NULL DEFAULT ''
);

-- Entity names are unique per type. Two entities can share a name only if
-- they have different types (e.g. a person Raymond and a project Raymond).
CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_name_type
    ON entities(name, entity_type);
CREATE INDEX IF NOT EXISTS idx_entities_type
    ON entities(entity_type);
CREATE INDEX IF NOT EXISTS idx_entities_updated_at
    ON entities(updated_at);

CREATE TABLE IF NOT EXISTS observations (
    id           TEXT PRIMARY KEY,
    entity_id    TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    content      TEXT NOT NULL,
    observed_at  INTEGER NOT NULL,
    valid_from   INTEGER,
    valid_until  INTEGER,
    confidence   REAL NOT NULL DEFAULT 1.0,
    source       TEXT NOT NULL DEFAULT '',
    tombstoned   INTEGER NOT NULL DEFAULT 0,
    access_count INTEGER NOT NULL DEFAULT 0,
    memory_tier  TEXT NOT NULL DEFAULT 'episodic'
);

CREATE INDEX IF NOT EXISTS idx_observations_entity
    ON observations(entity_id);
CREATE INDEX IF NOT EXISTS idx_observations_validity
    ON observations(valid_from, valid_until);
CREATE INDEX IF NOT EXISTS idx_observations_observed_at
    ON observations(observed_at);
CREATE INDEX IF NOT EXISTS idx_observations_tombstoned
    ON observations(tombstoned, observed_at)
    WHERE tombstoned = 1;

CREATE TABLE IF NOT EXISTS relations (
    id            TEXT PRIMARY KEY,
    from_entity   TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    to_entity     TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    relation_type TEXT NOT NULL,
    weight        REAL NOT NULL DEFAULT 1.0,
    created_at    INTEGER NOT NULL,
    valid_from    INTEGER,
    valid_until   INTEGER,
    source        TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_relations_from
    ON relations(from_entity);
CREATE INDEX IF NOT EXISTS idx_relations_to
    ON relations(to_entity);
CREATE INDEX IF NOT EXISTS idx_relations_type
    ON relations(relation_type);
";

/// v2 — fielded observation columns plus concept/file side tables. Lets
/// callers tag observations with a caller-provided `title`, `summary`,
/// `importance` (used as a ranking prior, not indexed), and free-form
/// `source_kind`; the side tables carry the arrays so a future fielded
/// FTS5 schema can weight them per-column without changing the row shape.
const V2_SQL: &str = "
ALTER TABLE observations ADD COLUMN title       TEXT;
ALTER TABLE observations ADD COLUMN summary     TEXT;
ALTER TABLE observations ADD COLUMN importance  REAL;
ALTER TABLE observations ADD COLUMN source_kind TEXT;

CREATE TABLE IF NOT EXISTS observation_concepts (
    observation_id TEXT NOT NULL REFERENCES observations(id) ON DELETE CASCADE,
    concept        TEXT NOT NULL,
    PRIMARY KEY (observation_id, concept)
);
CREATE INDEX IF NOT EXISTS idx_concept_lookup
    ON observation_concepts(concept);

CREATE TABLE IF NOT EXISTS observation_source_files (
    observation_id TEXT NOT NULL REFERENCES observations(id) ON DELETE CASCADE,
    file_path      TEXT NOT NULL,
    PRIMARY KEY (observation_id, file_path)
);
CREATE INDEX IF NOT EXISTS idx_source_file_lookup
    ON observation_source_files(file_path);
";

/// v3 — audited observation writes and durable index visibility.
const V3_SQL: &str = r#"
ALTER TABLE observations ADD COLUMN current_revision_id TEXT;
ALTER TABLE observations ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE observations ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';

CREATE TABLE domain_state (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    semantic_generation INTEGER NOT NULL DEFAULT 0,
    indexed_generation INTEGER NOT NULL DEFAULT 0,
    mirror_generation INTEGER NOT NULL DEFAULT 0,
    backfill_cursor TEXT,
    backfill_complete INTEGER NOT NULL DEFAULT 0,
    index_repair_required INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO domain_state(id) VALUES(1);

CREATE TABLE change_sets (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_version INTEGER NOT NULL,
    request_hash TEXT NOT NULL,
    submit_mode TEXT NOT NULL CHECK(submit_mode IN ('apply_immediately', 'propose')),
    actor_kind TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    authority_snapshot TEXT NOT NULL,
    reason TEXT NOT NULL,
    source TEXT NOT NULL,
    base_generation INTEGER NOT NULL,
    committed_generation INTEGER,
    state TEXT NOT NULL CHECK(state IN ('proposed', 'applied', 'rejected', 'conflicted', 'reverted')),
    state_version INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(space_id, idempotency_key)
);
CREATE INDEX idx_change_sets_state ON change_sets(state, created_at, id);
CREATE INDEX idx_change_sets_space ON change_sets(space_id, created_at, id);

CREATE TABLE change_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    changeset_id TEXT NOT NULL REFERENCES change_sets(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    object_kind TEXT NOT NULL CHECK(object_kind IN ('entity', 'observation', 'relation')),
    logical_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    payload_version INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    expected_revision_id TEXT,
    expected_row_version INTEGER,
    expected_lifecycle TEXT,
    UNIQUE(changeset_id, ordinal)
);

CREATE TABLE change_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    changeset_id TEXT NOT NULL REFERENCES change_sets(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    object_kind TEXT,
    logical_id TEXT,
    revision_id TEXT,
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(changeset_id, ordinal)
);
CREATE INDEX idx_change_events_object ON change_events(object_kind, logical_id, id);

CREATE TABLE observation_revisions (
    revision_id TEXT PRIMARY KEY,
    observation_id TEXT NOT NULL REFERENCES observations(id) ON DELETE CASCADE,
    parent_revision_id TEXT,
    changeset_id TEXT NOT NULL REFERENCES change_sets(id),
    content TEXT NOT NULL,
    title TEXT,
    summary TEXT,
    concepts_json TEXT NOT NULL,
    source_files_json TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    valid_from INTEGER,
    valid_until INTEGER,
    confidence REAL NOT NULL,
    source TEXT NOT NULL,
    source_kind TEXT,
    importance REAL,
    memory_tier TEXT NOT NULL,
    semantic_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_observation_revisions_head ON observation_revisions(observation_id, created_at, revision_id);

CREATE TABLE index_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    semantic_generation INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    operation TEXT NOT NULL CHECK(operation IN ('upsert', 'delete')),
    object_kind TEXT NOT NULL,
    logical_id TEXT NOT NULL,
    revision_id TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    applied_at INTEGER,
    created_at INTEGER NOT NULL,
    UNIQUE(semantic_generation, ordinal)
);
CREATE INDEX idx_index_outbox_pending ON index_outbox(applied_at, semantic_generation, ordinal);
"#;

/// v4 — audited entity/relation history and canonical mirror provenance.
const V4_SQL: &str = r#"
ALTER TABLE entities ADD COLUMN current_revision_id TEXT;
ALTER TABLE entities ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE entities ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';
ALTER TABLE relations ADD COLUMN canonical_relation_id TEXT;
ALTER TABLE relations ADD COLUMN relation_role TEXT NOT NULL DEFAULT 'canonical';
ALTER TABLE relations ADD COLUMN current_revision_id TEXT;
ALTER TABLE relations ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE relations ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';

CREATE TABLE entity_revisions (
    revision_id TEXT PRIMARY KEY,
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    parent_revision_id TEXT,
    changeset_id TEXT NOT NULL REFERENCES change_sets(id),
    name TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    confidence REAL NOT NULL,
    source TEXT NOT NULL,
    aliases_json TEXT NOT NULL DEFAULT '[]',
    identifiers_json TEXT NOT NULL DEFAULT '[]',
    semantic_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_entity_revisions_head ON entity_revisions(entity_id, created_at, revision_id);

CREATE TABLE entity_identifiers (
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL,
    value TEXT NOT NULL,
    trust TEXT NOT NULL CHECK(trust IN ('claimed', 'verified')),
    source TEXT NOT NULL,
    verified_at INTEGER,
    PRIMARY KEY(entity_id, namespace, value)
);

CREATE TABLE relation_revisions (
    revision_id TEXT PRIMARY KEY,
    relation_id TEXT NOT NULL REFERENCES relations(id) ON DELETE CASCADE,
    parent_revision_id TEXT,
    changeset_id TEXT NOT NULL REFERENCES change_sets(id),
    from_entity TEXT NOT NULL,
    to_entity TEXT NOT NULL,
    relation_type TEXT NOT NULL,
    weight REAL NOT NULL,
    valid_from INTEGER,
    valid_until INTEGER,
    source TEXT NOT NULL,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    semantic_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_relation_revisions_head ON relation_revisions(relation_id, created_at, revision_id);

CREATE TABLE relation_contributions (
    canonical_relation_id TEXT NOT NULL,
    target_relation_id TEXT NOT NULL,
    origin_space_id TEXT NOT NULL,
    origin_revision_id TEXT NOT NULL,
    contribution_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(target_relation_id, origin_space_id, origin_revision_id)
);

CREATE TABLE mirror_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    canonical_relation_id TEXT NOT NULL,
    target_domain TEXT NOT NULL,
    operation TEXT NOT NULL CHECK(operation IN ('upsert', 'delete')),
    revision_id TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    applied_at INTEGER,
    created_at INTEGER NOT NULL,
    UNIQUE(canonical_relation_id, target_domain, operation, revision_id)
);
CREATE INDEX idx_mirror_outbox_pending ON mirror_outbox(applied_at, id);
"#;

/// v5 — retain lifecycle as part of every immutable observation revision.
const V5_SQL: &str = r#"
ALTER TABLE observation_revisions
    ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active'
    CHECK(lifecycle IN ('active', 'retired', 'destroyed'));
"#;

/// v6 — make revisions self-contained and retain them after canonical
/// observation cleanup.
const V6_SQL: &str = r#"
CREATE TABLE observation_revisions_v6 (
    revision_id TEXT PRIMARY KEY,
    observation_id TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    parent_revision_id TEXT,
    changeset_id TEXT NOT NULL REFERENCES change_sets(id),
    content TEXT NOT NULL,
    title TEXT,
    summary TEXT,
    concepts_json TEXT NOT NULL,
    source_files_json TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    valid_from INTEGER,
    valid_until INTEGER,
    confidence REAL NOT NULL,
    source TEXT NOT NULL,
    source_kind TEXT,
    importance REAL,
    memory_tier TEXT NOT NULL,
    semantic_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    lifecycle TEXT NOT NULL CHECK(lifecycle IN ('active', 'retired', 'destroyed'))
);
INSERT INTO observation_revisions_v6(
    revision_id, observation_id, entity_id, parent_revision_id, changeset_id,
    content, title, summary, concepts_json, source_files_json, observed_at,
    valid_from, valid_until, confidence, source, source_kind, importance,
    memory_tier, semantic_hash, created_at, lifecycle
)
SELECT revisions.revision_id, revisions.observation_id, observations.entity_id,
       revisions.parent_revision_id, revisions.changeset_id, revisions.content,
       revisions.title, revisions.summary, revisions.concepts_json,
       revisions.source_files_json, revisions.observed_at, revisions.valid_from,
       revisions.valid_until, revisions.confidence, revisions.source,
       revisions.source_kind, revisions.importance, revisions.memory_tier,
       revisions.semantic_hash, revisions.created_at, revisions.lifecycle
FROM observation_revisions AS revisions
JOIN observations ON observations.id = revisions.observation_id;
DROP TABLE observation_revisions;
ALTER TABLE observation_revisions_v6 RENAME TO observation_revisions;
CREATE INDEX idx_observation_revisions_head
    ON observation_revisions(observation_id, created_at, revision_id);
"#;

/// v7 — short-lived, hash-bound destruction confirmations and content-free
/// receipts. Confirmation secrets are never persisted.
const V7_SQL: &str = r#"
CREATE TABLE destruction_confirmations (
    token_hash TEXT PRIMARY KEY,
    space_id TEXT NOT NULL,
    object_kind TEXT NOT NULL CHECK(object_kind IN ('observation')),
    logical_id TEXT NOT NULL,
    expected_revision_id TEXT,
    expected_row_version INTEGER NOT NULL,
    preview_hash TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    authority_snapshot TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_destruction_confirmations_expiry
    ON destruction_confirmations(consumed_at, expires_at);

CREATE TABLE destruction_receipts (
    changeset_id TEXT PRIMARY KEY REFERENCES change_sets(id),
    space_id TEXT NOT NULL,
    object_kind TEXT NOT NULL CHECK(object_kind IN ('observation')),
    logical_id TEXT NOT NULL,
    preview_hash TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    destroyed_at INTEGER NOT NULL
);
CREATE INDEX idx_destruction_receipts_object
    ON destruction_receipts(space_id, object_kind, logical_id, destroyed_at);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        conn
    }

    #[test]
    fn migrate_creates_all_tables() {
        let conn = open();
        migrate(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(tables.contains(&"entities".to_string()));
        assert!(tables.contains(&"observations".to_string()));
        assert!(tables.contains(&"relations".to_string()));
        assert!(tables.contains(&"memory_meta".to_string()));
    }

    #[test]
    fn migrate_idempotent() {
        let conn = open();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let v: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, MEMORY_SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_v1_to_v2_adds_observation_columns() {
        let conn = open();
        // Stage a v1 store: apply only V1_SQL and pin schema_version=1.
        Migrator::new(&conn, "memory_meta")
            .apply(1, &[(1, V1_SQL)])
            .unwrap();

        // Now migrate forward via the public entry point.
        migrate(&conn).unwrap();

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(observations)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for col in &["title", "summary", "importance", "source_kind"] {
            assert!(cols.contains(&(*col).to_string()), "missing {col}");
        }

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for t in &["observation_concepts", "observation_source_files"] {
            assert!(tables.contains(&(*t).to_string()), "missing table {t}");
        }

        let v: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, MEMORY_SCHEMA_VERSION.to_string());
    }

    #[test]
    fn migrate_records_schema_version() {
        let conn = open();
        migrate(&conn).unwrap();
        let v: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v.parse::<u32>().unwrap(), MEMORY_SCHEMA_VERSION);
    }

    #[test]
    fn migrate_creates_v2_tables_on_fresh_store() {
        let conn = open();
        migrate(&conn).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(tables.contains(&"observation_concepts".to_string()));
        assert!(tables.contains(&"observation_source_files".to_string()));
    }

    #[test]
    fn migrate_rejects_future_version() {
        let conn = open();
        conn.execute_batch(
            "CREATE TABLE memory_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO memory_meta (key, value) VALUES ('schema_version', '99');",
        )
        .unwrap();

        let err = migrate(&conn).unwrap_err();
        match err {
            crate::MemoryError::Schema(msg) => {
                assert!(
                    msg.contains("99"),
                    "msg should mention current version: {msg}"
                );
            }
            other => panic!("expected Schema, got {other:?}"),
        }
    }

    #[test]
    fn entities_unique_name_type_constraint() {
        let conn = open();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
             VALUES ('1', 'Raymond', 'person', 0, 0)",
            [],
        )
        .unwrap();
        // Same name + same type is a conflict.
        let res = conn.execute(
            "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
             VALUES ('2', 'Raymond', 'person', 0, 0)",
            [],
        );
        assert!(res.is_err());
        // Same name + different type is allowed.
        conn.execute(
            "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
             VALUES ('3', 'Raymond', 'project', 0, 0)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn observations_cascade_on_entity_delete() {
        let conn = open();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
             VALUES ('e1', 'X', 'fact', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO observations (id, entity_id, content, observed_at)
             VALUES ('o1', 'e1', 'something', 0)",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM entities WHERE id = 'e1'", [])
            .unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 0, "FK ON DELETE CASCADE should drop observations");
    }

    #[test]
    fn relations_cascade_on_entity_delete() {
        let conn = open();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO entities (id, name, entity_type, created_at, updated_at) VALUES
             ('a', 'A', 'person', 0, 0),
             ('b', 'B', 'project', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO relations (id, from_entity, to_entity, relation_type, created_at)
             VALUES ('r1', 'a', 'b', 'maintains', 0)",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM entities WHERE id = 'a'", [])
            .unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM relations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn configure_sets_pragmas() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        let foreign_keys: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
        // WAL is unavailable for :memory: databases — only check FK on the
        // in-memory path; the on-disk smoke test in MemoryStore covers WAL.
    }

    #[test]
    fn populated_v2_migrates_without_scanning_a_baseline() {
        let conn = open();
        Migrator::new(&conn, "memory_meta")
            .apply(2, &[(1, V1_SQL), (2, V2_SQL)])
            .unwrap();
        conn.execute(
            "INSERT INTO entities(id, name, entity_type, created_at, updated_at)
             VALUES('e1', 'Ada', 'person', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO observations(id, entity_id, content, observed_at)
             VALUES('o1', 'e1', 'legacy', 1)",
            [],
        )
        .unwrap();
        migrate(&conn).unwrap();
        let version: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, MEMORY_SCHEMA_VERSION.to_string());
        assert_eq!(
            conn.query_row(
                "SELECT content FROM observations WHERE id = 'o1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "legacy"
        );
        let current: Option<String> = conn
            .query_row(
                "SELECT current_revision_id FROM observations WHERE id = 'o1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(current.is_none(), "baseline creation is explicitly lazy");
    }

    #[test]
    fn failed_v3_step_rolls_back_schema_and_version() {
        let conn = open();
        Migrator::new(&conn, "memory_meta")
            .apply(2, &[(1, V1_SQL), (2, V2_SQL)])
            .unwrap();
        // V3 creates this table; the pre-existing incompatible name makes the
        // step fail after its ALTER statements and exercises transaction scope.
        conn.execute("CREATE TABLE domain_state(id TEXT PRIMARY KEY)", [])
            .unwrap();
        assert!(migrate(&conn).is_err());
        let version: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "2");
        let has_revision: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('observations') WHERE name = 'current_revision_id')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!has_revision);
    }
}
