# Delivery Sequence

## Working method

Implement as small reviewable slices. Each slice ends green under narrow tests
and the relevant workspace feature matrix. Do not maintain a long-lived branch
where migrations, APIs, and tests are all broken until the final day.

Append production implementation observations, measurements, failed options,
and corrections to the existing `LOGBOOK.md`; never rewrite prior prototype
entries. Suggested prefix: `P000`, `P001`, ... with revision/command/result.

The phases below are dependency order. They may be commits/PRs only when the
user authorizes git publication; the technical boundaries apply regardless.

## Phase 0 — Baseline and safe module extraction

Work:

1. Record HEAD/status/toolchain and run current narrow/full gates.
2. Reproduce and fix/platform-correct the watcher readiness flake without
   sleeps or semantic weakening.
3. Extract daemon route/auth/health/runtime helpers from the 2,164-line
   `lib.rs` with no endpoint/wire behavior change.
4. Add admin submodule/re-export skeleton and engine `space`/`merge` module
   skeletons without runtime changes.
5. Correct crate/benchmark documentation drift discovered in assessment.

Exit:

- Existing tests/goldens pass repeatedly.
- No API or storage change.
- New feature code has focused destinations rather than monolith accretion.

## Phase 1 — Pure contracts and planner crate

Work:

1. Add `openmemory-core::space` validated types/tests.
2. Add `openmemory-merge` crate with canonical records, explicit hashing,
   evidence packets, receipts, three-way classifier, planner, accounting.
3. Port generic prototype invariants—not prototype I/O/framework—into unit and
   property tests.
4. Port sanitized real-world fixture goldens to the production crate.
5. Add pure planner/identity Criterion groups.

Exit:

- Pure crate has no I/O/model/policy dependencies.
- Homonym/`cerpheus`, candidate coverage, contribution preservation, endpoint
  rewiring, three-way conflicts, plan/result hash cases pass.
- 10k/100k planning budgets have recorded release measurements or an optimized
  streaming implementation before proceeding.

## Phase 2 — Product catalog, local authority, and manifests

Work:

1. Refactor product store to ordered v1->v4 migrations with fixture tests.
2. Implement installation principal, local teams/membership generations,
   projects/workspace mappings, space catalog CRUD, capability storage.
3. Implement `space.toml`, path/root validation, atomic write/verify/reconcile.
4. Bind legacy profile root as personal-global in place with crash tests.
5. Implement bounded `SpaceRegistry` open/lease/eviction/readiness.
6. Add space/context admin DTOs and service-level tests; keep routes hidden or
   capability-reported false until end-to-end behavior is ready.

Exit:

- Old profile opens unchanged and has exactly one binding after repeated crash/
  restart.
- New spaces are physically distinct, manifests fail closed on mismatch, and
  handles remain bounded at 10k catalog rows.
- No semantic read/write routing changes yet.

## Phase 3 — Graph v3 audit and index correctness

Work:

1. Add graph v3 migration, changeset/revision/diff/outbox modules and migration
   fixtures.
2. Implement immediate/proposal/approval/reject/revert for observations.
3. Route current remember/batch/set-tier/forget-observation compatibility paths
   through audited transaction helpers.
4. Implement lazy legacy baseline revision and resumable backfill.
5. Implement durable index outbox/generation/repair and readiness failure.
6. Add graph transaction/concurrency/crash/property tests and audited-write
   benchmarks.

Exit:

- New observation writes are atomically auditable and manually editable.
- Proposal invisibility, stale rejection, idempotency, index crash repair, and
  legacy output compatibility pass.
- Audited write budget passes. Audit capability can run shadow/hidden for
  projection comparison.

## Phase 4 — Context resolver and bounded layered runtime

Work:

1. Implement `MemoryContext` resolver/capability lifecycle and team policy.
2. Implement `SpaceHandle`, single-space fast path, layered recall/provenance/
   deterministic fusion/cache invalidation.
3. Route daemon active-store memory services through registry/context.
4. Refactor MCP backend to accept fixed/resolved context while preserving old
   `handle` and tests.
5. Bind watch/ingest to one explicit target.
6. Add isolation, revocation, concurrency, 1k/10k catalog, layered benchmarks.

Exit:

- Project A/B/personal/team fixtures are exactly isolated; explicit overlay is
  the only multi-space view.
- Old no-context requests see personal-global, never all spaces.
- Agent team writes are proposals and cannot self-approve.
- Recall/context performance budgets pass.

## Phase 5 — Human audit/edit surfaces

Work:

1. Add admin changeset/history/diff/edit/lifecycle routes and stable errors.
2. Add CLI space/project/context/changeset/review/memory commands.
3. Add agent-safe MCP context/history/propose tools and provenance fields.
4. Add review SSE events without content leakage.
5. Integrate backup/restore/status/consolidate/prune/domain migration with
   explicit space targets.
6. Update user/operator docs and complete HTTP/MCP/CLI E2E tests.

Exit:

- A user can inspect and modify every new observation without raw SQL.
- Stale UI/CLI state cannot overwrite newer memory.
- Existing CLI/MCP/admin tests remain compatible.
- Backup captures catalog plus all spaces/pending audit state.

## Phase 6 — Graph v4 entity/relation readiness and identity ledger

Work:

1. Add graph v4 entity/relation revisions, verified identifiers, origin
   contributions, canonical relation IDs, mirror outbox.
2. Implement lazy/backfill entity/relation baselines and legacy mirror rebuild.
3. Enable entity lifecycle/same-domain edit; implement staged re-home rename.
4. Enable relation add/edit/retire only after mirror readiness.
5. Implement identity candidate discovery, packet/evidence policy, decision
   events/receipts/revalidation and admin review routes.
6. Implement optional asynchronous `IdentityAgent` trait plus deterministic
   fake; no surprise outbound model call or default provider.
7. Run adversarial candidate/evidence/role/agent evaluations.

Exit:

- Canonical export round-trips every semantic kind/history/contribution.
- Relation mirrors are repairable/idempotent and edit readiness is explicit.
- Shared names never auto-merge; proof/review receipts are current/auditable.
- Agent failure leaves safe separate concepts.

## Phase 7 — Preview, cherry-pick, and production merge planning

Work:

1. Adapt graph canonical snapshots to `openmemory-merge` types.
2. Implement consistent whole-space snapshot and lineage base lookup.
3. Implement cherry-pick destination changesets and provenance.
4. Implement merge preview job: discovery, identity review, complete receipt
   validation, three-way conflicts, deterministic plan/diff/accounting.
5. Add CLI/admin preview/candidate/decision/resolve/status surfaces.
6. Run all three permanent real-world corpora through the same production path.

Exit:

- Identical inputs produce identical structured diff/plan/result hash.
- All candidates/source objects/relations are accounted; stale/missing input
  fails closed; source unchanged.
- Preview contains enough explanation for a human to make every decision.

## Phase 8 — Material merge and recovery

Work:

1. Extract/reuse domain migration promotion primitives.
2. Implement disk preflight, staged materialization, derived rebuild, thorough
   verification, fresh-target recheck, confirmation binding, promotion intent,
   registry close/reopen, catalog/lineage/job commit.
3. Implement exhaustive recovery and separate backup cleanup maintenance task.
4. Add subprocess aborts at every transaction/fsync/rename/catalog boundary.
5. Integrate `doctor`, backup, daemon health/events, CLI apply/recover.
6. Run materialization/stress/disk/RSS/pause/recovery measurements on macOS and
   Linux.

Exit:

- Every injected crash yields verified old/new target, no mixture, source hash
  unchanged, recovery idempotent.
- Moved target/unresolved conflict/expired confirmation/cross-filesystem/quota
  failure prevents rename.
- Performance/disk/pause budgets pass; existing domain migration still passes.

## Phase 9 — Rollout, cleanup, and final gate

Work:

1. Shadow-audit projection/hash comparison on legacy fixtures and representative
   stores.
2. Enable capabilities in order: legacy binding -> spaces/context -> audit/edit
   -> local team review -> identity preview -> material merge.
3. Make new-install defaults production-ready while upgrades retain safe legacy
   personal-global behavior.
4. Finish all docs/changelog/config examples and capability discovery.
5. Run retention cleanup, legal destruction, backup restore, downgrade/future-
   schema failure, and recovery drills.
6. Run every command/gate in the test plan and record exact results/benchmarks.

Exit:

- Every final definition-of-done item in `INDEX.md` and release blocker in the
  test plan is explicitly checked.
- No TODO, ignored test, fixture-specific branch, unbounded placeholder, or
  undocumented degraded behavior remains in the delivered scope.
- `LOGBOOK.md` contains baseline, design deviations, failures, fixes, final
  measurements, and known non-goals.

## Rollout and rollback

Runtime capabilities are separate:

```text
spaces
audit_observations
manual_edit
team_spaces
identity_review
material_merge
```

Capability is true only when required migrations/backfills/readiness pass.
Clients query `/admin/capabilities`; they do not infer from binary version.

Rollback means disabling routing/UI to a capability while keeping the newer
binary/schema readable. Never run an older binary against a newer database;
future-version refusal remains the safety mechanism. A promoted target rolls
back only through verified retained-backup promotion, not file copying over an
open root.

## Decisions already resolved for implementation

- Team authority: local explicit membership/roles only; no hosted sync.
- Team agent writes: proposal; agent cannot decide.
- Applied history retention: retained by default.
- Rejected payload retention: 30 days, then compact metadata/hash.
- Merge backup retention: 7 days default, quota enforced.
- Read-set/open-space bounds: 4 / 8 defaults.
- Cross-space labels/embeddings/agent output: never automatic proof.
- Windows material merge: not advertised until platform tests exist.
- Surprise outbound model/network calls: forbidden; optional adapter explicit.

No additional product decision is required to implement this repository scope.

## Final handoff evidence

The implementing agent's completion report must contain:

- Files/modules/schema versions/API changes by crate.
- Compatibility behavior for existing profile, CLI, MCP, daemon/admin.
- Test commands and pass counts, including repeated concurrency/crash/platform
  matrices.
- Benchmark environment, controls, sample counts, p50/p95/p99/throughput/RSS/
  disk figures, and comparison to every budget.
- Real-world fixture results and merge accounting.
- Migration/backfill/backup/recovery drill results.
- Security review findings and explicit non-goals.
- Any deviation from this plan, with evidence and updated plan/logbook before
  code depended on it.

