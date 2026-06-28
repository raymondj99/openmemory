//! The bus core: sharded acceptance, epoch drains, batched commits,
//! durability watermarks, and the maintenance tick.
//!
//! See the crate docs for the full pipeline; this module owns the
//! hot path. `submit` appends to a per-shard queue (plus the journal
//! when enabled) and returns a [`Ticket`] in sub-microsecond time;
//! flusher threads drain whole shards per epoch into single
//! `remember_batch` transactions routed by the partition layer;
//! cacheline-aligned per-shard watermarks publish durability to
//! [`ContextEngine::wait_durable`] with one acquire load.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write as IoWrite};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use openmemory_graph::batch::BatchOptions;
use openmemory_graph::{MemoryError, MemoryResult, MemoryStore, RememberRequest};

use crate::partition::DomainStore;

/// Cacheline-padded atomic counter, per flux-rs `CachelineAlignedAtomic`.
/// 128-byte alignment keeps neighbouring shards' watermarks off the same
/// cacheline so a flush on shard A never false-shares with a
/// `wait_durable` spin on shard B.
#[repr(align(128))]
#[derive(Default)]
struct PaddedAtomicU64(AtomicU64);

/// Receipt for a submitted write. `seq` is the shard-local sequence
/// number; the write is durable once the shard's watermark reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    pub shard: usize,
    pub seq: u64,
}

/// Tuning knobs for [`ContextEngine`].
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Number of shards. Submissions hash by entity name.
    pub shards: usize,
    /// Epoch length: how often flushers drain idle shards.
    pub flush_interval: Duration,
    /// Per-shard queue capacity. `submit` applies backpressure (blocks)
    /// when a shard is full, bounding memory under a write storm.
    pub shard_capacity: usize,
    /// Background flusher threads. The store has a single writer mutex,
    /// so >2 rarely helps; 2 lets one thread group/serialise the next
    /// batch while the other is inside SQLite.
    pub flush_threads: usize,
    /// Run the store's fuzzy entity-name normalization per drained
    /// group. Trusted bulk sources can turn this off; offline
    /// consolidation still dedups.
    pub normalize: bool,
    /// Directory for per-shard crash-recovery journals. `None` disables
    /// journaling (pure write-behind).
    pub journal_dir: Option<PathBuf>,
    /// Maintenance cadence: how often the engine runs a WAL checkpoint,
    /// persists deferred search-index changes, and truncates
    /// fully-committed journals. The engine disables SQLite's
    /// auto-checkpoint while it runs (checkpoint fsyncs move out of
    /// commit paths onto this tick), so this interval also bounds WAL
    /// growth.
    pub checkpoint_interval: Duration,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            shards: 32,
            flush_interval: Duration::from_millis(20),
            shard_capacity: 4096,
            flush_threads: 2,
            normalize: true,
            journal_dir: None,
            checkpoint_interval: Duration::from_secs(1),
        }
    }
}

/// Counters published by the engine. All monotonic.
#[derive(Debug, Default)]
pub struct EngineStats {
    /// Requests accepted by `submit`.
    pub submitted: AtomicU64,
    /// Requests committed to the store (excludes replays).
    pub committed: AtomicU64,
    /// Journal entries replayed at startup.
    pub replayed: AtomicU64,
    /// Shard drains that found work.
    pub flushes: AtomicU64,
    /// Largest single drain, in requests.
    pub max_drain: AtomicU64,
    /// `submit` calls that blocked on a full shard.
    pub backpressure_waits: AtomicU64,
    /// Entity groups dropped after the batch commit AND the per-group
    /// retry both failed.
    pub write_errors: AtomicU64,
    /// Maintenance WAL checkpoint passes.
    pub wal_checkpoints: AtomicU64,
    /// Journal truncations performed by maintenance. A truncation only
    /// happens once every journaled entry is covered by a complete,
    /// power-loss-durable checkpoint.
    pub journal_truncations: AtomicU64,
}

struct ShardQueue {
    buf: Vec<(u64, RememberRequest)>,
    /// Next sequence number to assign. Assigned under the queue lock, so
    /// drain order always matches ticket order. Survives restarts via
    /// the checkpoint + journal scan in `start`.
    next_seq: u64,
    /// Append-side journal handle; `None` when journaling is off.
    journal: Option<BufWriter<File>>,
}

struct Shard {
    queue: Mutex<ShardQueue>,
    /// Serialises drains of this shard (steady-state flusher + quiesce
    /// can overlap). Without it, two drains could commit out of seq
    /// order and a crash between them would break exactly-once replay.
    drain_lock: Mutex<()>,
    /// Signalled by the flusher after a drain; `submit` waits on this
    /// when the shard is at capacity.
    space: Condvar,
    /// Durability watermark: highest seq committed to SQLite.
    durable: PaddedAtomicU64,
}

struct Inner {
    domains: Arc<DomainStore>,
    shards: Vec<Shard>,
    opts: EngineOptions,
    stats: EngineStats,
    shutdown: AtomicBool,
    /// Flusher wakeup: signalled when a shard crosses the early-flush
    /// threshold or at shutdown.
    wake: Mutex<()>,
    wake_cv: Condvar,
}

impl Inner {
    /// The domain that owns shard `idx`. Entity hashes agree by
    /// construction: `start_partitioned` requires the domain count to
    /// divide the shard count, so `(hash % shards) % domains ==
    /// hash % domains` for every entity.
    fn domain_of_shard(&self, idx: usize) -> usize {
        idx % self.domains.domains()
    }

    /// The store holding shard `idx`'s entities and checkpoint.
    fn store_of_shard(&self, idx: usize) -> &Arc<MemoryStore> {
        &self.domains.stores()[self.domain_of_shard(idx)]
    }
}

/// Concurrent ingestion front-end over a shared [`MemoryStore`] (or a
/// domain-partitioned family of them, via
/// [`ContextEngine::start_partitioned`]).
pub struct ContextEngine {
    inner: Arc<Inner>,
    flushers: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for ContextEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextEngine")
            .field("shards", &self.inner.shards.len())
            .field("domains", &self.inner.domains.domains())
            .field("flush_threads", &self.flushers.len())
            .finish_non_exhaustive()
    }
}

impl ContextEngine {
    /// Start the engine over a single store. Convenience wrapper around
    /// [`Self::start_partitioned`] with one domain.
    pub fn start(store: Arc<MemoryStore>, opts: EngineOptions) -> MemoryResult<Self> {
        Self::start_partitioned(Arc::new(DomainStore::from_single(store)), opts)
    }

    /// Start the engine over a domain-partitioned store and spawn the
    /// background flusher threads. Each shard maps statically onto one
    /// domain (`shard % domains`), so a whole shard drains into a single
    /// domain transaction; the domain count must divide the shard count
    /// for the entity hash to agree at both levels.
    ///
    /// With a journal directory configured this first replays any
    /// journal entries newer than each shard's committed checkpoint
    /// (crash recovery), then resumes sequence numbering where the
    /// previous process stopped. Journals are reclaimed by the first
    /// maintenance pass once a WAL checkpoint covers the replay.
    pub fn start_partitioned(domains: Arc<DomainStore>, opts: EngineOptions) -> MemoryResult<Self> {
        let shard_count = opts.shards.max(1);
        if shard_count % domains.domains() != 0 {
            return Err(MemoryError::InvalidInput(format!(
                "engine shards ({shard_count}) must be a multiple of the domain \
                 count ({}) so entity hashes agree at both levels",
                domains.domains()
            )));
        }

        let mut replayed_total = 0u64;
        let mut queues = Vec::with_capacity(shard_count);
        for idx in 0..shard_count {
            let (next_seq, journal) = match &opts.journal_dir {
                Some(dir) => {
                    std::fs::create_dir_all(dir)?;
                    let (last_seq, replayed) =
                        crate::journal::replay(&domains, dir, idx, opts.normalize)?;
                    replayed_total += replayed;
                    // Re-open in append mode WITHOUT truncating: the
                    // replay commit is durable against process crash but
                    // not yet against power loss (WAL, synchronous=NORMAL).
                    // Truncation happens in `maintenance` only after a
                    // complete checkpoint covers every journaled entry;
                    // until then replay's checkpoint filter keeps old
                    // entries idempotent.
                    let file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(crate::journal::journal_path(dir, idx))?;
                    (last_seq, Some(BufWriter::new(file)))
                }
                None => (0, None),
            };
            queues.push(ShardQueue {
                buf: Vec::new(),
                next_seq,
                journal,
            });
        }

        let shards: Vec<Shard> = queues
            .into_iter()
            .map(|q| {
                let durable = PaddedAtomicU64::default();
                durable.0.store(q.next_seq, Ordering::Relaxed);
                Shard {
                    queue: Mutex::new(q),
                    drain_lock: Mutex::new(()),
                    space: Condvar::new(),
                    durable,
                }
            })
            .collect();

        let inner = Arc::new(Inner {
            domains,
            shards,
            opts: opts.clone(),
            stats: EngineStats::default(),
            shutdown: AtomicBool::new(false),
            wake: Mutex::new(()),
            wake_cv: Condvar::new(),
        });
        inner
            .stats
            .replayed
            .store(replayed_total, Ordering::Relaxed);

        // The engine owns checkpointing for the lifetime of its stores:
        // SQLite's default auto-checkpoint (1000 pages) would otherwise
        // run inside drain commits — roughly ten fsync-bearing stalls per
        // second at full ingest. `shutdown` restores the default.
        for store in inner.domains.stores() {
            if let Err(e) = store.set_wal_autocheckpoint(0) {
                tracing::warn!(error = %e, "could not disable WAL auto-checkpoint");
            }
        }

        // One maintenance pass up front: checkpoints the replay commits
        // and reclaims any journals they fully cover.
        maintenance(&inner);

        let threads = opts.flush_threads.max(1);
        let flushers = (0..threads)
            .map(|worker| {
                let inner = Arc::clone(&inner);
                std::thread::Builder::new()
                    .name(format!("om-engine-flush-{worker}"))
                    .spawn(move || flusher_loop(&inner, worker, threads))
                    .expect("spawn flusher thread")
            })
            .collect();

        Ok(Self { inner, flushers })
    }

    /// Hash an entity name onto its shard. Same partition idea as
    /// flux-rs domains: writes to different shards share no locks.
    fn shard_for(&self, entity: &str) -> usize {
        (crate::partition::hash_lowercase_key(entity) as usize) % self.inner.shards.len()
    }

    /// Enqueue a write. Returns immediately with a [`Ticket`] unless the
    /// target shard is at capacity, in which case it blocks until the
    /// flusher drains it (backpressure, not data loss).
    ///
    /// With journaling on, the request is appended to the shard journal
    /// before this returns; it is fsynced at the next epoch flush.
    pub fn submit(&self, req: RememberRequest) -> Ticket {
        let shard_idx = self.shard_for(&req.name);
        let shard = &self.inner.shards[shard_idx];

        let mut q = shard.queue.lock().expect("shard queue poisoned");
        while q.buf.len() >= self.inner.opts.shard_capacity {
            self.inner
                .stats
                .backpressure_waits
                .fetch_add(1, Ordering::Relaxed);
            self.inner.wake_cv.notify_all();
            q = shard.space.wait(q).expect("shard queue poisoned");
        }
        q.next_seq += 1;
        let seq = q.next_seq;
        if let Some(journal) = q.journal.as_mut() {
            // Buffered append under the queue lock; ordering in the file
            // matches seq order by construction.
            crate::journal::append(journal, seq, &req);
        }
        q.buf.push((seq, req));
        let early_flush = q.buf.len() >= self.inner.opts.shard_capacity / 2;
        drop(q);

        self.inner.stats.submitted.fetch_add(1, Ordering::Relaxed);
        if early_flush {
            self.inner.wake_cv.notify_all();
        }
        Ticket {
            shard: shard_idx,
            seq,
        }
    }

    /// Block until the ticket's write is committed to SQLite. Spin with
    /// backoff on the shard watermark (flux-style: `Acquire` load on the
    /// counter is the only synchronisation; the data needs nothing).
    pub fn wait_durable(&self, ticket: Ticket) {
        let durable = &self.inner.shards[ticket.shard].durable.0;
        let mut spins = 0u32;
        while durable.load(Ordering::Acquire) < ticket.seq {
            spins += 1;
            if spins < 64 {
                std::hint::spin_loop();
            } else if spins < 256 {
                std::thread::yield_now();
            } else {
                // park_timeout instead of sleep: same bounded wait, and
                // spurious wakeups are harmless because the loop re-checks
                // the watermark.
                std::thread::park_timeout(Duration::from_micros(100));
            }
        }
    }

    /// Drain every shard from the calling thread and wait until all
    /// previously issued tickets are durable.
    pub fn quiesce(&self) {
        let watermarks: Vec<u64> = self
            .inner
            .shards
            .iter()
            .map(|s| s.queue.lock().expect("shard queue poisoned").next_seq)
            .collect();
        for idx in 0..self.inner.shards.len() {
            drain_shard(&self.inner, idx);
        }
        for (idx, target) in watermarks.into_iter().enumerate() {
            self.wait_durable(Ticket {
                shard: idx,
                seq: target,
            });
        }
    }

    pub fn stats(&self) -> &EngineStats {
        &self.inner.stats
    }

    /// The domain-partitioned store this engine commits into. Read
    /// surfaces share it so recall and lookups see the same domains the
    /// engine writes.
    pub fn domain_store(&self) -> &Arc<DomainStore> {
        &self.inner.domains
    }

    /// Flush outstanding work, make it power-loss durable, and stop the
    /// flusher threads. Leaves the store with fully checkpointed WALs,
    /// truncated journals, and SQLite's default auto-checkpoint
    /// restored (the store usually outlives the engine).
    pub fn shutdown(mut self) {
        self.quiesce();
        self.inner.shutdown.store(true, Ordering::Release);
        self.inner.wake_cv.notify_all();
        for handle in self.flushers.drain(..) {
            let _ = handle.join();
        }
        maintenance(&self.inner);
        for store in self.inner.domains.stores() {
            if let Err(e) = store.set_wal_autocheckpoint(1000) {
                tracing::warn!(error = %e, "could not restore WAL auto-checkpoint");
            }
        }
    }
}

fn flusher_loop(inner: &Arc<Inner>, worker: usize, total_workers: usize) {
    let mut last_maintenance = std::time::Instant::now();
    loop {
        {
            let guard = inner.wake.lock().expect("wake mutex poisoned");
            let _unused = inner
                .wake_cv
                .wait_timeout(guard, inner.opts.flush_interval)
                .expect("wake mutex poisoned");
        }
        let shutting_down = inner.shutdown.load(Ordering::Acquire);
        // Static round-robin shard ownership, like flux's contiguous
        // domain assignment: no two workers fight over a drain in the
        // steady state (quiesce may overlap; the per-shard drain lock
        // serialises that).
        for idx in (worker..inner.shards.len()).step_by(total_workers) {
            drain_shard(inner, idx);
        }
        // Worker 0 doubles as the maintenance thread: checkpoint
        // cadence, deferred index persistence, journal reclamation.
        if worker == 0 && last_maintenance.elapsed() >= inner.opts.checkpoint_interval {
            maintenance(inner);
            last_maintenance = std::time::Instant::now();
        }
        if shutting_down {
            return;
        }
    }
}

/// One maintenance pass: persist deferred search-index changes, run a
/// PASSIVE WAL checkpoint, and — when the checkpoint covers every
/// journaled entry — truncate the shard journals.
///
/// The truncation predicate is deliberately conservative. Watermarks
/// are snapshotted BEFORE the checkpoint: only commits at or below the
/// snapshot are provably inside the checkpointed WAL. A shard's journal
/// is reclaimed only when (a) the checkpoint transferred every frame
/// (`complete`), (b) the shard has no queued work, and (c) no sequence
/// was issued past the snapshot — checked under the queue lock, which
/// journal appends also hold, so no entry can slip in mid-truncate.
/// Anything else just waits for the next pass; replay's checkpoint
/// filter keeps a long-lived journal idempotent in the meantime.
fn maintenance(inner: &Inner) {
    for store in inner.domains.stores() {
        if let Err(e) = store.persist_search_index() {
            tracing::warn!(error = %e, "maintenance: search-index persist failed");
        }
    }

    let watermarks: Vec<u64> = inner
        .shards
        .iter()
        .map(|s| s.durable.0.load(Ordering::Acquire))
        .collect();
    let mut all_complete = true;
    for store in inner.domains.stores() {
        match store.wal_checkpoint() {
            Ok(report) => all_complete &= report.complete,
            Err(e) => {
                tracing::warn!(error = %e, "maintenance: WAL checkpoint failed");
                return;
            }
        }
    }
    inner.stats.wal_checkpoints.fetch_add(1, Ordering::Relaxed);

    if !all_complete || inner.opts.journal_dir.is_none() {
        return;
    }
    for (idx, shard) in inner.shards.iter().enumerate() {
        let mut q = shard.queue.lock().expect("shard queue poisoned");
        if !q.buf.is_empty() || q.next_seq != watermarks[idx] {
            continue;
        }
        if let Some(journal) = q.journal.as_mut() {
            if crate::journal::truncate(journal) {
                inner
                    .stats
                    .journal_truncations
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Drain one shard: swap out its queue, fsync the journal, group by
/// entity, and commit the whole drain as ONE batched transaction with
/// the shard checkpoint. Advances the durability watermark with
/// `fetch_max(Release)`.
fn drain_shard(inner: &Inner, idx: usize) {
    let shard = &inner.shards[idx];
    // Serialise drains of this shard so checkpoints advance in seq order.
    let _drain_guard = shard.drain_lock.lock().expect("drain lock poisoned");

    let (drained, journal_file) = {
        let mut q = shard.queue.lock().expect("shard queue poisoned");
        if q.buf.is_empty() {
            return;
        }
        // Push buffered journal lines to the OS under the lock (cheap);
        // the expensive fsync happens below on a cloned handle, outside
        // the lock, so submitters never stall behind disk latency.
        let journal_file = q.journal.as_mut().and_then(|journal| {
            let _ = journal.flush();
            journal.get_ref().try_clone().ok()
        });
        (std::mem::take(&mut q.buf), journal_file)
    };
    shard.space.notify_all();
    if let Some(file) = journal_file {
        // Make every drained entry crash-safe before the SQLite commit.
        let _ = file.sync_data();
    }

    inner.stats.flushes.fetch_add(1, Ordering::Relaxed);
    inner
        .stats
        .max_drain
        .fetch_max(drained.len() as u64, Ordering::Relaxed);

    let max_seq = drained.last().map_or(0, |(seq, _)| *seq);
    let drained_len = drained.len() as u64;

    // Group by entity so N requests about one entity become one group;
    // the whole drain then commits as one transaction.
    let mut order: Vec<(String, openmemory_graph::EntityType, String)> = Vec::new();
    let mut groups: HashMap<(String, openmemory_graph::EntityType, String), RememberRequest> =
        HashMap::new();
    for (_, req) in drained {
        let key = (req.name.to_lowercase(), req.entity_type, req.source.clone());
        if let Some(existing) = groups.get_mut(&key) {
            existing.observations.extend(req.observations);
            existing.relations.extend(req.relations);
        } else {
            order.push(key.clone());
            groups.insert(key, req);
        }
    }
    let batch: Vec<RememberRequest> = order
        .iter()
        .map(|key| groups.remove(key).expect("group present"))
        .collect();

    let domain = inner.domain_of_shard(idx);
    let key = crate::journal::checkpoint_key(idx);
    let opts = BatchOptions {
        normalize: inner.opts.normalize,
        checkpoint: Some((key.clone(), max_seq)),
    };
    match inner
        .domains
        .remember_batch_in_domain_with_options(domain, &batch, &opts)
    {
        Ok(_) => {
            inner
                .stats
                .committed
                .fetch_add(drained_len, Ordering::Relaxed);
        }
        Err(err) => {
            // The batch rolled back as a whole. Retry per group so one
            // poisoned request doesn't sink its neighbours; then advance
            // the checkpoint regardless so journal replay never loops on
            // the poisoned group.
            tracing::warn!(shard = idx, error = %err, "batch commit failed; retrying per group");
            for group in &batch {
                let retry = inner.domains.remember_batch_in_domain_with_options(
                    domain,
                    std::slice::from_ref(group),
                    &BatchOptions {
                        normalize: inner.opts.normalize,
                        checkpoint: None,
                    },
                );
                match retry {
                    Ok(_) => {
                        inner
                            .stats
                            .committed
                            .fetch_add((group.observations.len().max(1)) as u64, Ordering::Relaxed);
                    }
                    Err(err) => {
                        inner.stats.write_errors.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            shard = idx,
                            entity = %group.name,
                            error = %err,
                            "dropping entity group after failed retry"
                        );
                    }
                }
            }
            let _ = inner.store_of_shard(idx).remember_batch(
                &[],
                &BatchOptions {
                    normalize: false,
                    checkpoint: Some((key, max_seq)),
                },
            );
        }
    }

    // Publish: everything drained this epoch is now visible to readers
    // and waiters. The journal is NOT truncated here — this commit is
    // durable against process crash, but not against power loss until a
    // WAL checkpoint covers it; `maintenance` reclaims the journal then.
    shard.durable.0.fetch_max(max_seq, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_core::config::Config;
    use openmemory_graph::recall::RecallFilters;
    use openmemory_graph::{EntityType, ObservationInput, RelationInput};

    fn test_store() -> Arc<MemoryStore> {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = Config::default();
        // Leak the tempdir so the store outlives the test body; the OS
        // reclaims the files. open() (not open_in_memory) so the read
        // pool is real.
        let store = MemoryStore::open(&config, dir.keep().as_path()).expect("open store");
        Arc::new(store)
    }

    fn req(entity: &str, content: &str) -> RememberRequest {
        RememberRequest::new(entity, EntityType::Fact)
            .with_observations(vec![ObservationInput::new(content)])
            .with_source("test")
    }

    #[test]
    fn submit_then_wait_durable_persists() {
        let store = test_store();
        let engine = ContextEngine::start(Arc::clone(&store), EngineOptions::default()).unwrap();

        let ticket = engine.submit(req("alpha", "first observation about alpha"));
        engine.wait_durable(ticket);

        let status = store.status().expect("status");
        assert_eq!(status.total_observations, 1);
        engine.shutdown();
    }

    #[test]
    fn concurrent_submitters_lose_nothing() {
        let store = test_store();
        let engine = Arc::new(
            ContextEngine::start(
                Arc::clone(&store),
                EngineOptions {
                    shards: 8,
                    flush_interval: Duration::from_millis(5),
                    ..EngineOptions::default()
                },
            )
            .unwrap(),
        );

        let threads = 16u64;
        let ops = 50u64;
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let engine = Arc::clone(&engine);
                std::thread::spawn(move || {
                    for op in 0..ops {
                        engine.submit(req(
                            &format!("entity-{}", (t * ops + op) % 10),
                            &format!("observation {op} from thread {t}"),
                        ));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("writer thread");
        }
        engine.quiesce();

        let status = store.status().expect("status");
        assert_eq!(status.total_observations, threads * ops);
        assert_eq!(engine.stats().write_errors.load(Ordering::Relaxed), 0);

        Arc::try_unwrap(engine)
            .map_err(|_| ())
            .expect("sole engine ref")
            .shutdown();
    }

    #[test]
    fn flushed_writes_are_recallable() {
        let store = test_store();
        let engine = ContextEngine::start(Arc::clone(&store), EngineOptions::default()).unwrap();

        let ticket = engine.submit(req(
            "quarterly-planning",
            "decided to ship the context engine",
        ));
        engine.wait_durable(ticket);

        let results = store
            .recall("context engine", 5, &RecallFilters::new())
            .expect("recall");
        assert!(!results.is_empty(), "flushed write should be searchable");
        engine.shutdown();
    }

    #[test]
    fn relations_pass_through_to_the_graph() {
        let store = test_store();
        let engine = ContextEngine::start(Arc::clone(&store), EngineOptions::default()).unwrap();

        let ticket = engine.submit(
            RememberRequest::new("Raymond", EntityType::Person)
                .with_observations(vec![ObservationInput::new("builds the context engine")])
                .with_relations(vec![RelationInput::new(
                    "maintains",
                    "openmemory",
                    EntityType::Project,
                )])
                .with_source("test"),
        );
        engine.wait_durable(ticket);

        let raymond = store.get_entity("Raymond").unwrap().unwrap();
        let rels = store.get_entity_relations(&raymond.id).unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relation_type, "maintains");
        engine.shutdown();
    }

    /// Crash simulation: journal entries written, flusher never runs
    /// (huge epoch), engine dropped without shutdown. A fresh engine on
    /// the same store + journal dir must replay exactly the lost writes.
    #[test]
    fn journal_replays_unflushed_writes_after_crash() {
        let store = test_store();
        let journal_dir = tempfile::tempdir().unwrap().keep();
        let opts = EngineOptions {
            shards: 4,
            // Flushers sleep far longer than the test runs: submits stay
            // queue-only, like a crash before the first epoch.
            flush_interval: Duration::from_secs(3600),
            journal_dir: Some(journal_dir.clone()),
            ..EngineOptions::default()
        };

        let engine = ContextEngine::start(Arc::clone(&store), opts.clone()).unwrap();
        for i in 0..10 {
            engine.submit(req(&format!("entity-{i}"), &format!("pre-crash fact {i}")));
        }
        // Force journal pages to the file without draining: flush the
        // BufWriters the way a real crash test would find them. submit()
        // writes through a BufWriter, so simulate the epoch fsync.
        for shard in &engine.inner.shards {
            let mut q = shard.queue.lock().unwrap();
            if let Some(j) = q.journal.as_mut() {
                j.flush().unwrap();
            }
        }
        drop(engine); // "crash": no quiesce, no shutdown
        assert_eq!(store.status().unwrap().total_observations, 0);

        let revived = ContextEngine::start(Arc::clone(&store), opts).unwrap();
        assert_eq!(revived.stats().replayed.load(Ordering::Relaxed), 10);
        assert_eq!(store.status().unwrap().total_observations, 10);
        revived.shutdown();
    }

    /// Exactly-once: committed writes are checkpointed, so a restart
    /// with an untruncated journal must not duplicate them.
    #[test]
    fn journal_replay_skips_checkpointed_entries() {
        let store = test_store();
        let journal_dir = tempfile::tempdir().unwrap().keep();
        let opts = EngineOptions {
            shards: 2,
            flush_interval: Duration::from_millis(5),
            journal_dir: Some(journal_dir.clone()),
            ..EngineOptions::default()
        };

        let engine = ContextEngine::start(Arc::clone(&store), opts.clone()).unwrap();
        for i in 0..20 {
            engine.submit(req(&format!("entity-{i}"), &format!("fact {i}")));
        }
        engine.quiesce();
        assert_eq!(store.status().unwrap().total_observations, 20);
        // Drop without shutdown: journals may or may not be truncated
        // depending on drain timing; replay must rely on the checkpoint,
        // not on truncation.
        drop(engine);

        let revived = ContextEngine::start(Arc::clone(&store), opts).unwrap();
        assert_eq!(
            revived.stats().replayed.load(Ordering::Relaxed),
            0,
            "checkpointed entries must not replay"
        );
        assert_eq!(store.status().unwrap().total_observations, 20);
        revived.shutdown();
    }

    /// A torn trailing journal line (crash mid-append) must not poison
    /// replay of the valid prefix.
    #[test]
    fn journal_replay_tolerates_torn_trailing_line() {
        let store = test_store();
        let journal_dir = tempfile::tempdir().unwrap().keep();
        std::fs::create_dir_all(&journal_dir).unwrap();

        let valid = serde_json::to_string(&crate::journal::JournalRecord {
            seq: 1,
            req: req("alpha", "survives the crash"),
        })
        .unwrap();
        std::fs::write(
            crate::journal::journal_path(&journal_dir, 0),
            format!("{valid}\n{{\"seq\":2,\"req\":{{\"name\":\"tor"),
        )
        .unwrap();

        let engine = ContextEngine::start(
            Arc::clone(&store),
            EngineOptions {
                shards: 1,
                journal_dir: Some(journal_dir),
                ..EngineOptions::default()
            },
        )
        .unwrap();
        assert_eq!(engine.stats().replayed.load(Ordering::Relaxed), 1);
        assert_eq!(store.status().unwrap().total_observations, 1);
        engine.shutdown();
    }

    /// Power-loss safety: a drain commit alone must NOT reclaim the
    /// journal (the commit is not yet checkpoint-durable); the
    /// maintenance pass checkpoints first, then truncates.
    #[test]
    fn journal_truncates_only_after_checkpoint() {
        let store = test_store();
        let journal_dir = tempfile::tempdir().unwrap().keep();
        let opts = EngineOptions {
            shards: 1,
            // Flushers stay asleep; the test drives drains and
            // maintenance explicitly.
            flush_interval: Duration::from_secs(3600),
            checkpoint_interval: Duration::from_secs(3600),
            journal_dir: Some(journal_dir.clone()),
            ..EngineOptions::default()
        };
        let engine = ContextEngine::start(Arc::clone(&store), opts).unwrap();

        let ticket = engine.submit(req("alpha", "must survive power loss"));
        engine.quiesce();
        engine.wait_durable(ticket);

        let path = crate::journal::journal_path(&journal_dir, 0);
        assert!(
            std::fs::metadata(&path).unwrap().len() > 0,
            "journal must survive the drain commit until a checkpoint covers it"
        );

        let truncations_before = engine.stats().journal_truncations.load(Ordering::Relaxed);
        maintenance(&engine.inner);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            0,
            "a complete checkpoint reclaims the journal"
        );
        assert!(engine.stats().journal_truncations.load(Ordering::Relaxed) > truncations_before);
        engine.shutdown();
    }

    /// Maintenance must not reclaim a journal that still covers
    /// uncommitted (queued) submissions.
    #[test]
    fn maintenance_keeps_journal_with_pending_entries() {
        let store = test_store();
        let journal_dir = tempfile::tempdir().unwrap().keep();
        let engine = ContextEngine::start(
            Arc::clone(&store),
            EngineOptions {
                shards: 1,
                flush_interval: Duration::from_secs(3600),
                checkpoint_interval: Duration::from_secs(3600),
                journal_dir: Some(journal_dir.clone()),
                ..EngineOptions::default()
            },
        )
        .unwrap();

        engine.submit(req("alpha", "queued but not drained"));
        // Push the buffered journal line to the file so size is visible.
        {
            let mut q = engine.inner.shards[0].queue.lock().unwrap();
            q.journal.as_mut().unwrap().flush().unwrap();
        }
        maintenance(&engine.inner);
        assert!(
            std::fs::metadata(crate::journal::journal_path(&journal_dir, 0))
                .unwrap()
                .len()
                > 0,
            "journal with undrained entries must survive maintenance"
        );
        engine.shutdown();
    }

    /// Clean shutdown leaves nothing behind: WAL checkpointed, journal
    /// empty, and every write durable.
    #[test]
    fn shutdown_checkpoints_and_reclaims_journals() {
        let store = test_store();
        let journal_dir = tempfile::tempdir().unwrap().keep();
        let engine = ContextEngine::start(
            Arc::clone(&store),
            EngineOptions {
                shards: 2,
                journal_dir: Some(journal_dir.clone()),
                ..EngineOptions::default()
            },
        )
        .unwrap();
        for i in 0..10 {
            engine.submit(req(&format!("entity-{i}"), "shutdown durability"));
        }
        engine.shutdown();

        for shard in 0..2 {
            let len = std::fs::metadata(crate::journal::journal_path(&journal_dir, shard))
                .unwrap()
                .len();
            assert_eq!(len, 0, "shard {shard} journal must be empty after shutdown");
        }
        assert_eq!(store.status().unwrap().total_observations, 10);
    }

    /// Sequence numbering resumes past the journal high-water mark so
    /// new tickets never collide with checkpointed history.
    #[test]
    fn sequence_numbers_resume_after_restart() {
        let store = test_store();
        let journal_dir = tempfile::tempdir().unwrap().keep();
        let opts = EngineOptions {
            shards: 1,
            flush_interval: Duration::from_millis(5),
            journal_dir: Some(journal_dir.clone()),
            ..EngineOptions::default()
        };

        let engine = ContextEngine::start(Arc::clone(&store), opts.clone()).unwrap();
        let last = (0..5)
            .map(|i| engine.submit(req("alpha", &format!("fact {i}"))))
            .last()
            .unwrap();
        engine.quiesce();
        drop(engine);

        let revived = ContextEngine::start(Arc::clone(&store), opts).unwrap();
        let next = revived.submit(req("alpha", "after restart"));
        assert!(
            next.seq > last.seq,
            "seq must resume past the checkpoint ({} <= {})",
            next.seq,
            last.seq
        );
        revived.wait_durable(next);
        assert_eq!(store.status().unwrap().total_observations, 6);
        revived.shutdown();
    }

    fn test_domains(k: usize) -> Arc<crate::partition::DomainStore> {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        Arc::new(
            crate::partition::DomainStore::open(&Config::default(), &dir, k).expect("open domains"),
        )
    }

    #[test]
    fn shard_hash_agrees_with_domain_hash() {
        let domains = test_domains(4);
        let engine = ContextEngine::start_partitioned(
            Arc::clone(&domains),
            EngineOptions {
                shards: 16,
                ..EngineOptions::default()
            },
        )
        .unwrap();

        for name in [
            "Alpha",
            "Project Alpha",
            "STRASSE",
            "Straße",
            "İstanbul",
            "alpha\0bravo",
        ] {
            let shard = engine.shard_for(name);
            assert_eq!(
                engine.inner.domain_of_shard(shard),
                domains.domain_for(name),
                "routing mismatch for {name:?}"
            );
        }
        engine.shutdown();
    }

    #[test]
    fn partitioned_engine_requires_divisible_shards() {
        let domains = test_domains(3);
        let err = ContextEngine::start_partitioned(
            domains,
            EngineOptions {
                shards: 8,
                ..EngineOptions::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("multiple"), "got: {err}");
    }

    #[test]
    fn partitioned_engine_routes_shards_to_domains_and_loses_nothing() {
        let domains = test_domains(4);
        let engine = Arc::new(
            ContextEngine::start_partitioned(
                Arc::clone(&domains),
                EngineOptions {
                    shards: 8,
                    flush_interval: Duration::from_millis(5),
                    ..EngineOptions::default()
                },
            )
            .unwrap(),
        );

        let threads = 8u64;
        let ops = 25u64;
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let engine = Arc::clone(&engine);
                std::thread::spawn(move || {
                    for op in 0..ops {
                        engine.submit(req(
                            &format!("entity-{}", (t * ops + op) % 40),
                            &format!("observation {op} from thread {t}"),
                        ));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("writer thread");
        }
        engine.quiesce();

        let status = domains.status().expect("status");
        assert_eq!(status.total_observations, threads * ops);
        assert_eq!(engine.stats().write_errors.load(Ordering::Relaxed), 0);

        // Every entity is reachable through the facade, and more than
        // one domain holds data.
        assert!(domains.get_entity("entity-0").unwrap().is_some());
        let populated = domains
            .stores()
            .iter()
            .filter(|s| s.status().unwrap().total_observations > 0)
            .count();
        assert!(populated > 1, "expected writes across domains");

        Arc::try_unwrap(engine)
            .map_err(|_| ())
            .expect("sole engine ref")
            .shutdown();
    }

    #[test]
    fn partitioned_engine_mirrors_cross_domain_relations() {
        let domains = test_domains(4);
        let engine = ContextEngine::start_partitioned(
            Arc::clone(&domains),
            EngineOptions {
                shards: 4,
                ..EngineOptions::default()
            },
        )
        .unwrap();

        // Find a pair of names in different domains.
        let a = "source-entity".to_string();
        let b = (0..1000)
            .map(|i| format!("target-{i}"))
            .find(|b| domains.domain_for(b) != domains.domain_for(&a))
            .expect("cross-domain name");

        let ticket = engine.submit(
            RememberRequest::new(a.clone(), EntityType::Person)
                .with_observations(vec![ObservationInput::new("knows the target")])
                .with_relations(vec![RelationInput::new(
                    "knows",
                    b.clone(),
                    EntityType::Person,
                )])
                .with_source("test"),
        );
        engine.wait_durable(ticket);

        let b_entity = domains.get_entity(&b).unwrap().expect("target exists");
        let b_rels = domains.get_entity_relations(&b_entity.id).unwrap();
        assert_eq!(b_rels.len(), 1, "mirror edge visible from the target side");
        assert_eq!(domains.status().unwrap().total_relations, 1);
        engine.shutdown();
    }

    /// Crash recovery at K>1: replay routes each journal entry to its
    /// entity's domain.
    #[test]
    fn partitioned_journal_replay_routes_by_domain() {
        let dir = tempfile::tempdir().unwrap().keep();
        let journal_dir = tempfile::tempdir().unwrap().keep();
        let config = Config::default();
        let opts = EngineOptions {
            shards: 4,
            flush_interval: Duration::from_secs(3600),
            checkpoint_interval: Duration::from_secs(3600),
            journal_dir: Some(journal_dir.clone()),
            ..EngineOptions::default()
        };

        {
            let domains = Arc::new(crate::partition::DomainStore::open(&config, &dir, 4).unwrap());
            let engine =
                ContextEngine::start_partitioned(Arc::clone(&domains), opts.clone()).unwrap();
            for i in 0..12 {
                engine.submit(req(&format!("entity-{i}"), &format!("pre-crash {i}")));
            }
            for shard in &engine.inner.shards {
                let mut q = shard.queue.lock().unwrap();
                if let Some(j) = q.journal.as_mut() {
                    j.flush().unwrap();
                }
            }
            drop(engine); // crash: nothing drained
            assert_eq!(domains.status().unwrap().total_observations, 0);
        }

        let domains = Arc::new(crate::partition::DomainStore::open(&config, &dir, 4).unwrap());
        let revived = ContextEngine::start_partitioned(Arc::clone(&domains), opts).unwrap();
        assert_eq!(revived.stats().replayed.load(Ordering::Relaxed), 12);
        assert_eq!(domains.status().unwrap().total_observations, 12);
        // Every entity must be in its OWN home domain after replay.
        for i in 0..12 {
            let name = format!("entity-{i}");
            let home = domains.domain_for(&name);
            assert!(
                domains.stores()[home].get_entity(&name).unwrap().is_some(),
                "{name} must land in its home domain {home}"
            );
        }
        revived.shutdown();
    }

    #[test]
    fn normalize_off_is_passed_through_to_the_store() {
        let store = test_store();
        let engine = ContextEngine::start(
            Arc::clone(&store),
            EngineOptions {
                normalize: false,
                ..EngineOptions::default()
            },
        )
        .unwrap();

        let t1 = engine.submit(req("Project Alpha", "one"));
        engine.wait_durable(t1);
        let t2 = engine.submit(req("project alpha", "two"));
        engine.wait_durable(t2);

        // Same lowercase key lands on the same shard and the same group,
        // but the exact-match SQL is case-sensitive and fuzzy matching is
        // off: two entities.
        assert_eq!(store.status().unwrap().total_entities, 2);
        engine.shutdown();
    }
}
