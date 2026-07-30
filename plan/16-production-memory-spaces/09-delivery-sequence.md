# Delivery Sequence

## Method

Implement in dependency order as small green slices. At each phase:

1. record HEAD, dirty state, toolchain, design choices, commands, failures,
   measurements, and deviations in an append-only
   `plan/16-production-memory-spaces/IMPLEMENTATION_LOG.md`;
2. pass the applicable stage verification contract: unit/property, production
   integration, permanent real-world scenario, compatibility,
   fault/concurrency/security, and performance/resource tests;
3. map evidence to the phase's prototype failure-matrix blockers;
4. complete the per-slice principles checklist in
   `10-engineering-principles.md`, including general-path parity, normalized
   policy, cache/publication, ownership, representation, and portability items
   that the slice reaches;
5. append the verification packet and stop for review before starting the next
   phase.

Do not keep migrations, APIs, and tests broken across phases. Prototype code is
evidence only and is never added to the production workspace.

## Required real-world verification by phase

| Phase | Production-path real-world proof required before exit |
|---|---|
| 0 | Repeated workspace suite plus a populated legacy-profile fixture and supported watcher scenario remain behaviorally unchanged. |
| 1 | All sanitized identity/merge corpora run through the pure crate with generic discovery, accounting, and hash goldens. |
| 2 | A populated legacy profile binds across restart; managed personal/project/team roots, revocation, locks, and 10k catalog metadata use production services. |
| 3 | Populated v1/v2 graphs migrate, then remember/propose/approve/edit/reopen/repair through production graph APIs and actual SQLite/index files. |
| 3.5 | Verified real corpora of differing sizes drive the recall-path quality harness; fusion, identity, routing, and telemetry decisions are backed by recorded measurements. |
| 4 | Actual 1/4-space × 1/4-domain stores exercise cold/warm recall, writes, cache races, revocation, 10k closed spaces, and global bounds. |
| 5 | Spawned daemon, CLI, and MCP clients complete context, proposal, review, edit, retire, backup/restore, watch/ingest, and legacy journeys. |
| 6 | Project/team and repository corpora use production backfill, mirror repair, discovery, agent fake, and human receipt paths. |
| 7 | Actual stores produce immutable snapshots; every permanent corpus completes production cherry-pick and preview with deterministic accounting. |
| 8 | Permanent corpora materialize into real staged domains and promote/recover through process locks, fsync/rename, VFS faults, and subprocess aborts on macOS/Linux. |
| 9 | Representative legacy/new profiles complete upgrade, rollout, backup/restore, retention/destruction, recovery, and rollback-readiness drills. |

These scenarios supplement rather than replace focused unit tests. A phase exit
requires both.

## Phase 0 — Baseline and extraction

Work:

- revalidate `00-head-assessment.md`;
- reproduce/fix or correctly platform-gate the watcher readiness race without
  sleeps;
- remove or safely redesign the embedding bootstrap's explicit unsafe
  environment mutation; no exemption is accepted;
- split daemon/auth/routes/runtime helpers, admin DTO modules, and engine
  space/merge module shells with no behavior change;
- correct crate/storage/benchmark documentation drift.

Exit:

- repeated existing tests/goldens and feature gates pass;
- no API/storage/runtime behavior changed;
- new behavior has focused module owners.

## Phase 1 — Pure contracts and merge crate

Work:

- add validated core space/authority, representation-boundary, and explicit
  selection-source types;
- pin a simple bounded, test-only materialized reference model for canonical
  identity and merge semantics before optimizing their execution shape;
- add pure `openmemory-merge` canonical/hash/evidence/receipt/three-way/
  streaming-planner modules with one explicit source/sink
  begin/emit/finish/abort protocol;
- port generic prototype properties and sanitized real fixtures, not prototype
  storage/framework code;
- force reference/streaming implementations over the same generated and
  permanent corpora and compare actions, hashes, accounting, errors, and
  terminal outcomes;
- benchmark 10k/100k planning and memory.

Exit:

- no I/O/model/policy dependency in merge crate;
- homonym, proof, receipt, accounting, rewiring, conflict, forged action, and
  streamed predicted-hash cases pass;
- reference and streaming planners are observably equivalent, and every
  injected source/sink error aborts without a usable plan;
- 100k time/RSS budgets pass before a storage implementation depends on the
  planner format.

## Phase 2 — Catalog, authority, manifests, registry

Work:

- product v1→v4 ordered migrations;
- installation principal, teams/grants/generations, projects/workspaces,
  capability storage;
- one immutable product/context/platform/filesystem policy normalization
  boundary with explicit selection provenance and lossless workspace keys;
- managed-root/manifest atomic validation and legacy binding;
- stable cross-process lifetime locks for daemon and legacy opens;
- bounded lazy `SpaceRegistry`, authorization leases, readiness;
- hidden admin DTO/service contracts.

Exit:

- every binding crash/restart yields one personal-global row/root;
- new spaces are physically unique and path attacks fail closed;
- revocation and registry bounds pass at 10k catalog rows;
- semantic routing remains legacy personal-global.

## Phase 3 — Observation audit and index correctness

Work:

- graph v3 migration and focused changeset/revision/diff/outbox modules;
- observation immediate/proposal/review/reject/revert and compatibility wrappers;
- lazy baseline/resumable backfill;
- durable index generation/repair;
- transaction, concurrency, abort, and SQLite VFS fault tests.

Exit:

- new observation writes are atomically auditable/editable;
- stale/idempotency/proposal/index/disk-full cases pass;
- old output remains compatible;
- audited-write budget passes with hidden/shadow projection comparison.

## Phase 3.5 — Retrieval evidence and contract corrections

Inserted because Phase 4 freezes layered-recall semantics, and three of
those semantics were shown to be wrong when measured. Phase 4 must not
start until this phase's decisions are implemented in the plan and its
gates exist.

Work:

- retrieval-quality harness driving `MemoryStore::recall` with
  categories, abstention risk/coverage, latency percentiles, and
  judgment-provenance splits (`openmemory-eval`);
- verified corpora with completion receipts recording source revision,
  chunker policy, and embedding model identity;
- adopt the cross-space invariant (never compare uncalibrated scores)
  and deterministic rank interleaving as the provisional policy;
  record its promotion gate (`05-context-and-team-spaces.md`);
- adopt immutable `EntityId` identity and routing; delete
  `entity_rehome` (`01-contract-and-invariants.md`,
  `04-changesets-and-manual-editing.md`);
- schedule access-count feedback out of default ranking; make
  `record_access = false` mandatory on evaluation and read-only paths;
- record baselines for every category so Phase 4 has something to
  regress against.

Exit:

- quality gates in `08-test-performance-and-release.md` run in CI and
  fail on a category regression;
- the fusion rule, identity model, and routing model in this plan match
  what the evidence supports;
- a baseline exists for every query category on at least two spaces of
  different sizes, and the small-space contribution gate passes;
- the decay defect is either fixed or explicitly accepted with a
  recorded λ justification, including the λ=0 control;
- every reported category is reachable at its K, bound to an immutable
  corpus receipt, and human-reviewed categories are reported separately
  with a stated minimum count;
- no generated-only or unreachable category is used as a regression
  baseline;
- rule comparisons carry paired uncertainty, not point estimates;
- corpus age and corpus size are not confounded across the spaces used
  for a cross-space conclusion.

## Phase 4 — Shared executor and context recall

Implement in this order:

1. Add one daemon-owned `ExecutionRuntime` and private `DomainExecutor` walking
   skeleton with worker/task/coordinator/byte admission, job classes,
   foreground reserve, barrier, deterministic errors, panic containment,
   deadline/cancellation, metrics, and bounded shutdown.
2. Prove global bounds, saturation, maintenance fairness, stuck-task behavior,
   and daemon lifecycle before adding a client.
3. Extract `DomainStore` facade cache probe/cold-reduce/conditional-publish
   without changing current recall.
4. Inventory every cache shaping input/generation, then replace scoped fan-out
   with the shared executor and prove atomic publication, false-hit,
   write-race, filter/mode, and access-count parity against the forced general
   path.
5. Add context resolver/capabilities, space handles, flattened layered recall,
   rank-based fusion/provenance, and one-space fast path. Fusion combines
   per-space ranks; per-space scores are never compared across spaces.
6. Route daemon/MCP fixed/resolved context and bind watch/ingest to one target.

Exit:

- every concurrent 1/4-space × 1/4-domain workload shares one global bound;
- nested fan-out is impossible and all execution blockers D-12 through D-17
  have production evidence;
- existing facade behavior is parity-pinned before replacement;
- project/personal/team isolation and revocation pass;
- context/recall budgets pass; omitted context is personal-global;
- retrieval-quality gates pass, including the small-space contribution
  gate, with no category regression against the Phase 3.5 baseline.

## Phase 5 — Human audit/edit and compatibility surfaces

Work:

- admin history/diff/edit/lifecycle/review routes and stable errors;
- CLI space/project/context/changeset/review/memory commands;
- agent-safe MCP context/history/proposal and provenance;
- redacted SSE/doctor/readiness;
- explicit-space backup/status/maintenance/domain migration integration;
- complete admin/MCP/CLI/watch/backup compatibility tests and docs.

Exit:

- every new observation is inspectable/correctable without SQL;
- stale state cannot overwrite;
- legacy clients and backup/restore pass;
- capabilities advertise only proven Phase 2–5 readiness.

## Phase 6 — Entity/relation history and identity

Work:

- graph v4 entity/relation revisions, identifiers, contributions, canonical
  relation IDs, mirror outbox;
- lazy/backfill baselines and verified mirror rebuild;
- same-domain entity/lifecycle edit, staged re-home rename, gated relation edit;
- indexed candidate discovery, evidence packets, agent proposals, human decision
  events/receipts/revalidation;
- brute-force reference discovery parity, conservative fact publication, and
  deterministic fallback/typed unavailability for stale indexes and the
  optional agent;
- adversarial identity/authority/agent tests.

Exit:

- canonical export round-trips all semantic truth;
- mirror readiness/repair is explicit;
- shared names never auto-merge; current human receipts are auditable;
- agent failure or contradiction leaves concepts safely separate.

## Phase 7 — Quiesced snapshots and preview

Work:

- one-pause whole-space snapshot with writer/outbox/mirror drain, numeric
  generation capture, executor domain work, full barrier, manifest fsync, and
  atomic publication;
- explicit snapshot producer/consumer terminal protocols and one RAII owner for
  every admission guard, lock, handle, staging root, and publication token;
- snapshot deadline/failure/crash/platform tests, default concurrency 1;
- lineage lookup and cherry-pick;
- streaming production preview: discovery, review, three-way conflict, action
  stream, accounting, hashes;
- admin/CLI preview/review/resolve/status and all permanent corpora.

Exit:

- no partial snapshot publishes; every immutable input revalidates;
- identical inputs produce identical action/plan/result hashes;
- source/target/candidate/relation accounting is complete;
- snapshot blockers D-14/R-10 and failure/publication matrix are closed.

## Phase 8 — Staged materialization and recovery

Work:

- reuse migration promotion primitives;
- disk preflight and bounded per-domain spools;
- executor staged construction/verification, default concurrency 2;
- coordinator global verification, target recheck, confirmation, exclusive
  admission, intent/fsync/rename/catalog ordering;
- exhaustive recovery/cleanup, `doctor`, backup, health/events;
- VFS/filesystem faults and every-boundary aborts on macOS/Linux;
- centralized immutable platform/filesystem capabilities, with a regression
  and removal condition for every platform workaround;
- sequential/1/2/4 performance, foreground, RSS, disk, and pause measurements.

Exit:

- every injected failure yields verified old/new target, unchanged source, and
  idempotent recovery;
- no local or supported cross-process writer can enter between target recheck
  and verified promotion;
- quota/cross-device/stale/unsupported-platform failures occur before rename;
- performance/disk/platform gates pass and domain migration remains green.

## Phase 9 — Rollout and final gate

Work:

- shadow audit/projection/hash comparison on legacy and representative stores;
- enable capabilities in dependency order:
  `spaces → audit → manual_edit → team_spaces → identity_review →
  material_merge`;
- keep upgrades safely personal-global; enable production defaults for new
  installs only when readiness is proven;
- finish docs/changelog/config/runbooks;
- remove duplicate policy/compatibility truth and record why every remaining
  specialization, cache, optional feature, and workaround still exists;
- run retention/destruction/restore/future-schema/recovery drills and every
  release gate.

Exit:

- every `INDEX.md` definition and `08` blocker has explicit evidence;
- no TODO, ignored test, fixture branch, unbounded placeholder, or undocumented
  degraded behavior remains;
- implementation log contains exact final compatibility, tests, crash/platform
  matrices, benchmarks, security review, and approved deviations.

## Rollback

Rollback disables routing/UI through readiness while keeping the newer binary
and schema. Never run an older binary against a newer database. A promoted
target rolls back only through verified retained-backup promotion under an
intent, never by copying over an open root.

## Final handoff

Report:

- modules, schema/hash/API versions, and ownership by crate;
- legacy profile/CLI/MCP/admin behavior;
- exact test commands/counts and repeated concurrency/crash/platform results;
- benchmark environment, controls, samples, p50/p95/p99/throughput/RSS/disk
  against every budget;
- real-corpus accounting;
- migration/backfill/backup/restore/recovery drills;
- security findings, non-goals, and every approved plan deviation.
