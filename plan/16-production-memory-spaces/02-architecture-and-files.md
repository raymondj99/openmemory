# Architecture and File Plan

## Dependency direction

Keep policy above storage:

```text
openmemory-core
├── openmemory-index
├── openmemory-merge          (pure; core only)
├── openmemory-graph          (core, index, optional embed, merge types)
└── openmemory-engine         (core, graph, index, merge)

openmemory-admin              (wire DTOs only)
openmemory-mcp                (typed backend over engine/graph)
openmemory-daemon             (policy/services; owns admin, engine, MCP)
openmemory-watch              (one pre-authorized fixed space handle)
openmemory-cli                (admin client and legacy adapters)
```

No lower crate reads product authority, opens a catalog, calls a model, or
chooses a space. Route handlers contain no SQL, planner, or promotion logic.

## General pipelines and normalized policy

Each capability has one semantic pipeline:

```text
raw request
  -> boundary decode and bounds
  -> immutable resolved policy + explicit-intent provenance
  -> authorization and resource reservation
  -> reusable typed mechanism
  -> complete verification
  -> one observable result/publication adapter
```

Daemon startup resolves config files, environment, defaults, hard caps, and
compile-time platform policy into one immutable `ResolvedProductPolicy`.
Context resolution adds authenticated catalog/authority facts and produces a
request-scoped `ResolvedOperation`. Lower crates receive only the narrow
validated policy they need; they do not reread config, environment, current
directory, catalog state, or feature flags.

The owning policy modules are focused, not a generic framework:

```text
openmemory-daemon/src/policy.rs       # product defaults and explicit choices
openmemory-engine/src/portability.rs  # platform/filesystem capability facts
```

`ResolvedProductPolicy` contains typed execution, context, identity, retention,
merge, and optional-agent policies plus a `ResolvedPlatformPolicy`. A managed
root also has a generation/device-bound `FilesystemCapabilities` value created
by explicit preflight. Platform `cfg` branches and filesystem probes stay in
the portability owner; services consume ordinary capability data.

Explicit request choices retain `SelectionSource::Explicit` and outrank
workspace mappings and defaults. An unsupported explicit mode fails with its
typed error. Heuristics may suggest project identity, candidates, or
specialization eligibility; they never authorize, establish identity, or
override intent.

### Conservative facts and specialization seams

Interfaces expose one-sided facts whose truth is sufficient for a cheaper path
and whose absence only misses an optimization:

- recall probe: valid cache hit or a miss ticket bound to key/write version;
- space handle: verified identity/readiness/generation and authorized role;
- execution reservation: exact admitted task/result-byte bounds;
- snapshot/plan manifest: independently verifiable completeness/hash facts;
- platform/filesystem capability: supported atomicity/fsync/locking facts.

The general path remains semantic authority. Current planned specializations
are:

| Specialization | Conservative eligibility | General fallback |
|---|---|---|
| One-space recall | resolved read set length is exactly one | layered fusion |
| One-domain recall/write | pinned verified domain count is one | numeric domain execution/reduction |
| Recall cache hit | complete key, TTL, write version, readiness and authority generations match | cold recall |
| Deterministic identity proof | current verified unique-ID/lineage facts with no contradiction | human review or keep separate |
| Optional embedding/agent | configured, ready, consented adapter | keyword/deterministic path |
| Material promotion | platform/filesystem capabilities prove required semantics | capability unavailable; no unsafe emulation |

Every specialization enters and exits through the same typed contract. It must
have adversarial equivalence tests for values, ordering, errors, provenance,
telemetry, and cleanup. False-positive eligibility is a correctness defect.
Failure of an optional accelerator falls back; failure of a required
publication primitive fails loudly.

## Shared execution runtime

Add focused engine modules:

```text
crates/openmemory-engine/src/
├── execution/
│   ├── mod.rs          # ExecutionRuntime, config, lifecycle, metrics
│   ├── admission.rs    # coordinator/task/byte reservations
│   ├── executor.rs     # private fixed pool and operation queues
│   └── operation.rs    # job class, deadline, cancellation, receipts
├── space/
│   ├── mod.rs
│   ├── handle.rs
│   ├── layered.rs
│   ├── manifest.rs
│   └── snapshot.rs
└── merge/
    ├── mod.rs
    ├── materialize.rs
    ├── promotion.rs
    ├── recovery.rs
    └── verify.rs
```

`DomainExecutor` and task submission are `pub(crate)`. A top-level process owns
one `Arc<ExecutionRuntime>`:

- daemon: exactly one in `DaemonState`, shared by every `SpaceRuntime`;
- daemon-less legacy CLI/MCP: one standalone runtime for its fixed
  personal-global store;
- tests: explicit isolated runtimes.

`DomainStore::open_with_runtime` receives that runtime. Existing `open`
wrappers remain for compatibility but are not used by daemon services. No
space worker may call a fanning `DomainStore` method; cold per-domain
primitives are engine-private.

### Admission and scheduling

Admission is all-or-nothing and occurs before closures/result slots are
allocated. Each `OperationSpec` declares:

- class: `ForegroundRecall`, `ForegroundWrite`, `Snapshot`, `Materialize`, or
  `Maintenance`;
- deterministic task count and indices;
- maximum captured/result bytes and cost units;
- queue and execution deadline; and
- cancellation behavior.

Reservations derive from hard serialized record/candidate limits, not caller
estimates. Task sinks charge captured/result bytes before growth and fail the
whole atomic operation (or one non-atomic child receipt) before exceeding the
reservation.

Defaults/hard limits:

| Resource | Default | Hard limit |
|---|---:|---:|
| Worker threads | `min(4, max(2, available_parallelism))` | 64 |
| Queued tasks | 256 | 4,096 |
| Concurrent coordinators | 64 | 256 |
| Reserved result/capture bytes | 64 MiB | 256 MiB |
| Queued context writes | 8,192 | 65,536 |
| Queued context-write bytes | 64 MiB | 256 MiB |
| Snapshot domain concurrency | 1 | worker count |
| Materialize domain concurrency | 2 | worker count |
| Large maintenance operations | 1 | 1 |

Production configurations that enable maintenance require at least two
workers. Maintenance permits are at most `workers - 1`; work is chunked so a
one-worker test/standalone runtime reaches a foreground queue boundary.

One `DomainIoBudget` backs executor workers, direct single-domain fast paths,
and context-engine SQLite drains. Direct paths run on the caller thread but
must acquire one permit; this preserves the fast path without exceeding the
global active-domain-work bound. Existing flusher threads remain separately
bounded and may not enter SQLite without this permit. Context-engine submission
also reserves global queued-write count and canonical serialized bytes before
journaling; per-shard capacity is a secondary fairness bound, not the process
memory limit. Journal replay streams bounded records through the same budget;
startup never loads all pending journals into memory.

The executor provides:

- one fixed named pool shared by runtime clones;
- bounded per-operation queues scheduled round-robin;
- bounded ticketed admission: FIFO within a job class, deadline removal, and
  aging between classes without consuming the foreground reserve; younger
  same-class requests cannot bypass a head request merely because they are
  smaller;
- foreground admission independent from the maintenance backlog;
- numeric result slots and a complete barrier;
- lowest numeric `(space, domain)` error selection;
- `catch_unwind` panic containment; the pool remains usable, while the affected
  domain is degraded until integrity/derived-state repair verifies it;
- thread-local re-entrancy rejection;
- ordered independent receipts for non-atomic work; and
- counters/histograms for admission, queue/execute time, active/peak workers,
  bytes, saturation, panic, timeout, cancellation, and shutdown.

Workers own `'static` task state and touch one assigned graph/index family.
They cannot authorize, mutate the catalog, perform final identity accounting,
or publish a space.

All multi-domain recall, snapshot, materialization, non-atomic bulk,
index/mirror repair, backfill, consolidation, pruning, and verification use
this runtime in bounded waves. None may create a private pool or scoped
per-call threads. Small readiness repair may use a foreground class; corpus
work is maintenance and yields at every persisted page.

### Deadlines, cancellation, shutdown

Cancellation is cooperative; Rust threads are never force-killed. Check tokens
before work and at every bounded SQL/import/checkpoint chunk. Set SQLite busy
limits from the remaining deadline and use an interrupt handle for cancellable
long reads where supported.

On deadline:

- cancel queued siblings;
- interrupt/checkpoint active tasks where safe;
- wait for a bounded stop barrier;
- return no partial semantic result or publication;
- mark the affected operation/space degraded if a task cannot stop.

Snapshot admissions remain paused and promotion remains disabled until every
task has stopped or operator recovery closes the space.

For a canonical writer, cancellation before commit rolls back. If commit may
have occurred, return an outcome-unknown receipt keyed by the idempotency key;
retry reads the durable terminal receipt. Never report “not applied” without
proving rollback.

`ExecutionRuntime::shutdown(deadline)` stops admission, cancels queued work,
waits for active operations, and returns `ShutdownIncomplete` rather than
blocking forever. `Drop` signals cancellation but performs no unbounded join.
Daemon shutdown reports stuck workers and exits only through its established
bounded shutdown policy.

## Pure merge crate

Add `crates/openmemory-merge` with:

```text
canonical.rs   # validated sorted records and streaming interfaces
hash.rs        # explicit versioned length-framed BLAKE3 encodings
identity.rs    # evidence, packets, receipts, validation
three_way.rs   # pure classification and conflicts
planner.rs     # dispositions, actions, accounting, predicted hash
error.rs       # exhaustive validation/planning errors
```

Dependencies: `openmemory-core`, `blake3`, `serde` for typed interchange, and
`thiserror`; no graph, SQLite, async runtime, filesystem, model, or daemon.
Register it in root members/workspace dependencies at the workspace version,
edition, MSRV, and license; forbid unsafe code and pass repository dependency
policy.

The planner consumes sorted iterators and emits actions to a bounded caller
sink. It does not retain or clone complete source, target, and result graphs.
The sink is an interface, not I/O owned by the pure crate. Hashes never use
serde JSON.

Canonical sources and action sinks use an explicit lifecycle:

```text
begin(validated header)
  -> zero or more ordered bounded records
  -> finish(complete accounting + semantic hash)
```

Any decode, validation, sink, deadline, or cancellation error transitions to
`abort` and can never produce a finished plan. The sink owns record framing and
backpressure; the planner owns semantic order/accounting. A bounded test sink,
hashing sink, paged artifact sink, and independent verifier all implement the
same protocol. Preview pagination reads the completed immutable artifact and
does not terminate planning early.

## Crate changes

### `openmemory-core`

Add `src/space.rs` for validated IDs, owners, contexts, roles, authority
snapshots, and read sets. Add small validated config sections only after their
services exist. Keep workflow errors in owning crates; do not add a general
repository/service framework.

### `openmemory-graph`

Extract:

```text
canonical.rs, changeset.rs, diff.rs, history.rs, lifecycle.rs,
mutation.rs, outbox.rs, provenance.rs, relation_history.rs, revision.rs
```

- `schema.rs`: ordered v3/v4 `Migrator` steps and fixtures.
- `remember.rs`/`batch.rs`: validation and precomputation remain; one private
  transaction helper commits canonical rows, revisions, events, generations,
  checkpoint metadata, and outbox.
- `store.rs`: retain one verified `SpaceId`, readiness generations, and narrow
  accessors; no workflow.
- `export.rs`: keep raw domain-migration export unchanged; add a distinct
  canonical stream API.
- `forget.rs`: preserve legacy wrappers; new lifecycle is audited.

Only graph-private trusted builders construct transaction operations. Public
surfaces submit typed drafts.

### `openmemory-engine`

- Extract the existing `partition.rs` cache probe/search/publish sequence
  without changing keys, TTL, write-version invalidation, result ordering, or
  access-count semantics.
- `layered.rs` flattens cold work across `(space, domain)` through the shared
  runtime, reduces domains per space, then fuses spaces.
- `snapshot.rs` coordinates the one-pause generation-bound protocol.
- `materialize.rs` streams deterministic domain spools and performs bounded
  staged construction.
- A crate-private `open_staging` accepts only a verified job staging token and
  registry-derived root. It may build the target `SpaceId` beside the live
  target but can never be returned as a live/catalog handle.
- Reuse the tested migration promotion primitive for material merge. New roots
  rename a whole `store/`; legacy root promotion enumerates only DomainStore
  artifacts.
- Journals are bound to a concrete `SpaceId`; replay cannot reroute.

### `openmemory-daemon`

First split the current `lib.rs` with no behavior change:

```text
auth.rs, health.rs, runtime_files.rs
routes/{existing,spaces,changesets,identity,merges}.rs
services/{context,spaces,changesets,identity,merges}.rs
space_registry.rs
product_store/{mod,schema,jobs,spaces,identity,merges}.rs
```

`DaemonState` owns product authority, the single `ExecutionRuntime`, and the
bounded `SpaceRegistry`. Services own policy/state machines; routes only
deserialize, authenticate, invoke, and map.

### `openmemory-admin`, MCP, CLI, watch

- Split admin DTOs into `spaces`, `changesets`, `identity`, and `merges`, with
  root re-exports for compatibility.
- Refactor MCP behind fixed/resolved context backends. Preserve `handle`; add
  internal `handle_with_context`. MCP never receives catalog/store handles.
- Add focused CLI command modules; daemon-required administration never opens a
  second product/graph writer.
- Bind watch/ingest to one pre-authorized space handle for the process
  lifetime. Events never auto-route by current directory.

## Registry ownership

`SpaceRegistry` lazily owns `SpaceRuntime { manifest, DomainStore,
ContextEngine, readiness }`:

- one open attempt per `SpaceId`;
- RAII request/job leases;
- default 8, hard 64 open non-legacy spaces;
- default 64/hard 512 open domain families across all spaces;
- default 192/hard 1,024 graph SQLite connections, reserved before open;
- default 2/hard 8 active context engines and default 4/hard 16 total flusher
  threads;
- personal-global pinned for compatibility;
- idle eviction only when unleased, after flush/checkpoint;
- generation-bound cache entries;
- exclusive close-for-promotion that blocks every writer/read handle;
- zero catalog-size polling or eager opening.

`MemoryStore::open_scoped` receives a reserved reader-pool size instead of
blindly allocating `Config::num_jobs()` for every open domain. Keep the legacy
personal-global pool size when budget permits; other domains receive at least
one reader, and the registry evicts or rejects before exceeding the process
budget. Index handles and writer connections are included in the reservation.

Every `DomainStore` also holds a shared cross-process space lock for its entire
open lifetime. Snapshot, restore, domain migration, and promotion first block
new registry leases, close local handles, then acquire the stable lock
exclusively with a deadline. The lock file is control state outside the
replaceable store root. This covers daemon-less legacy CLI/MCP processes; a
busy external holder aborts maintenance instead of being bypassed.

The registry derives roots from catalog identities beneath restrictive
registry-owned parents. Callers never provide a root path.

## Cache and publication inventory

Before adding or changing a cache, its owning module records and tests:

| Cache/publication | Complete identity and validity |
|---|---|
| Existing domain recall | normalized query, `top_k`, mode, every filter/weight/record-access input, store/space identity, pre-search write version, readiness generation, TTL policy |
| Resolved context | principal, actor, profile, explicit selection, workspace/project/team, catalog/team/membership/credential generations, expiry/revocation |
| Space registry runtime | `SpaceId`, root key, catalog/manifest/schema/readiness generations, reserved resource shape |
| Embedding cache | model canonical identity/version, tokenizer/pooling/input encoding, semantic text hash |
| Snapshot/plan/staged result | immutable manifest/hash/version and exact source/target generations; atomic rename is the completeness signal |

Security/authorization facts are never omitted from a reusable cache key.
Mutable entries are built privately and published only after the final
mutation/hash succeeds. False-hit tests vary every shaping input individually;
race tests prove readers observe old-complete or new-complete state, never a
partial entry. No fused layered-result cache is planned.

## Ownership and terminal paths

Leases, permits, locks, connections, files, staged directories, task slots,
child processes, and streams have one named RAII owner. Acquisition happens
before the next fallible operation; ownership transfer neutralizes the source.
Success, error, cancellation, timeout, panic containment, early client
disconnect, and shutdown all reach an idempotent terminal path that releases
budgets and either removes unpublished staging or preserves it under a typed
recovery state.

## Compatibility boundary

Keep existing Rust wrappers, MCP tool names/required fields, JSON-RPC behavior,
admin routes, auth, and direct personal-global stdio. New wire fields are
optional/defaulted until one deliberate API-version change. Omitted context
means personal-global, never all spaces.
