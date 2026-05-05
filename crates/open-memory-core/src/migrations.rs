use rusqlite::Connection;

use crate::error::{OmError, OmResult};

pub struct Migrator<'a> {
    conn: &'a Connection,
    table: &'static str,
}

impl<'a> Migrator<'a> {
    pub fn new(conn: &'a Connection, table: &'static str) -> Self {
        Self { conn, table }
    }

    pub fn current(&self) -> OmResult<u32> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [self.table],
                |row| row.get::<_, i64>(0).map(|_| true),
            )
            .unwrap_or(false);
        if !exists {
            return Ok(0);
        }

        let sql = format!(
            "SELECT value FROM {} WHERE key = 'schema_version'",
            self.table
        );
        let raw: Option<String> = self.conn.query_row(&sql, [], |row| row.get(0)).ok();
        match raw {
            None => Ok(0),
            Some(s) => s.parse::<u32>().map_err(|e| {
                OmError::Migration {
                    version: 0,
                    reason: format!("invalid schema_version row: {e}"),
                }
            }),
        }
    }

    pub fn apply(&self, target: u32, steps: &[(u32, &str)]) -> OmResult<()> {
        let create_sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            self.table
        );
        self.conn.execute_batch(&create_sql)?;

        let current = self.current()?;
        if current > target {
            return Err(OmError::SchemaTooNew {
                current,
                max: target,
            });
        }

        for &(version, sql) in steps {
            if version <= current || version > target {
                continue;
            }
            self.conn.execute_batch("BEGIN")?;
            let result: OmResult<()> = (|| {
                self.conn.execute_batch(sql).map_err(|e| OmError::Migration {
                    version,
                    reason: e.to_string(),
                })?;
                let upsert = format!(
                    "INSERT OR REPLACE INTO {} (key, value) VALUES ('schema_version', ?1)",
                    self.table
                );
                self.conn.execute(&upsert, [version.to_string()])?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    self.conn.execute_batch("COMMIT")?;
                }
                Err(e) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }
        }

        if self.current()? < target {
            let upsert = format!(
                "INSERT OR REPLACE INTO {} (key, value) VALUES ('schema_version', ?1)",
                self.table
            );
            self.conn.execute(&upsert, [target.to_string()])?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn current_returns_zero_for_fresh_db() {
        let conn = open();
        let m = Migrator::new(&conn, "meta");
        assert_eq!(m.current().unwrap(), 0);
    }

    #[test]
    fn apply_runs_steps_in_order() {
        let conn = open();
        let m = Migrator::new(&conn, "meta");
        m.apply(
            2,
            &[
                (1, "CREATE TABLE a (id INTEGER PRIMARY KEY)"),
                (2, "CREATE TABLE b (id INTEGER PRIMARY KEY)"),
            ],
        )
        .unwrap();
        assert_eq!(m.current().unwrap(), 2);

        for t in ["a", "b"] {
            conn.query_row(
                "SELECT 1 FROM sqlite_master WHERE name = ?1",
                [t],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        }
    }

    #[test]
    fn apply_skips_already_run_steps() {
        let conn = open();
        let m = Migrator::new(&conn, "meta");
        m.apply(1, &[(1, "CREATE TABLE a (id INTEGER PRIMARY KEY)")])
            .unwrap();
        m.apply(
            2,
            &[
                (1, "CREATE TABLE a (id INTEGER PRIMARY KEY)"),
                (2, "CREATE TABLE b (id INTEGER PRIMARY KEY)"),
            ],
        )
        .unwrap();
        assert_eq!(m.current().unwrap(), 2);
    }

    #[test]
    fn apply_rejects_future_db() {
        let conn = open();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta (key, value) VALUES ('schema_version', '5');",
        )
        .unwrap();

        let m = Migrator::new(&conn, "meta");
        let err = m.apply(2, &[]).unwrap_err();
        assert!(matches!(err, OmError::SchemaTooNew { current: 5, max: 2 }));
    }

    #[test]
    fn apply_rolls_back_failing_step() {
        let conn = open();
        let m = Migrator::new(&conn, "meta");
        let err = m
            .apply(
                2,
                &[
                    (1, "CREATE TABLE a (id INTEGER PRIMARY KEY)"),
                    (2, "INVALID SQL"),
                ],
            )
            .unwrap_err();
        assert!(matches!(err, OmError::Migration { version: 2, .. }));
        assert_eq!(m.current().unwrap(), 1);
    }

    #[test]
    fn apply_records_target_when_no_steps() {
        let conn = open();
        let m = Migrator::new(&conn, "meta");
        m.apply(7, &[]).unwrap();
        assert_eq!(m.current().unwrap(), 7);
    }
}
