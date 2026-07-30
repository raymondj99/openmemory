# Production Memory Spaces

Status: Phases 0–2 are complete locally. Phase 3's production implementation
and ordinary release matrix pass, but Phase 3 completion remains withheld
pending the exact controllable-SQLite-VFS fault matrix and the audited-write
hidden/shadow performance comparison required by the plan. Phase 4 has not
started.

Baseline: `bcd10fd`, OpenMemory `0.4.4`, Rust `1.85`, edition 2021.
Phase 0 revalidated this HEAD and recorded all drift and verification evidence
in `IMPLEMENTATION_LOG.md`.

## Authority

Use these sources in this order:

1. `PROMPT.md` defines the product contract.
2. `10-engineering-principles.md` defines how the audited
   `~/PRINCIPLES.md` engineering policy applies to this product.
3. The numbered design documents define the production implementation.
4. `experiments/memory-model/NOTEBOOK.md` may refine an implementation tactic
   only where it records evidence or an unresolved production blocker.
5. Prototype source proves isolated behavior only. Do not copy its schemas,
   module layout, or storage code into production.

If implementation evidence invalidates a plan detail, stop, record the result,
update the affected plan document, and obtain review before depending on the
change.

## Amendments from measured evidence (2026-07-27)

Four decisions in this plan were changed after being tested against real
corpora. Evidence is recorded in `../../LOGBOOK.md`; the reasoning lives
in the owning documents.

| Change | Was | Now | Evidence |
|---|---|---|---|
| Cross-space composition | sort by per-space score | **invariant:** never compare uncalibrated scores across spaces. **provisional policy:** deterministic rank interleaving | raw score gave one space **0.0%** of fused positions across 839 queries; corpus age is a demonstrated sufficient cause (λ=0 and an age-matched arm each remove it); size untested |
| Entity identity | unique `(name, entity_type)` | immutable `EntityId`; names are assertions | two different people under one name silently merged into one row |
| Physical routing | name hash, with `entity_rehome` | immutable `EntityId`, no re-home | a rename is now one local revision |
| Access-count boost | ranking input | scheduled for removal | changed top-1 for 67% of queries with no evidence of benefit |

One defect is recorded and unresolved, and it is now the primary one:
multiplicative priors applied after fusion overwhelm the fused signal.
Disabling decay raised recall from 0.629 to 0.839 on the same corpus and
queries, and it is the root cause of the cross-space failure above. See
`08-test-performance-and-release.md`.

Confidence levels differ and are stated per item. Immutable `EntityId`
is **established** — a schema constraint and a reproduced coalescence
both demonstrate a correctness violation. Cross-space composition has an
**established invariant** and a **provisional policy** with an explicit
promotion gate in `05-context-and-team-spaces.md`. Access-count removal
is a **risk-reduction decision**: its churn is measured, its utility is
not.

The plan also gained retrieval-quality release gates. It previously
specified latency, memory, disk, and crash budgets in detail and had no
way to state that retrieval had become worse.

## Fixed architecture

- A memory space is an authorization, ownership, retention, audit, backup, and
  publication silo. It owns one complete `DomainStore`.
- An engine domain is a storage and execution shard inside one space. It is
  never an authorization scope.
- Entity identity is an immutable `EntityId`. Names and aliases are
  versioned assertions and never determine identity or physical routing.
- Cross-space composition combines ranks, never uncalibrated scores.
  The current rule is deterministic rank interleaving and is provisional.
- The existing profile store becomes personal-global in place.
- A request has one authorized ordered read set of at most four spaces and
  exactly one write target.
- Interactive atomic changesets are confined to one engine domain.
- Cross-domain work uses one shared bounded executor, a complete barrier,
  deterministic numeric reduction, and serialized publication.
- Graph rows and immutable revisions are semantic truth. Product SQLite is
  control-plane truth. Indexes, caches, mirrors, stubs, and journals are
  derived or operational state.
- Identity is conservative: labels, embeddings, timestamps, claimed IDs, and
  agent output are never proof.
- Overlay is the default composition mechanism. Material merge is directional:
  source remains unchanged; a staged, verified target replaces the old target.
- No model or network call is on recall, ordinary remember, approval, or
  promotion critical paths. Agents produce proposals only.
- No `unsafe` code. No Windows material-merge capability until its rename and
  directory-fsync behavior is tested.
- One correct typed pipeline owns each operation. Direct paths, caches,
  optional models, platform adapters, and other specializations implement the
  same semantic contract behind conservative eligibility facts and a general
  fallback.

## Document map

| File | Purpose |
|---|---|
| `00-head-assessment.md` | Baseline, reusable mechanisms, structural risks |
| `01-contract-and-invariants.md` | Vocabulary, types, authorization, semantic invariants |
| `02-architecture-and-files.md` | Crate ownership, module boundaries, shared executor |
| `03-persistence-and-migrations.md` | Layout, schemas, migration, snapshots, recovery |
| `04-changesets-and-manual-editing.md` | Atomic audit, revisions, editing, lifecycle |
| `05-context-and-team-spaces.md` | Catalog, capabilities, registry, layered recall |
| `06-identity-and-merge.md` | Evidence, pure planning, streaming materialization |
| `07-surfaces-and-compatibility.md` | Admin, CLI, MCP, readiness, operator behavior |
| `08-test-performance-and-release.md` | Proof matrix, budgets, release blockers |
| `09-delivery-sequence.md` | Reviewable implementation phases and exit gates |
| `10-engineering-principles.md` | Binding application of the audited engineering principles |
| `11-entity-identity-migration.md` | Name-keyed → id-keyed identity: schema, adapter, backfill, cost |

## Implementation discipline

- Follow `09-delivery-sequence.md` in dependency order.
- Build and pin the correct general pipeline before adding a specialization.
  Every cheaper path uses the same typed seam and has a conservative
  eligibility test, general fallback, and equivalence tests.
- Normalize config, environment, platform capability, authorization, and
  explicit user selection once at the owning boundary. Hot loops consume
  immutable resolved policy and never reread ambient state.
- Keep authorization and publication on coordinators; domain workers touch
  only their assigned SQLite/index family.
- Preserve the existing single-space and single-domain fast paths.
- Extract focused modules before adding behavior to current monoliths.
- Validate bounds and reserve execution/memory budgets before mutation or task
  allocation.
- Use typed states, private fields, exhaustive matches, checked conversions,
  deterministic ordering, and explicit canonical encodings.
- Keep OS paths as paths and semantic/wire text as validated UTF-8. Never use a
  lossy display conversion as identity, cache-key, authorization, or hash
  input.
- For every cache, record its owner, complete key inputs, validity generation,
  publication point, invalidation events, and false-hit regression.
- Acquire resources into named RAII owners before later fallible work. Every
  streaming or long-running state machine has explicit terminal success,
  error, cancellation, and cleanup transitions.
- Use the repository `Migrator`, `FixedClock`, `TempDir`, admission pause,
  checkpoint, and verified promotion patterns.
- Do not hide uncertainty with sleeps, retries without bounds, best-effort
  durability, or claims of cross-SQLite atomicity.

## Definition of done

Completion requires all of the following:

- Existing profiles, CLI commands, MCP tools, admin behavior, feature
  combinations, backup/restore, and domain migration remain compatible.
- Spaces are physically isolated and authorization revocation invalidates
  handles, capabilities, and caches before reuse.
- Every new semantic mutation is atomically auditable; derived-state failure is
  explicit and repairable.
- Layered recall preserves facade-cache semantics and enforces one global
  worker/admission bound across concurrent 1/4-space × 1/4-domain workloads.
- Identity and merge have complete deterministic accounting, immutable
  provenance, independently verified hashes, and no silent deletion.
- Snapshot and material publication are generation-bound, fsynced, fail-closed,
  and recover to a verified old or verified new target.
- Production snapshot/materialization streams are bounded; no implementation
  retains complete source, target, and result graphs in memory.
- Every release blocker in `08-test-performance-and-release.md` is closed with
  production evidence on the required platforms.
- Every principle-conformance gate in `10-engineering-principles.md` is mapped
  to production evidence, including normalization, fallback equivalence,
  representation fidelity, cache completeness, portability, and cleanup.
- Every delivery phase passes focused unit/property tests and its required
  production-path real-world scenario; neither can substitute for the other.
- Formatting, tests, clippy, rustdoc, dependency policy, crash matrices,
  compatibility suites, and performance budgets pass and are recorded.
