# Implementation Prompt

You are implementing production memory spaces, audit/manual editing, team
review, identity resolution, and safe cross-space merge for the Rust
`openmemory` repository.

This is an end-to-end implementation task. Continue until the complete scoped
feature is implemented, documented, measured, and verified. Do not stop after a
prototype, schema sketch, partial API, or happy-path test.

## Repository and source of truth

Start in the `openmemory` repository. The plan was grounded in committed
`main` at:

```text
bcd10fd3f736290c536ac94daf5a9014ef711828
feat: harden context engine for desktop production
```

First inspect the actual current HEAD and dirty worktree. Preserve all unrelated
user changes and untracked files. Reconcile harmless newer changes with the
plan; if current production architecture has materially changed, record the
evidence and minimally update the plan before implementing. Do not reset,
overwrite, or delete user work.

Read these files completely, in order:

1. `plan/16-production-memory-spaces/INDEX.md`
2. `plan/16-production-memory-spaces/00-head-assessment.md`
3. `plan/16-production-memory-spaces/01-contract-and-invariants.md`
4. `plan/16-production-memory-spaces/02-architecture-and-files.md`
5. `plan/16-production-memory-spaces/03-persistence-and-migrations.md`
6. `plan/16-production-memory-spaces/04-changesets-and-manual-editing.md`
7. `plan/16-production-memory-spaces/05-context-and-team-spaces.md`
8. `plan/16-production-memory-spaces/06-identity-and-merge.md`
9. `plan/16-production-memory-spaces/07-surfaces-and-compatibility.md`
10. `plan/16-production-memory-spaces/08-test-performance-and-release.md`
11. `plan/16-production-memory-spaces/09-delivery-sequence.md`
12. `LOGBOOK.md`, especially the memory-space/identity/merge experiments.

The numbered plan directory is the implementation specification. The prior
prototype and `plan/15-memory-spaces-audit-and-merge.md` are supporting evidence,
not code to copy blindly. If wording differs, the numbered implementation plan
takes precedence.

## Objective

Deliver all of the following in production code:

1. Physically isolated personal-project, team-project, personal-global, and
   team-global memory spaces, with the existing profile root bound in place as
   personal-global.
2. Stable project/workspace mapping, local team membership/roles, short-lived
   context capabilities, a bounded lazy runtime registry, an authorized ordered
   read set of at most four spaces, and exactly one write target.
3. Deterministic layered recall with space/revision provenance, a one-space fast
   path, bounded concurrency, exact semantic duplicate presentation, and
   generation/authorization-correct caching.
4. Atomic changesets and immutable semantic revisions for new writes, proposals,
   approval/rejection/conflict, idempotency, typed diff/history, manual edit,
   retire/restore/revert, guarded destruction, and resumable legacy backfill.
5. A durable index-generation/outbox repair contract so canonical SQLite and
   search never silently disagree; canonical relation IDs and mirror outbox
   before relation edit/merge.
6. A small pure `openmemory-merge` crate with explicit canonical hashing,
   bounded identity packets/evidence, reviewed revision-bound receipts,
   three-way classification, complete source/candidate/relation accounting,
   contribution-preserving deterministic planning, and predicted result hashes.
7. Optional asynchronous agent identity proposals behind a trait/queue and
   strict packet schema. No default outbound provider or surprise network call.
   Agents can propose but never authorize, approve themselves, override trusted
   contradictions, or write canonical identity state.
8. Overlay, cherry-pick, merge preview/conflict resolution, and directional
   material merge using immutable snapshots, staging, derived-state rebuild,
   complete verification, fresh-target recheck, fsynced intent, same-filesystem
   promotion, retained backup, and exhaustive old-or-new recovery. Source must
   never change.
9. Complete typed admin, CLI, and agent-safe MCP surfaces with backward
   compatibility, stable errors, capability discovery, redaction, docs, and
   backup/status/watch/ingest/domain-migration integration.
10. Production-grade unit/property/migration/integration/concurrency/crash/
    security tests, permanent real-world fixtures, Criterion/CodSpeed and
    release stress measurements, and every repository gate green.

## Fixed decisions—do not redesign these away

- A semantic memory space owns one full `DomainStore` root. Existing engine
  domains remain performance shards only.
- Existing data is not moved at upgrade; it becomes personal-global in place.
- Recall uses only the resolved read set and never scans the catalog.
- One interactive changeset is atomic only inside one engine domain. Reject
  cross-domain drafts; use staged whole-root promotion for truly space-wide
  atomic operations.
- SQLite graph rows/revisions are semantic truth. Product SQLite is control-
  plane truth. Indexes, access counts, journals, stubs, mirrors, and caches are
  derived/operational state and are never semantically merged.
- Shared names, fuzzy strings, embeddings, descriptions, graph neighborhoods,
  claimed identifiers, agent confidence, and timestamps may discover/rank a
  candidate but never automatically prove cross-space identity.
- Only current lineage, compatible source-verified unique identifiers, or a
  current authorized human review can coalesce identities. `cerpheus` in two
  projects remains separate by default.
- Coalescence keeps the target projection and appends immutable source origin
  contributions. It never replaces target state with a source projection.
- Overlay is the safe default. Material merge is directional; source is read-
  only and target promotion is staged/verified/recoverable.
- New space roots promote by directory rename. The legacy personal-global store
  shares its profile directory with sibling/control state, so promote only its
  enumerated `DomainStore` artifacts via the existing sentinel/recovery pattern;
  never rename the whole profile root.
- Team support in this scope is local-first explicit membership/roles. Do not
  invent hosted sync, SSO, invitations, or distributed authority.
- Agent team writes are proposals. Agent actor kind cannot approve/reject,
  decide identity, confirm merge, manage membership, or hard-destroy.
- Do not introduce a model/network call on recall, remember, approval, or merge
  promotion critical paths.
- Do not advertise Windows material merge without platform-specific tests.

## Engineering method

Follow `09-delivery-sequence.md` phase by phase. At the start:

- Record current revision, toolchain, status, and baseline commands in
  `LOGBOOK.md` as a new production entry; never rewrite prior entries.
- Reproduce and resolve the known watcher integration readiness flake without
  replacing causal synchronization with sleep.
- Make behavior-preserving module extractions before adding feature blocks to
  the daemon/admin/engine/graph/MCP monoliths.

For every phase:

1. Implement the smallest coherent vertical slice.
2. Write failing tests for its invariants and shortcuts first or alongside it.
3. Run focused tests, formatting, relevant default/all/no-default Clippy/tests,
   and rustdoc before advancing.
4. Append commands/results/measurements/failures/decisions to `LOGBOOK.md`.
5. Keep old surfaces working through explicit compatibility adapters.
6. If evidence invalidates a numeric budget or implementation detail, profile
   and compare alternatives. Update the relevant plan and logbook with data
   before choosing; never silently relax a correctness invariant.

Do not make commits, push, or open a pull request unless the user explicitly
authorizes those git actions. Reviewable phase boundaries still apply to the
working tree.

## Rust and code-quality requirements

- Preserve Rust 1.85 MSRV, edition 2021, 100-column formatting, workspace
  pedantic Clippy/warnings-denied, rustdoc warnings-denied, cargo-deny, feature
  matrix, release profiles, and no-unsafe posture.
- Prefer explicit structs/enums/state machines, private fields, narrow traits,
  deterministic ordered collections at hash/wire boundaries, and short focused
  modules.
- Do not add speculative frameworks, generic repositories, stringly typed
  policy, global mutable state, or fixture-specific branches.
- Validate all sizes/IDs/paths/evidence before mutation. Reject NaN/inf and
  unknown mutation fields. Keep errors typed and content-safe.
- Compute embeddings/hashes outside writer locks; use batched SQL and bounded
  memory; retain the direct single-space fast path.
- Reuse `Migrator`, `FixedClock`, TempDir, `pause_admissions`, raw migration
  export, WAL checkpointing, and the existing verified promotion concepts.
- Route HTTP handlers and CLI/MCP tools through typed services; no SQL or merge
  logic in surface handlers.
- Startup must not scan/backfill the corpus. Use lazy baseline revisions and
  resumable stable-ID page jobs.
- Never claim atomicity across independent SQLite databases.
- No `sleep`-based synchronization in tests. Use barriers, channels, condition
  variables, paused Tokio time, injected clocks, and subprocess abort workers.

## Required implementation order and exits

Implement every phase in `09-delivery-sequence.md`:

0. Baseline/watcher/module extraction.
1. Core contracts and pure merge crate with production fixtures/perf.
2. Product migrations/catalog/local authority/manifests/registry.
3. Graph v3 observation audit/revisions/index outbox.
4. Context capabilities/layered runtime/MCP backend/watch-ingest targeting.
5. Human admin/CLI/MCP audit/edit surfaces and backup/status integration.
6. Graph v4 entity/relation history/mirror readiness/identity ledger/agent
   proposal boundary.
7. Canonical snapshots/cherry-pick/preview/conflict/production planning.
8. Materialization/promotion/recovery/doctor/crash/performance.
9. Capability rollout/docs/cleanup/full release gate.

Do not enable a capability before its phase exit tests pass. Capability status
must be read from runtime migration/backfill/readiness, not guessed from binary
version.

## Tests and experiments that are mandatory

Implement the full matrix in `08-test-performance-and-release.md`, including:

- schema v1/v2/product v1 migration fixtures and future-version/rollback tests;
- at least 256-case property tests for pure invariants;
- deterministic Project A/Project B/team one- and four-domain isolation;
- exact `cerpheus` homonym behavior;
- two-reviewer/competing-proposal/revocation/registry/target-write races;
- subprocess `abort()` at every graph/index/catalog/rename/product commit durable
  boundary, repeated recovery, macOS and Linux;
- sanitized production fixtures for Codex/Homebrew Tools, Axum/Actix Web, and
  Mathlib/Lean 4 using the same generic planner;
- tampered/missing/extra/stale receipt/accounting/plan/hash cases;
- adversarial homonym/claimed-ID/payload/path/capability/prompt-injection cases;
- old MCP/CLI/admin E2E compatibility plus new context/provenance/team proposal;
- backup/restore/domain migration after promotion primitive reuse.

No ignored release-blocking tests, no fixture-directed production code, no
error-return-only substitute for process crash tests, and no manual-only proof.

## Performance requirements

Add the exact benchmark groups and meet the budgets in
`08-test-performance-and-release.md`. At minimum record repeated release results
for:

- 1/1k/10k catalog context resolution and single-space no-trend;
- one/two/four-space cold/warm keyword/hybrid recall;
- 512->128 fusion;
- unaudited control vs audited immediate/proposal/approval/index repair;
- bounded identity analysis;
- merge planning at 100/1k/10k/100k objects;
- materialization/index rebuild/promotion pause/recovery throughput, RSS, and
  disk amplification;
- daemon space/audit/merge route overhead.

Use deterministic seeds, fixed fixtures, documented toolchain/features/hardware,
interleaved controls, multiple samples, and p50/p95/p99 where meaningful.
CodSpeed detects regressions; enforce absolute budgets only on documented
reference machines.

## Required final gates

Run and make green:

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo test --workspace --locked --all-features
cargo test --workspace --locked --no-default-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

Also run the repeated concurrency suite, full crash matrices, permanent
real-world fixtures, Criterion/CodSpeed comparison, release 10k/100k merge
stress, backup/restore/domain migration, and CLI/MCP process E2E specified in
the plan.

## Completion conditions

Do not declare completion if any release blocker in
`08-test-performance-and-release.md` remains. Specifically, completion requires:

- no unauthorized or implicit cross-space read/write;
- every new semantic mutation atomically auditable and manually correctable;
- index/mirror lag explicit and repairable;
- identity same decisions provable/reviewed and revision-bound;
- complete deterministic merge accounting with source unchanged;
- verified old-or-new crash recovery at every tested boundary;
- bounded handles/threads/candidate/payload/memory/disk and performance budgets;
- backward-compatible existing behavior;
- all docs, config, admin DTOs, CLI completions, MCP descriptors/instructions,
  changelog, and logbook updated;
- all required gates green on the supported macOS/Linux matrix.

Your final report must be evidence-based and include:

1. Architecture/schema/API changes by crate.
2. Existing-install and old-client compatibility behavior.
3. Test commands/pass counts and repeated concurrency/crash/platform results.
4. Benchmark environment and every budget comparison.
5. Real-world fixture identity/disposition/accounting results.
6. Migration/backfill/backup/recovery drill results.
7. Security/adversarial findings.
8. Any plan deviations with the exact evidence and corresponding plan/logbook
   update.
9. Remaining explicit non-goals only; no hidden incomplete required work.
