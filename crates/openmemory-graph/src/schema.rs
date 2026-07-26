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
pub const MEMORY_SCHEMA_VERSION: u32 = 4;

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
        &[(1, V1_SQL), (2, V2_SQL), (3, V3_SQL), (4, V4_SQL)],
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

/// v3 — immutable observation history, optimistic changesets, and a durable
/// derived-index outbox. Existing rows remain valid and acquire a baseline
/// lazily on their first semantic mutation.
const V3_SQL: &str = "
ALTER TABLE observations ADD COLUMN current_revision_id TEXT;
ALTER TABLE observations ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE observations ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';

CREATE TABLE domain_state (
    singleton             INTEGER PRIMARY KEY CHECK(singleton = 1),
    semantic_generation   INTEGER NOT NULL DEFAULT 0,
    indexed_generation    INTEGER NOT NULL DEFAULT 0,
    mirror_generation     INTEGER NOT NULL DEFAULT 0,
    backfill_state        TEXT NOT NULL DEFAULT 'pending'
);
INSERT OR IGNORE INTO domain_state(singleton) VALUES(1);

CREATE TABLE change_sets (
    id                    TEXT PRIMARY KEY,
    space_id              TEXT NOT NULL,
    idempotency_key       TEXT NOT NULL,
    request_hash          BLOB NOT NULL,
    actor_principal       TEXT NOT NULL,
    actor_kind            TEXT NOT NULL,
    authorization_generation INTEGER NOT NULL,
    reason                TEXT NOT NULL,
    source                TEXT NOT NULL,
    state                 TEXT NOT NULL CHECK(state IN
                              ('proposed','applied','rejected','conflicted','reverted')),
    base_generation       INTEGER NOT NULL,
    committed_generation  INTEGER,
    created_at            INTEGER NOT NULL,
    decided_at            INTEGER,
    decided_by            TEXT,
    UNIQUE(space_id, idempotency_key)
);

CREATE INDEX idx_change_sets_state_created
    ON change_sets(state, created_at, id);

CREATE TABLE change_requests (
    change_set_id         TEXT NOT NULL REFERENCES change_sets(id),
    ordinal               INTEGER NOT NULL,
    payload_version       INTEGER NOT NULL,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    operation             TEXT NOT NULL,
    expected_revision_id  TEXT,
    expected_row_version  INTEGER,
    requested_payload_json TEXT NOT NULL,
    PRIMARY KEY(change_set_id, ordinal)
) WITHOUT ROWID;

CREATE TABLE change_events (
    change_set_id         TEXT NOT NULL REFERENCES change_sets(id),
    ordinal               INTEGER NOT NULL,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    operation             TEXT NOT NULL,
    before_revision_id    TEXT,
    after_revision_id     TEXT,
    before_lifecycle      TEXT,
    after_lifecycle       TEXT,
    compact_payload_json  TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(change_set_id, ordinal)
) WITHOUT ROWID;

CREATE TABLE observation_revisions (
    id                    TEXT PRIMARY KEY,
    observation_id        TEXT NOT NULL REFERENCES observations(id),
    parent_revision_id    TEXT,
    semantic_hash         BLOB NOT NULL,
    content               TEXT NOT NULL,
    observed_at           INTEGER NOT NULL,
    valid_from            INTEGER,
    valid_until           INTEGER,
    confidence            REAL NOT NULL,
    source                TEXT NOT NULL,
    memory_tier           TEXT NOT NULL,
    title                 TEXT,
    summary               TEXT,
    importance            REAL,
    source_kind           TEXT,
    created_by_change_set TEXT NOT NULL REFERENCES change_sets(id),
    created_at            INTEGER NOT NULL,
    UNIQUE(observation_id, semantic_hash, created_by_change_set)
);

CREATE INDEX idx_observation_revisions_object_created
    ON observation_revisions(observation_id, created_at, id);

CREATE TABLE observation_revision_concepts (
    revision_id           TEXT NOT NULL REFERENCES observation_revisions(id),
    concept               TEXT NOT NULL,
    PRIMARY KEY(revision_id, concept)
) WITHOUT ROWID;

CREATE TABLE observation_revision_source_files (
    revision_id           TEXT NOT NULL REFERENCES observation_revisions(id),
    file_path             TEXT NOT NULL,
    PRIMARY KEY(revision_id, file_path)
) WITHOUT ROWID;

CREATE TABLE index_outbox (
    generation            INTEGER NOT NULL,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    operation             TEXT NOT NULL CHECK(operation IN ('upsert','delete')),
    revision_id           TEXT,
    attempts              INTEGER NOT NULL DEFAULT 0,
    last_error            TEXT,
    PRIMARY KEY(generation, object_kind, logical_id)
) WITHOUT ROWID;
";

/// v4 — entity/relation immutable history, verified identifiers, origin
/// contributions, and explicitly repairable partition mirrors.
const V4_SQL: &str = "
DROP INDEX IF EXISTS idx_entities_name_type;
CREATE INDEX idx_entities_name_type ON entities(name, entity_type);

ALTER TABLE entities ADD COLUMN current_revision_id TEXT;
ALTER TABLE entities ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE entities ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';

ALTER TABLE relations ADD COLUMN canonical_relation_id TEXT;
ALTER TABLE relations ADD COLUMN mirror_role TEXT NOT NULL DEFAULT 'canonical';
ALTER TABLE relations ADD COLUMN current_revision_id TEXT;
ALTER TABLE relations ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE relations ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';

CREATE UNIQUE INDEX idx_relations_canonical_role
    ON relations(canonical_relation_id, mirror_role)
    WHERE canonical_relation_id IS NOT NULL;

CREATE TABLE entity_revisions (
    id                    TEXT PRIMARY KEY,
    entity_id             TEXT NOT NULL REFERENCES entities(id),
    parent_revision_id    TEXT,
    semantic_hash         BLOB NOT NULL,
    name                  TEXT NOT NULL,
    entity_type           TEXT NOT NULL,
    confidence            REAL NOT NULL,
    source                TEXT NOT NULL,
    created_by_change_set TEXT NOT NULL REFERENCES change_sets(id),
    created_at            INTEGER NOT NULL
);

CREATE INDEX idx_entity_revisions_object_created
    ON entity_revisions(entity_id, created_at, id);

CREATE TABLE entity_revision_aliases (
    revision_id           TEXT NOT NULL REFERENCES entity_revisions(id),
    alias                 TEXT NOT NULL,
    PRIMARY KEY(revision_id, alias)
) WITHOUT ROWID;

CREATE TABLE entity_revision_identifiers (
    revision_id           TEXT NOT NULL REFERENCES entity_revisions(id),
    namespace             TEXT NOT NULL,
    raw_value             TEXT NOT NULL,
    canonical_value       TEXT,
    trust                 TEXT NOT NULL CHECK(trust IN ('claimed','source_verified')),
    source_snapshot_id    TEXT,
    verifier_version      TEXT,
    resolver_generation   INTEGER,
    PRIMARY KEY(revision_id, namespace, raw_value)
) WITHOUT ROWID;

CREATE INDEX idx_verified_identifier_lookup
    ON entity_revision_identifiers(namespace, canonical_value, revision_id)
    WHERE trust = 'source_verified' AND canonical_value IS NOT NULL;

CREATE TABLE relation_revisions (
    id                    TEXT PRIMARY KEY,
    relation_id           TEXT NOT NULL,
    parent_revision_id    TEXT,
    semantic_hash         BLOB NOT NULL,
    from_entity           TEXT NOT NULL,
    to_entity             TEXT NOT NULL,
    relation_type         TEXT NOT NULL,
    weight                REAL NOT NULL,
    valid_from            INTEGER,
    valid_until           INTEGER,
    source                TEXT NOT NULL,
    evidence_json         TEXT NOT NULL DEFAULT '{}',
    created_by_change_set TEXT NOT NULL REFERENCES change_sets(id),
    created_at            INTEGER NOT NULL
);

CREATE INDEX idx_relation_revisions_object_created
    ON relation_revisions(relation_id, created_at, id);

CREATE TABLE origin_contributions (
    object_kind           TEXT NOT NULL,
    target_logical_id     TEXT NOT NULL,
    origin_space_id       TEXT NOT NULL,
    origin_logical_id     TEXT NOT NULL,
    origin_revision_id    TEXT NOT NULL,
    semantic_hash         BLOB NOT NULL,
    contribution_json     TEXT NOT NULL,
    imported_by_change_set TEXT NOT NULL REFERENCES change_sets(id),
    created_at            INTEGER NOT NULL,
    PRIMARY KEY(object_kind, target_logical_id,
                origin_space_id, origin_logical_id, origin_revision_id)
) WITHOUT ROWID;

CREATE TABLE mirror_outbox (
    generation            INTEGER NOT NULL,
    canonical_relation_id TEXT NOT NULL,
    operation             TEXT NOT NULL CHECK(operation IN ('upsert','delete')),
    target_domain         INTEGER NOT NULL,
    payload_json          TEXT NOT NULL,
    attempts              INTEGER NOT NULL DEFAULT 0,
    last_error            TEXT,
    PRIMARY KEY(generation, canonical_relation_id, target_domain)
) WITHOUT ROWID;

CREATE TABLE destruction_previews (
    confirmation_hash     BLOB PRIMARY KEY,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    scope                 TEXT NOT NULL,
    expected_revision_id  TEXT,
    expected_row_version  INTEGER NOT NULL,
    expected_lifecycle    TEXT NOT NULL,
    expected_generation   INTEGER NOT NULL,
    inventory_json        TEXT NOT NULL,
    expires_at            INTEGER NOT NULL,
    consumed_at           INTEGER
);

CREATE INDEX idx_destruction_previews_expiry
    ON destruction_previews(expires_at);

CREATE TABLE destruction_receipts (
    id                    TEXT PRIMARY KEY,
    object_kind           TEXT NOT NULL,
    logical_id_hash       BLOB NOT NULL,
    scope                 TEXT NOT NULL,
    inventory_hash        BLOB NOT NULL,
    reason_hash           BLOB NOT NULL,
    actor_principal       TEXT NOT NULL,
    semantic_generation   INTEGER NOT NULL,
    destroyed_at          INTEGER NOT NULL
);
";

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
    fn migrate_v1_to_v4_adds_history_schema() {
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
        for col in &[
            "title",
            "summary",
            "importance",
            "source_kind",
            "current_revision_id",
            "row_version",
            "lifecycle",
        ] {
            assert!(cols.contains(&(*col).to_string()), "missing {col}");
        }

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for t in &[
            "observation_concepts",
            "observation_source_files",
            "domain_state",
            "change_sets",
            "change_requests",
            "change_events",
            "observation_revisions",
            "index_outbox",
            "entity_revisions",
            "relation_revisions",
            "origin_contributions",
            "mirror_outbox",
            "destruction_previews",
            "destruction_receipts",
        ] {
            assert!(tables.contains(&(*t).to_string()), "missing table {t}");
        }

        let v: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, "4");
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
    fn populated_v2_fixture_migrates_without_rewriting_truth() {
        let conn = open();
        Migrator::new(&conn, "memory_meta")
            .apply(2, &[(1, V1_SQL), (2, V2_SQL)])
            .unwrap();
        conn.execute(
            "INSERT INTO entities
                (id, name, entity_type, created_at, updated_at, confidence, source)
             VALUES ('e1', 'legacy', 'fact', 10, 11, 0.75, 'fixture')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO observations
                (id, entity_id, content, observed_at, confidence, source, memory_tier,
                 title, importance)
             VALUES ('o1', 'e1', 'preserved', 12, 0.8, 'fixture', 'episodic',
                     'Legacy title', 0.4)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO relations
                (id, from_entity, to_entity, relation_type, created_at, source)
             VALUES ('r1', 'e1', 'e1', 'references', 13, 'fixture')",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let observation: (String, i64, String, Option<String>) = conn
            .query_row(
                "SELECT content, row_version, lifecycle, current_revision_id
                 FROM observations WHERE id = 'o1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            observation,
            ("preserved".to_string(), 1, "active".to_string(), None)
        );
        let relation: (String, i64, String, Option<String>) = conn
            .query_row(
                "SELECT relation_type, row_version, lifecycle, canonical_relation_id
                 FROM relations WHERE id = 'r1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            relation,
            ("references".to_string(), 1, "active".to_string(), None)
        );
        let state: (i64, i64, i64, String) = conn
            .query_row(
                "SELECT semantic_generation, indexed_generation, mirror_generation,
                        backfill_state FROM domain_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(state, (0, 0, 0, "pending".to_string()));
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
    fn entities_allow_reviewed_homonyms_inside_one_materialized_space() {
        let conn = open();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
             VALUES ('1', 'Raymond', 'person', 0, 0)",
            [],
        )
        .unwrap();
        // Logical IDs, not labels, distinguish reviewed homonyms after a
        // cross-space material merge. Serialized write services still reuse
        // an existing name/type during ordinary remember calls.
        conn.execute(
            "INSERT INTO entities (id, name, entity_type, created_at, updated_at)
             VALUES ('2', 'Raymond', 'person', 0, 0)",
            [],
        )
        .unwrap();
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
}
