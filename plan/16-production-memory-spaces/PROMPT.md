# Production Memory Spaces Implementation Contract

Implement the complete plan in this directory for the current OpenMemory
workspace. Work phase by phase; do not treat this as permission for one
unreviewable rewrite.

## Product contract

Deliver:

1. Physically isolated personal/team and global/project memory spaces. Bind
   existing profile data as personal-global without moving it.
2. Stable project/workspace mappings, local team roles, short-lived bounded
   context capabilities, an ordered read set of at most four spaces, and one
   authorized write target.
3. Deterministic layered recall with complete provenance, a direct one-space
   path, a shared global execution bound, and exact facade-cache behavior.
4. Atomic domain-local changesets, immutable revisions, proposal/review,
   conflicts, history/diff/edit/lifecycle operations, and resumable backfill.
5. Semantic/index generations, durable index and mirror outboxes, canonical
   relation IDs, explicit degraded readiness, repair, and verification.
6. A small pure `openmemory-merge` crate for canonical hashing, evidence,
   reviewed receipts, three-way classification, complete accounting, streamed
   actions, and predicted-result hashing.
7. Optional asynchronous agent identity proposals. Agents cannot approve,
   establish identity, create unverified relations, manage membership, confirm
   promotion, or destroy data.
8. Overlay, cherry-pick, preview, and directional material merge from immutable
   snapshots. Material merge stages, rebuilds, verifies, rechecks, writes an
   fsynced intent, promotes by same-filesystem rename, retains a backup, and
   recovers deterministically. Source is never modified.
9. Typed admin, CLI, and MCP surfaces with capability discovery, stable errors,
   redaction, provenance, status, backup/restore, watch/ingest, maintenance, and
   domain-migration compatibility.
10. Deterministic unit/property/concurrency/crash/security/compatibility tests,
    permanent real fixtures, release benchmarks, and explicit rollout gates.

## Non-negotiable semantics

- One semantic space owns one complete `DomainStore`; engine domains are only
  execution/storage shards.
- Recall touches only the authorized read set, never the catalog.
- An interactive atomic draft must route entirely to one engine domain.
- Canonical graph rows plus immutable revisions are semantic truth. Product
  SQLite owns catalog/authority/job truth. Derived indexes, caches, mirrors,
  stubs, and journals are rebuilt, not semantically merged.
- Only exact lineage, compatible source-verified unique identifiers, or a
  current packet-bound human review may establish identity.
- Fuzzy names, aliases, descriptions, graph neighborhoods, embeddings,
  timestamps, assertions, and agent output are discovery context only.
- Coalescence keeps the target projection and adds immutable source
  contributions.
- New space roots promote by whole `store/` rename. The legacy profile root
  promotes only an enumerated `DomainStore` artifact set; never rename the
  profile directory.
- Team authority is local only. No hosted sync, remote membership, SSO, or
  distributed atomicity is claimed.
- No model or network call appears on recall, ordinary remember, approval,
  snapshot publication, or promotion paths.

## Domain execution contract

`openmemory-engine` owns one shared `DomainExecutor` for a daemon runtime.
Daemon construction exposes no second-pool path; standalone/test runtimes are
explicit and isolated.

Every multi-domain operation:

1. authenticates, validates, reserves coordinator/task/byte budgets, and builds
   deterministic numeric work on its coordinator;
2. submits domain-owned work to the shared fixed pool;
3. waits at a complete barrier;
4. selects errors and reduces results in numeric `(space, domain)` order;
5. verifies the complete generation/accounting result; and
6. publishes only through a serialized coordinator path.

Never nest a per-space fan-out over `DomainStore::recall`. Flatten cold
`(space, domain)` searches beneath the existing facade cache. Preserve its
pre-search write version, key normalization, TTL, write-race invalidation, and
access-count semantics.

Snapshots pause admissions once, quiesce writes and derived work, capture an
ordered generation vector, export/checkpoint domains, verify a complete
barrier, fsync one manifest, and publish atomically. Snapshot concurrency
defaults to one.

Materialization streams canonical input into deterministic per-domain staging
work, builds/verifies domains with a default concurrency of two, then performs
global accounting and promotion serially. Authorization, interactive
cross-domain transactions, final identity accounting, intent, rename, fsync,
recovery, and catalog commit are never domain-worker tasks.

Normal live opens hold a shared per-space process lock outside the replaceable
root. Snapshot, restore, migration, and promotion require it exclusively after
closing local handles; daemon-less supported writers therefore cannot bypass
quiescence or rename safety.

## Engineering constraints

- Rust `1.85`, edition 2021, repository formatting, warnings denied, rustdoc
  warnings denied, `cargo deny`, all feature combinations, and no `unsafe`.
- Private validated value types; bounded collections and text; no NaN/Infinity;
  future-schema refusal; explicit checked state transitions.
- Resolve raw config, environment, CLI flags, platform/filesystem capability,
  authorization, and inferred context into immutable validated policy objects
  before storage/executor/planner hot paths. Explicit user intent outranks
  inferred defaults.
- Implement one correct general pipeline per operation. Fast paths, caches,
  optional embeddings/agents, and platform adapters enter through stable typed
  seams, use conservative eligibility facts, fall back to the general path
  when optional specialization is unavailable, and pass equivalence tests.
- Versioned, domain-separated, length-framed canonical hashes. Never hash serde
  JSON or incidental SQL/file order.
- Compute hashes/embeddings outside writer locks; batch SQL; keep lock scopes
  short; use bounded queues and streams.
- No generic repository framework, stringly workflow, global mutable singleton,
  fixture branch, unbounded retry, or startup corpus scan.
- No lossy path/text conversion at a semantic boundary. Managed roots use
  canonical IDs; workspace paths remain platform-native and must either have a
  lossless supported encoding or fail with a stable typed error.
- Every cache has a reviewed complete key and generation/invalidation contract;
  readers observe only atomically published complete entries.
- Portability decisions are centralized in one resolved capability value.
  Platform workarounds are documented, tested, and carry a removal condition.
- HTTP/MCP/CLI handlers deserialize, authenticate, call a typed service, and
  map results. They contain no graph SQL, planner, or filesystem promotion
  logic.
- Use crash-safe atomic file creation, restrictive registry-owned roots,
  symlink refusal, same-filesystem checks, file and parent-directory fsync, and
  recoverable intents.

## Required process

1. Revalidate `00-head-assessment.md` against current HEAD.
2. Follow the phases and exit gates in `09-delivery-sequence.md`.
3. At each phase, satisfy the stage verification contract in
   `08-test-performance-and-release.md`: focused unit/property tests,
   production integration tests, at least one permanent real-world scenario
   through the production path, compatibility regression tests, and the
   relevant fault/performance checks.
4. Do not advance while a corresponding blocking row in
   `experiments/memory-model/FAILURE_MATRIX.md` remains unproved in production.
5. If evidence requires a contract deviation, update this plan and obtain
   review before implementation depends on it.

Prototype tests, mocks, or pure-unit success alone never satisfy a phase exit.
Record every command, test count, fixture/provenance, platform, result, and
failure in the append-only implementation log before requesting phase review.

Completion is the definition of done in `INDEX.md` and every test, performance,
platform, compatibility, and release gate in
`08-test-performance-and-release.md`; passing the isolated prototype is not a
substitute.
