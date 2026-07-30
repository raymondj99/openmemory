# Production Memory Spaces Implementation Log

This file is append-only. Each reviewed implementation slice adds a verification
packet without rewriting earlier evidence.

## 2026-07-26 — Phase 0, revalidate `00-head-assessment.md`

### Scope and environment

- Exact work item: Phase 0 — Baseline and extraction, first work item,
  “revalidate `00-head-assessment.md`.”
- HEAD: `bcd10fd3f736290c536ac94daf5a9014ef711828`
  (`main`, matching `origin/main`).
- Dirty state before editing: no tracked modifications; untracked
  `experiments/` and `plan/`; five existing stashes. All were preserved.
- Toolchain: `rustc 1.85.0 (4d91de4e 2025-02-17)`,
  `cargo 1.85.0 (d73d2caf9 2024-12-31)`, edition 2021.
- Platform: Apple arm64, macOS 26.5.2 build 25F84, Darwin 25.5.0.
- Workspace filesystem: APFS, `/dev/disk3s5` mounted at
  `/System/Volumes/Data`.

### Contract and failure-matrix mapping

- Invariant: this slice changes assessment/evidence documentation only; no
  production API, storage, runtime, authorization, or behavior changes.
- Invariant: prototype code remains outside the production workspace and no
  prototype schema, module, storage, or fixture shortcut is copied.
- Failure-matrix mapping: O-01 (prototype isolation guard) was confirmed;
  O-03 (compatibility baseline) received default-workspace evidence but remains
  a production integration blocker. S-11 and D-12 through D-17 remain
  production blockers; none was claimed closed.
- Revalidation found that the prior “no unsafe code” baseline statement was
  false: `openmemory-embed::bootstrap::init_ort_env` locally allows and uses
  one unsafe `std::env::set_var`. The no-unsafe product contract is unchanged.
- Revalidation corrected an overstatement: domain migration retains a backup
  and blocks open on an fsynced sentinel, but has no automatic interrupted-swap
  recovery routine.

### Files and boundaries changed

- `plan/16-production-memory-spaces/00-head-assessment.md`: recorded the exact
  revalidated HEAD, corrected the safety and migration baseline, and recorded
  the reproduced watcher failure and proposed Phase 0 amendment.
- `plan/16-production-memory-spaces/IMPLEMENTATION_LOG.md`: created this
  append-only evidence ledger.
- No production crate, schema, API, test, fixture, benchmark, or configuration
  file changed.

### Commands and results

- `git rev-parse HEAD`, `git status --short --branch`, `git stash list`:
  baseline and dirty state matched the scope above.
- `wc -l` over the seven structural-risk modules: exact counts
  `2164, 1619, 1606, 1561, 1429, 1591, 774`; all matched the assessment.
- `rg -n '\bunsafe\b|allow\(unsafe_code\)|forbid\(unsafe_code\)' crates
  --glob '*.rs'`: found one explicit unsafe block at
  `crates/openmemory-embed/src/bootstrap.rs:84` and its local allow at line 58.
- `cargo test -p openmemory-engine --lib --locked`: passed, 52 passed,
  0 failed, 0 ignored, 0 measured, 0 filtered out. This exercised actual
  SQLite/index/filesystem domain routing, cache invalidation, journal replay,
  populated domain migration, sentinel refusal, and restart paths.
- `cargo test -p openmemory-watch --test integration --locked`: passed,
  4 passed, 0 failed, 0 ignored, 0 measured, 0 filtered out. This exercised
  the real notify backend and filesystem production path.
- `cargo test --workspace --locked`: failed with exit 101 at the watcher
  integration target. That target reported 3 passed, 1 failed, 0 ignored,
  0 measured, 0 filtered out; `watcher_indexes_create_modify_delete` failed
  because the backend never became live after eight causal warmup pokes.
  Cargo reports per-test-binary counts and aborted at this first failing
  binary, so it emitted no workspace aggregate. All targets reported before
  that binary passed.

### Properties, concurrency, crash, and real-world evidence

- No property was added or changed. No property seed/case count is claimed for
  this documentation-only slice.
- No new concurrency or crash boundary became reachable, so no new
  concurrency/crash repetition count is applicable.
- Existing permanent production-path scenario:
  `openmemory-engine::migrate::tests::migrate_single_store_to_four_domains_and_back`
  at revision `bcd10fd` uses the repository's populated `seed` profile and
  actual `DomainStore`, SQLite, index, staging, verification, rename, backup,
  reopen, and reverse-migration path; it passed in the 52-test engine run.
- Supported watcher scenario:
  `openmemory-watch/tests/integration.rs` at revision `bcd10fd` uses the real
  notify backend, APFS files, SQLite metadata, and production watcher loop. It
  passed focused and then failed under the workspace suite. This is evidence
  of the known readiness race, not a Phase 0 exit proof.
- The synthetic populated migration profile is not a provenance-documented
  real-world legacy corpus. The required permanent populated legacy-profile
  fixture and repeated macOS/Linux watcher matrix remain Phase 0 blockers.

### Performance and resources

- No runtime behavior changed, so latency/RSS/disk comparison is not applicable
  to this slice. No p50/p95/p99 or resource result is claimed.
- The watcher latency smoke test passed in both watcher invocations, but its
  measurements were not emitted without `--nocapture` and are not used as a
  performance gate.

### Failures, corrections, and remaining blockers

- Corrected the assessment's unsafe-free and automatic-migration-recovery
  claims. Proposed for human review: add removal or safe redesign of the
  embedding bootstrap exception to Phase 0 before relying on the no-unsafe
  gate. No exemption was approved.
- The watcher readiness race reproduced and remains for the next Phase 0 work
  item. No sleep, weakened assertion, retry expansion, platform gate, or
  production fix was added in this slice.
- All Phase 0 exit layers, feature combinations, rustdoc, dependency policy,
  repeated compatibility suites, permanent real-world fixture, Linux evidence,
  and performance/resource gates remain outstanding. This packet does not
  claim Phase 0 completion.

### Post-edit verification addendum

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed for all
  twelve workspace crates.
- `env RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps`: passed;
  twelve workspace crate documentation targets generated with warnings denied.
- Correction to the preceding remaining-gates statement: default-feature
  rustdoc is complete for this slice. All-feature/no-default feature gates,
  `cargo deny`, repeated suites, permanent fixture, Linux, and performance
  evidence remain outstanding for Phase 0 and were not applicable to this
  documentation-only work item.

## 2026-07-26 — Phase 0, baseline and extraction completion

### Scope and environment

- Exact work item: complete every remaining Phase 0 action and stop before
  Phase 1.
- Baseline HEAD remained
  `bcd10fd3f736290c536ac94daf5a9014ef711828` on `main`. No commit, push,
  stash mutation, or prototype import was performed.
- The original untracked `experiments/` and `plan/` trees and five existing
  stashes were preserved.
- Toolchain and platform remained Rust/Cargo 1.85.0, edition 2021, Apple arm64,
  macOS 26.5.2 build 25F84, APFS.

### Contract and failure-matrix mapping

- Existing public API names, v1alpha1 wire shapes, SQLite schema versions,
  profile layout, single-domain fast path, migration format, and CLI/MCP/admin
  behavior remain unchanged.
- The one intended runtime implementation change is the approved watcher
  platform gate: macOS uses notify's kqueue backend instead of FSEvents;
  Linux remains on inotify. Create/modify/delete, ignore, deduplication, and
  latency semantics remain pinned by the production integration scenario.
- O-01 remains closed: no prototype schema, storage code, fixtures, or module
  layout entered the production workspace. O-03 now has a repeated
  compatibility baseline and remains a later production-integration blocker
  for the features that do not yet exist. S-11 and D-12 through D-17 remain
  assigned to Phase 4 and are not claimed closed by module shells.
- No explicit unsafe block/function/implementation or local unsafe allow
  remains under `crates/`.

### Watcher causal repair and platform gate

- `openmemory-watch::runtime` now constructs and recursively registers the
  debounced backend before the initial scan and ready notification. Events
  arriving during the scan queue for immediate processing, closing the
  post-notification startup gap.
- notify 8 is built with `macos_kqueue`. FSEvents was rejected because its
  device-level purge on short-lived watcher teardown made immediate restart
  readiness unprovable; kqueue and inotify acknowledge recursive registration
  synchronously.
- The integration scenario uses one production-shaped stream for causal
  readiness, create/modify/delete, ignored-directory behavior, continued
  operation, and six latency iterations. Restart deduplication uses two real
  on-disk store opens and production initial scans. There is no sleep or
  weakened event assertion.
- One captured run measured create p50/p99 95.732/100.198 ms, modify
  96.400/99.149 ms, and delete 97.061/98.612 ms against an 800 ms bound.
- Ten independent macOS process repetitions passed: 20 tests passed, 0 failed.
  Three final-state workspace repetitions also passed the watcher target.
- `.github/workflows/ci.yml` now pins Rust 1.85.0 and runs five independent
  watcher repetitions on both `ubuntu-latest` and `macos-latest` before the
  workspace suite. A local Linux container run was unavailable because the
  configured Docker/OrbStack daemon socket was absent; the required Linux
  evidence is therefore an explicit pre-merge CI gate, not a local claim.

### Safe embedding bootstrap

- Removed the unsafe `std::env::set_var` and its local unsafe allowance.
- Bootstrap now uses ort's safe process-global `init_from` loader-path
  configuration for an explicit or discovered library. Actual library loading
  remains lazy, preserving keyword-only startup and the existing rule that
  model download is skipped when no configured/discovered runtime exists.
- Added a regression proving an empty model cache falls back without eagerly
  loading ONNX Runtime. The final focused embed run passed 99 tests.

### Focused extraction and ownership

- Daemon extraction added `auth.rs`, `health.rs`, `runtime_files.rs`,
  `routes/{existing,spaces,changesets,identity,merges}.rs`,
  `services/{context,spaces,changesets,identity,merges}.rs`,
  `space_registry.rs`, and
  `product_store/{mod,schema,jobs,spaces,identity,merges}.rs`.
  Existing handlers and re-exports preserve call sites. The daemon root fell
  from 2,164 to 1,724 lines.
- Admin's existing v1alpha1 DTOs moved behind a 16-line root compatibility
  facade with public `spaces`, `changesets`, `identity`, and `merges` module
  owners and root re-exports.
- Engine gained private
  `space/{handle,layered,manifest,snapshot}.rs` and
  `merge/{materialize,promotion,recovery,verify}.rs` ownership shells. They add
  no API or behavior.
- The spawned-daemon CLI integration test now reports the complete stderr
  chain when startup fails instead of discarding every line after the first.

### Permanent real-world compatibility fixture

- Added the provenance-documented `legacy-profile-v2.json`: four sanitized
  repository-derived entities, five observations, and three relations with no
  user data, credentials, absolute paths, network input, or prototype schema.
- `legacy_profile.rs` populates the fixture only through public production
  `DomainStore`/graph APIs, verifies the legacy root layout, reopens it,
  performs recall, migrates one to four real SQLite/index domains, verifies,
  migrates four back to one, reopens, and verifies again.
- No fixture name or field is inspected by production code. The permanent
  scenario passed in every default/all/no-default workspace run.

### Documentation and CI drift

- `docs/crates.md` now describes all twelve workspace crates and the actual
  benchmark crate instead of nonexistent index benchmark files.
- `docs/development.md` now documents the real test/benchmark inventory and
  commands.
- `docs/watcher.md` now records register-before-scan startup, the macOS/Linux
  backend gate, and the actual `ScanReport`/`BatchSummary` API.
- `docs/storage.md` now covers product SQLite, domain manifests/families,
  engine journals, migration staging/intent/backup artifacts, future-version
  refusal, and the absence of cross-database transactions or automatic
  interrupted-swap recovery.
- `00-head-assessment.md`, `INDEX.md`, and the Phase 0 delivery contract now
  record the corrected baseline, safe-bootstrap requirement, extraction
  result, watcher gate, and remaining Linux CI evidence.

### Final verification

- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.
- Explicit-unsafe `rg` audit over Rust production sources: no matches.
- `cargo test --workspace --locked`: passed three consecutive final-state
  repetitions, 841 tests per repetition, 0 failed or ignored.
- `cargo test --workspace --locked --all-features`: passed, 847 tests,
  0 failed or ignored.
- `cargo test --workspace --locked --no-default-features`: passed, 807 tests,
  0 failed or ignored.
- `cargo clippy --workspace --all-features --all-targets --locked --
  -D warnings`: passed.
- `cargo clippy --workspace --all-targets --no-default-features --locked --
  -D warnings`: passed.
- `env RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked`:
  passed for all twelve crates.
- `env RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
  --all-features --locked`: passed for all twelve crates.
- `cargo deny check`: advisories, bans, licenses, and sources all passed.

### Failures and disposition

- The original focused watcher passed while the workspace target failed; the
  reordered registration alone still reproduced the macOS restart failure.
  Selecting the synchronous kqueue backend resolved ten independent process
  repetitions without timing sleeps.
- An intermediate eager `ort::commit` design could have loaded the runtime
  before a cached model existed. Review caught it before handoff; the final
  implementation configures only the safe loader path and has a focused lazy
  fallback regression.
- `cargo deny` first failed because the sandbox could not lock Cargo's
  advisory database. The approved direct rerun passed all policy categories.
- Two piped/looped workspace invocations lost the approved network-test
  execution context and macOS denied the daemon test's loopback bind with
  `Operation not permitted`. Direct invocations in the supported test context
  passed three consecutive final-state suites; this was a harness sandbox
  artifact, not a product retry or ignored failure.

### Phase exit

- All Phase 0 implementation actions and local exit gates are complete. No
  Phase 1 contract, schema, API, or behavior has started.
- The macOS half of the supported platform matrix has direct evidence. Linux
  watcher repetition is wired as a mandatory CI gate and must be green before
  merge; Phase 0 is not represented as cross-platform release evidence until
  that external run exists.

## 2026-07-26 — Pre-Phase 1 engineering-principles audit

### Scope and source

- Exact work item: ensure the plan conforms to `~/PRINCIPLES.md` before any
  Phase 1 implementation begins.
- Audited source:
  `/Users/rjow/PRINCIPLES.md`, SHA-256
  `577435372aa83abba17243f9b3a6dc021c72826e1015ec25f583215f573dc011`.
- Read the complete principles source and every plan document from
  `PROMPT.md` and `00-head-assessment.md` through
  `09-delivery-sequence.md`, plus this implementation log.
- This was a documentation/contract audit only. No Phase 1 crate, production
  code, schema, API, fixture, or runtime behavior was added.

### Findings and binding decisions

- No product-contract conflict was found. Existing space isolation,
  authorization, typed invariants, shared bounded execution, pure merge
  planning, deterministic ordering, causal tests, atomic promotion, and
  compatibility gates already align strongly with the principles.
- Previously implicit requirements are now explicit gates: one correct general
  pipeline; one-time policy and intent normalization; conservative
  specialization facts; exact fallback/equivalence; lossless path/text/hash
  boundaries; complete cache identities; atomic cache/artifact publication;
  visible RAII ownership and terminal protocols; centralized portability; and
  removal conditions for workarounds.
- Phase 1 now must establish a bounded, test-only materialized reference oracle
  before the streaming planner, use one begin/emit/finish/abort source/sink
  contract, and prove reference/streaming equivalence for actions, hashes,
  accounting, errors, cleanup, and publication.
- Optional identity-agent, embedding, indexed-discovery, direct-domain,
  cache-hit, and platform paths remain adapters or specializations behind the
  general contract. Optional failure falls back safely; an explicitly required
  but unavailable capability fails with a typed result.

### Plan files amended

- `INDEX.md` and `PROMPT.md`: made the principles audit authoritative and added
  general-pipeline, policy-normalization, representation, cache, ownership, and
  portability constraints.
- `01-contract-and-invariants.md`: added explicit selection provenance,
  representation-boundary rules, typed failures, and owner/publication
  invariants.
- `02-architecture-and-files.md`: added normalized policy objects,
  conservative specialization seams, explicit streaming lifecycle, cache
  inventory/publication rules, and RAII terminal ownership.
- `03-persistence-and-migrations.md`: bound workspace paths to lossless
  versioned keys and separated semantic hashes from file encodings with atomic
  completeness publication.
- `05-context-and-team-spaces.md`: made context normalization, explicit-intent
  precedence, ambient-state isolation, and context cache generations explicit.
- `06-identity-and-merge.md`: defined discovery as a conservative optimization,
  the agent as an optional same-seam adapter, a materialized reference planner,
  and the planner's terminal source/sink protocol.
- `08-test-performance-and-release.md`: added causal conformance tests and
  release blockers for the new gates.
- `09-delivery-sequence.md`: added the principles decision record to every
  slice and made the Phase 1 reference/equivalence work an exit requirement.
- `10-engineering-principles.md`: recorded the source checksum, mapped all 18
  principles to binding plan anchors, and added a 14-question per-slice record
  plus pre-specialization/cache/platform/phase-exit gates.

### Verification

- Recomputed the principles SHA-256 and matched the audited checksum above.
- Counted 18 numbered entries in the principle map.
- Verified every document named by the plan map and principles map exists.
- A trailing-whitespace and conflict-marker audit over every plan Markdown file
  passed.
- `git diff --check` passed for the tracked Phase 0 worktree.
- Production tests were not rerun because this audit changes plan
  documentation only; the Phase 0 verification packet above remains the latest
  production-code evidence.

### Gate result

- The plan now conforms to the audited principles and is ready for Phase 1
  review.
- Phase 1 has not started.

## 2026-07-26 — Pre-Phase 1 worktree-scope audit

### Request and result

- Audited every tracked and untracked production-code change before beginning
  Phase 1, specifically to remove any accidentally implemented work outside
  Phase 0.
- No out-of-scope production implementation was present, so no code was
  reverted.
- `crates/openmemory-merge` does not exist and
  `openmemory-core::space` has not been added; Phase 1 implementation remains
  unstarted at this checkpoint.

### Classification

- Watcher runtime/tests, the `notify` backend configuration, repeated CI gate,
  and watcher documentation belong to the Phase 0 causal readiness fix.
- Embedding bootstrap changes belong to the Phase 0 safe-bootstrap action.
- Admin/daemon extraction, the moved product-store implementation, and the
  engine space/merge ownership files belong to the Phase 0 behavior-preserving
  module extraction. All future-facing route/service/product-store/engine files
  are documentation-only ownership shells.
- The CLI diagnostic change and permanent populated legacy fixture belong to
  the Phase 0 compatibility and real-world verification work.
- Storage/crate/development documentation changes belong to the Phase 0 drift
  correction.
- `experiments/memory-model` and the plan directory were already untracked at
  the original Phase 0 baseline. The prototype remains isolated evidence and
  is not a workspace member or production implementation.

### Verification

- Compared `git diff --name-status` and every untracked production path with
  the Phase 0 scope and file inventory recorded above.
- Confirmed every future-facing extracted shell contains ownership
  documentation only.
- Confirmed there is no `crates/openmemory-merge` path and no
  `pub mod space` declaration in `openmemory-core`.
- No file was deleted, reset, or restored because there was no matching
  accidental change.

## 2026-07-26 — Phase 1 pure contracts and merge semantics

### Scope and result

- Completed only Phase 1 from `09-delivery-sequence.md`; Phase 2 catalog,
  authority, manifest, and registry work has not started.
- Added validated `openmemory-core::space` identifiers, path/representation
  boundaries, authority snapshots, ownership/role grants, ordered read sets,
  write-target validation, and explicit selection provenance.
- Added the workspace member `openmemory-merge`. Its direct dependencies are
  exactly `openmemory-core`, `blake3`, `serde`, and `thiserror`; it has no
  graph, SQLite, async-runtime, filesystem, model, daemon, clock, or product
  policy dependency and forbids unsafe code.
- Added no production storage, schema, catalog, authorization, materialization,
  publication, CLI, MCP, daemon, or runtime behavior.

### Pure merge contract

- Added bounded canonical entity/observation/relation projections, typed
  domain-separated hashes, validated snapshot headers, and a resettable sorted
  `SnapshotSource` seam.
- Added conservative target-scoped candidate discovery with explicit caps and
  truncation, proof/context evidence separation, packet hashes, human/system
  identity receipts, keep-distinct receipts, and one-to-one disposition
  validation.
- Labels, aliases, claimed identifiers, timestamps, and optional heuristics
  never prove identity. Current source-bound authoritative identifiers and
  verified lineage are the only same-identity proofs; current contradictions
  fail closed.
- Added exact three-way base/source/target field and lifecycle classification,
  including delete-versus-edit conflicts.
- Added the single `begin`/`emit`/`finish`/`abort` action-sink protocol. Only a
  successful `finish` exposes a plan; every injected target/source stream,
  emit, and finish failure aborts and leaves no actions or usable plan.
- The planner keeps every target record, accounts for every source record
  exactly once, derives reserved qualified import IDs, detects existing and
  repeated qualified-ID collisions, rewires relation endpoints, rejects
  forbidden self-loops and duplicate assertions, merges semantic observation
  and relation origins, and binds actions/accounting/predicted result into a
  canonical plan hash.
- Snapshot and action verification contain no production `unwrap`, `expect`,
  panic, unsafe, I/O, serde-JSON hashing, or ambient-state read.

### Reference, property, and fixture proof

- `tests/common/mod.rs` contains a bounded, test-only materialized reference
  planner and independently implemented canonical result hasher. It does not
  share the production streaming indexes or result-token accumulator.
- Successful generated and permanent corpora compare exact action order,
  action counts, predicted hashes, plan verification, and terminal plan state
  against the reference implementation.
- The adversarial materialized reference independently classifies missing
  resolution, many-to-one, qualified-collision, duplicate-assertion, and
  self-loop-policy errors; production streaming returns the same typed codes.
- Four property tests run 256 generated cases each for source accounting/order
  invariance, conservative evidence, indexed-discovery/brute-force parity, and
  symmetric identity resolution with directional packet hashes.
- Adversarial coverage includes homonyms, missing resolutions, stale/unsorted
  streams, many-to-one coalescence, qualified-ID collisions, duplicate relation
  assertions, forbidden rewiring, forged action order, and all stream/sink
  terminal failures.
- Added three compact, sanitized, pinned public-repository fixtures:
  `codex-homebrew-tools`, `axum-actix-web`, and `mathlib-lean4`. Their
  provenance/license metadata and golden plan/predicted hashes are permanent
  test inputs; no prototype storage or framework code was copied.

### Principles decision record

1. The general pipeline is validated snapshots plus current receipts into one
   typed planner and one action-sink lifecycle; `SnapshotSource`, `ActionSink`,
   `MergeAction`, and `MergePlan` are the stable seams.
2. Product policy remains above the crate. `MergePolicy`, `IdentityPolicy`,
   `MemoryContext`, and `SelectionProvenance` are immutable normalized inputs;
   no ambient config or path inference is reread.
3. Core owns identity, representation, authority, and context invariants;
   evidence/discovery/receipt modules own identity facts; the planner owns the
   closed action order, accounting, and predicted result; caller sinks own any
   later persistence.
4. Correctness is exact action order, stable errors, immutable provenance,
   complete accounting, canonical hashes, abort cleanup, golden compatibility,
   and unchanged legacy workspace behavior.
5. Interfaces carry only validated records, typed IDs/hashes, bounded
   candidates, current revision-bound receipts, immutable policy generations,
   and explicit result identities.
6. The streaming planner is the production execution shape; the bounded
   materialized reference is the force-selectable test oracle. Exact action,
   count, hash, and terminal-state equivalence is exercised on generated and
   permanent corpora.
7. The common path performs sorted source passes, compact fixed-size semantic
   index keys, one source-disposition vector, one result-identity map, compact
   collision sets, and a sorted/deduplicated result-token vector. Release
   measurements below bound its time and incremental memory.
8. Phase 1 is deliberately single-coordinator and synchronous. There is no
   worker concurrency or publication; the coordinator alone owns ordering and
   accounting.
9. Borrowed sources and the caller-owned sink live for one plan call.
   Intermediate maps/vectors are RAII-owned by that call. UTF-8 semantic values
   and versioned platform path keys are distinct; non-current paths require an
   explicit platform check. Every begun sink ends in finish or abort.
10. Phase 1 adds no cache or durable artifact. Snapshot, packet, candidate,
    receipt, action, predicted-result, and plan hashes include their shaping
    versions/generations and complete canonical inputs.
11. Platform behavior is isolated in `PathPlatform` and reversible
    `WorkspacePathKey`; unsupported/native non-UTF-8 conversion is typed failure.
    The merge pipeline itself is platform- and optional-capability-independent.
12. Cheapest causal tests are inline validation/unit tests, then materialized
    reference/property/adversarial tests, then the full legacy workspace
    feature matrix and release scale harness.
13. No pre-existing duplicate production path was introduced or retained.
    The prototype remains outside the workspace; the materialized oracle is
    test-only; the production planner has one implementation and sink protocol.
14. Candidate caps, policy/resolver generations, normalizer versions, snapshot
    formats, canonical hash domains, action limits, and scale budgets remain
    explicit, versioned, bounded, and revisable rather than semantic truth.

### Verification

- Final focused suites: `openmemory-core` 51 default tests;
  `openmemory-merge` 16 unit + 6 adversarial + 3 permanent-fixture + 5 planner
  + 4 property tests, all passing. The property suite executes 1,024 generated
  cases per run.
- Final workspace compatibility: `cargo build --workspace --locked`;
  workspace tests under default, `--all-features`, and
  `--no-default-features`; strict Clippy with all features and no default
  features; default/all-feature rustdoc with `-D warnings`; and
  `cargo deny check` all passed.
- `cargo bench -p openmemory-merge --bench planning --offline` passed its
  10-sample 10k and 100k Criterion targets. The final release helper also ran
  20 samples at each scale to report direct nearest-rank percentiles.
- Final 10k entities + 10k relations: p50 108.215 ms, p95 112.466 ms,
  p99 112.833 ms, below the 150 ms p95 gate.
- Final 100k entities + 100k relations: p50 1,061.039 ms,
  p95 1,116.649 ms, p99 1,117.751 ms, below the 1.5 s p95 gate.
- The 100k canonical snapshots encode to 67,400,794 bytes. Paired macOS
  `/usr/bin/time -l` runs measured 221,790,208 bytes peak RSS for the retained
  input/build-only control and 306,233,344 bytes for planning: 84,443,136 bytes
  (80.531 MiB) incremental, 1.2529× canonical bytes and below 256 MiB.
- Benchmark environment: Apple M3 Pro MacBook Pro, 12 cores, 36 GB RAM,
  arm64 macOS 26.5.2; Rust/Cargo 1.85.0; release profile; baseline commit
  `bcd10fd3f736290c536ac94daf5a9014ef711828`; no concurrent benchmark workload
  intentionally introduced.

### Gate result

- Every Phase 1 work item and exit condition passes locally.
- Documentation now lists thirteen workspace members and the merge test/
  benchmark inventory.
- Phase 2 remains unstarted and requires review of this Phase 1 packet before
  proceeding.

## 2026-07-26 — Phase 2, catalog, authority, manifests, and registry

### Scope and environment

- Exact work item: Phase 2 in `09-delivery-sequence.md`.
- HEAD: `bcd10fd3f736290c536ac94daf5a9014ef711828`; Rust 1.85.0 on Apple
  arm64 macOS 26.5.2.  The Phase 0/1 dirty worktree was preserved; this slice
  adds only the Phase 2 control-plane implementation, compatibility wiring,
  focused tests, and documentation.
- No public space/context route was enabled.  The product service and admin
  DTO seams are private until the scheduled Phase 4/5 routing work.

### Design and durable boundaries

- Product SQLite now migrates one transaction at a time through v1–v4.  It
  updates `product_meta.schema_version` and `PRAGMA user_version` together,
  rejects a nonzero disagreement or future version, and rolls a failed step
  back in full.  v2 adds catalog spaces, installation principals, teams/grants,
  projects/workspace mappings, and hashed context capabilities; v3 adds
  identity/merge control records; v4 reserves maintenance records.
- The catalog accepts only validated core IDs, closed owner/context/lifecycle
  tags, bounded labels, canonical BLAKE3 binding hashes, and reversible
  `WorkspacePathKey` fields.  Capabilities store only a BLAKE3 token hash,
  typed serialized context, expiry, and revocation state.  Team changes bump
  authority/catalog generations.
- Legacy personal-global remains the single semantic default.  A durable
  `.space-id`, creating-state catalog row, atomic `space.toml`, and later
  activation make every restart repair/reuse one identity rather than create a
  second root.  Direct CLI/MCP legacy opens and daemon profile opens retain a
  shared lifetime lock; maintenance uses an exclusive lock only after closing
  the registry entry.
- Managed roots are fixed to `data/<profile>/spaces/<space-id>`, receive a
  canonical manifest and per-root lock, and reject symlinks/non-directories.
  `FilesystemCapabilities` records that material promotion and race-hardening
  are unavailable: no path-canonicalization claim substitutes for a reviewed
  nofollow primitive.
- The private `SpaceRegistry` is lazy and bounded by explicit limits for open
  spaces, domains, graph connections, engines, and flusher threads.  A lease
  owns each open entry; revocation/maintenance blocks new leases, drains them,
  and uses RAII lock release.  Catalog cardinality alone never opens a root.

### Principles conformance

1. The general path is the catalog → validated manifest → registry/lock path;
   the legacy opener uses the same persisted space identity and lifetime-lock
   contract rather than a competing store model.
2. `ResolvedProductPolicy` freezes product/context/platform/filesystem inputs
   and explicit selection provenance before service work; workspace identity is
   a lossless, versioned key rather than ambient current-directory text.
3. Core owns IDs, authority, context, and representation validation; product
   SQLite owns control-plane rows; manifest/registry own path and resource
   validation; engine/graph own the actual per-space data.
4. Correctness is a canonical catalog binding hash, one root per root key,
   explicit state transitions, typed authority snapshots, and future/mismatch
   migration refusal—not a best-effort lookup.
5. Private service inputs carry validated `SpaceRef`, profile, policy, or
   capability records; raw paths, bearer secrets, and untyped ownership tags
   do not cross that boundary.
6. A catalog/memory-meta space ID check rejects a mismatched scoped store
   before it becomes usable.  Legacy compatibility is deliberately narrow and
   has no authority escalation path.
7. Registry limits are checked before admission; a 10k-row catalog test proves
   only a bounded number of active entries and resources are retained.
8. The registry controls admission and maintenance; domain/graph handles
   remain under their existing store owners.  This slice introduces no
   background executor or nested fan-out.
9. `SpaceLock`, `LegacySpaceLock`, leases, and store runtime retain native
   files in named RAII owners.  Activation failures leave an inspectable,
   unreferenced managed root instead of deleting uncertain state.
10. The registry has no result cache.  Authority/capability validation uses
    current generation/state at the product boundary; a changed team grant or
    revoked capability cannot reuse an old authorization result.
11. Platform facts are immutable `FilesystemCapabilities`; managed-path
    security is conservative and explicitly unavailable where a portable safe
    primitive is absent.  No lossy path conversion is used as identity.
12. Focused migration, manifest, authority, lock, and bound tests precede the
    full feature matrix; the cross-process lock test is the causal concurrency
    proof.
13. There is one production catalog/manifest/registry path.  Existing direct
    CLI/MCP profile opens were adapted to the narrow engine legacy adapter,
    rather than retaining an unbound parallel open path.
14. Manifest format, capability size, schema versions, workspace normalizer,
    BLAKE3 hash format, lifecycle tags, and registry resource ceilings are
    explicit and bounded for later revision.

### Verification and release evidence

- `cargo test -p openmemory-daemon --lib phase2_tests --no-fail-fast` passed:
  11 tests in 0.33 s.  It exercises v1→v4 migration/future+marker refusal and
  transactional rollback; legacy bind/restart repair; unique managed roots and
  symlink rejection; workspace/capability/authority data; team revocation and
  lease draining; 10,000 catalog records with bounded registry eviction;
  policy provenance; scoped-store identity rejection; and an actual spawned
  process proving the shared/exclusive lock boundary.
- Workspace tests passed with default features, `--all-features`, and
  `--no-default-features` (`--no-fail-fast` for each).  The final all-feature
  suite passed after the strict-lint fixes.
- `cargo fmt --all -- --check`; strict Clippy for all features and no default
  features; rustdoc with `RUSTDOCFLAGS=-D warnings` for default and all
  features; and `cargo deny check` all passed.
- `docs/storage.md`, `docs/crates.md`, and `docs/development.md` now describe
  the v4 product schema, legacy/managed roots and locks, private control-plane
  modules, and the focused verification command.

### Gate result

- Every Phase 2 work item and stated exit condition passes locally: a legacy
  profile has one restart-stable personal-global root; managed roots are
  distinct and reject tested path attacks; authority revocation and bounded
  10k-catalog registry behavior pass; compatibility opens remain
  personal-global by default.
- Phase 3 has not started.

## 2026-07-26 — Phase 3, observation audit and index correctness

> **Gate correction:** the Phase 2 and Phase 3 completion claims below are
> withdrawn while remediation is in progress. The recorded focused suites did
> not cover every binding requirement: the Phase 2 registry did not yet own and
> verify real `DomainStore` runtimes or reserve their actual resources, and the
> Phase 3 evidence explicitly omitted the required SQLite fault harness while
> several semantic/index paths still bypassed or weakened the audited protocol.
> This section remains historical evidence, not a completion declaration.

### Scope and environment

- Exact work item: Phase 3 in `09-delivery-sequence.md`.
- HEAD: `bcd10fd3f736290c536ac94daf5a9014ef711828`; Rust 1.85.0, edition 2021,
  Apple arm64 macOS 26.5.2. The Phase 0–2 dirty worktree was preserved.
- This slice adds the graph audit protocol, schema migrations, revision/diff/
  outbox modules, compatibility wiring, focused causal tests, and storage docs.
  Public admin/CLI/MCP history routes remain deliberately deferred to Phase 5.

### Durable design and production path

- Graph migration version 4 now applies v1, v2, v3, and v4 as individually
  transactional steps. v3 adds `domain_state`, changesets, requests/events,
  observation revisions, and an index outbox. v4 adds entity/relation revision
  and mirror-outbox columns/tables needed by the later identity phase. Existing
  populated v2 rows migrate without a baseline scan; a failed v3 step rolls
  back both schema objects and the recorded version.
- `ChangeSetDraft` is bounded and typed. Explicit binary framing (including
  operation tags, scalar encodings, option markers, ordered collections, and
  authority digest bytes) produces the BLAKE3 request hash; persisted JSON is
  inspection data only. Idempotency is keyed by `(space_id, idempotency_key)`
  and changed request hashes fail closed.
- Immediate requests apply canonical rows, immutable observation revisions,
  transition events, semantic generation, and outbox rows in one SQLite
  transaction. Proposals persist intent only; approval/rejection uses exact
  state-version compare-and-swap. Update, retire, restore, and revert enforce
  row-version/revision preconditions and return typed stale errors.
- Legacy `remember`, `remember_batch`, `forget`, and memory-tier promotion use
  the same audited projection without changing their existing result shapes.
  The index is a derived projection: post-commit repair drains a bounded,
  ordered outbox, records attempts/errors, exposes `index_repair_required`, and
  makes recall fail closed while stale. Lazy baseline creation is resumable by
  cursor, and revision diffs are deterministic field lists.
- Entity/relation history schema is present for the planned Phase 6 seam, but
  no new identity or public relation-edit surface is enabled in this phase.

### Principles conformance

1. One general typed changeset pipeline owns decode, validation, mutation,
   revision, event, outbox, and publication; compatibility wrappers delegate to
   it rather than creating alternate semantics.
2. Request policy (space, actor, authority, reason, source, mode) is normalized
   at submission; the transaction consumes the immutable draft and never reads
   ambient configuration.
3. Graph transactions own canonical/audit atomicity; the index repair owner
   owns derived publication; future product routes remain separate seams.
4. Idempotency, ordering, stale errors, generations, lifecycle, and legacy
   return values are explicit observable compatibility contracts.
5. Bounds live in validated draft/operation types; row preconditions live in
   graph transactions; derived readiness lives in `domain_state`.
6. Receipts expose verified state/generation/readiness facts, not optimistic
   success; stale index state is a typed fail-closed condition.
7. Direct legacy paths are conservative compatibility adapters with the audited
   fallback; no optional model or index shortcut changes canonical semantics.
8. Operation, object, lifecycle, submit-mode, and changeset-state worlds are
   closed enums with SQL constraints and exhaustive matches.
9. Requests, baseline pages, and repair batches are bounded before mutation;
   outbox order and generation arithmetic are checked and deterministic.
10. SQLite/domain ownership remains single-writer and index repair is serialized
    by the graph rebuild barrier; no nested worker fan-out was introduced.
11. Proposal, approval, rejection, revision, outbox, and repair transitions are
    explicit durable protocols with terminal stale/error outcomes.
12. Request hashes use framed binary values and authority digest bytes; JSON is
    not hash input, and no lossy path/display representation crosses the seam.
13. Barriers, database connections, transactions, and repair guards are named
    RAII owners; guards are released before post-commit index work.
14. Hard payload/text/count caps, typed errors, exact affected-row checks, and
    explicit repair-required state prevent silent partial success.
15. The index outbox names its semantic generation and revision, and publication
    is only marked current after derived work succeeds; recall rejects stale data.
16. No new platform/filesystem assumption was introduced; SQLite and index
    mechanisms stay behind existing crate seams and optional embeddings remain
    absent-safe.
17. Causal tests cover populated migration, rollback, proposal invisibility,
    idempotency, stale edit, lifecycle reversal, resumable baseline, deterministic
    diff, compatibility writes, and the full production workspace matrix.
18. The slice is complete vertically (schema → graph API → compatibility →
    repair → docs) while leaving later public surfaces and identity behavior
    behind their planned stable seams.

### Verification and release evidence

- `cargo test -p openmemory-graph --lib --no-fail-fast`: 166 tests passed with
  default features. `cargo test --workspace --all-features --no-fail-fast` and
  `cargo test --workspace --no-default-features --no-fail-fast` both passed;
  the all-feature graph run included 169 tests. No tests were ignored.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` and
  the corresponding `--no-default-features` command passed.
- `cargo fmt --all -- --check`, `git diff --check`, default and all-feature
  rustdoc (all warnings denied), and `cargo deny check` all passed.
- Focused audit/schema tests exercise v2→v4 populated migration, migration
  rollback, proposal visibility/idempotency, stale lifecycle/edit handling,
  resumable baseline, and deterministic revision diff. Workspace integration
  tests exercise the existing CLI/MCP/daemon/engine compatibility surfaces.
- No public Phase 5 review/edit route or Phase 6 identity/relation mutation was
  enabled. A controllable custom SQLite VFS is not required by the current
  rusqlite production seam; transaction rollback and derived-state failure
  paths are covered here, with dedicated VFS fault injection reserved for the
  storage fault-harness work before its corresponding release gate.

### Gate result

- Phase 3 graph audit, migration, compatibility, baseline, revision diff, and
  durable index-repair actions are complete and verified locally.
- Existing outputs and feature combinations remain green; recall fails closed
  on persistent stale derived state, and repair remains explicit/idempotent.
- Phase 4 has not started.

## 2026-07-27 — Phase 2/3 production remediation

### Phase 2 repairs and gate result

- Product migrations now update their metadata row and
  `PRAGMA user_version` in the same `BEGIN IMMEDIATE` transaction, reject
  metadata/user-version disagreement, and reject missing or out-of-order
  pending steps.
- Managed and adopted legacy bindings open and verify the actual scoped
  `DomainStore` before activation. Manifest publication also verifies the
  store, persists the derived index, and requires a complete WAL checkpoint.
- `SpaceRegistry` now owns the real shared `Arc<DomainStore>` runtime and its
  actual resource accounting. Daemon leases retain that registry ownership;
  eviction and maintenance flush/checkpoint the owned store instead of
  manufacturing a second store or lock owner.
- Authorization leases can construct the graph review authorization only after
  revalidating the current Review role. Revocation, generation changes, and
  registry draining therefore fail before review authority is reused.
- The focused Phase 2 production-path suite passes, including ordered
  migrations, scoped-store identity, real-store ownership, authorization,
  restart, path rejection, cross-process locking, and the bounded 10,000-row
  catalog case. Phase 2's stated exit conditions are restored and Phase 2 is
  complete locally.

### Phase 3 repairs

- Graph schema v7 now retains self-contained immutable revisions after
  canonical cleanup and adds hash-bound, short-lived, one-use destruction
  confirmations plus content-free destruction receipts.
- Change drafts have private validated fields and canonically framed request
  hashes. Immediate, proposal, approval, rejection, conflict, stale-head, and
  idempotent-retry behavior use typed outcomes and exact affected-row checks.
- Typed revision history and typed diffs live in focused `revision` and `diff`
  modules. Diffs include lifecycle and safe provenance without exposing
  unbounded content; history uses bounded keyset pagination.
- Index repair snapshots work before mutation, publishes only the exact rows
  applied, and leaves recall fail-closed while derived state is stale.
  Consolidation and MCP entity forgetting now use audited lifecycle mutations
  rather than bypassing revisions, generations, and the outbox.
- A compatibility batch creates one changeset and one semantic-generation
  advance for the entire caller batch. Legacy source/actor metadata and
  optional timestamp hashing are now canonical and collision-safe.
- Rejected proposal payloads have bounded retention while immutable request
  hashes, safe changeset metadata, review events, and terminal receipts remain.
  The retention test also found and fixed a post-commit writer-mutex deadlock in
  rejection receipt lookup.
- Confirmed hard destruction is human Maintainer-only, is routed through the
  audited transaction, consumes a preview/head/actor/authority-bound token, and
  leaves no canonical content or content-bearing destruction receipt.

### Verification evidence and completion decision

- Full locked workspace tests pass with default, all, and no-default features.
  Focused graph runs pass with 176 default unit tests, 179 all-feature unit
  tests, 175 no-default unit tests, and 16 integration tests; no test is
  ignored.
- Strict workspace Clippy passes for all targets with all features and with no
  default features. Default and all-feature rustdoc pass with warnings denied.
  `cargo fmt --all -- --check`, `git diff --check`, and all four
  `cargo deny check` policy classes pass.
- CI now runs the dependency-policy gate in addition to the feature, lint,
  documentation, and repeated watcher matrices.
- Phase 3 is **not marked complete**. The implementation and ordinary release
  matrix are green, but the binding plan additionally requires a controllable
  SQLite VFS proving `SQLITE_FULL`, short-write, sync, checkpoint, WAL,
  corrupt-page, and reopen faults, plus a release-runner audited-small-write
  hidden/shadow comparison proving both `<=25%` p95 and `<2 ms` absolute
  overhead. Existing real-SQLite disk-full/rollback/reopen/index-repair tests
  are useful but do not satisfy that exact VFS matrix, and no substitute is
  being silently treated as equivalent.

## 2026-07-28 — Phase 3 SQLite VFS fault matrix

### Scope and environment

- Exact work item: the SQLite VFS fault half of Phase 3's
  "transaction, concurrency, abort, and SQLite VFS fault tests" in
  `09-delivery-sequence.md`, which the 2026-07-27 remediation entry
  explicitly recorded as unmet.
- HEAD: `bcd10fd3f736290c536ac94daf5a9014ef711828`; Rust 1.85.0, edition
  2021, Apple arm64, macOS 26.5.2. Bundled SQLite 3.45.3 via
  `libsqlite3-sys` 0.30.1 under `rusqlite` 0.32.1.
- Added: `crates/openmemory-graph/tests/fault_vfs/mod.rs` (843 lines, the
  shim) and `crates/openmemory-graph/tests/vfs_fault_matrix.rs` (977 lines,
  11 tests). No production file was changed and no dependency was added.
- Not in scope and not claimed: the audited-small-write p95 comparison on a
  release runner, which is Phase 3's other outstanding exit condition.

### Durable design and production path

- The harness is a real pass-through SQLite VFS, not a mock of the store. It
  is built directly against `rusqlite::ffi` (`sqlite3_vfs_register`,
  `sqlite3_vfs`, `sqlite3_io_methods`) and registered as the **process
  default** with `makeDflt = 1` before any store is opened. Because it is the
  default, `MemoryStore::open` reaches it through an unmodified
  `Connection::open`: there is no injection seam, no test hook, and no
  `open_with_flags_and_vfs` call anywhere in the library. Faults land below
  SQLite, so the audited changeset transaction, WAL checkpointing, index
  repair, and recall above it are the production code paths.
- A full custom VFS *was* reachable with the current dependency set, so no
  substitute was needed. `rusqlite` exposes no safe VFS binding, so the shim
  is unsafe FFI; it lives only in the `vfs_fault_matrix` test binary, and the
  library keeps `#![forbid(unsafe_code)]` untouched. The workspace
  `unsafe_code = "warn"` lint is suppressed in that one module with the
  reason recorded inline.
- Every file object is `ShimFile { base, real, state, methods }` with the
  delegate's object in the trailing bytes reserved through `szOsFile`. The
  vtable is mirrored slot by slot from the host VFS, so a method the host
  leaves empty is never advertised. One deliberate deviation: `xFetch` always
  answers "no mapping available", which forces every page read through
  `xRead` so read faults are observable. This does not diverge from
  production, where `PRAGMA mmap_size` is 0 and the store never raises it.
- Fault targeting is `(directory, base file name, file kind, operation,
  minimum offset, nth match, fire budget, effect)`. File kind comes from the
  `xOpen` flags rather than the name, so a journal cannot silently miss.
  Nothing is random or time-based. Rules are keyed by directory and every
  test uses its own `tempdir`, so the eleven tests are isolated under cargo's
  default parallelism. `FaultHandle` is an RAII owner: a panicking test
  cannot leak a fault into a parallel one.
- Two faults are *self-calibrating* rather than hard-coded. A never-firing
  probe rule counts the qualifying operations a warm-up run performs, and the
  real fault is armed just past that count. This is how the corrupt-read
  fault is placed on the scan path rather than the open path, and how the
  index-publication fault is placed on the outbox transaction rather than the
  canonical commit. Measured on this store a submit performs 54 WAL writes
  with the last 6 belonging to publication, but neither number appears in the
  source: a schema change moves the boundary and the calibration moves with
  it.

### The matrix: injection point and surviving invariant

| Fault | Injected as | Invariant asserted to survive |
|---|---|---|
| `SQLITE_FULL` | `xWrite` on `-wal`, 1st write of the commit | Submit returns `Err`; the changeset leaves **no row at all**; canonical content is byte-identical to the pre-fault baseline; reopen recovers it exactly |
| Short write | `xWrite` on `-wal` writes 8 bytes and reports success | All-or-nothing: the changeset row's existence and the content's visibility must **agree**; the pre-fault baseline survives; the store stays openable |
| Sync failure | `xSync` on the main DB during `wal_checkpoint` | Checkpoint must not report `complete`; the already-committed change is **not lost**; reopen replays it |
| Checkpoint failure | `xWrite` on the main DB during `wal_checkpoint` | Checkpoint must not report `complete`; committed data stays readable through the open handle; the WAL is retained and replayed on reopen |
| WAL failure | `xOpen` on `-wal` returns `SQLITE_CANTOPEN` | `MemoryStore::open` fails rather than running in an unknown journal mode; the file stays structurally sound; a later open with the fault cleared recovers the baseline |
| Corrupt page (open) | `xRead` on the main DB, byte 0 XOR `0xFF`, offset ≥ 4096 | Either the open fails closed, or it succeeds with a **correct** answer; a plausible-but-wrong result is a failure; corruption is transient and must not be persisted |
| Corrupt page (scan) | Same, calibrated past the open's reads | The read path fails closed (`database disk image is malformed`), not silently wrong; reopen returns the exact pre-fault content |
| Reopen after fault | `SQLITE_FULL` held armed across a second open | A handle opened mid-fault must not see the failed change; after the fault clears the changeset is absent, the store recovers its exact prior state, and is writable again with the audit trail intact |
| Index publication | `xWrite` on `-wal`, calibrated onto the outbox transaction | Canonical commit is durable **and** `index_repair_required` is set; `recall` returns `IndexRepairRequired` rather than results missing the change; `repair_index` clears it and recall works again |

Every test additionally runs a structural check against the file through a
read-only connection that never migrates: `PRAGMA integrity_check`,
`PRAGMA foreign_key_check`, no observation head pointing at a missing
revision, no revision naming a vanished changeset, no applied changeset
without events, and no unapplied outbox row without `index_repair_required`.
The reopen path also cross-checks that `recall`'s behaviour agrees with
`domain_state`, in both directions.

### Proving the harness injects

This was treated as a first-class requirement, because the failure mode it
guards against (a suite that stops injecting and passes everything) is the
same class as the feature-gated suite that compiled to zero tests.

- Each rule carries two counters. `matched` counts operations its predicate
  accepted; `fired` counts times the effect was applied. Every fault test
  calls `FaultHandle::assert_fired`, whose failure message reports both.
- `harness_counts_matches_separately_from_fires` arms a rule whose `nth` can
  never be reached and asserts `matched() > 0 && fired() == 0`. Without it,
  `assert_fired` could degenerate into a constant; with it, the two counters
  are demonstrably independent.
- `harness_shim_is_default_vfs_and_sees_production_io` asserts that
  `sqlite3_vfs_find(NULL)` resolves to this shim by name, and that the
  production store actually routed main-DB open/read/write/sync and WAL
  open/write traffic through it. A shim that is registered but bypassed
  fails here.
- The suite is **not** feature-gated. It compiles and runs 11 tests under
  default features, `--all-features`, and `--no-default-features`.
- The guards did their job during development. The first full run had all
  nine tests fail with "never fired": on macOS `TMPDIR` resolves through
  `/var` to `/private/var`, so rule paths and SQLite's post-`xFullPathname`
  paths were spelled differently and nothing could ever match. A suite
  without these checks would have reported nine green fault tests that
  injected nothing. A second instance: the corrupt-read fault initially never
  fired because the page cache was already warm and reads never reached the
  VFS at all.

### Findings

No invariant failed. Every assertion above passed on the first full run once
the harness itself was correct, and the matrix is stable: 12 consecutive runs
under default parallelism and 3 clean runs in each of the three feature
configurations, with no flakes.

Two behaviours are worth recording precisely, neither of which is a defect:

1. **A short WAL write is acknowledged and then lost.** `submit_changeset`
   returned `Ok` and the change was absent after reopen. This is correct
   layering, not a bug: the VFS reported success for a partial write, and
   SQLite's WAL frame checksum then discarded the torn frame at recovery
   rather than replaying garbage. The invariant that matters, no torn state, held. The general point it makes concrete is that an audited-write
   receipt is not a durability claim under `synchronous=NORMAL`;
   `wal_checkpoint().complete` is the store's durability predicate, and the
   sync/checkpoint tests confirm it correctly refuses to claim completeness
   when the fsync or the page copy failed.
2. **The commit path performs no WAL fsync.** With `synchronous=NORMAL` in
   WAL mode the durability barrier is the checkpoint, so a sync fault cannot
   land on commit and necessarily lands on the checkpoint. That is a property
   of the production pragma set, not a limitation of the harness, and the
   test documents it rather than working around it.

### What this does and does not rule out

Stated deliberately, because a clean matrix invites over-reading.

- It **does** establish that below-SQLite I/O failures on the graph database
  and its WAL leave no torn canonical state, no partially-applied changeset,
  no silently-stale derived index, and a store that reopens to its exact
  prior content or fails with a typed error.
- It does **not** cover real hardware failure modes: reordered writes,
  partial or lying `fsync` on network filesystems, controller-level cache
  loss, or byte rot on disk. The shim is in-process and cooperative; it
  models a filesystem that fails where told, not one that fails where it
  likes.
- It does **not** cover the index files outside SQLite's own I/O. The vector
  index and, without `fts5`, the BM25 store are written through plain
  `std::fs`, which never enters a VFS. Their failure behaviour is untested
  here.
- The corrupt-page faults inject *transient* read corruption. They prove the
  store fails closed on a bad read and does not persist the damage; they
  prove nothing about durable on-disk corruption, whose detection belongs to
  SQLite's page format rather than to this store.
- Coverage is single-process. Cross-process fault interleaving, and faults
  during the Phase 2 lifetime locks and manifest publication, are not
  exercised. Phase 8's "VFS/filesystem faults and every-boundary aborts on
  macOS/Linux" is a strictly larger obligation that this work does not
  discharge; the shim is reusable for it.
- Results are macOS/arm64 only. The unix VFS is shared with Linux, but the
  calibrated `nth` values are re-derived at runtime rather than assumed, so
  the suite should port without edits. That is an expectation, not a
  measurement.

### Verification and release evidence

- `cargo test -p openmemory-graph --test vfs_fault_matrix`: 11 passed, 0
  failed, 0 ignored, in each of default, `--all-features`, and
  `--no-default-features`. Repeated 12 times under default parallelism with
  no flakes.
- Full crate suite unaffected: 178/16/11/1 tests pass with default features,
  181/16/11/1 with `--all-features`, 177/16/11/1 with
  `--no-default-features`. No test is ignored.
- `cargo clippy -p openmemory-graph --all-targets --all-features -- -D
  warnings` and the `--no-default-features` equivalent both pass. One
  `cast_ptr_alignment` finding was fixed properly (stepping one `ShimFile`
  forward instead of casting through `*mut u8`) rather than suppressed.
- `cargo fmt --all -- --check` reports no diff in either new file.
- Workspace-wide commands were not run to completion: `openmemory-eval` does
  not currently build (`min_query_ceiling` / `max_capped_queries` on
  `QualityGates`) under a concurrent edit by another agent. That breakage is
  unrelated to this work and this crate builds and tests clean in isolation.

### Gate result

- The VFS half of Phase 3's fault requirement is satisfied in my judgement:
  every named fault (`SQLITE_FULL`, short write, sync, checkpoint, WAL,
  corrupt page, reopen) is injected below SQLite through a controllable VFS
  against the production storage path, each asserts a surviving invariant
  rather than a returned error, and the harness proves it injected.
- Before fully trusting it I would still want: the same matrix run on Linux
  in CI; the suite wired into the CI job list so it cannot silently stop
  running; and coverage of the non-SQLite index files, which no VFS can
  reach.
- **Phase 3 is not marked complete.** Its other exit condition, the
  audited-small-write p95 hidden/shadow comparison on a release runner, is
  outside this work and unmeasured here.
