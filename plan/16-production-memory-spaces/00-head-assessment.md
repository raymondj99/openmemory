# HEAD Assessment

Baseline inspected and revalidated on 2026-07-26:
`bcd10fd3f736290c536ac94daf5a9014ef711828`, OpenMemory `0.4.4`,
Rust `1.85.0`, edition 2021. `main` matched `origin/main`; the tracked
production tree was clean. The untracked `plan/` and `experiments/` trees and
all existing stashes were preserved.

No production Memory Spaces implementation was present. The workspace members,
schema versions, module sizes, domain facade, context engine, migration path,
product store, and tests were inspected directly rather than inferred from the
prototype.

## Existing architecture to preserve

| Area | Current strength |
|---|---|
| Graph | `MemoryStore` owns one SQLite family, a writer/read pool, rebuild barrier, WAL/foreign-key setup, future-schema refusal, and derived indexes. |
| Domain facade | `DomainStore` pins domain count, routes canonical writes by entity name, has a direct single-domain path, deterministic multi-domain reduction, and a versioned TTL recall cache. |
| Context engine | Bounded shard queues, backpressure, durable journals, admission pause, quiesce, checkpoints, and replay watermarks. |
| Migration | Existing domain-count migration stages, verifies, and promotes under an fsynced sentinel while retaining the old layout as a backup. An interrupted swap is detected and blocked, but production does not yet recover it automatically. |
| Tests | Fixed clocks, temporary roots, process tests, feature gates, migration fixtures, and deterministic engine coverage. |
| Safety | Typed errors and validation are established repository patterns. At baseline, `openmemory-embed::bootstrap` had one explicit `#[allow(unsafe_code)]` environment-mutation block; Phase 0 removed it. |

The production design extends these mechanisms. It does not replace SQLite,
the domain router, index backends, MCP protocol implementation, or the context
engine.

## Baseline structural risks

- `openmemory-daemon/src/lib.rs` is 2,164 lines.
- `openmemory-graph/src/remember.rs` is 1,619 lines.
- `openmemory-engine/src/partition.rs` is 1,606 lines.
- `openmemory-engine/src/engine.rs` is 1,561 lines.
- `openmemory-graph/src/store.rs` is 1,429 lines.
- `openmemory-mcp/src/tools/memory.rs` is 1,591 lines.
- `openmemory-admin/src/lib.rs` is 774 lines.

Add new behavior only after focused extraction with no behavioral change.
Module boundaries are an acceptance gate, not aesthetic cleanup.

The line counts above were unchanged when revalidated. Phase 0 then reduced
the daemon root to 1,724 lines, reduced the admin root to a 16-line
compatibility facade, and established the planned focused module owners. The
other baseline monoliths remain later-phase risks.

## Correctness gaps this plan closes

- Multi-domain recall creates scoped threads per cache miss. The bound is local
  to a call and would multiply under layered spaces.
- The recall facade cache is valuable and subtle: it captures a write version,
  normalizes keys, uses TTL invalidation, and intentionally changes cached
  access-count behavior. Executor integration must preserve it exactly.
- Index visibility has no durable semantic-generation outbox contract.
- Cross-domain relation mirror failure can leave degraded traversal without a
  durable repair ledger.
- Semantic spaces, local authority, context capabilities, revisions, identity
  receipts, whole-space snapshots, and material merge do not exist in the
  tracked production tree.
- Product SQLite is schema version 1 and needs ordered `Migrator` steps with
  future-version refusal.
- Existing fuzzy in-space coalescence is not sufficient proof for cross-space
  identity.
- Backup/status/docs/fixtures do not yet describe catalog plus multiple roots.
- The existing domain migration has crash detection and a retained backup, not
  an implemented old-or-new recovery state machine.
- At baseline, the embedding bootstrap's explicit unsafe environment mutation
  conflicted with the plan's no-unsafe production gate. Phase 0 replaced it
  with ort's safe, lazy loader-path configuration.

## Prototype evidence and limits

The isolated `experiments/memory-model` package proves useful pure and
scheduling properties:

- 132 release tests and 28,672 generated property cases passed.
- A fixed shared pool held its global worker bound across concurrent
  coordinators, rejected re-entrancy, contained panics, and reduced results
  deterministically.
- Flattened 4-space × 4-domain recall improved release p95 from 1.389 ms to
  1.159 ms.
- Staged bundle work benefited from bounded parallelism; snapshot p95 was
  variable. Safe defaults are snapshot concurrency 1 and staged construction 2.
- Streaming predicted-result hashing reduced planner peak delta to 1.29×
  canonical input at 100k source + 100k target entities.
- Full in-memory materialization used about 2.25 GB and is explicitly rejected
  for production.
- Eight filesystem-state combinations and 80 abort/reopen cycles proved the
  prototype recovery model.

It did not prove production authorization, facade-cache parity, request-memory
admission, deadlines/shutdown, whole-space generation quiescence, index/mirror
rebuild, disk-full behavior, backup compatibility, or Linux/macOS promotion
semantics. Those remain release blockers.

## Baseline prerequisite

The workspace all-feature suite previously exposed an FSEvents readiness flake
in `openmemory-watch/tests/integration.rs`. Reproduce it and either fix the
causal readiness protocol or apply a correct platform gate. Do not add sleeps
or weaken assertions. Phase 0 is complete only when repeated isolated and
workspace runs are stable on the supported macOS/Linux matrix.

Phase 0 revalidation reproduced the failure on macOS 26.5.2/APFS at the
baseline HEAD. A focused watcher integration run passed all four tests, then
`cargo test --workspace --locked` failed
`watcher_indexes_create_modify_delete`: the backend did not become live after
eight causal warmup pokes.

Phase 0 closes the startup gap by registering the recursive backend before the
initial scan/ready notification and selects notify's synchronously registered
kqueue backend on macOS; Linux remains on inotify. The production scenario
uses a bounded causal event barrier with no sleep. It passed ten independent
macOS process repetitions locally. CI now repeats the same scenario five times
on both macOS and Linux before the workspace suite; Linux CI evidence remains
required before merge.

## Revalidation disposition

The architectural baseline and all seven baseline structural line counts were
accurate. Two assessment statements required correction:

1. Existing domain migration detects an interrupted swap and preserves a
   backup; it does not perform deterministic automatic recovery.
2. The baseline workspace was not unsafe-free because
   `crates/openmemory-embed/src/bootstrap.rs` contained one explicit unsafe
   `std::env::set_var` call beneath a module-local allow.

The production contract still requires safe Rust. Phase 0 added the bootstrap
redesign to its delivery contract, removed the exception, and verified that no
explicit unsafe block/function/implementation or local unsafe allow remains
under `crates/`.

## Consequences for implementation

- One daemon runtime owns one engine execution resource and every space handle
  shares it.
- Authorization, final accounting, and publication stay out of domain workers.
- The existing single-space/single-domain path remains direct.
- Product and graph migrations are small ordered steps; row backfill is lazy or
  resumable and never a startup scan.
- Staging uses bounded streams and batched SQL, not complete in-memory graphs.
- Capability discovery remains false until migrations, backfills, repair,
  platform gates, backup, and compatibility proofs are all ready.
