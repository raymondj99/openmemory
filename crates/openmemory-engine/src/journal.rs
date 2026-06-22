//! The bus's durability layer: per-shard append-only journals.
//!
//! Every accepted submission is appended (buffered) to its shard's
//! JSONL journal before the ticket is returned, fsynced at each epoch
//! flush before the SQLite commit, checkpointed INSIDE the commit
//! transaction (`engine:journal:<shard>` in `memory_meta`), and
//! reclaimed by the maintenance tick only once a complete WAL
//! checkpoint makes the commit power-loss durable. On startup,
//! `replay` applies exactly the entries above the checkpoint:
//! committed-but-untruncated entries are skipped, journaled-but-
//! uncommitted entries are applied once, and torn trailing lines from
//! a crash mid-append are detected and dropped.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openmemory_graph::batch::BatchOptions;
use openmemory_graph::{MemoryResult, RememberRequest};
use serde::{Deserialize, Serialize};

use crate::partition::DomainStore;

/// One journaled submission. The `seq` ties the JSONL line to the
/// shard's checkpoint watermark.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct JournalRecord {
    pub(crate) seq: u64,
    pub(crate) req: RememberRequest,
}

/// Checkpoint key for a shard's journal watermark in `memory_meta`.
pub(crate) fn checkpoint_key(shard: usize) -> String {
    format!("engine:journal:{shard}")
}

pub(crate) fn journal_path(dir: &Path, shard: usize) -> PathBuf {
    dir.join(format!("shard-{shard}.jsonl"))
}

/// Append one record to a shard journal. Buffered; the caller owns
/// flush/fsync cadence. Best-effort by design: a journal I/O error
/// degrades durability, never availability.
pub(crate) fn append(journal: &mut BufWriter<File>, seq: u64, req: &RememberRequest) {
    let record = JournalRecord {
        seq,
        req: req.clone(),
    };
    if let Ok(line) = serde_json::to_string(&record) {
        let _ = journal.write_all(line.as_bytes());
        let _ = journal.write_all(b"\n");
    }
}

/// Truncate a journal whose every entry is covered by a durable
/// checkpoint. The caller must hold the shard queue lock (appends
/// happen under the same lock, so no entry can slip in mid-truncate).
pub(crate) fn truncate(journal: &mut BufWriter<File>) -> bool {
    let _ = journal.flush();
    let file = journal.get_ref();
    if file.set_len(0).is_ok() {
        let _ = file.sync_data();
        true
    } else {
        false
    }
}

/// Replay one shard's journal: apply every record above the committed
/// checkpoint in one batch (checkpoint advances atomically with it).
/// Returns `(last_seq_seen, replayed_count)` so the caller can resume
/// sequence numbering.
///
/// The checkpoint lives in the shard's domain store (the same database
/// the batch commits into), so the watermark and the data can never
/// disagree. Cross-domain mirror edges are re-issued by the replay
/// commit — mirrors are at-least-once under crash recovery.
pub(crate) fn replay(
    domains: &Arc<DomainStore>,
    dir: &Path,
    shard: usize,
    normalize: bool,
) -> MemoryResult<(u64, u64)> {
    let domain = shard % domains.domains();
    let store = &domains.stores()[domain];
    let key = checkpoint_key(shard);
    let checkpoint = store.get_checkpoint(&key)?.unwrap_or(0);
    let path = journal_path(dir, shard);
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((checkpoint, 0));
        }
        Err(e) => return Err(e.into()),
    };

    let mut last_seq = checkpoint;
    let mut pending: Vec<RememberRequest> = Vec::new();
    let mut max_pending_seq = 0u64;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // A torn trailing line (crash mid-append) fails to parse; skip
        // it. Its seq was never acknowledged as durable.
        let Ok(record) = serde_json::from_str::<JournalRecord>(&line) else {
            tracing::warn!(shard, "skipping unparseable journal line (torn write?)");
            continue;
        };
        last_seq = last_seq.max(record.seq);
        if record.seq > checkpoint {
            max_pending_seq = max_pending_seq.max(record.seq);
            pending.push(record.req);
        }
    }

    let replayed = pending.len() as u64;
    if !pending.is_empty() {
        domains.remember_batch_in_domain_with_options(
            domain,
            &pending,
            &BatchOptions {
                normalize,
                checkpoint: Some((key, max_pending_seq)),
            },
        )?;
        tracing::info!(shard, count = replayed, "replayed journal entries");
    }
    Ok((last_seq, replayed))
}
