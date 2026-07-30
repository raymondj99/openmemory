# Engineering-Principles Conformance

## Audited source and status

This document is the binding application of `~/PRINCIPLES.md` to the Production
Memory Spaces plan. It records a repository-local interpretation so builds and
reviews do not depend on a mutable file outside the repository.

- Source: `/Users/rjow/PRINCIPLES.md`
- Audited: 2026-07-26
- SHA-256:
  `577435372aa83abba17243f9b3a6dc021c72826e1015ec25f583215f573dc011`
- Result: no conflict with `PROMPT.md`; the plan was amended to make every
  principle enforceable before Phase 1.

If the source checksum changes, re-audit it and update this document before
advancing a phase. `PROMPT.md` remains product-contract authority; this
document governs engineering choices within that contract. Any genuine
conflict stops implementation for plan review rather than being resolved
silently.

The governing thesis for this plan is: make one correct general pipeline
composable, resolve policy and explicit intent before the hot path, expose
conservative facts, and specialize only where measured eligibility, exact
fallback, and observable equivalence preserve meaning.

## Principle map

| # | Principle | Binding application in this plan | Primary anchors |
|---:|---|---|---|
| 1 | One correct, composable general pipeline | Each operation has one typed decode → resolve → authorize/reserve → mechanism → verify → publish path. Caches, direct paths, streaming, indexes, agents, and platform adapters cannot become alternate semantics. | `INDEX.md`, `02-architecture-and-files.md`, `06-identity-and-merge.md` |
| 2 | Separate policy from mechanism; normalize once | Daemon/context owners resolve config, environment, platform, authority, and selection provenance into immutable policy. Lower mechanisms and hot loops do not reread ambient state. | `01-contract-and-invariants.md`, `02-architecture-and-files.md`, `05-context-and-team-spaces.md` |
| 3 | Integrate vertically; share mechanisms horizontally | Product behavior is delivered in complete tested slices while execution, canonical encoding, migration, locking, and publication primitives have one reusable owner. Route/storage/model layers do not duplicate policy. | `02-architecture-and-files.md`, `09-delivery-sequence.md` |
| 4 | Observable semantics and compatibility are specifications | Stable values, ordering, errors, provenance, telemetry, durability, cleanup, CLI/MCP/admin output, and legacy behavior are parity-pinned and release-gated. | `07-surfaces-and-compatibility.md`, `08-test-performance-and-release.md` |
| 5 | Put each invariant at its owner | Validated types own local bounds; graph transactions own mutation/audit atomicity; product services own authority/catalog state; the coordinator owns cross-domain accounting and publication. | `01-contract-and-invariants.md`, `03-persistence-and-migrations.md`, `04-changesets-and-manual-editing.md` |
| 6 | Expose conservative semantic facts | Interfaces expose verified readiness, authorization leases, cache hit/miss tickets, reservations, manifests, receipts, and platform capabilities—not suggestive proxies. | `02-architecture-and-files.md`, `05-context-and-team-spaces.md`, `06-identity-and-merge.md` |
| 7 | Specialize only behind proof and fallback | Every specialization has conservative eligibility, the general fallback or a typed required-capability failure, and forced-path equivalence tests. A false-positive eligibility fact is a correctness defect. | `02-architecture-and-files.md`, `08-test-performance-and-release.md` |
| 8 | Encode closed worlds explicitly | IDs, enums, lifecycle states, tags, versions, canonical encodings, transitions, SQL constraints, and generated surface descriptors are exhaustive and single-owned. | `01-contract-and-invariants.md`, `03-persistence-and-migrations.md`, `07-surfaces-and-compatibility.md` |
| 9 | Optimize work and data shape | Work is admitted before allocation, streamed in bounded pages/waves, reduced numerically, batched at storage owners, and benchmarked for passes, copies, allocations, handles, RSS, and disk. | `02-architecture-and-files.md`, `03-persistence-and-migrations.md`, `08-test-performance-and-release.md` |
| 10 | Parallelize at natural ownership boundaries | A shared executor parallelizes independent domain-family work; workers own one family while coordinators retain authorization, ordering, global accounting, and publication. | `02-architecture-and-files.md`, `05-context-and-team-spaces.md` |
| 11 | Make streaming and phases explicit protocols | Planner sources/sinks, snapshots, jobs, changesets, staging, promotion, and recovery have typed phases, one finish point, idempotent abort/cleanup, and no partial published state. | `03-persistence-and-migrations.md`, `04-changesets-and-manual-editing.md`, `06-identity-and-merge.md` |
| 12 | Treat representation boundaries as APIs | OS paths remain path types; semantic/wire text is validated UTF-8; hashes use explicit binary framing; file hashes and semantic hashes differ; lossy display forms never confer identity or authority. | `01-contract-and-invariants.md`, `03-persistence-and-migrations.md`, `07-surfaces-and-compatibility.md` |
| 13 | Make ownership, lifetime, and affinity visible | Named RAII owners cover permits, leases, locks, handles, child processes, staging artifacts, and publication tokens; transfers are explicit and every terminal path cleans up once. | `02-architecture-and-files.md`, `03-persistence-and-migrations.md`, `08-test-performance-and-release.md` |
| 14 | Design resources, errors, and security up front | Hard caps and reservations precede work; failures are typed and fail closed; authority is revalidated at publication; secrets/content/paths are redacted; degraded states are explicit. | `01-contract-and-invariants.md`, `02-architecture-and-files.md`, `07-surfaces-and-compatibility.md` |
| 15 | Cache immutable identity; publish atomically | Every cache names its owner, complete shaping key, validity generations, invalidation, and false-hit tests. Artifacts become visible only after write, sync, hash, verification, and atomic naming. | `02-architecture-and-files.md`, `03-persistence-and-migrations.md`, `05-context-and-team-spaces.md` |
| 16 | Centralize portability and reproducible decisions | Platform/filesystem facts are immutable capability data from one owner; compile-time branches and probes stay there. Toolchains are pinned and every workaround has a regression and removal condition. | `02-architecture-and-files.md`, `08-test-performance-and-release.md`, `09-delivery-sequence.md` |
| 17 | Prefer causal tests at the cheapest diagnostic layer | Pure reference/property tests prove semantics; real SQLite/filesystem/process tests prove integration; crash tests prove durability; full scenarios and benchmarks prove compatibility and budgets. | `08-test-performance-and-release.md` |
| 18 | Keep optional capabilities behind stable seams; prefer simple complete changes | Embeddings, agents, platform promotion, caches, and fast paths share stable contracts and safe absent/unavailable behavior. Delivery uses complete vertical slices and removes superseded truth. | `02-architecture-and-files.md`, `08-test-performance-and-release.md`, `09-delivery-sequence.md` |

## Per-slice decision record

Before implementation of a slice, its plan/log entry must answer these
questions with a concrete owner and evidence target, or state why the item is
not applicable:

1. What is the correct general pipeline and stable typed seam?
2. Where is raw product policy normalized once, and how is explicit intent
   retained?
3. Which layer owns each new invariant, state transition, and closed world?
4. Which observable values, ordering, errors, provenance, telemetry,
   durability, cleanup, and compatibility behavior define correctness?
5. Which conservative semantic facts cross each interface?
6. What specialization is proposed, what proves eligibility, what is the exact
   fallback, and how will forced-path equivalence be tested?
7. What passes, allocations, copies, syscalls, locks, closures, event turns,
   bytes, handles, workers, disk, and latency does the common path pay?
8. Does concurrency follow independent ownership while one coordinator retains
   deterministic ordering, accounting, and publication?
9. For every boundary value/resource, what are its owner, lifetime, thread
   affinity, encoding, allocator where relevant, and terminal cleanup paths?
10. For every cache/artifact, does identity cover all shaping inputs and
    generations, and is complete publication atomic?
11. Are platform and optional choices isolated as immutable capability data
    behind the same stable seam?
12. Which cheapest causal test fails before the change and which production
    integration/scenario proves the complete contract afterward?
13. Which duplicate truth, dead path, cache, compatibility branch, or obsolete
    workaround will this slice remove?
14. Which measured heuristics remain visible, versioned, bounded, and
    revisable rather than becoming semantic truth?

Answers are part of the append-only implementation verification packet. A
slice cannot defer an applicable answer merely because a later phase will add
more users of the behavior.

## Phase gates

- **Before Phase 1:** this audit, the reference merge semantics, explicit
  source/sink terminal protocol, representation types, and equivalence targets
  are binding.
- **Before any specialization is enabled:** the general implementation is
  correct and force-selectable, eligibility is conservative, fallback behavior
  is explicit, and adversarial equivalence passes.
- **Before any cache or durable artifact is consumed:** its complete identity,
  invalidation/generation rules, construction owner, verification, and atomic
  publication are tested.
- **Before any platform capability is advertised:** centralized capability
  selection and supported-platform/filesystem evidence pass; an unproved
  primitive remains unavailable.
- **At every phase exit:** `08-test-performance-and-release.md` evidence and the
  decision record above are appended to `IMPLEMENTATION_LOG.md`, duplicate
  truth is removed, and the next phase does not start until review.
