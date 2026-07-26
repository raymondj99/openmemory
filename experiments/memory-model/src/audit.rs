//! Transactional semantic mutations with an inspectable audit ledger.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{PocError, PocResult};

const DB_FILE: &str = "audit.sqlite";
const SCHEMA_VERSION: i64 = 1;

/// Current, recall-visible state of one logical memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub logical_id: String,
    pub revision_id: String,
    pub content: String,
    pub row_version: u64,
    pub deleted: bool,
    pub previous_revision_id: Option<String>,
}

impl MemorySnapshot {
    /// Semantic equality intentionally excludes storage revisions.
    #[must_use]
    pub fn semantically_eq(&self, other: &Self) -> bool {
        self.logical_id == other.logical_id
            && self.content == other.content
            && self.deleted == other.deleted
    }
}

/// A typed mutation. Expected versions make review approval optimistic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftOp {
    Add {
        logical_id: String,
        content: String,
    },
    /// Correct the active revision in place while retaining its revision ID.
    Patch {
        logical_id: String,
        expected_version: u64,
        content: String,
    },
    /// Record changed knowledge as a new immutable semantic revision.
    Supersede {
        logical_id: String,
        expected_version: u64,
        content: String,
    },
    Delete {
        logical_id: String,
        expected_version: u64,
    },
    Restore {
        logical_id: String,
        expected_version: u64,
    },
    /// Administrative upsert used by an explicit material merge.
    MergePut {
        logical_id: String,
        expected_version: Option<u64>,
        content: String,
    },
}

impl DraftOp {
    fn logical_id(&self) -> &str {
        match self {
            Self::Add { logical_id, .. }
            | Self::Patch { logical_id, .. }
            | Self::Supersede { logical_id, .. }
            | Self::Delete { logical_id, .. }
            | Self::Restore { logical_id, .. }
            | Self::MergePut { logical_id, .. } => logical_id,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Add { .. } => "add",
            Self::Patch { .. } => "patch",
            Self::Supersede { .. } => "supersede",
            Self::Delete { .. } => "delete",
            Self::Restore { .. } => "restore",
            Self::MergePut { .. } => "merge_put",
        }
    }
}

/// Caller-authored changeset before review or application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSetDraft {
    pub idempotency_key: String,
    pub actor: String,
    pub reason: String,
    pub ops: Vec<DraftOp>,
}

impl ChangeSetDraft {
    #[must_use]
    pub fn new(
        idempotency_key: impl Into<String>,
        actor: impl Into<String>,
        reason: impl Into<String>,
        ops: Vec<DraftOp>,
    ) -> Self {
        Self {
            idempotency_key: idempotency_key.into(),
            actor: actor.into(),
            reason: reason.into(),
            ops,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitMode {
    Propose,
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetState {
    Proposed,
    Applied,
    Rejected,
}

impl ChangeSetState {
    fn parse(value: &str) -> PocResult<Self> {
        match value {
            "proposed" => Ok(Self::Proposed),
            "applied" => Ok(Self::Applied),
            "rejected" => Ok(Self::Rejected),
            other => Err(PocError::Invalid(format!(
                "unknown changeset state {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSetReceipt {
    pub id: String,
    pub state: ChangeSetState,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditedOp {
    pub ordinal: u64,
    pub logical_id: String,
    pub kind: String,
    pub before: Option<MemorySnapshot>,
    pub after: Option<MemorySnapshot>,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
}

/// Explicit subprocess-only crash injection points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyCrashPoint {
    AfterLedgerInsert,
    AfterCanonicalMutation,
    AfterCommit,
}

/// A small, independently-openable `SQLite` store.
///
/// Every method opens its own connection. This is uninteresting overhead for
/// the POC, but makes concurrent reviewer tests model separate daemon clients
/// instead of accidentally sharing a process mutex.
#[derive(Debug, Clone)]
pub struct AuditStore {
    root: PathBuf,
    db_path: PathBuf,
}

impl AuditStore {
    /// Open or initialize an audit store.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory, database, or schema cannot be
    /// opened or initialized.
    pub fn open(root: &Path) -> PocResult<Self> {
        std::fs::create_dir_all(root)?;
        let store = Self {
            root: root.to_path_buf(),
            db_path: root.join(DB_FILE),
        };
        let mut conn = store.connect()?;
        Self::migrate(&mut conn)?;
        Ok(store)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn connect(&self) -> PocResult<Connection> {
        let conn = Connection::open(&self.db_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;",
        )?;
        Ok(conn)
    }

    fn migrate(conn: &mut Connection) -> PocResult<()> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS poc_meta (
                 schema_version INTEGER NOT NULL,
                 generation INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS memory_revisions (
                 revision_id TEXT PRIMARY KEY,
                 logical_id TEXT NOT NULL,
                 content TEXT NOT NULL,
                 state TEXT NOT NULL CHECK(state IN ('active','superseded','deleted')),
                 previous_revision_id TEXT REFERENCES memory_revisions(revision_id),
                 created_by_change_id TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_revisions_logical
                 ON memory_revisions(logical_id);
             CREATE TABLE IF NOT EXISTS memory_heads (
                 logical_id TEXT PRIMARY KEY,
                 revision_id TEXT NOT NULL REFERENCES memory_revisions(revision_id),
                 row_version INTEGER NOT NULL CHECK(row_version > 0),
                 deleted INTEGER NOT NULL CHECK(deleted IN (0,1)),
                 updated_by_change_id TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS change_sets (
                 id TEXT PRIMARY KEY,
                 idempotency_key TEXT NOT NULL UNIQUE,
                 request_hash TEXT NOT NULL,
                 actor TEXT NOT NULL,
                 reason TEXT NOT NULL,
                 state TEXT NOT NULL CHECK(state IN ('proposed','applied','rejected')),
                 base_generation INTEGER NOT NULL,
                 committed_generation INTEGER,
                 created_at INTEGER NOT NULL,
                 committed_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS change_ops (
                 change_id TEXT NOT NULL REFERENCES change_sets(id),
                 ordinal INTEGER NOT NULL,
                 logical_id TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 requested_json TEXT NOT NULL,
                 before_json TEXT,
                 after_json TEXT,
                 before_hash TEXT,
                 after_hash TEXT,
                 PRIMARY KEY(change_id, ordinal)
             );
             CREATE TABLE IF NOT EXISTS review_decisions (
                 change_id TEXT NOT NULL REFERENCES change_sets(id),
                 reviewer TEXT NOT NULL,
                 decision TEXT NOT NULL CHECK(decision IN ('approved','rejected')),
                 decided_at INTEGER NOT NULL,
                 PRIMARY KEY(change_id, reviewer)
             );
             CREATE TABLE IF NOT EXISTS compact_change_sets (
                 id TEXT PRIMARY KEY,
                 idempotency_key TEXT NOT NULL UNIQUE,
                 actor TEXT NOT NULL,
                 reason TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );",
        )?;
        let rows: i64 = tx.query_row("SELECT count(*) FROM poc_meta", [], |row| row.get(0))?;
        if rows == 0 {
            tx.execute(
                "INSERT INTO poc_meta(schema_version, generation) VALUES (?1, 0)",
                [SCHEMA_VERSION],
            )?;
        }
        let version: i64 =
            tx.query_row("SELECT schema_version FROM poc_meta", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Err(PocError::Invalid(format!(
                "unsupported POC schema {version}, expected {SCHEMA_VERSION}"
            )));
        }
        tx.commit()?;
        Ok(())
    }

    /// Submit a proposed or immediately applied semantic change.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, idempotency conflicts, stale
    /// versions, or a failed database transaction.
    pub fn submit(&self, draft: &ChangeSetDraft, mode: SubmitMode) -> PocResult<ChangeSetReceipt> {
        self.submit_inner(draft, mode, None)
    }

    /// Abort the process at a precise durability boundary. Only crash-worker
    /// subprocesses should call this method.
    ///
    /// # Errors
    ///
    /// Returns the same validation and storage errors as [`Self::submit`]
    /// before the configured abort point is reached.
    pub fn submit_with_crash_point(
        &self,
        draft: &ChangeSetDraft,
        point: ApplyCrashPoint,
    ) -> PocResult<ChangeSetReceipt> {
        self.submit_inner(draft, SubmitMode::Apply, Some(point))
    }

    fn submit_inner(
        &self,
        draft: &ChangeSetDraft,
        mode: SubmitMode,
        crash_point: Option<ApplyCrashPoint>,
    ) -> PocResult<ChangeSetReceipt> {
        validate_draft(draft)?;
        let request_json = serde_json::to_vec(draft)?;
        let request_hash = blake3::hash(&request_json).to_hex().to_string();
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(existing) = existing_receipt(&tx, &draft.idempotency_key, &request_hash)? {
            return Ok(existing);
        }

        let id = Uuid::now_v7().to_string();
        let generation = generation(&tx)?;
        tx.execute(
            "INSERT INTO change_sets(
                 id, idempotency_key, request_hash, actor, reason, state,
                 base_generation, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'proposed', ?6, ?7)",
            params![
                id,
                draft.idempotency_key,
                request_hash,
                draft.actor,
                draft.reason,
                generation,
                now_secs()
            ],
        )?;
        for (ordinal, op) in draft.ops.iter().enumerate() {
            tx.execute(
                "INSERT INTO change_ops(
                     change_id, ordinal, logical_id, kind, requested_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id,
                    i64::try_from(ordinal).map_err(|_| {
                        PocError::Invalid("too many changeset operations".to_string())
                    })?,
                    op.logical_id(),
                    op.kind(),
                    serde_json::to_string(op)?
                ],
            )?;
        }

        maybe_abort(crash_point, ApplyCrashPoint::AfterLedgerInsert);

        if mode == SubmitMode::Propose {
            tx.commit()?;
            return Ok(ChangeSetReceipt {
                id,
                state: ChangeSetState::Proposed,
                generation,
            });
        }

        let committed_generation = apply_ops(&tx, &id, &draft.ops)?;
        maybe_abort(crash_point, ApplyCrashPoint::AfterCanonicalMutation);
        mark_applied(&tx, &id, committed_generation)?;
        tx.commit()?;
        maybe_abort(crash_point, ApplyCrashPoint::AfterCommit);
        Ok(ChangeSetReceipt {
            id,
            state: ChangeSetState::Applied,
            generation: committed_generation,
        })
    }

    /// Atomically approve and apply a proposed changeset.
    ///
    /// # Errors
    ///
    /// Returns an error if the proposal is absent, no longer pending, stale,
    /// invalid, or cannot be committed.
    pub fn approve(&self, change_id: &str, reviewer: &str) -> PocResult<ChangeSetReceipt> {
        if reviewer.trim().is_empty() {
            return Err(PocError::Invalid("reviewer must not be empty".to_string()));
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, generation) = changeset_state(&tx, change_id)?;
        if state == ChangeSetState::Applied {
            return Ok(ChangeSetReceipt {
                id: change_id.to_string(),
                state,
                generation,
            });
        }
        if state != ChangeSetState::Proposed {
            return Err(PocError::Conflict(format!(
                "changeset {change_id} is {state:?}"
            )));
        }
        let ops = load_ops(&tx, change_id)?;
        let committed_generation = apply_ops(&tx, change_id, &ops)?;
        tx.execute(
            "INSERT OR IGNORE INTO review_decisions(change_id, reviewer, decision, decided_at)
             VALUES (?1, ?2, 'approved', ?3)",
            params![change_id, reviewer, now_secs()],
        )?;
        mark_applied(&tx, change_id, committed_generation)?;
        tx.commit()?;
        Ok(ChangeSetReceipt {
            id: change_id.to_string(),
            state: ChangeSetState::Applied,
            generation: committed_generation,
        })
    }

    /// Reject a proposed changeset without modifying live memory.
    ///
    /// # Errors
    ///
    /// Returns an error if the proposal is absent, no longer pending, or the
    /// decision cannot be committed.
    pub fn reject(&self, change_id: &str, reviewer: &str) -> PocResult<()> {
        if reviewer.trim().is_empty() {
            return Err(PocError::Invalid("reviewer must not be empty".to_string()));
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, _) = changeset_state(&tx, change_id)?;
        if state != ChangeSetState::Proposed {
            return Err(PocError::Conflict(format!(
                "changeset {change_id} is {state:?}"
            )));
        }
        tx.execute(
            "INSERT OR IGNORE INTO review_decisions(change_id, reviewer, decision, decided_at)
             VALUES (?1, ?2, 'rejected', ?3)",
            params![change_id, reviewer, now_secs()],
        )?;
        tx.execute(
            "UPDATE change_sets SET state = 'rejected', committed_at = ?2 WHERE id = ?1",
            params![change_id, now_secs()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Read one logical memory, including a retained tombstone.
    ///
    /// # Errors
    ///
    /// Returns an error when the database read fails.
    pub fn get(&self, logical_id: &str) -> PocResult<Option<MemorySnapshot>> {
        let conn = self.connect()?;
        load_snapshot(&conn, logical_id)
    }

    /// List recall-visible memory heads.
    ///
    /// # Errors
    ///
    /// Returns an error when the database read fails.
    pub fn visible(&self) -> PocResult<Vec<MemorySnapshot>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT h.logical_id, h.revision_id, r.content, h.row_version,
                    h.deleted, r.previous_revision_id
             FROM memory_heads h
             JOIN memory_revisions r ON r.revision_id = h.revision_id
             WHERE h.deleted = 0
             ORDER BY h.logical_id",
        )?;
        let rows = stmt.query_map([], snapshot_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List all logical heads, including tombstones.
    ///
    /// # Errors
    ///
    /// Returns an error when the database read fails.
    pub fn snapshot(&self) -> PocResult<Vec<MemorySnapshot>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT h.logical_id, h.revision_id, r.content, h.row_version,
                    h.deleted, r.previous_revision_id
             FROM memory_heads h
             JOIN memory_revisions r ON r.revision_id = h.revision_id
             ORDER BY h.logical_id",
        )?;
        let rows = stmt.query_map([], snapshot_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Read the monotonically increasing semantic generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the database read fails.
    pub fn generation(&self) -> PocResult<u64> {
        generation(&self.connect()?)
    }

    /// Read a changeset state.
    ///
    /// # Errors
    ///
    /// Returns an error when the changeset is absent or the database read
    /// fails.
    pub fn changeset_state(&self, change_id: &str) -> PocResult<ChangeSetState> {
        let conn = self.connect()?;
        changeset_state(&conn, change_id).map(|(state, _)| state)
    }

    /// Read ordered before/after audit records for a changeset.
    ///
    /// # Errors
    ///
    /// Returns an error when storage access or payload decoding fails.
    pub fn audit_ops(&self, change_id: &str) -> PocResult<Vec<AuditedOp>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT ordinal, logical_id, kind, before_json, after_json,
                    before_hash, after_hash
             FROM change_ops WHERE change_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map([change_id], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (ordinal, logical_id, kind, before, after, before_hash, after_hash) = row?;
            Ok(AuditedOp {
                ordinal,
                logical_id,
                kind,
                before: before.as_deref().map(serde_json::from_str).transpose()?,
                after: after.as_deref().map(serde_json::from_str).transpose()?,
                before_hash,
                after_hash,
            })
        })
        .collect()
    }

    /// Produce a transactionally consistent `SQLite` copy for staging.
    ///
    /// # Errors
    ///
    /// Returns an error if the destination exists or the snapshot cannot be
    /// created and initialized.
    pub fn snapshot_to(&self, destination_root: &Path) -> PocResult<Self> {
        if destination_root.exists() {
            return Err(PocError::Invalid(format!(
                "staging destination already exists: {}",
                destination_root.display()
            )));
        }
        std::fs::create_dir_all(destination_root)?;
        let destination = destination_root.join(DB_FILE);
        let conn = self.connect()?;
        conn.execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])?;
        Self::open(destination_root)
    }

    /// Unlogged control path used only to quantify ledger overhead. It is not
    /// a candidate production API because it intentionally violates the audit
    /// invariant.
    #[doc(hidden)]
    pub fn direct_add_for_benchmark(&self, logical_id: &str, content: &str) -> PocResult<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if load_snapshot(&tx, logical_id)?.is_some() {
            return Err(PocError::Conflict(format!(
                "logical memory {logical_id:?} already exists"
            )));
        }
        insert_new_head(&tx, "unlogged-benchmark-control", logical_id, content, None)?;
        tx.commit()?;
        Ok(())
    }

    /// Compact control: immutable revisions carry the before/after content,
    /// while one changeset header carries actor, reason, and idempotency.
    #[doc(hidden)]
    pub fn compact_audited_add_for_benchmark(
        &self,
        idempotency_key: &str,
        logical_id: &str,
        content: &str,
    ) -> PocResult<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if load_snapshot(&tx, logical_id)?.is_some() {
            return Err(PocError::Conflict(format!(
                "logical memory {logical_id:?} already exists"
            )));
        }
        let change_id = Uuid::now_v7().to_string();
        tx.execute(
            "INSERT INTO compact_change_sets(
                 id, idempotency_key, actor, reason, created_at
             ) VALUES (?1, ?2, 'benchmark', 'compact immutable audit', ?3)",
            params![change_id, idempotency_key, now_secs()],
        )?;
        insert_new_head(&tx, &change_id, logical_id, content, None)?;
        tx.commit()?;
        Ok(())
    }

    #[doc(hidden)]
    pub fn durable_bytes_for_benchmark(&self) -> PocResult<u64> {
        let conn = self.connect()?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        [
            self.db_path.clone(),
            self.db_path.with_extension("sqlite-wal"),
            self.db_path.with_extension("sqlite-shm"),
        ]
        .into_iter()
        .try_fold(0_u64, |total, path| match std::fs::metadata(path) {
            Ok(metadata) => Ok(total.saturating_add(metadata.len())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(total),
            Err(error) => Err(PocError::Io(error)),
        })
    }

    #[doc(hidden)]
    pub fn ledger_payload_bytes_for_benchmark(&self) -> PocResult<(u64, u64)> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT
                 COALESCE(SUM(length(requested_json)), 0),
                 COALESCE(SUM(length(before_json)), 0)
                   + COALESCE(SUM(length(after_json)), 0)
                   + COALESCE(SUM(length(before_hash)), 0)
                   + COALESCE(SUM(length(after_hash)), 0)
             FROM change_ops",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(Into::into)
    }
}

fn validate_draft(draft: &ChangeSetDraft) -> PocResult<()> {
    if draft.idempotency_key.trim().is_empty() {
        return Err(PocError::Invalid(
            "idempotency key must not be empty".to_string(),
        ));
    }
    if draft.actor.trim().is_empty() {
        return Err(PocError::Invalid("actor must not be empty".to_string()));
    }
    if draft.reason.trim().is_empty() {
        return Err(PocError::Invalid("reason must not be empty".to_string()));
    }
    if draft.ops.is_empty() {
        return Err(PocError::Invalid(
            "changeset must contain at least one operation".to_string(),
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for op in &draft.ops {
        if op.logical_id().trim().is_empty() {
            return Err(PocError::Invalid(
                "logical ID must not be empty".to_string(),
            ));
        }
        if !ids.insert(op.logical_id()) {
            return Err(PocError::Invalid(format!(
                "changeset mutates logical ID {:?} more than once",
                op.logical_id()
            )));
        }
        let content = match op {
            DraftOp::Add { content, .. }
            | DraftOp::Patch { content, .. }
            | DraftOp::Supersede { content, .. }
            | DraftOp::MergePut { content, .. } => Some(content),
            DraftOp::Delete { .. } | DraftOp::Restore { .. } => None,
        };
        if content.is_some_and(|value| value.trim().is_empty()) {
            return Err(PocError::Invalid("content must not be empty".to_string()));
        }
    }
    Ok(())
}

fn existing_receipt(
    tx: &Transaction<'_>,
    key: &str,
    request_hash: &str,
) -> PocResult<Option<ChangeSetReceipt>> {
    let row = tx
        .query_row(
            "SELECT id, request_hash, state,
                    COALESCE(committed_generation, base_generation)
             FROM change_sets WHERE idempotency_key = ?1",
            [key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((id, existing_hash, state, generation)) = row else {
        return Ok(None);
    };
    if existing_hash != request_hash {
        return Err(PocError::Conflict(format!(
            "idempotency key {key:?} was reused with a different request"
        )));
    }
    Ok(Some(ChangeSetReceipt {
        id,
        state: ChangeSetState::parse(&state)?,
        generation,
    }))
}

fn load_ops(tx: &Transaction<'_>, change_id: &str) -> PocResult<Vec<DraftOp>> {
    let mut stmt = tx.prepare(
        "SELECT requested_json FROM change_ops
         WHERE change_id = ?1 ORDER BY ordinal",
    )?;
    let rows = stmt.query_map([change_id], |row| row.get::<_, String>(0))?;
    rows.map(|row| {
        let json = row?;
        serde_json::from_str(&json).map_err(PocError::from)
    })
    .collect()
}

fn apply_ops(tx: &Transaction<'_>, change_id: &str, ops: &[DraftOp]) -> PocResult<u64> {
    for (ordinal, op) in ops.iter().enumerate() {
        let before = load_snapshot(tx, op.logical_id())?;
        apply_op(tx, change_id, op, before.as_ref())?;
        let after = load_snapshot(tx, op.logical_id())?;
        let before_json = before.as_ref().map(serde_json::to_string).transpose()?;
        let after_json = after.as_ref().map(serde_json::to_string).transpose()?;
        tx.execute(
            "UPDATE change_ops
             SET before_json = ?3, after_json = ?4,
                 before_hash = ?5, after_hash = ?6
             WHERE change_id = ?1 AND ordinal = ?2",
            params![
                change_id,
                i64::try_from(ordinal).map_err(|_| {
                    PocError::Invalid("too many changeset operations".to_string())
                })?,
                before_json,
                after_json,
                snapshot_hash(before.as_ref())?,
                snapshot_hash(after.as_ref())?
            ],
        )?;
    }
    let next = generation(tx)?
        .checked_add(1)
        .ok_or_else(|| PocError::Invalid("generation counter overflowed".to_string()))?;
    tx.execute("UPDATE poc_meta SET generation = ?1", [next])?;
    Ok(next)
}

fn apply_op(
    tx: &Transaction<'_>,
    change_id: &str,
    op: &DraftOp,
    before: Option<&MemorySnapshot>,
) -> PocResult<()> {
    match op {
        DraftOp::Add {
            logical_id,
            content,
        } => {
            if before.is_some() {
                return Err(PocError::Conflict(format!(
                    "logical memory {logical_id:?} already exists"
                )));
            }
            insert_new_head(tx, change_id, logical_id, content, None)?;
        }
        DraftOp::Patch {
            logical_id,
            expected_version,
            content,
        } => {
            let current = require_version(before, logical_id, *expected_version)?;
            if current.deleted {
                return Err(PocError::Conflict(format!(
                    "cannot patch deleted memory {logical_id:?}"
                )));
            }
            tx.execute(
                "UPDATE memory_revisions SET content = ?2 WHERE revision_id = ?1",
                params![current.revision_id, content],
            )?;
            bump_head(tx, change_id, logical_id, current.row_version, false)?;
        }
        DraftOp::Supersede {
            logical_id,
            expected_version,
            content,
        } => {
            let current = require_version(before, logical_id, *expected_version)?;
            if current.deleted {
                return Err(PocError::Conflict(format!(
                    "cannot supersede deleted memory {logical_id:?}"
                )));
            }
            supersede(tx, change_id, current, content)?;
        }
        DraftOp::Delete {
            logical_id,
            expected_version,
        } => {
            let current = require_version(before, logical_id, *expected_version)?;
            if current.deleted {
                return Err(PocError::Conflict(format!(
                    "memory {logical_id:?} is already deleted"
                )));
            }
            tx.execute(
                "UPDATE memory_revisions SET state = 'deleted' WHERE revision_id = ?1",
                [current.revision_id.as_str()],
            )?;
            bump_head(tx, change_id, logical_id, current.row_version, true)?;
        }
        DraftOp::Restore {
            logical_id,
            expected_version,
        } => {
            let current = require_version(before, logical_id, *expected_version)?;
            if !current.deleted {
                return Err(PocError::Conflict(format!(
                    "memory {logical_id:?} is not deleted"
                )));
            }
            tx.execute(
                "UPDATE memory_revisions SET state = 'active' WHERE revision_id = ?1",
                [current.revision_id.as_str()],
            )?;
            bump_head(tx, change_id, logical_id, current.row_version, false)?;
        }
        DraftOp::MergePut {
            logical_id,
            expected_version,
            content,
        } => match (before, expected_version) {
            (None, None) => insert_new_head(tx, change_id, logical_id, content, None)?,
            (Some(current), Some(expected)) if current.row_version == *expected => {
                supersede(tx, change_id, current, content)?;
            }
            (current, expected) => {
                return Err(PocError::Conflict(format!(
                    "merge expected {logical_id:?} at version {expected:?}, found {:?}",
                    current.map(|value| value.row_version)
                )));
            }
        },
    }
    Ok(())
}

fn insert_new_head(
    tx: &Transaction<'_>,
    change_id: &str,
    logical_id: &str,
    content: &str,
    previous_revision_id: Option<&str>,
) -> PocResult<()> {
    let revision_id = Uuid::now_v7().to_string();
    tx.execute(
        "INSERT INTO memory_revisions(
             revision_id, logical_id, content, state,
             previous_revision_id, created_by_change_id
         ) VALUES (?1, ?2, ?3, 'active', ?4, ?5)",
        params![
            revision_id,
            logical_id,
            content,
            previous_revision_id,
            change_id
        ],
    )?;
    tx.execute(
        "INSERT INTO memory_heads(
             logical_id, revision_id, row_version, deleted, updated_by_change_id
         ) VALUES (?1, ?2, 1, 0, ?3)",
        params![logical_id, revision_id, change_id],
    )?;
    Ok(())
}

fn supersede(
    tx: &Transaction<'_>,
    change_id: &str,
    current: &MemorySnapshot,
    content: &str,
) -> PocResult<()> {
    tx.execute(
        "UPDATE memory_revisions SET state = 'superseded' WHERE revision_id = ?1",
        [current.revision_id.as_str()],
    )?;
    let revision_id = Uuid::now_v7().to_string();
    tx.execute(
        "INSERT INTO memory_revisions(
             revision_id, logical_id, content, state,
             previous_revision_id, created_by_change_id
         ) VALUES (?1, ?2, ?3, 'active', ?4, ?5)",
        params![
            revision_id,
            current.logical_id,
            content,
            current.revision_id,
            change_id
        ],
    )?;
    tx.execute(
        "UPDATE memory_heads
         SET revision_id = ?2, row_version = row_version + 1,
             deleted = 0, updated_by_change_id = ?3
         WHERE logical_id = ?1 AND row_version = ?4",
        params![
            current.logical_id,
            revision_id,
            change_id,
            current.row_version
        ],
    )?;
    Ok(())
}

fn bump_head(
    tx: &Transaction<'_>,
    change_id: &str,
    logical_id: &str,
    expected_version: u64,
    deleted: bool,
) -> PocResult<()> {
    let changed = tx.execute(
        "UPDATE memory_heads
         SET row_version = row_version + 1, deleted = ?3, updated_by_change_id = ?4
         WHERE logical_id = ?1 AND row_version = ?2",
        params![logical_id, expected_version, i64::from(deleted), change_id],
    )?;
    if changed != 1 {
        return Err(PocError::Conflict(format!(
            "lost optimistic update for memory {logical_id:?}"
        )));
    }
    Ok(())
}

fn require_version<'a>(
    current: Option<&'a MemorySnapshot>,
    logical_id: &str,
    expected: u64,
) -> PocResult<&'a MemorySnapshot> {
    let current = current.ok_or_else(|| PocError::NotFound(logical_id.to_string()))?;
    if current.row_version != expected {
        return Err(PocError::Conflict(format!(
            "memory {logical_id:?} expected version {expected}, found {}",
            current.row_version
        )));
    }
    Ok(current)
}

fn load_snapshot(conn: &Connection, logical_id: &str) -> PocResult<Option<MemorySnapshot>> {
    conn.query_row(
        "SELECT h.logical_id, h.revision_id, r.content, h.row_version,
                    h.deleted, r.previous_revision_id
             FROM memory_heads h
             JOIN memory_revisions r ON r.revision_id = h.revision_id
             WHERE h.logical_id = ?1",
        [logical_id],
        snapshot_from_row,
    )
    .optional()
    .map_err(Into::into)
}

fn snapshot_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemorySnapshot> {
    Ok(MemorySnapshot {
        logical_id: row.get(0)?,
        revision_id: row.get(1)?,
        content: row.get(2)?,
        row_version: row.get(3)?,
        deleted: row.get::<_, i64>(4)? != 0,
        previous_revision_id: row.get(5)?,
    })
}

fn snapshot_hash(snapshot: Option<&MemorySnapshot>) -> PocResult<Option<String>> {
    snapshot
        .map(|value| {
            serde_json::to_vec(value)
                .map(|json| blake3::hash(&json).to_hex().to_string())
                .map_err(PocError::from)
        })
        .transpose()
}

fn generation(conn: &Connection) -> PocResult<u64> {
    conn.query_row("SELECT generation FROM poc_meta", [], |row| row.get(0))
        .map_err(Into::into)
}

fn changeset_state(conn: &Connection, change_id: &str) -> PocResult<(ChangeSetState, u64)> {
    let row = conn
        .query_row(
            "SELECT state, COALESCE(committed_generation, base_generation)
             FROM change_sets WHERE id = ?1",
            [change_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
        )
        .optional()?;
    let (state, generation) = row.ok_or_else(|| PocError::NotFound(change_id.to_string()))?;
    Ok((ChangeSetState::parse(&state)?, generation))
}

fn mark_applied(tx: &Transaction<'_>, change_id: &str, generation: u64) -> PocResult<()> {
    tx.execute(
        "UPDATE change_sets
         SET state = 'applied', committed_generation = ?2, committed_at = ?3
         WHERE id = ?1 AND state = 'proposed'",
        params![change_id, generation, now_secs()],
    )?;
    Ok(())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn maybe_abort(configured: Option<ApplyCrashPoint>, reached: ApplyCrashPoint) {
    if configured == Some(reached) {
        std::process::abort();
    }
}
