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
pub const MEMORY_SCHEMA_VERSION: u32 = 1;

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
    migrator.apply(MEMORY_SCHEMA_VERSION, &[(1, V1_SQL)])?;
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
        assert_eq!(v, "1");
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
}
