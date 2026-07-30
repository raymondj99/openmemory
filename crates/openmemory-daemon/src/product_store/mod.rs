//! Product control-plane SQLite ownership.

mod identity;
mod jobs;
mod merges;
mod schema;
mod spaces;

use std::path::{Path, PathBuf};
use std::time::Duration;

use openmemory_admin::{AdminEvent, AdminJob};
use rusqlite::{params, Connection};
use thiserror::Error;

pub(crate) use spaces::{CatalogSpace, CatalogSpaceState, RootKey};

const PRODUCT_DIR: &str = "product";
const PRODUCT_DB_FILE: &str = "product.sqlite";

#[derive(Debug, Error)]
pub(crate) enum ProductStoreError {
    #[error("product metadata filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("product metadata database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("product metadata JSON operation failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("product metadata schema version {found} is newer than supported version {supported}")]
    UnsupportedSchema { found: i64, supported: i64 },
    #[error("product metadata schema version is invalid: {0}")]
    InvalidSchemaVersion(String),
    #[error("invalid product control-plane value: {0}")]
    InvalidValue(String),
    #[error("product control-plane state is corrupt: {0}")]
    Corrupt(String),
}

#[derive(Debug, Clone)]
pub(crate) struct ProductStore {
    path: PathBuf,
}

impl ProductStore {
    pub(crate) fn open(home: &Path) -> Result<Self, ProductStoreError> {
        let dir = home.join(PRODUCT_DIR);
        std::fs::create_dir_all(&dir)?;
        let store = Self {
            path: dir.join(PRODUCT_DB_FILE),
        };
        store.initialize()?;
        Ok(store)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn load_jobs(&self) -> Result<Vec<AdminJob>, ProductStoreError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT job_json FROM daemon_jobs
             ORDER BY created_at_unix_secs ASC, id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(serde_json::from_str(&row?)?);
        }
        Ok(jobs)
    }

    pub(crate) fn next_event_sequence(&self) -> Result<u64, ProductStoreError> {
        let conn = self.connect()?;
        let next = conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM daemon_events",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        Ok(next)
    }

    pub(crate) fn upsert_job(&self, job: &AdminJob) -> Result<(), ProductStoreError> {
        let conn = self.connect()?;
        let job_json = serde_json::to_string(job)?;
        let state = serde_json::to_string(&job.state)?;
        let kind = serde_json::to_string(&job.kind)?;
        conn.execute(
            "INSERT INTO daemon_jobs (
                 id, kind_json, state_json, profile, created_at_unix_secs,
                 updated_at_unix_secs, job_json
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                 kind_json = excluded.kind_json,
                 state_json = excluded.state_json,
                 profile = excluded.profile,
                 updated_at_unix_secs = excluded.updated_at_unix_secs,
                 job_json = excluded.job_json",
            params![
                job.id,
                kind,
                state,
                job.profile,
                job.created_at_unix_secs,
                job.finished_at_unix_secs
                    .or(job.started_at_unix_secs)
                    .unwrap_or(job.created_at_unix_secs),
                job_json,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn insert_event(&self, event: &AdminEvent) -> Result<(), ProductStoreError> {
        let conn = self.connect()?;
        let event_json = serde_json::to_string(event)?;
        let event_type = serde_json::to_string(&event.event_type)?;
        let job_id = event.job.as_ref().map(|job| job.id.as_str());
        conn.execute(
            "INSERT INTO daemon_events (
                 sequence, unix_secs, event_type_json, job_id, event_json
             )
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.sequence,
                event.unix_secs,
                event_type,
                job_id,
                event_json,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn events_after(
        &self,
        sequence: u64,
        limit: usize,
    ) -> Result<Vec<AdminEvent>, ProductStoreError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT event_json FROM daemon_events
             WHERE sequence > ?1
             ORDER BY sequence ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![sequence, limit as u64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(serde_json::from_str(&row?)?);
        }
        Ok(events)
    }

    fn initialize(&self) -> Result<(), ProductStoreError> {
        let mut conn = self.connect()?;
        schema::migrate(&mut conn)?;
        Ok(())
    }

    pub(crate) fn connect(&self) -> Result<Connection, ProductStoreError> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }
}
