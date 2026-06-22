# The context engine

`openmemory-engine` is the concurrency bus between agents and the
memory stores: it accepts writes in microseconds, journals them for
crash safety, routes them by entity hash onto shards and storage
domains, commits whole epochs as single batched transactions, and
publishes durability through lock-free watermarks. Reads run against
the same partitioned facade with a write-version-invalidated merged
cache. This document covers the architecture, the measured results
behind each design decision, and the alternatives that were tried and
rejected. The module-level rustdoc in
`crates/openmemory-engine/src/lib.rs` tells the same story from the
code's point of view (`engine.rs` is the hot path, `journal.rs` the
durability layer, `partition.rs` the domain facade, `migrate.rs`
re-homing, `adapter.rs` source connectors).

## Motivation

Multiple enterprise reports converge on the same diagnosis: agents are
not failing because they lack intelligence; they fail because they lack
access to the right organizational context. The bottleneck is the seam
between the agent and enterprise data (meeting notes, decisions, docs,
chat, audio transcripts).

openmemory already had the right primitives for the read side: a
knowledge graph (entities, observations, relations), hybrid
vector + keyword search, temporal validity, and decay scoring. What it
lacked was an ingestion side that could absorb many concurrent writers
in real time: every mutation serialized on a single
`Mutex<rusqlite::Connection>` and paid a full SQLite transaction plus
index flush per call. One agent at a time was fine; thousands convoyed
(measured: 234-262 writes/s with a 68 s p99 ack at 4000 concurrent
writers).

## The design (borrowed from flux-rs)

flux-rs (`~/Playground/flux-rs`) partitions a node graph into
contiguous per-thread domains, lets each domain write to a local buffer
with relaxed ordering, and publishes to a remote buffer at explicit
synchronization points guarded by cacheline-aligned per-domain epoch
counters (acquire/release ladder). The counter is the synchronization
point, not the data. `crates/openmemory-engine` maps that onto durable
ingestion:

| flux-rs concept | engine equivalent |
|---|---|
| Contiguous node domains per thread | N shards, writes hash by entity name |
| Local buffer (relaxed writes) | Per-shard in-memory queue behind a short per-shard lock |
| Synchronization point | Epoch flush: flusher threads drain whole shards on an interval or when a shard fills |
| Remote buffer (published snapshot) | SQLite WAL + read-only connection pool; readers always see the last flushed epoch, never a torn write |
| Cacheline-aligned per-domain epoch counters | 128-byte-aligned per-shard durability watermark (`AtomicU64`), advanced with `fetch_max(Release)` after commit, read with `Acquire` by `wait_durable` |

## Implemented

### Phase A: ingestion throughput and durability

- **`MemoryStore::remember_batch`** (`openmemory-graph/src/batch.rs`):
  many entity groups, one transaction, one search-index sync. Options:
  `normalize` (skip the fuzzy entity scan for trusted bulk sources) and
  `checkpoint` (a key/value upserted into `memory_meta` inside the same
  transaction; the upsert is monotonic so checkpoints never regress).
- **`ContextEngine`** (`openmemory-engine/src/lib.rs`): sharded
  write-behind queue. `submit(RememberRequest)` returns a `Ticket` in
  microseconds; each epoch, a drain commits the whole shard as one
  batched transaction (requests grouped by entity first), then
  publishes the shard's durability watermark. `wait_durable(ticket)`
  gives lock-free read-your-writes. Full shards block `submit`
  (backpressure, never data loss). Relations pass through. A failed
  batch retries per group so one poisoned request cannot sink its
  neighbours.
- **Crash-durable journal with exactly-once replay**: with
  `journal_dir` set, every accepted request is appended to a per-shard
  JSONL journal before `submit` returns and fsynced at the epoch flush
  before the SQLite commit. The commit carries the shard checkpoint
  (`engine:journal:<shard>` = highest committed seq) in the same
  transaction, so on startup the engine replays exactly the entries
  above the checkpoint: committed-but-untruncated entries are skipped,
  journaled-but-uncommitted entries are applied once. Torn trailing
  lines (crash mid-append) are detected and skipped. Per-shard drains
  serialize on a drain lock so checkpoints advance in seq order.
- **`[engine]` config section** (`openmemory-core`): enabled, shards,
  flush_interval_ms, shard_capacity, flush_threads, durable_ack,
  normalize, journal. Validated; documented in docs/configuration.md.

### Phase B: ingestion surfaces

- **MCP**: `openmemory_remember` routes through the engine when
  `[engine] enabled = true` and returns a receipt
  `{accepted, durable, entity, shard, seq}`. By default it waits for
  the commit (read-your-writes, bounded by one epoch); per-call
  `durable: false` makes it a microsecond fire-and-forget. The CLI
  `openmemory mcp` quiesces the engine on graceful exit; after a crash
  the journal replays instead.
- **`SourceAdapter` trait + reference adapters**
  (`openmemory-engine/src/adapter.rs`): adapters yield batches of
  `RememberRequest`s; the engine stays a dumb, fast, ordered lane.
  Shipped: `MarkdownNotesAdapter` (directory of meeting notes: entity
  per file, observation per `##` section, `Attendees:` lines become
  `has_participant` relations to Person entities) and
  `ChatJsonlAdapter` (Slack-style export: entity per channel,
  observation per message stamped with the message timestamp).
  Audio enters through an external transcription step as one of these
  text shapes.
- **`openmemory ingest <path> [--format markdown|chat]`**: bulk-load a
  source through the engine and quiesce, with `--no-normalize` and
  `--json` flags.

## Measured results

Harness: `cargo run --release -p openmemory-engine --example stress`.
Real OS threads per agent, all released by a barrier at t=0 (burst
pattern), 8 reader threads running `recall` in a loop throughout.
Keyword-only build (no ONNX). Apple Silicon macOS, 2026-06-11.
Every run verifies the store row count equals submissions (no lost
writes).

### 4000 agents x 5 ops over 1000 entities

| metric | direct `remember` | engine | engine + journal |
|---|---|---|---|
| time to all writes durable | 76.3 s (262/s) | 0.61 s (32,742/s) | 0.76 s (26,413/s) |
| write ack p50 / p99 / max | 4.1 ms / 67 s / 76 s | 0.4 us / 19 us / 16 ms | 5.8 us / 32 ms / 39 ms |
| sampled durability lag | n/a | 54 ms | 46 ms |
| recall max latency under load | 15.7 s | 74 ms | 110 ms |
| lost writes | 0 | 0 | 0 |

### 1000 agents x 10 ops over 500 entities

| metric | direct | engine |
|---|---|---|
| time to all writes durable | 40.2 s (249/s) | 0.39 s (25,826/s) |
| write ack p99 | 31.2 s | 2.3 ms |
| recall throughput under load | 702/s | 220,131/s |

### Findings

1. **The single writer mutex was the whole problem, and it fell
   without replacing SQLite.** Direct mode collapses into a convoy
   whose tail grows with writer count. The engine absorbs the same
   burst in milliseconds because acceptance is decoupled from commit.
2. **Batching delivers the durable-throughput win in two stages.**
   Grouping a drained shard by entity (one `remember` per entity)
   reached ~2,200/s; committing the whole drain as one
   `remember_batch` transaction reached ~33,000/s. That is 125x the
   direct path.
3. **Write pressure no longer poisons readers.** `recall` bumps access
   counts through the writer mutex, so direct-mode reader max latency
   reached 15.7 s under the storm. With the engine soaking writes,
   reader max stayed near 100 ms.
4. **The flux watermark pattern transfers cleanly to durability.** A
   per-shard cacheline-aligned counter with acquire/release ordering
   gives every caller a precise, lock-free "is my write committed"
   primitive; the same counter doubles as the journal checkpoint.
5. **Journal cost is ~15-20% durable throughput plus tail ack latency**
   (p95 ~25 ms under a full 20k burst; p50 stays ~6 us). The fsync runs
   outside the shard lock; the remaining tail is queue-lock contention
   during the burst, bounded by the epoch interval.

### Phase C: commit-path optimization and domain scale-out

Implemented after a measured investigation (single-writer ceiling
~66k obs/s on the commit path regardless of transaction size) and an
adversarial verification pass that corrected two assumptions: WAL
commits were checkpoint-punctuated (default 1000-page auto-checkpoint
ran inside drain commits, ~10 fsync stalls/s at full ingest), and
every drain rewrote and fsynced the ENTIRE vectors.bin (O(corpus),
~3 GB per drain at 1M embedded observations).

- **Dirty-flag vector persistence** (`openmemory-index`): the O(corpus)
  index save is skipped when nothing changed; batched writes defer it
  to the engine's maintenance cadence with a `Drop` backstop.
- **Batched pre-lock embeddings** (`openmemory-graph`): `remember` /
  `remember_batch` embed every observation in ONE embedder call before
  taking the rebuild write lock (Lucene-DWPT-style pre-commit
  parallelism); lock hold time covers only SQLite + index inserts.
- **Checkpoint cadence** (`MemoryStore::wal_checkpoint`,
  `set_wal_autocheckpoint`; engine maintenance tick, default 1 s): the
  engine owns checkpointing, moving fsyncs out of commit paths. Journal
  truncation now happens ONLY after a complete checkpoint covers every
  journaled entry, closing a verified power-loss window where a drain
  could be lost with its journal already truncated.
- **Domain partitioning** (`openmemory-engine::partition::DomainStore`,
  `[engine] domains`): K independent store families with parallel
  writers, entities hash-routed by name; a per-profile manifest pins K.
  Cross-domain relations double-write TAO-style (mirror edge + marked
  stub entities, subtracted from listings/status); recall fans out and
  merges by score; the engine maps whole shards onto domains so drains
  stay single-domain transactions; journal replay routes by entity.
  Every read/write surface (MCP tools, CLI scriptable commands, ingest)
  routes through the facade; TUI and watch guard against partitioned
  profiles for now.

End-to-end measurements (stress example, 4000 agents x 10 ops, journal
on, 4 flush threads, same machine as above): one domain sustains
~41k obs/s durable; four domains ~80k obs/s (+95%) with lower
durability lag (55 ms vs 740 ms sampled mid-burst) — the facade
recall cache absorbs the concurrent readers (measured 6.7M cached
recalls/s, p99 167 ns, during the storm), freeing the cores for
commits. The commit-path-only experiment
(`a commit-path-only experiment (since folded into these numbers)`) shows the ceiling moving from ~66k (one
writer, any batch size) to ~134-148k obs/s at four domains. The cost
is fan-out recall under heavy concurrent writes (milliseconds instead
of cached sub-microsecond) and no cross-domain fuzzy normalization;
partition only when sustained ingest demands it.

## Remaining roadmap

- Per-request error reporting for fire-and-forget submissions (beyond
  the current per-group retry + error counters) would need result
  channels on tickets.
- More adapters as the connector surface grows: calendar events,
  ticket systems, transcription pipelines. The `SourceAdapter` trait
  is the seam; openmemory-watch is a natural future adapter (and the
  watch command does not support partitioned profiles yet).
- Domain-count migration tooling (re-home entities when K changes);
  today the manifest pins K and a mismatch is a hard error.
- Fan-out read refinements when partitioned read load matters: a
  reader thread pool, per-facade merged-result caching, and
  merge-per-modality-then-RRF for exact cross-domain ranking.
- TUI support for partitioned profiles.
