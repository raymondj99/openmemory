# Test, Performance, and Release Plan

## Quality bar

Match the repository's strongest existing patterns: deterministic unit tests,
property tests for pure invariants, real SQLite transaction/migration tests,
HTTP/MCP/CLI contract tests, concurrent race tests, subprocess crash recovery,
release-mode measurements, and full feature-matrix gates.

Line coverage is not the goal. Each non-negotiable invariant must have at least
one test that would fail under the obvious shortcut implementation.

## Baseline prerequisite

Before final feature validation, stabilize or correctly platform-gate the
pre-existing `openmemory-watch/tests/integration.rs` readiness flake documented
in the HEAD assessment. Preserve causal synchronization; do not replace it with
a long sleep. Run repeated macOS/Linux isolated and workspace tests to prove the
change. Record baseline/fix evidence in `LOGBOOK.md`.

This prerequisite is separate from semantic-space behavior but required before
claiming the whole workspace is green.

## Unit tests by ownership

### `openmemory-core`

- Every ID accepts canonical valid encoding and rejects empty, noncanonical,
  path-shaped, separator/control/oversize input.
- serde/Display/FromStr round trips.
- Owner/context/role ordering and capability checks.
- `ReadSet` rejects zero, duplicate, unauthorized, >4, and preserves explicit
  precedence.
- New config sections default/round-trip/validate hard bounds.

### `openmemory-merge`

- Canonical snapshot rejects duplicate IDs/revisions/sets, dangling relation
  endpoints, cycles where forbidden, unknown versions, and over-limit fields.
- Semantic hashes are independent of input order and change for every semantic
  field; derived telemetry never changes them.
- Identity evidence classification exhaustively tests lineage, verified IDs,
  claimed IDs, resolver generations, ontology compatible/incompatible/unknown,
  simultaneous proof/contradiction, and context-only packets.
- Packet/receipt/plan hash tampering and staleness fail.
- Candidate coverage rejects missing, extra, duplicate, stale, truncated-
  unacknowledged, and undetermined-as-same resolutions.
- Planner accounts every source/target entity/claim/relation exactly once.
- One-to-one coalescence, deterministic source-qualified IDs, contribution
  union, endpoint rewiring, exact assertion dedup, no silent delete.
- Three-way unchanged/source-only/target-only/same/disjoint/conflict and
  delete-vs-edit cases.
- Plan and predicted result deterministic across allocation/insertion order.

### `openmemory-graph`

- Fresh/v1/v2 schema migration to v4, populated fixtures, idempotent reopen,
  rollback per failed step, future-version rejection.
- Immediate changeset canonical/audit/generation/outbox all-or-nothing.
- Proposal invisibility; approve/reject/conflict/revert transition table.
- Idempotency same key/same hash and same key/different hash across reopen.
- Lazy baseline revision for legacy observations/entities/relations.
- Semantic edit creates one head/row version; telemetry update does not revise.
- Retire/restore/revert and guarded destruction.
- Cross-domain draft rejected before changeset insertion.
- Index outbox success, failure, crash-after-commit, idempotent repair, and
  recall refusal while unrepaired.
- Canonical relation ID/mirror outbox idempotency and traversal readiness.
- Canonical export/import/hash round trip with tombstones/history/contributions.

### `openmemory-engine`

- Manifest round trip, catalog-binding mismatch, symlink/path escape, domain
  count mismatch, legacy root binding, stable `.space-id`, and cross-domain
  bound-ID mismatch rejection.
- One-space layered recall exact fast-path parity.
- Two/four-space isolation, provenance, deterministic completion-order fusion,
  exact semantic dedup with all origins, cache generation/auth invalidation.
- Bounded concurrency; no more than configured tasks/handles under stress.
- Consistent snapshot generation vector under concurrent accepted writes.
- Staged materialization produces predicted hash and rebuilds derived state.
- Promotion/recovery state transition table and target-moved rejection.
- Existing domain migration still passes after promotion primitive extraction.
- Journal replay cannot route a request into another `SpaceId`.

### `openmemory-daemon`

- Product v1->v4 migration, future schema, migration rollback, jobs/events
  persistence unchanged.
- Legacy personal-global binding crash/reconcile combinations.
- Catalog owner/context uniqueness including global `project_key=''`.
- Project/workspace create/move/remap and ambiguous VCS suggestion requiring
  confirmation.
- Team role matrix, self-approval policy, membership expiry/revocation, authority
  generation invalidation, agent-vs-human actor enforcement.
- Capability token hash storage, expiry/revocation/rotation, bearer binding,
  context cannot broaden target.
- Registry concurrent one-open, leases, eviction, close-for-promotion, max-open,
  error recovery.
- Route auth/status/error/code/pagination/content-redaction contract.
- Changeset/identity/merge job persistence/restart and event ordering.
- Identity agent adapter timeout/invalid schema/injection-like payload leaves
  unresolved; hard proof cannot be overridden.

### MCP/CLI/watch/admin

- Admin DTO old/new serde goldens and stable snake_case codes.
- Existing MCP E2E initializes/lists/calls every old tool unchanged.
- Context capability selects exact spaces; omission stays personal-global.
- Team remember returns proposal; agent approval tools do not exist.
- Provenance fields and context instructions/golden registry are deterministic.
- HTTP wrong/expired/mismatched context gets safe auth error; bearer alone does
  not let raw requested IDs broaden scope.
- CLI parse/completion/output/JSON/destructive confirmation for every new
  command; daemon-required repair message.
- Watch/ingest bind one space for entire run and reports provenance.

## Property tests

Use `proptest` with at least 256 cases in CI defaults for pure, cheap models:

- ID parsing never panics on arbitrary Unicode/bytes and accepted IDs are path
  safe.
- Read-set resolution never emits duplicate/unauthorized/>4 spaces.
- Fusion result independent of component completion order and bounded by
  `top_k`.
- Canonical hash independent of row/set insertion order.
- Random revision operation sequences agree with a small in-memory reference
  model for heads/lifecycle/history.
- Three-way merge symmetry where direction is semantically symmetric and
  explicit asymmetry tests where source/target direction matters.
- Independent disjoint changes commute.
- Candidate analysis symmetric under entity-pair canonicalization.
- Label/alias/embedding/claimed-ID-only packets can never yield deterministic
  same proof.
- Planner has complete accounting, no dangling endpoints, no target overwrite,
  and contribution union order invariance for arbitrary small graphs.
- Plan/result hash changes under any semantic action mutation.

Persist failing seeds in test output and add minimized regressions for every
found bug.

## Concurrency tests

Use barriers/channels/condition variables and fixed clocks; no timing sleeps.

- Two reviewers approve one proposal: exactly one commit, both receive same
  terminal result.
- Two proposals from same head: at most one apply, one conflict, no lost update.
- Approval races edit/retire/restore/revert and membership revocation.
- Recall races commit/index repair; it sees coherent old/new or typed repair,
  never canonical-new/index-old success.
- Context resolution races team revocation and space close.
- Registry eviction races recall and merge target close.
- Merge confirmation races source/target writes: movement before confirmation
  invalidates preview; target movement after confirmation aborts before rename;
  later source writes are not included and are never modified by the job.
- Two merge jobs target same space: one exclusive lease; other conflicts.
- Multiple process/daemon attempts cannot own the same promotion intent.

Run selected concurrency tests under a repeat harness (at least 100 iterations
locally/reference CI) to expose rare ordering bugs without making ordinary CI
unbounded.

## Crash/recovery tests

Use self-spawned integration test workers or injectable private fault hooks that
call `abort()` in a subprocess. Error-return simulation alone is insufficient.

### Changeset/index boundaries

- Before/after changeset insert.
- After revisions/projection/events/outbox but before transaction commit.
- Immediately after graph commit, before index apply.
- During index apply and before indexed-generation update.
- During mirror outbox apply.
- Reopen/repair repeatedly; result is exactly old or new semantic state and
  never duplicate revision/contribution.

### Space/catalog boundaries

- Catalog `creating` before manifest.
- Manifest temp/file/parent fsync and before catalog activation.
- Registry close/checkpoint.
- Backfill page before/after cursor update.

### Material promotion boundaries

- Staging creation/import/index rebuild/verification.
- Intent write and fsync.
- Target -> backup rename and parent fsync.
- Staging -> target rename and parent fsync.
- Promoted open/verify.
- Product catalog/lineage/job commit.
- Intent clear and backup cleanup.

For every boundary, assert repeated recovery yields verified old or new target,
source hash unchanged, no ambiguous open, retained usable backup when required,
and idempotent final product job state. Run on macOS and Linux.

## Permanent fixtures

Commit small inspectable fixtures under production test directories:

```text
crates/openmemory-graph/tests/fixtures/schema_v1.sql
crates/openmemory-graph/tests/fixtures/schema_v2.sql
crates/openmemory-graph/tests/fixtures/audit_legacy_rows.json
crates/openmemory-engine/tests/fixtures/spaces_project_a_b_team.json
crates/openmemory-merge/tests/fixtures/github_codex_homebrew_tools.json
crates/openmemory-merge/tests/fixtures/github_axum_actix_web.json
crates/openmemory-merge/tests/fixtures/github_mathlib_lean4.json
crates/openmemory-merge/tests/fixtures/homonyms_and_identifiers.json
```

Sanitize experiment fixtures to the minimum facts/evidence needed; retain
source revision URLs/licenses/provenance and no large copied repository text.
Goldens include expected candidates, decisions, diff/accounting/hashes, and
recall assertions. Production code has no fixture-name special case.

The Project A/B/team fixture includes:

- same label with different meanings;
- same verified identity across spaces;
- same fact with distinct origin;
- contradictory claims;
- relation rewiring through coalesced and distinct endpoints;
- personal/team read/write/review behavior;
- one- and four-domain variants.

## Security/adversarial tests

- Raw owner/team/project/space IDs, source values, repo URLs, paths, and MCP
  target fields cannot escalate scope.
- Context capability substitution, replay after expiry/rotation/revocation,
  wrong bearer/principal/actor kind, and forged JSON fail.
- Manifest/root/staging path traversal, symlink escape, hard links where
  relevant, cross-filesystem promotion, and malicious imported manifests fail.
- Candidate explosion via common labels/aliases/claimed IDs remains bounded and
  cannot starve verified proof.
- Oversized/deep JSON, unknown fields, invalid UTF-8 boundaries, duplicate
  evidence IDs, NaN/inf scores, and hash mismatch fail before mutation.
- Prompt-injection text in entity content remains inert data. Agent proposal
  output cannot cite/invoke instructions outside packet schema.
- Logs, SSE events, errors, job reports, and Debug impls omit tokens, semantic
  content, and unredacted local paths by default.
- Imported backup/team membership does not become local authority.

## Criterion/CodSpeed benchmarks

Add deterministic groups to `openmemory-bench/benches/openmemory.rs` or split
the file into focused bench files if it becomes unwieldy:

1. `context_resolution`: warm indexed resolution with 1, 1k, 10k catalog
   spaces; membership/no-team/team cases.
2. `layered_recall`: one/two/four spaces, keyword/hybrid, cache cold/warm,
   1k/10k observations per space; verify results outside timed loop.
3. `layered_fusion`: merge 64/256/512/1,024 prebuilt candidates to top 10/128.
4. `changeset_write`: legacy wrapper vs audited immediate, proposal insert,
   approval, edit, outbox repair at representative 1 KiB/16 KiB payloads.
5. `audit_storage`: bytes per canonical+history revision for 1 KiB/16 KiB/
   256 KiB content, reported rather than micro-timed.
6. `identity_analysis`: bounded packets of 4/16/64/128 evidence items and
   adversarial homonym candidate pages.
7. `merge_plan`: 100/1k/10k/100k entities with similar relation counts,
   exact/half/distinct/conflict mixes; setup snapshots outside timed loop.
8. `merge_materialize_small`: 100/1k/10k canonical objects into a temp root,
   derived rebuild measured separately.
9. `daemon_space_api`: list/context/proposal/diff/merge status routes, extending
   existing admin benchmark style.

All data is fixed-seed. Report throughput and allocation/peak RSS for planner
and materialization with a release harness where Criterion cannot.

## Performance budgets

Use repeated interleaved control/feature measurements on a documented reference
Apple Silicon machine and one Linux x86_64 reference. Shared CI/CodSpeed detects
regressions but does not enforce noisy absolute wall-clock thresholds.

| Path | Gate |
|---|---|
| Legacy one-space recall orchestration | <1 ms added warm p95 and no catalog-size trend. |
| Two-space warm recall | p95 <= `1.25 * slower component p95 + 5 ms`. |
| Fusion 512 -> 128 | <2 ms p95. |
| Warm context resolution with 10k catalog rows | <2 ms p95, indexed query, bounded rows. |
| Audited small immediate write | <=25% p95 overhead vs same canonical transaction without history and <2 ms absolute added overhead. |
| Proposal insert / approval bookkeeping excluding embedding | <2 ms p95 each on reference fixture. |
| Identity deterministic packet analysis | <100 us p95 at configured max evidence. |
| Merge plan 10k entities + 10k relations | <150 ms p95 release. |
| Merge plan 100k entities + 100k relations | <1.5 s p95, peak working memory <=3x canonical input bytes. |
| Materialization canonical import | >=70% of existing domain-migration raw-import throughput on equivalent rows. |
| Final target admission pause | <250 ms p95 excluding a moved-target abort; staging/index work happens before pause. |
| Disk amplification during promotion | <=2.2x live target bytes plus explicitly retained backup; preflight enforces quota. |
| Registry | open runtime count <= configured cap; idle CPU effectively zero. |

If a gate misses, profile before changing it. Prefer sorted streaming iterators
for 100k planning, batched SQL, precomputed hashes/embeddings, and bounded
scratch buffers. Do not add caches without invalidation evidence.

## Evaluation metrics for optional agent proposals

On versioned representative/adversarial identity fixtures report:

- Candidate recall/coverage before agent.
- Deterministic proof/contradiction rate.
- Agent same/different/undetermined/invalid/timeout rate.
- False-same and false-different (both release blocking above agreed threshold;
  false-same is weighted more severely).
- Human review rate and agreement.
- Median/p95 review time in a small human study before default enablement.
- Model/prompt version, cost, and latency.

No network/model evaluation is required for ordinary CI. A deterministic fake
adapter exercises lifecycle; real adapter evaluation is an explicit release
artifact. Default production behavior remains safe without a configured model.

## CI and release commands

Per implementation slice, run the narrow owning tests plus:

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

Before completion also run:

- production merge real-world fixture tests;
- repeated concurrency suite;
- all subprocess crash matrices on macOS and Linux CI;
- Criterion/CodSpeed comparison with recorded revision/toolchain/features;
- release merge stress/materialization harness at 10k and 100k;
- backup/restore/domain-migration compatibility tests;
- MCP and CLI process E2E tests.

## Release blockers

- Any unauthorized cross-space read, traversal, cache result, backup, restore,
  or write.
- Any automatic same identity based only on label, embedding, agent output,
  claimed identifier, timestamps, or uncontrolled kind.
- Any agent path that approves itself, mutates canonical identity directly, or
  overrides trusted contradiction.
- Canonical mutation without audit or audit without canonical mutation.
- Silent stale-index recall, lost update, non-idempotent retry, or partial
  same-domain changeset.
- Relation edit/merge before canonical relation/mirror readiness.
- Incomplete candidate/source/relation accounting or silent delete.
- Material promotion without fresh target check, same-filesystem preflight,
  predicted hash verification, fsynced intent, retained recovery path, and
  tested old-or-new recovery.
- Startup/backfill proportional to corpus size.
- Unbounded catalog scan, threads, handles, candidate fanout, payload, memory,
  staging disk, or event content.
- Any required workspace gate failing, including the known watcher flake.
