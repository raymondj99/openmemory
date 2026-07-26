# Memory Spaces, Audit, and Merge Validation Logbook

## Charter

This is the engineering ledger for validating scoped memory spaces,
personal/team layering, git-like audit history, manual modification, and
cross-space merge semantics before production code is designed.

Entries are chronological. Failed experiments and superseded decisions remain
in the file. Corrections are recorded as later entries rather than silently
rewriting earlier conclusions.

The proof-of-concept code is intentionally isolated from the production crates.
Production code should not absorb an abstraction until the corresponding
correctness, recovery, and performance hypotheses below have evidence.

## Non-negotiable invariants

1. Project B is never queried from a Project A context without an explicit
   overlay or merge request.
2. A semantic object belongs to exactly one memory space; sharing is an
   explicit provenance-bearing copy.
3. Proposed changes never affect live recall or indexes.
4. Every applied semantic mutation and its audit record commit atomically.
5. Manual updates use optimistic concurrency; stale proposals never overwrite
   newer state.
6. Merge never uses fuzzy identity matching or wall-clock last-write-wins as an
   automatic conflict resolution rule.
7. Search indexes, access telemetry, partition stubs, and relation mirrors are
   derived state and never authoritative merge input.
8. Crash recovery produces either the old graph or the fully validated new
   graph, never a partially merged graph.
9. The source of a directional merge is never modified.
10. The hot recall path remains bounded by the active read set, not by the
    number of registered projects or teams.
11. A shared label or embedding similarity can discover an identity candidate,
    but cannot establish that two cross-space concepts are the same.
12. Agent output is a complete-packet-bound proposal. It never directly mutates
    the canonical graph, bypasses deterministic contradictions or decisions,
    resolves conflicting trusted proof, or authorizes its own team-space
    decision.
13. External identifiers and directional relations become trusted evidence
    only through source-bound ingestion verification; user/agent claims remain
    context.
14. Candidate retrieval is deterministic and bounded. Truncation is explicit,
    and omitted candidates are never interpreted as different identities.

## Validation gates

- Deterministic unit tests for state transitions and merge classification.
- Property tests for ordering, isolation, idempotency, and three-way merge
  symmetry properties where symmetry is semantically valid.
- Concurrent writer/reviewer tests with no lost updates.
- Subprocess crash tests at every durable state transition.
- Reopen/replay tests proving idempotency.
- Release-mode benchmarks with deterministic data and explicit budgets.
- Production workspace gates: format, default/all/no-default tests, Clippy,
  and rustdoc.

## Experiment ledger

### E000 — Repository and environment baseline

**Date:** 2026-07-17

**Repository:** `main` at `bcd10fd3f736290c536ac94daf5a9014ef711828`

**Toolchain:**

- `rustc 1.85.0 (4d91de4e4 2025-02-17)`
- `cargo 1.85.0 (d73d2caf9 2024-12-31)`
- Host triple: `aarch64-apple-darwin`
- Darwin 25.5.0, ARM64 T6030 kernel target
- CPU model and RAM queries were blocked by the managed sandbox; do not use
  this machine's absolute timings as portable performance claims.

**Existing engineering rigor observed:**

- CI covers macOS and Linux default builds plus all-features and
  no-default-features tests.
- Clippy runs all targets with warnings denied; rustdoc warnings are denied.
- Tests use fixed clocks, temp directories, deterministic data, and explicit
  concurrency synchronization.
- Property tests cover normalization, vector persistence, and hybrid fusion.
- Engine tests cover journal replay, torn trailing records, exactly-once
  checkpoints, admission pause, multi-domain routing, and lossless concurrent
  submissions.
- Domain migration already uses a sentinel-guarded staging build, verification,
  and filesystem swap.
- Criterion/CodSpeed benchmarks cover vector search, hybrid search, graph
  recall, relation spreading, consolidation, and daemon admin routes.
- Release stress examples report throughput, latency percentiles, durability
  lag, recall under write load, and lost-write checks.

**Baseline command:**

```text
cargo test --workspace --all-features --locked
```

**Result:** All completed crates passed, but two `openmemory-watch` integration
tests failed in the parallel workspace run because the watcher backend did not
become live after eight probes. The isolated serial rerun passed all four tests:

```text
cargo test --locked -p openmemory-watch --all-features --test integration -- --test-threads=1
4 passed; 0 failed; finished in 8.45s
```

**Observation:** The watcher suite has an environment-sensitive parallel-start
flake on this machine. This predates the POC. POC tests must not use filesystem
watch notifications or real-time readiness polling.

### E001 — Initial hypotheses and options

**H1: Physical space isolation.** One independent `DomainStore` family per
memory space should provide exact ANN, relation, consolidation, and deletion
isolation without per-row filters.

**H2: Bounded layered recall.** Querying a fixed read set of 1–4 spaces in
parallel and deterministically merging candidates should scale with active
context, not registered space count. Target: warm two-space overhead no more
than `1.25x` the slower component recall plus 5 ms; merge of 512 candidates
under 2 ms p95 on reference-class hardware.

**H3: Transactional changesets.** Recording a changeset and applying its
canonical mutation in one SQLite transaction should add modest write overhead
while providing exact auditability. Target: p95 applied-write overhead below
25% for small mutations; proposed writes should be cheaper than applied writes.

**H4: Optimistic review.** Per-object versions plus expected hashes should
reject stale approvals without global serialization and remain idempotent under
retries.

**H5: Immutable semantic revision.** Patches can retain stable object identity;
knowledge changes should create a superseding version. Recall must expose only
the active semantic version by default.

**H6: Merge strategy.** Three-way logical merge plus a verified staging build
and directory swap should be simpler and more crash-robust than an in-place
cross-domain transaction coordinator. It will cost O(target + source) work, but
material merges are administrative operations rather than hot-path writes.

**Options to compare:**

1. Physical graph roots versus one database with `space_id` columns.
2. Sequential versus parallel layered recall.
3. Full before/after JSON audit records versus typed deltas only.
4. In-place cross-domain merge journal versus rebuild-and-swap.
5. Stable-ID patch versus immutable-version supersession for content changes.

**Next:** Build an isolated Rust crate containing only the minimum storage,
merge, crash harness, and benchmarks needed to falsify these hypotheses.

### E002 — Isolated executable model and correctness suite

**Date:** 2026-07-17

Created `experiments/memory-model`, a standalone Rust 1.85 workspace. It is
not a member of the production workspace and does not change a production
crate. It depends on the real `MemoryStore`/`DomainStore` implementation for
search and partition behavior, while its audit and merge schema is deliberately
throwaway.

The model contains:

- Path-safe `SpaceId`, orthogonal `SpaceOwner` and `SpaceContext`, lazy catalog
  entries, and a unique ordered read set capped at four spaces.
- Independent production `DomainStore` roots, including the composition of
  four engine partitions inside each semantic memory space.
- Sequential and scoped-thread layered recall with exact duplicate fusion and
  retained space provenance.
- Proposed/applied/rejected changesets, idempotency keys, full before/after
  records, per-object optimistic versions, patch, supersede, delete, restore,
  and merge-put operations.
- Stable logical memory IDs with explicit semantic revision lineage.
- A deterministic three-way merge planner using only canonical semantic state.
- A verified copy/build/promote materializer with durable intent and retained
  backup directories.
- Dedicated subprocesses which call `abort()` at real transaction and
  filesystem boundaries.

Commands:

```text
cd experiments/memory-model
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

Result: 23 deterministic scenario/concurrency/crash tests passed. Three
property tests ran 128 cases each (384 generated cases). Clippy passed with
warnings denied. The suite includes:

- Proposed changes are invisible until approval.
- A stale operation rolls back every operation in its changeset.
- Two concurrent reviewers of the same object produce one commit and one
  conflict, never a lost update.
- Identical idempotency retries produce one mutation; key reuse with different
  content conflicts.
- Patch retains a revision ID; supersede creates a linked revision.
- Delete retains a tombstone and rejected restore remains invisible.
- Transaction abort after ledger insert or canonical mutation reopens at the
  old state; abort after commit reopens at the new state exactly once.
- Divergent source/target edits conflict; disjoint edits merge; a stale target
  invalidates the complete plan; source is unchanged.
- Recovery rolls forward after abort following intent fsync, backup rename, or
  staging promotion. The promoted graph and retained backup are both
  hash-verified; a second recovery is a no-op.
- Two semantic spaces remain isolated when each contains four production
  engine domains. Explicit overlay is the only path that sees both.

**Observation:** The model is small enough to inspect, but it exercises real
SQLite transactions, WAL recovery, directory renames, and production graph
recall. Error-return-only crash simulation would not have been sufficient.

### E003 — Scope storage and read-set options

**Physical roots:** Validated. Project A could not recall Project B content
using either single-domain roots or four-domain `DomainStore` roots. An
explicit two-space read set returned B content. Registering 10,000 closed
spaces did not open or query them.

**`space_id` columns in one graph:** Rejected for the first production
implementation after code-path inspection. SQLite canonical rows could be
filtered, but the current FTS/vector engine and relation spreading do not carry
an exact namespace predicate. ANN overfetch followed by a row filter can omit
valid in-scope neighbors. Retrofitting every canonical, derived, traversal,
consolidation, prune, and cache path creates a much larger leakage surface than
independent roots.

**Read-set cardinality:** Keep the hard cap at four. The catalog is metadata;
the hot path resolves only selected handles. A future all-project diagnostic
search must be a separate administrative job, not a larger agent read set.

**Sequential versus parallel:** Neither wins universally at sub-millisecond
keyword latency. Scoped-thread creation can cost more than the second cheap
query, while it wins when individual graph work is slower. Production should
not blindly nest read-set threads around `DomainStore`'s own domain fan-out.
Use one bounded executor or an adaptive rule, and retain a sequential path for
two very cheap/cached components.

### E004 — Audit representation and write cost

The initial benchmark ran each variant in a separate phase. Its relative p95
overhead varied from below zero to 83% as filesystem conditions changed. That
method was invalidated. The runner now rotates the ordering of direct, full
JSON, compact immutable, and proposal writes for every sample and records 800
samples per variant. Three independent release runs produced:

| Variant | p95 range | Relative p95 versus paired unlogged control |
|---------|-----------|----------------------------------------------|
| Unlogged canonical add | 1,036–1,344 us | control |
| Full requested + before/after JSON + hashes | 1,195–1,736 us | 15%, 20%, 29% |
| Compact header + immutable revision linkage | 1,093–1,483 us | 5%, 10%, 10% |
| Proposal only | 1,025–1,334 us | approximately control cost |

At 820 warmup/measured writes, checkpointed durable sizes were stable across
the three runs:

| Representation | Bytes | Multiple of unlogged control |
|----------------|------:|-----------------------------:|
| Unlogged canonical | 372,736 | 1.00x |
| Full JSON audit | 987,136 | 2.65x |
| Compact immutable audit | 565,248 | 1.52x |

In the full representation, requested operation JSON occupied 66,220 bytes;
duplicated before/after snapshots and hashes occupied another 200,700 bytes.

**Decision:** H3 is rejected for full snapshot JSON because one of three
interleaved runs missed the 25% p95 gate and storage amplification is poor.
The compact immutable representation passed the gate in all three runs. Use
canonical immutable revision rows as the diff payload, a compact changeset
header for actor/reason/idempotency, and typed events only where revision
linkage cannot express the change (delete, restore, relation endpoint change,
or metadata patch). Generate UI JSON at read time.

This changes the manual-edit decision: a prose/content correction should also
create a new revision. Stable **logical** identity is preserved, but in-place
content mutation is not worth a second history representation. In-place patch
remains appropriate only for non-semantic metadata with an explicit typed
before/after event.

**Caveat:** Both control and audited POC paths reopen a connection per call, so
the comparison isolates incremental durable work but is not the final daemon
latency. Production integrates changesets into the existing connection and
index synchronization path; it needs its own Criterion and stress gates.

### E005 — Layered recall measurements

The release runner used two production graph roots with 256 matching
observations each, keyword recall, access telemetry disabled, 200 warm samples,
and a deterministic 512-candidate fusion input. Across the three final runs:

| Path | p95 range |
|------|-----------|
| One selected space | 117–121 us |
| Two spaces sequential | 172–178 us |
| Two spaces parallel | 164–205 us |
| Two spaces parallel + 10,000 closed catalog entries | 167–206 us |
| Fuse 512 candidates to 128 | 199–202 us |

Both H2 budgets passed in every run. The closed-catalog result is within run
noise of the two-open-space result, supporting the required O(read-set) hot
path. Candidate fusion has roughly 10x headroom against the 2 ms gate.

**Limit:** These measurements isolate orchestration using production FTS. The
fusion cost is backend-independent, but vector/HNSW component latency and
nested domain/read-set concurrency must be exercised by production benchmark
fixtures before release.

### E006 — Merge options, scale, and recovery

A deterministic 10,000-memory three-way plan, with 5,000 independent source
changes and 5,000 independent target changes, produced 5,000 actions and zero
conflicts. Across the final runs its p95 was 9.43–12.15 ms. Building, applying,
verifying, and promoting a 1,000-memory merge took 53.8–86.5 ms.

The naive alternative was made executable: commit one independent SQLite
partition, inject failure before the second, and observe the partial logical
merge. This is unavoidable without a transaction coordinator and recovery
protocol. It is rejected.

The staging alternative survived aborts at each durable boundary and always
recovered to a hash-verified new target while retaining a hash-verified old
target. It reuses the same architectural shape as the existing domain
migration code.

**Decision:** Material merge is an administrative job:

1. Require a stored common-base snapshot/hash for automatic three-way merge.
2. Compare stable logical IDs only. Never fuzzy-match identities.
3. Stop on divergent semantic edits, delete/edit, or incompatible relation
   changes.
4. Build a complete staged target from the target snapshot.
5. Apply one provenance-bearing merge changeset to canonical state.
6. Rebuild all derived indexes, mirrors, and caches in staging.
7. Verify canonical counts, referential integrity, index parity, and result
   hash.
8. Fsync intent and parent directory, retain old target as backup, promote by
   same-filesystem rename, and make recovery idempotent.

Overlay and copy/cherry-pick remain distinct operations. Overlay is read-only
and cheap. Cherry-pick copies selected logical revisions with provenance.
Material merge changes the target graph and therefore uses the heavy path.

### E007 — Unresolved production hazards and questions

The POC validates the storage mechanics, not every product/security choice.
Production work must resolve:

1. **Authorization:** Who may construct a team read set, propose, approve,
   reject, merge, restore, or delete? The resolver must return an already
   authorized immutable `MemoryContext`; storage must not infer access from a
   caller-controlled `source` string.
2. **Team policy:** Are personal memories ever eligible for team promotion by
   an agent, or only by a human? Is one or two reviewers required for team
   changes? These are policy above the changeset mechanism.
3. **Merge bases:** Store per source/target lineage after cherry-pick or merge.
   Unrelated spaces have no safe automatic three-way base; same-ID divergent
   rows must conflict.
4. **Relations:** Current partition mirrors have independent IDs and
   best-effort writes. Add a canonical logical relation ID and rebuild mirrors
   as derived state before relation editing/merge can be robust.
5. **Entity rename:** Entity-name hashing changes its physical domain. Treat
   rename as an administrative move/rebuild, not a normal row patch.
6. **Index atomicity:** The canonical mutation and changeset can share one
   SQLite transaction, but FTS/vector files are derived state outside that
   transaction. Production needs a durable index outbox/generation and reopen
   repair test.
7. **Deletion/retention:** Current tombstones are pruned after 14 days and
   entity deletion cascades. Audit/revert promises require a separate retention
   policy; legal hard-delete must explicitly destroy both content and history.
8. **Telemetry:** Recall access counts are mutable ranking feedback, not
   semantic history. They must stay outside changesets and outside merge input.
9. **Filesystem contract:** Promotion requires staging and target on one
   filesystem. Validate device/volume before work and retain backups according
   to an explicit quota/retention policy.
10. **Scale envelope:** Physical roots trade exact isolation for file handles,
    disk overhead, backup breadth, and open latency. The daemon registry must
    remain lazy and bounded.

### E008 — Validated path forward

The concept is viable with these constraints:

- A `MemorySpace` is the semantic isolation unit and owns one complete
  `DomainStore` family. Workspace, project, user, and team are metadata used to
  resolve spaces; none is the storage identity itself.
- Ownership (`User` or `Team`) is orthogonal to context (`Global` or
  `Project`). A bounded ordered read set composes authorized spaces without
  merging them.
- Every semantic write enters as a compact changeset. Proposal is invisible;
  approval applies with optimistic versions in the same canonical transaction.
- Logical IDs are stable; semantic content is immutable and superseded by a new
  revision. Diff is reconstructed from revision lineage plus typed lifecycle
  events.
- Overlay is the default cross-space operation. Cherry-pick is explicit.
  Material merge requires lineage, preview, conflict resolution, staging,
  verification, and crash-safe promotion.

The production implementation plan is maintained in
`plan/15-memory-spaces-audit-and-merge.md`.

### E009 — Final validation gates

**Date:** 2026-07-17

POC gates:

```text
cargo fmt --all -- --check
cargo clippy --offline --all-targets -- -D warnings
cargo test --release --offline
RUSTDOCFLAGS='-D warnings' cargo doc --offline --no-deps
```

All passed. Release tests repeated the 23 scenario/property test functions,
including subprocess abort recovery and the four-domain-per-space integration.

Production workspace gates, which the POC does not modify:

```text
cargo fmt --all -- --check                                      passed
cargo clippy --workspace --all-features --all-targets --locked
  -- -D warnings                                                passed
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features
  --no-deps --locked                                            passed
```

Both all-feature and no-default workspace test runs again reached the
`openmemory-watch` integration suite after all preceding tests passed, then
showed the pre-existing watcher-readiness flake under parallel suite execution:

- All-features: two of four watcher integrations reported “backend never
  became live after 8 pokes.”
- No-default: one of four reported the same failure.
- Immediate isolated serial reruns passed 4/4 in 9.57 s (all-features) and
  8.13 s (no-default).

This is not caused by or exercised by the standalone POC, but it is a real
baseline quality issue and remains visible here. Production implementation
should not weaken its gates; the watcher readiness test should be made
event/synchronization-driven before treating parallel workspace runs as fully
green.

### E010 — Correction: atomicity boundary inside a partitioned space

**Date:** 2026-07-17

The worked Project A / Project B / Team design exposed an important boundary
which the initial production plan stated too broadly: a `MemorySpace` may own a
four-domain `DomainStore`, and those domains are independent SQLite files.
SQLite cannot make one transaction atomic across them.

The POC proved audit/canonical atomicity in one SQLite database and separately
proved that semantic space isolation composes with four production domains. It
did **not** prove that an arbitrary multi-object changeset spanning multiple
home domains can commit atomically. Claiming space-wide SQLite atomicity would
be incorrect.

Corrected contract:

- Every interactive changeset is routed to exactly one canonical home domain.
  Its canonical rows, immutable revisions, events, generation, and index outbox
  commit in that domain's SQLite transaction.
- Multiple operations are allowed atomically only when all canonical objects
  route to the same domain. A cross-domain request returns
  `changeset_cross_domain` before writing anything.
- “Approve all” is a batch of independent domain-local changesets and reports
  each result. It is not presented as one atomic operation.
- Cross-domain relation mirrors remain derived outbox work; the canonical edge
  and its audit record live in the source entity's home domain.
- A truly atomic cross-domain semantic operation uses the heavy materialization
  protocol: apply domain-local child changesets to an invisible staged copy,
  verify the complete space, then promote the whole root. The promoted root
  includes a manifest binding every child changeset to one merge job and result
  hash.
- Space version is a vector of durable per-domain generations plus a canonical
  root hash, not a fictitious single counter updated atomically across files.

This does not invalidate the common manual edit/review path, which mutates one
memory in one home domain. It changes batch API semantics, cache versioning,
merge audit layout, and several production tests. The production plan has been
corrected accordingly.

### E011 — Cross-space identity resolution and agent-assisted review

**Date:** 2026-07-17

**Question:** If Project A and Project B independently contain a concept named
“Cerpheus,” can an agent help decide identity without silently merging
homonyms, repeatedly nagging the user, or entering the graph's trusted write
path?

**Options exercised:**

1. Exact normalized-name equality as an automatic identity rule.
2. Proof-gated deterministic resolution using shared lineage and
   namespace-qualified authoritative identifiers.
3. Proof-gated resolution plus an asynchronous, untrusted agent proposal for
   context-only cases.

The isolated POC now contains `src/identity.rs`, `tests/identity.rs`,
`tests/identity_properties.rs`, the identity crash boundaries in
`poc-crash-worker`, and `identity-eval`. No production crate was modified.

#### Contract validated

- Entity addresses are `(space_id, logical_id)`; pair keys are symmetric and
  cross-space.
- Candidate retrieval uses indexes for normalized labels, stable lineage, and
  authoritative identifiers. It does not perform an all-pairs scan.
- The deterministic analyzer emits a revision-bound evidence packet. Shared
  lineage is identity proof. Matching configured unique identifiers are proof
  subject to workspace policy. Conflicting unique identifiers are a hard
  contradiction. Labels, description overlap, compatible kind, neighbors, and
  embeddings are context only.
- Agent output must name the exact pair and revisions, cite packet evidence,
  include model/prompt provenance and rationale, and pass structural
  validation. A recommendation of `same` cannot override a deterministic
  contradiction.
- Default personal policy automatically accepts lineage and matching trusted
  IDs. Default team policy automatically accepts lineage but sends a matching
  trusted ID to review. Neither default automatically applies an agent
  recommendation.
- Agent abstention is safe and non-blocking: both concepts remain separate. A
  durable `deferred` record suppresses repeated work for exactly those entity
  revisions without creating an identity decision.
- Applied `same`, `different`, and `undetermined` decisions are immutable
  events with one current head. Reversal creates a superseding event; it does
  not erase history.
- Durable decisions suppress identical future review cards. New deterministic
  evidence contradicting an old decision produces `Revalidate`, not silent
  suppression.
- Proposal submission is idempotent, review is optimistic, and concurrent
  reviewers produce exactly one decision.
- Subprocess aborts after decision insertion, proposal-state update, and commit
  reopen as exactly old or new state; no orphan decision or half-applied
  proposal was observed.

#### Synthetic safety and review-flow comparison

`identity-eval` uses 48 intentionally balanced pairs: 24 known-same, 16
known-different, and 8 genuinely ambiguous. The contextual “agent” in this
test is explicitly a reproducible heuristic control, not an LLM and not a
claim about model accuracy.

| Strategy | Correct auto | Wrong auto | Correct review recommendations | Abstain/keep separate | Unsafe ambiguous merge |
|----------|--------------|------------|--------------------------------|-----------------------|------------------------|
| Name only | 16 | 24 | 0 | 0 | 8 |
| Proof-gated personal default | 24 | 0 | 16 | 8 | 0 |
| Proof-gated team default | 16 | 0 | 24 | 8 | 0 |

The name-only control was wrong on 24 of 40 labeled cases and also merged all
8 ambiguous cases. This rejects label equality as an identity rule.

Both proof-gated defaults invoked contextual analysis for 24/48 pairs. On this
deliberately difficult candidate-only corpus, the personal flow presented 16
first-pass review cards and the team flow presented 24. After actually writing
the decisions and deferred abstentions to a fresh SQLite ledger, an identical
second pass produced zero review cards. This validates suppression mechanics;
it does not predict review rates in a real customer graph.

#### Release measurements

Four release runs used 40,000 entities per space (80,000 indexed), with 2,000
real candidates out of 1.6 billion possible cross products, plus 200 durable
proposal/review samples per run:

| Measurement | Repeated result |
|-------------|-----------------|
| Build exact candidate indexes for 80k entities | 24.6–32.6 ms |
| Indexed candidate lookup p95 | 292–458 ns |
| Deterministic evidence analysis p95 | 1.08–1.25 us |
| Durable proposal insert p95 | 751–973 us |
| Durable review transaction p95 | 768–968 us |
| Ledger bytes after 200 proposals + 200 decisions | 335,872 bytes |

These local timings are not portable hardware claims. They do establish that
candidate routing, deterministic evidence, and durable review are comfortably
outside the likely model/network latency envelope and need not touch normal
recall or write latency. Model calls must run asynchronously during an explicit
merge/import or background audit, never inline with ordinary memory capture.

#### Corrected conclusions and open validation

The architecture and safety boundary work. “Seamless to the user” is only
partially validated:

- Proven cases can disappear from the UI; uncertain cases remain lossless and
  non-blocking; decisions and abstentions prevent repeated prompts.
- The deliberately adversarial team corpus still required review on 24/48
  candidate pairs. The product needs batch review, concise evidence cards, and
  a way to leave a pair unresolved without blocking the merge.
- No real model was called. Semantic accuracy, calibration across entity kinds,
  prompt-injection resistance in memory content, model cost/latency, and
  abstention behavior require a versioned eval corpus built from redacted or
  consented real merges before model selection.
- No human usability study was run. Time-to-decision, mistaken approvals,
  reviewer comprehension, and whether evidence cards feel seamless remain
  release questions, not proven facts.
- Exact label normalization is intentionally conservative and will miss
  aliases, punctuation variants, transliteration, and renamed concepts unless
  lineage, authoritative IDs, or semantic candidate retrieval finds them.

Production should implement the deterministic packet, policy, and ledger
before adding a model adapter. A model integration is blocked from autonomous
team application until it passes offline quality/adversarial evals and the UI
passes a representative review study.

Final isolated POC gates after E011:

```text
cargo fmt --all -- --check                         passed
cargo clippy --offline --all-targets -- -D warnings passed
cargo test --release --offline                     passed
RUSTDOCFLAGS='-D warnings' cargo doc --offline
  --no-deps                                        passed
```

The release suite now contains 38 deterministic/scenario/property test
functions. Six property tests each execute 128 generated cases (768 generated
cases total), in addition to the concurrency and nine subprocess abort
boundaries across audit, identity review, and material promotion.

### E012 — Wikipedia two-project corpus and identity-policy redesign

**Date:** 2026-07-18

**Question:** Does the E011 identity contract survive real homonyms, aliases,
historical classification changes, namesake relations, hostile provenance,
policy changes, and high-fanout candidate buckets without making the user sort
through every pair?

#### Source-grounded fixture

Added `fixtures/wikipedia_two_projects.json`, `src/real_world.rs`,
`tests/wikipedia_identity.rs`, and `wikipedia-identity-eval`. The fixture has
two independent spaces, 19 entity records, 15 source pages, and 11 curated
cross-space expectations:

- **Project Helios — Solar System History Exhibit:** Mercury, Venus, Mars,
  Jupiter, and Saturn as planets; Ceres under its historical asteroid
  classification; Pluto under its historical planet classification; Roman
  mythology; and Jove.
- **Project Pantheon — Roman Mythology and Planetary Naming Exhibit:** the
  namesake deities, modern dwarf-planet records for Ceres and Pluto, Roman
  mythology, and Jupiter with the alias Jove.
- Seven equal-label pairs are distinct identities with directional
  `named_after` relationships. Four pairs are the same identity despite
  renaming or classification/context differences: Ceres / 1 Ceres, Pluto /
  134340 Pluto, Roman mythology, and Jove / Jupiter.

The factual basis is intentionally ordinary and independently inspectable:
Wikipedia describes the planets as named for the corresponding deities, Ceres
as first classified as an asteroid and later as a dwarf planet, Pluto's 2006
classification change, and Jove as a name for Jupiter. Source URLs and the
retrieval date are stored in the fixture. Descriptions are short paraphrases,
not copied article text.

This is a **curated architecture corpus**, not a held-out accuracy benchmark.
The expected pairs and decisions were written by hand from the cited sources.
No model generated the ground truth or the reported recommendations.

#### Unchanged-control result

The prior name-only/string-kind behavior was run without accommodating the
fixture:

| Metric | Legacy result |
|--------|--------------:|
| Expected identity pairs | 11 |
| Exact-label candidates found / missed | 8 / 3 |
| Correct decisions | 1 |
| False `same` decisions | 7 |
| False `different` decisions | 3 |
| True identities blocked by raw kind inequality | 2 |

The seven planet/deity homonyms defeat label equality. Renames defeat exact
labels. Ceres and Pluto demonstrate that `kind_a != kind_b` is not generally a
contradiction: the same object can legitimately cross a classification
boundary over time.

#### Defects found and redesigns made

1. **Aliases are retrieval signals, never proof.** Labels plus bounded aliases
   feed exact candidate indexes. `Ceres` finds `1 Ceres`, `Pluto` finds
   `134340 Pluto`, and `Jove` finds `Jupiter`; none auto-merge without lineage
   or a verified identifier.
2. **Ontology compatibility is explicit and versioned.** Controlled kind pairs
   are `compatible`, `incompatible`, or `unknown`. Unknown is context only.
   The fixture marks asteroid/dwarf-planet and planet/dwarf-planet as
   compatible historical views while planet/deity pairs remain incompatible.
3. **External IDs carry trust.** User/agent values enter as `Claimed`.
   `SourceVerified` records name a source already bound to the entity and an
   ingestion verifier. Only two source-verified values in a configured unique
   namespace can prove or contradict identity. Matching claimed `wikidata`
   strings, a missing bound source, and an unconfigured namespace remain
   context.
4. **Trusted evidence is not ordered by `if/else`.** Shared lineage plus a
   conflicting verified ID, or a verified ID plus a controlled contradiction,
   yields `ConflictingProofs`. It bypasses the agent and goes directly to
   human conflict review. A prior decision is reopened rather than suppressed.
5. **Proposals bind the complete packet.** A BLAKE3 binding covers the canonical
   pair, entity revisions, policy version, records, and evidence. Proposals,
   deferred abstentions, and decisions persist this hash. Changing only the
   ontology/evidence policy invalidates old proposals and reopens old
   decisions even when entity revisions are unchanged.
6. **Relations require directional evidence.** A `named_after` suggestion must
   cite a source-verified packet item bound to the exact subject, object, and
   relation type. Generic name/source overlap and agent/user relation claims
   cannot authorize a reviewable edge. Identity and relation application stay
   separate reviewed changesets.
7. **Candidate generation is bounded and proof-prioritized.** Index buckets are
   deterministic `BTreeSet`s. A request returns at most 256 candidates and an
   explicit `truncated` flag. Verified-ID and lineage candidates enter before
   names and untrusted identifiers. A 301-homonym Mercury test, including 300
   forged matching Wikidata claims, retained the one genuinely source-verified
   match under a limit of 16 in both forward and reverse insertion order.
8. **All untrusted structures are bounded.** Entity packet fields, aliases,
   sources, identifier/relation assertions, proposal evidence, rationale, and
   relation suggestions fail closed above fixed limits. Duplicate evidence and
   duplicate relation suggestions are rejected.

The redesign also fixed two concrete precedence errors discovered by the new
tests: shared lineage could previously outrank a conflicting verified ID, and
a shared verified ID plus incompatible kinds could become `NotCandidate` when
labels differed.

#### Revised real-world result

```text
expected pairs                         11
expected pairs found                   11
all indexed cross-space candidates     11
unexpected indexed candidates           0
deterministic correct decisions          7
curated recommendations sent to review   4
wrong decisions/recommendations           0
validated named_after suggestions         7
relation-type review groups               1
```

The four reviewed identity items are the cases without enough deterministic
proof in this fixture. The seven source-backed namesake suggestions can be
presented as one relation-type review group, but this only validates grouping
mechanics. It does not prove that a one-click batch approval UI is safe or
understandable.

#### Scale and durable-ledger measurements

Three final release runs used 80,000 indexed entities, two aliases per entity,
2,000 rich cross-space candidates out of 1.6 billion possible pairs, verified
identifiers, controlled incompatible kinds, and directional relation evidence:

| Measurement | Three-run range |
|-------------|----------------:|
| Build bounded exact indexes | 108.1–132.0 ms |
| Candidate lookup p95 | 1.71–1.96 us |
| Rich deterministic analysis p95 | 5.08–8.13 us |
| Durable packet-bound proposal p95 | 740–894 us |
| Durable packet-bound review p95 | 725–843 us |
| Ledger after 200 proposals + decisions | 389,120 bytes |

These local measurements are not portable throughput claims. They pass the
existing `<100 us` deterministic-analysis and `<2 ms` durable-operation gates
with substantial margin, and candidate work remains indexed rather than
Cartesian. The scale fixture is synthetic; Wikipedia contributes semantic
shape, not scale.

#### Remaining blockers—do not overclaim “flawless”

- Wikipedia pages are live references, not content-addressed snapshots. A
  production source assertion must retain page revision/content hash,
  retrieval time, extractor version, and the external-namespace resolver
  generation. Wikidata redirects/merges must canonicalize before two IDs are
  treated as conflicting.
- The four reviewed recommendations are curated, not model output. A real
  agent still requires a representative held-out corpus, prompt-injection
  fixtures, calibration/abstention measurements by entity kind, and a fixed
  false-same release threshold.
- No human review study was run. “One grouped namesake batch plus a few identity
  cards” is a product hypothesis. Time-to-decision, mistaken approvals,
  comprehension, reversal discovery, and accessibility remain unvalidated.
- The 11 expected pairs cover the complete exact candidate set for this
  fixture, but not transliteration, multilingual aliases, Wikidata redirects,
  source disagreement, polysemy within one ontology kind, or very large
  real-world alias buckets. Truncation must paginate; omitted candidates can
  never be interpreted as `different`.

**Decision:** The production path is viable only with the E012 trust, policy
binding, contradiction, relation-evidence, and bounded-candidate changes. The
architecture is ready to plan; autonomous semantic identity decisions and the
claim that the workflow is seamless remain release-blocked pending real model
and human evidence.

Final isolated gates after E012:

```text
jq empty fixtures/wikipedia_two_projects.json       passed
cargo fmt --all -- --check                          passed
cargo clippy --offline --all-targets -- -D warnings passed
cargo test --release --offline                      passed
RUSTDOCFLAGS=-Dwarnings cargo doc --offline --no-deps passed
```

The release suite now contains 54 scenario/property test functions. Ten
property tests each execute 128 generated cases (1,280 generated cases total),
in addition to concurrency and the nine subprocess abort boundaries across
audit, identity review, and material promotion.

## E013 — Java and Python collision corpora; relation-target discovery

**Date:** 2026-07-18

**Question:** Does the E012 identity design survive two independent real-world
collision patterns, including a related entity whose label is not a homonym
of the query? Can it discover that relationship without granting an agent or
an unverified claim power to expand scope?

### Source-grounded examples

Added two dated, paraphrased Wikipedia fixtures and a combined evaluator:

- `wikipedia_java_projects.json`: the Java programming language (`Q251`), Java
  island (`Q3757`), and Java coffee. The source language was initially called
  Oak and was renamed after Java coffee.
- `wikipedia_python_projects.json`: the Python programming language (`Q28865`),
  the snake genus Python (`Q271218`), and Monty Python (`Q16402`). The language
  was named with the comedy troupe in mind.

The identifiers, controlled kinds, source URLs, retrieval date, and curated
expectations are fixture data. Descriptions are short paraphrases. As in E012,
these are architecture corpora, not held-out model-accuracy benchmarks.

### Unchanged control and defect found

Both exact-label controls produced the same failure shape:

```text
expected pairs                 3
exact-label candidates found   1
exact-label candidates missed  2
correct label decisions        1
false same                     1
false different                1
```

The pre-E013 revised index handled Java completely because aliases linked all
three candidates. It found only 2/3 Python pairs. If the Monty Python pair was
manually supplied, evidence analysis correctly classified the identities as
different and validated the directional `named_after` suggestion, but the
index never produced the pair: `Python` and `Monty Python` share neither a
complete normalized label, lineage, nor identifier value. Correct downstream
analysis is irrelevant when candidate discovery cannot reach it.

### Redesign

Candidate discovery now has this exact priority:

1. shared source-verified identifier;
2. shared lineage;
3. source-verified directional relation target;
4. normalized label or bounded alias;
5. any remaining, non-authoritative identifier match.

A relation assertion can nominate a target only when all of these hold:

- the assertion is `SourceVerified`, not `Claimed`;
- its source reference is already bound to the subject record;
- its verifier is non-empty;
- the target has the exact normalized target label or alias;
- the target has the exact controlled target kind;
- the target is in the explicitly requested other space;
- the global per-entity candidate cap still applies.

This is only retrieval. It is not identity proof and does not create a graph
edge. The resulting pair still passes through authoritative-ID/lineage proof,
contradiction handling, a complete packet hash, identity policy, and a separate
relation changeset. Claimed, unbound, and wrong-kind selectors produced zero
candidates in adversarial tests.

### Results

| Pair | Candidate path | Identity outcome | Relation outcome |
|------|----------------|------------------|------------------|
| Java language / renamed Java language | alias + shared verified `Q251` | deterministic `same` | none |
| Java language / Java island | shared name + conflicting `Q251`/`Q3757` | deterministic `different` | none |
| Java language / Java coffee | alias + incompatible controlled kinds | curated `different` recommendation enters human review | source-bound `named_after` suggestion validated separately |
| Python language / renamed Python language | alias + shared verified `Q28865` | deterministic `same` | none |
| Python language / Python genus | shared name + conflicting `Q28865`/`Q271218` | deterministic `different` | none |
| Python language / Monty Python | relation target only + conflicting `Q28865`/`Q16402` | deterministic `different` | source-bound `named_after` suggestion validated separately |

Final evaluator totals:

```text
                                      Java   Python
expected/indexed pairs                  3/3      3/3
unexpected indexed pairs                 0        0
relation-target-only discoveries         0        1
deterministic correct decisions           2        3
reviewed correct recommendations          1        0
wrong decisions/recommendations           0        0
validated directional suggestions         1        1
```

Python's relation-only counter is computed by repeating candidate discovery on
the same record with relation assertions removed; it falls from 3/3 to 2/3.
This isolates the new path rather than inferring its effect from labels.

### Adversarial and property coverage

- A matching label with the wrong controlled kind is not nominated.
- An agent/user `Claimed` relation is not nominated.
- A nominally verified assertion whose source is absent from the subject is
  not nominated.
- Forward and reverse index insertion orders return identical bounded pages.
- Random case variation and whitespace-normalized labels still find exactly
  the right-kind target across 128 generated cases.
- Random claimed relation targets produce no candidate across 128 generated
  cases.
- A 301-record homonym/relation bucket under a limit of 16 still retains the
  genuinely source-verified identifier match, independent of insertion order.
- The original 11-pair Helios/Pantheon corpus remains 11/11 with no unexpected
  candidates or wrong decisions.

### Performance after relation-target indexing

Three release runs retained the 80,000-entity, two-alias, 2,000-candidate
fixture and exercised a source-verified relation assertion on every rich
candidate:

| Measurement | Three-run range |
|-------------|----------------:|
| Build exact indexes, including kind selector | 108.7–133.9 ms |
| Candidate lookup p95 | 2.125–2.250 us |
| Rich deterministic analysis p95 | 4.792–5.042 us |
| Durable packet-bound proposal p95 | 743–809 us |
| Durable packet-bound review p95 | 720–814 us |
| Ledger after 200 proposals + decisions | 389,120 bytes |

The extra exact map lookup is measurable but immaterial to the existing
envelope. Candidate work remains indexed against 2,000 actual pairs instead of
1.6 billion possible pairs. All fixed analysis and durable-operation gates
pass with substantial margin.

### Limits and production implications

- A label-plus-kind selector may deliberately return multiple same-kind
  homonyms. Production should prefer a source-verified canonical target
  identifier when available, retain all selector provenance, and otherwise
  present bounded ambiguity; it must never silently select one.
- Wikipedia URLs remain live rather than content-addressed snapshots. The
  production assertion contract from E012 still requires content hash/page
  revision, retrieval time, extractor version, and resolver generation.
- These fixtures validate deterministic discovery and routing, not an LLM
  extractor, multilingual entity linker, or review UI. Model and human studies
  remain release blockers.

**Decision:** Adopt source-bound relation-target selectors in the production
plan as a conservative candidate channel. Do not give agents or ordinary user
claims this capability. Prefer a verified target identifier over label/kind
when the source supplies one; ambiguity remains reviewable and bounded.

Final isolated gates after E013:

```text
jq empty fixtures/*.json                            passed
cargo fmt --all -- --check                          passed
cargo clippy --offline --all-targets -- -D warnings passed
cargo test --release --offline                      passed
RUSTDOCFLAGS=-Dwarnings cargo doc --offline --no-deps passed
```

The release suite now contains 60 scenario/property test functions. Twelve
property tests each execute 128 generated cases (1,536 generated cases total),
in addition to concurrency and the nine subprocess abort boundaries across
audit, identity review, and material promotion.

## E014 — Pinned GitHub repository spaces: Codex and Homebrew Tools

**Date:** 2026-07-18

**Question:** Can the scoped-memory design ingest two real repositories with a
shared owner and overlapping packaging vocabulary, preserve their separate
project graphs, and generate a complete semantic merge preview without
inventing a product or release integration?

### Immutable inputs and inspection scope

- Project A: `openai/codex` at
  `028edf8c1e0914d42556ac0731bcc5b8cfa4d2ca` (5,609 tracked files).
- Project B: `openai/homebrew-tools` at
  `065bc95028be59a1744ad7c66f61f5a0ab7e5bb1` (12 tracked files).

Both repositories were cloned shallowly at their current `main` heads and
kept read-only as experiment inputs. The Codex tree was completely inventoried
by tracked path, extension, top-level subsystem, Cargo member, public module
surface, and cross-repository reference search. Twenty pinned high-signal
source documents were then read across its product, Rust workspace, CLI,
core, TUI, exec, app-server, protocol, tools, plugins, state/thread storage,
memory, SDK, distribution, sandbox, and release boundaries. Every one of the
12 tracked Homebrew Tools files was read.

This is intentionally not a claim that every line of the 5,609-file Codex
repository received a human-style semantic review. It is a complete tree and
reference inventory plus a source-level architecture pass over each major
boundary. Homebrew Tools was small enough for complete file-by-file review.

### Project A snapshot: `openai/codex`

The fixture records 21 entities and 26 internal relations. Principal facts:

- Codex CLI is a local OpenAI coding agent exposed through an interactive TUI,
  non-interactive exec/review, app-server, MCP server, plugin management,
  cloud-task, sandbox, session, and desktop-launch commands.
- `codex-rs` is the primary Rust 2024 implementation and currently declares
  124 workspace members. `codex-core` retains compatibility-sensitive agent
  orchestration; `codex-protocol` holds low-dependency shared types;
  `codex-tools` owns reusable host-side tool models and adapters.
- `codex app-server` is the rich-client boundary. It uses bidirectional
  JSON-RPC-like messages over stdio, experimental WebSocket, or local Unix
  socket and exposes thread/turn/item primitives.
- Local persistence separates canonical rollout JSONL, a storage-neutral
  thread-store boundary, and queryable SQLite state. `agent-graph-store` stores
  parent/child thread-spawn topology; it is not a semantic knowledge graph.
- Codex memory itself is a bounded two-phase pipeline. Phase 1 extracts
  per-rollout structured memories into SQLite. Phase 2 serializes global
  consolidation into a git-baselined filesystem workspace, writes a git-style
  workspace diff, and runs a restricted consolidation agent. This independently
  corroborates the audit-diff direction of the OpenMemory POC, but it does not
  implement cross-project entity identity or graph-space merging.
- Distribution paths include standalone release assets, `@openai/codex`, and
  the upstream Homebrew cask token `codex`. The TUI checks
  `https://formulae.brew.sh/api/cask/codex.json` for a Brew-managed install.

### Project B snapshot: `openai/homebrew-tools`

The fixture records 16 entities and 19 internal relations. Principal facts:

- The repository is the custom `openai/tools` Homebrew tap.
- `Casks/openai.rb` packages a different product: the OpenAI CLI for the API,
  from release artifacts in `openai/openai-cli`. At the pinned snapshot it is
  version 1.4.0 with four SHA-256-pinned macOS/Linux artifacts plus a binary,
  man page, and shell completions.
- The four formulas are Tart 2.33.0, Softnet 0.20.1, Orchard 0.56.0, and Tart
  Guest Agent 0.11.0. Tart depends on the tap's Softnet formula.
- `validate_recipes.rb` fails closed on unknown formulas and checks generated
  headers, recipe identities, semantic versions, upstream repository and
  artifact paths, SHA-256 values, installation behavior, and dependencies.
- CI checks Ruby syntax, `brew readall`, and the structural validator. The
  GoReleaser auto-merge workflow additionally verifies the trusted app author,
  exact branch/version pattern, base/head identity, same-repository ownership,
  exactly one expected changed file, successful validation, and the immutable
  head SHA before squash-merging.

The local structural validator and `ruby -c` passed for all five recipes.

### Crucial negative result

There is no direct Codex/Homebrew Tools release integration at these snapshots:

```text
openai/codex:          /homebrew-tools|openai\/tools/ -> 0 matches in 5,609 tracked files
openai/homebrew-tools: /codex/                       -> 0 matches in 12 tracked files
```

Therefore, the merged graph must not assert that `openai/tools` packages
Codex or that `openai/homebrew-tools` contains the Codex cask. Absence evidence
is stored as snapshot-scoped search coverage, not as an eternal fact.

### Identity trial

The two isolated fixture spaces were evaluated through the E013 exact candidate
index and proof-gated identity policy:

| Candidate pair | Result | Basis / route |
|---|---|---|
| OpenAI / OpenAI | same | shared verified `github_owner=openai`; team review |
| Apache-2.0 / Apache-2.0 | same | shared verified SPDX identifier; team review |
| Homebrew / Homebrew | same recommendation | semantic candidate; agent proposal enters team review |
| Codex CLI / OpenAI CLI | different | conflicting verified source repositories; auto-keep separate |
| Codex cask / OpenAI cask | different | `codex` vs `openai/tools/openai`; auto-keep separate |
| Codex release / tap release | different recommendation | related vocabulary, different pipelines; team review |

Results:

```text
expected candidates                  6
expected candidates discovered       6
total indexed candidate pairs         6
unexpected candidate pairs            0
correct deterministic decisions       4
correct agent proposals to review     2
wrong decisions                       0
team reviews required                 4
```

The team policy deliberately reviews even authoritative-ID `same` decisions;
proof can skip the agent but does not silently mutate a shared team graph.
Conflicting authoritative identifiers can safely auto-apply `different`.

### Generated semantic diff

Merging Project B's knowledge into a target graph based on Project A produces:

```text
reviewed entity coalescences          3  (OpenAI, Apache-2.0, Homebrew)
explicit distinct candidate pairs     3
source entities added                13
source relations added/rewired       19
entities deleted                      0
```

Relations targeting the three coalesced source entities are rewired to their
target-space identities. All other Project B nodes remain new, including the
Homebrew Tools repository, tap, OpenAI CLI, its cask, four Tart-family
formulas, validator, CI/release workflows, ownership, and security policy.
Neither source space is mutated; the output is a reviewable target changeset.

### Performance and determinism

Three release-mode runs executed 10,000 complete evaluations each. A complete
evaluation parses and validates both fixtures, builds indexes, resolves all six
candidate packets, binds proposal hashes, applies team routing, validates graph
coverage, and renders the full diff.

| Measurement | Three-run range |
|---|---:|
| Complete merge preview mean | 213.340–216.664 µs |
| Evaluations per run | 10,000 |

Two consecutive reports serialize byte-for-byte identically. The focused suite
also asserts that all 16 source entities are accounted for as exactly three
merges plus 13 additions, all 19 source relations appear, no deletion exists,
and both false Codex/tap edges remain explicitly guarded non-edges.

### Artifacts and gates

- `fixtures/github_codex_homebrew_tools.json`: the two immutable spaces.
- `src/github_merge.rs`: validation, identity evaluation, and diff renderer.
- `src/bin/github-merge-eval.rs`: human and machine-readable output.
- `src/bin/github-merge-measure.rs`: fixed release-mode measurement.
- `results/codex_homebrew_tools_merge.diff`: checked-in golden semantic diff.
- `tests/github_merge.rs`: completeness, false-link, and byte-determinism tests.

Focused gates:

```text
jq empty fixtures/github_codex_homebrew_tools.json   passed
ruby scripts/validate_recipes.rb                     passed (source repository)
ruby -c Casks/*.rb Formula/*.rb                      passed (five recipes)
cargo test --offline --test github_merge             passed (3/3)
cargo clippy --offline --all-targets -- -D warnings  passed
```

Final isolated gates after E014:

```text
jq empty fixtures/*.json                            passed
cargo fmt --all -- --check                          passed
cargo clippy --offline --all-targets -- -D warnings passed
cargo test --release --offline                      passed
RUSTDOCFLAGS=-Dwarnings cargo doc --offline --no-deps passed
```

The release suite now contains 63 scenario/property test functions. The three
new repository tests also lock the rendered merge preview to a golden diff.

**Decision:** The scoped merge design handles this pair correctly. Shared
ownership and package-manager vocabulary are insufficient to invent product
identity. Verified product/repository and cask-token identifiers dominate;
semantic sameness and same-owner coalescence remain team-reviewed; graph edges
are imported only from pinned positive evidence; negative integration claims
remain bounded to the audited snapshots.

## E015 — Independent Rust web frameworks: Axum and Actix Web

**Date:** 2026-07-18

**Question:** Does the merge model remain conservative and usable when two
independent projects genuinely implement the same domain, expose nearly
identical vocabulary, share important dependencies, and contain concrete Rust
items whose names and roles look mergeable but whose APIs are not
interchangeable?

### Immutable inputs and inspection scope

- Project A: `tokio-rs/axum` at
  `b7e37889932edcf521ca54e5ed30245f01180994` (503 tracked files).
- Project B: `actix/actix-web` at
  `82eed0ba1a128af6654efcb0317e9f2b60b23428` (444 tracked files).

The GitHub connector first confirmed both repository identities and `main` as
their default branch. Both trees were then shallow-cloned read-only. Every
tracked path was inventoried; root and crate manifests, lockfiles, READMEs,
public API roots, routing, handler, extraction, state, request/response,
middleware, WebSocket, CI, benchmark, and license boundaries were inspected.
The fixture binds every observation to 19 Axum and 21 Actix source URLs at the
exact commits.

This is a complete tracked-tree inventory and source-level pass over every
merge-relevant architecture boundary, not a claim that every line in 947 files
received equal semantic attention.

### Project A snapshot: `tokio-rs/axum`

The isolated Axum space contains 26 entities and 31 relations.

- The root workspace expands `axum` and `axum-*` into four crates: `axum`,
  `axum-core`, `axum-extra`, and `axum-macros`. Examples are a separate nested
  workspace.
- The `axum` manifest is version 0.8.9 at this snapshot, while the README warns
  that `main` contains breaking work toward 0.9.
- `Router<S>` composes handlers and services and encodes state still required
  before serving. `Handler<T,S>` receives an Axum request plus explicit state
  and returns a response future.
- Extraction is split between `FromRequest<S,M>`, which may consume the body,
  and `FromRequestParts<S>`, which cannot. `Path<T>`, `Query<T>`, and `State<S>`
  are concrete Axum wrappers with Axum-specific rejection and state semantics.
- Request and response are public aliases to `http` 1.x types. The lock resolves
  `http` 1.4.0 and Tokio 1.49.0.
- Axum intentionally uses Tower's `Service`/`Layer` model rather than defining a
  bespoke middleware contract. Its WebSocket session wraps tokio-tungstenite
  over a Hyper-upgraded connection.
- CI checks formatting, Clippy, strict rustdoc, every feature, stable/beta,
  macros on nightly, MSRV, dependency policy, cross targets, sorting, and
  spelling.

### Project B snapshot: `actix/actix-web`

The isolated Actix space contains 33 entities and 43 relations.

- The root workspace lists ten crates: `actix-web`, `actix-http`,
  `actix-router`, `actix-files`, `actix-http-test`, `actix-multipart`,
  `actix-multipart-derive`, `actix-test`, `actix-web-codegen`, and `awc`.
- `actix-web` is version 4.14.0. The repository also contains
  `actix-web-actors` 4.3.1+deprecated, but that crate is not in the ten-member
  root workspace.
- `App<T>` is a top-level builder over Actix `ServiceFactory` composition.
  `Handler<Args>` receives arguments after separate extraction rather than an
  Axum-style request-and-state pair.
- Actix `FromRequest` has associated Error and Future types and receives
  `&HttpRequest` plus `&mut Payload`. Its `Path<T>`, `Query<T>`, and `Data<T>`
  wrappers have Actix-specific decoding, configuration, and layered state
  lookup semantics.
- `HttpRequest` and `HttpResponse<B>` are concrete Actix types. `actix-http`
  declares `http` 0.2 and the lock resolves 0.2.12 for that public stack; the
  same lock also contains a separate transitive `http` 1.4.2.
- Middleware is built from `actix_service::Transform` and `Service`, not Tower.
  The inspected actor WebSocket integration uses `WebsocketContext<A>`, Actor,
  and StreamHandler over actix-http codecs.
- CI tests MSRV and stable Rust across Linux, macOS, and Windows plus minimal
  and default configurations, docs, io-uring, and dependency policy. A separate
  main-branch workflow runs the server Criterion benchmark.

### The decisive identity boundary

The `http` crates.io package is one entity across both spaces, but a package
identity is not a Rust type identity. Merging the package node retains two
version observations. It does not equate `http@1::Request` with
`http@0.2::Request`, and it does not equate Axum's public alias with Actix's
custom `HttpRequest`.

The same rule separates abstract capabilities from implementations:

| Shared concept or package | Reviewed merge | Concrete items retained separately |
|---|---|---|
| HTTP request routing | yes | `axum::Router` / `actix_web::App` |
| request extraction | yes | both `FromRequest` traits; both `Path<T>` and `Query<T>` wrappers |
| application state | yes | `axum::extract::State<S>` / `actix_web::web::Data<T>` |
| HTTP middleware | yes | Tower `Service`/`Layer` / Actix `Service`/`Transform` |
| WebSocket protocol support | yes | Axum `WebSocket` / Actix actor `WebsocketContext` |
| Tokio package | yes | version and feature observations remain scoped |
| `http` package | yes | incompatible-major request/response types remain scoped |

The repository, framework crate, application root, handler trait, extractor
trait, `Path`, `Query`, state carrier, request type, response type, middleware
contract, WebSocket session, and license policy pairs all have conflicting
source-verified authoritative identifiers. They are deterministically distinct.
The five abstract concepts have no identity proof; an agent may recommend
`same`, but team policy routes every recommendation to human review. Shared
package identifiers skip the agent but still require team review before a team
graph changes.

### Identity and changeset results

```text
expected candidate pairs                 20
expected candidates discovered           20
total indexed candidate pairs             20
unexpected candidate pairs                 0
correct deterministic decisions           15
correct agent proposals routed to review   5
wrong decisions                            0
team reviews required                      7
```

Merging the Actix knowledge graph into an Axum-based target graph proposes:

```text
reviewed entity coalescences               7
explicit distinct candidate pairs         13
Actix entities added                      26
Actix relations added/rewired             43
entities deleted                           0
```

All 33 source entities are accounted for as exactly seven coalescences plus 26
additions. Relations to Tokio, the `http` package, and the five merged abstract
concepts are rewired to the target identities. All Actix-specific crates, API
items, CI, and licensing stay distinct. Both original spaces remain immutable;
the result is a reviewable target changeset.

There is no Cargo-level project integration at the snapshots. `actix[-_]`
matches zero tracked Axum Cargo manifests and `axum` matches zero tracked Actix
Cargo manifests. Axum has one source comment explaining that other libraries
such as Actix Web call nested routing a “scope”; this is useful vocabulary
evidence, not dependency or identity evidence.

### Adversarial validation

Seven mutation tests independently prove that evaluation fails closed for:

- a source URL changed from the exact commit to `main`;
- a relation targeting a nonexistent entity;
- a duplicate logical ID;
- removal of guarded non-edges;
- a supposedly clean negative search with a nonzero match;
- a curated `same` answer forced onto conflicting Rust item identifiers;
- an injected homonym that creates an unexpected candidate pair.

The golden-diff test additionally locks the 20 decisions, 26 additions, 43
relations, five non-edge guards, no deletion, and full source-accounting
invariants. Two complete reports serialize byte-for-byte identically. The
previous Codex/Homebrew repository corpus still passes after the renderer was
generalized, preventing pair-specific behavior from replacing the reusable
policy.

### Performance

Three release runs executed 10,000 complete evaluations each. Every evaluation
parses and validates 59 records and 74 relations, builds exact indexes, finds
and resolves all 20 candidate pairs, binds proposal hashes, applies team policy,
checks complete source accounting, and renders the 43-relation diff.

| Measurement | Three-run range |
|---|---:|
| Complete merge preview mean | 494.995–498.674 µs |
| Evaluations per run | 10,000 |

This is roughly 2,000 complete previews per second on the development machine;
the user-facing operation remains dominated by extraction and review rather
than deterministic identity and changeset construction.

### Artifacts, gates, and limits

- `fixtures/github_axum_actix_web.json`: two immutable provenance-bound spaces.
- `src/github_merge.rs`: now a reusable two-repository evaluator and diff
  renderer with explicit guarded non-edges and Rust/package identifier rules.
- `src/bin/framework-merge-eval.rs`: human and machine-readable output.
- `src/bin/framework-merge-measure.rs`: fixed release measurement.
- `results/axum_actix_web_merge.diff`: checked-in golden semantic diff.
- `tests/framework_merge.rs` plus module mutation tests: completeness,
  non-collapse, determinism, and fail-closed validation.

Final isolated gates after E015:

```text
jq empty fixtures/*.json                            passed
cargo fmt --all -- --check                          passed
cargo clippy --offline --all-targets -- -D warnings passed
cargo test --release --offline                      passed
RUSTDOCFLAGS='-D warnings' cargo doc --offline --no-deps passed
```

The release suite now contains 73 scenario/property test functions, including
the ten new framework and mutation tests.

This remains curated source extraction and identity evaluation, not a held-out
LLM accuracy or reviewer-usability study. Production still needs content-hash
ingestion, model-evaluation corpora, calibrated uncertainty, and UI studies.

**Decision:** Adopt the two-level identity rule. Shared dependencies and
framework-independent concepts are eligible for reviewed coalescence. Concrete
Rust items require canonical crate/item coordinates and remain separate across
frameworks even when names and roles match. Package coalescence must never
imply type compatibility across versions. This pair validates the design under
the strongest real-world vocabulary overlap tested so far.

## E016 — Mathematics and implementation: Mathlib and Lean 4

**Date:** 2026-07-18

**Question:** Can two tightly related knowledge spaces merge seamlessly when
one is a formal mathematics library and the other is the programming-language
implementation on which it directly depends? In particular, can the merge
recognize genuinely identical upstream entities in both directions without
collapsing Mathlib's mathematical corpus and extensions into Lean's compiler,
kernel, runtime, or core library modules?

### Immutable inputs and inspection scope

- Mathematics project: `leanprover-community/mathlib4` at
  `168b9f3a7b2a75315d46232ec28c97f66a467c61` (8,986 tracked files).
- Coding project: `leanprover/lean4` at
  `25ba8c3d3bcb1dded7ff5a6f3b6044b0a7970198` (13,121 tracked files).

The GitHub repository-orientation workflow first confirmed the repository
identities and default branches. Both repositories were then shallow-cloned
read-only at exact commits. The complete tracked trees were inventoried before
architecture extraction. The curated fixture binds 31 source documents to
commit-pinned GitHub URLs, and a local object check verified that all 31 paths
exist at the declared commits.

Mathlib's tree contains 8,807 Lean source files. Its main `Mathlib` subtree has
8,284 tracked files and `MathlibTest` has 389. The largest subject subtrees are
Algebra (1,342), CategoryTheory (1,089), Analysis (795), RingTheory (717),
Topology (674), Data (649), LinearAlgebra (367), Tactic (357), MeasureTheory
(314), Order (313), and NumberTheory (240).

Lean's tree contains 6,937 Lean source files, 2,495 generated/bootstrap C
sources, and a large conformance-test corpus. The `src` tree is organized around
`Lean` (1,215 files), `Init` (630), `Std` (482), `lake` (162), `runtime` (79),
`library` (45), `util` (42), and the trusted C++ `kernel` (39). The root build
defines stage0, stage1, stage2, and stage3 builds; `check-stage3` compares the
stage2 and stage3 compiler binaries.

This is full tracked-tree coverage and a source-level pass over every
merge-relevant architecture boundary. It does not claim equal line-by-line
semantic review of all 22,107 files.

### The two isolated graphs

The Mathlib space contains 30 entities and 43 relations. Its key structure is:

- the Mathlib repository, community organization, Lake package, generated
  `Mathlib` root, carefully curated `Mathlib.Init`, `Mathlib.Tactic`,
  `Mathlib.Lean.Meta`, and `Mathlib.Data.Nat.Prime.Basic`;
- the Algebra, Category Theory, Analysis, Topology, and Number Theory corpus
  nodes;
- the olean cache, build/test workflow, fixed-toolchain policy, and Apache-2.0
  policy;
- external references to the Lean repository, exact Lean toolchain, Lake, Std,
  `Nat`, and upstream namespaces;
- abstract Lean-language, theorem-proving, dependent-type, tactic, natural
  number, and module-system concepts.

The Lean space contains 34 entities and 50 relations. Its key structure is:

- the Lean repository and project plus `Lean`, `Init`, `Std`, and Lake roots;
- trusted kernel, runtime, compiler, parser, elaborator, metaprogramming, and
  language-server subsystems;
- the core `Nat` declaration, `Lean` and `Lean.Parser` namespaces,
  `Init.Tactics`, and the natural-number portion of `Init.Prelude`;
- stage0/stage1/stage2 and self-hosting records;
- CI and a distinct Lean-to-Mathlib compatibility pipeline;
- an external Mathlib repository record and the same six abstract concepts.

### The critical version distinction

This corpus exposes a failure mode that a name-oriented merge would miss.
Mathlib's `lean-toolchain` pins `leanprover/lean4:v4.33.0-rc1`, and its Lake
package declares `fixedToolchain := true` with the explicit policy that a
Mathlib version supports only the toolchain against which it was built. The
pinned Lean master snapshot declares version 4.34.0-pre.

Therefore:

- Mathlib's external `leanprover/lean4` repository record and the Lean source
  repository are the same durable project identity;
- Mathlib's v4.33.0-rc1 toolchain observation and Lean master 4.34.0-pre are not
  the same snapshot or release;
- importing the Lean graph enriches the existing upstream-repository node but
  does not rewrite Mathlib's active toolchain to current master.

This separates durable entity identity from revision-scoped observations.

### Identity boundary and results

Twenty-two exact candidates were deliberately present:

| Class | Candidate surface | Result |
|---|---|---|
| reciprocal repositories | Mathlib's Lean dependency / Lean repository; Lean's Mathlib downstream / Mathlib repository | same by verified GitHub coordinates; team review |
| shared upstream code | Lake, Std, `Nat`, namespace `Lean`, namespace `Lean.Parser` | same by verified Lean coordinates; team review |
| license | Apache-2.0 | same by verified SPDX identifier; team review |
| abstract semantics | Lean language, theorem proving, dependent types, tactics, natural numbers, module system | agent recommends same; team review |
| projects | Mathlib / Lean implementation | different by conflicting `lean_project` coordinates |
| module roots | `Mathlib` / `Lean`; `Mathlib.Init` / `Init` | different by conflicting module coordinates |
| extensions vs upstream | `Mathlib.Tactic` / `Init.Tactics`; `Mathlib.Lean.Meta` / `Lean.Meta` | different by conflicting module coordinates |
| mathematics vs datatype implementation | `Mathlib.Data.Nat.Prime.Basic` / `Init.Prelude` | different by conflicting module coordinates |
| repository operations | Mathlib CI / Lean CI | different by repository-bound workflow coordinates |
| version policy | fixed Mathlib toolchain / Lean master development state | different by conflicting policy coordinates |

Results:

```text
expected candidate pairs                 22
expected candidates discovered           22
total indexed candidate pairs             22
unexpected candidate pairs                 0
correct deterministic decisions           16
correct agent proposals routed to review   6
wrong decisions                            0
team reviews required                     14
```

Names and aliases nominate candidates only. Eight `same` results are proven by
source-verified coordinates; six semantic `same` recommendations require an
agent; team policy still reviews all fourteen coalescences. The eight
conflicting-coordinate pairs are deterministically kept separate without
asking an agent to override proof.

### Generated target changeset

Merging the Lean space into a target graph based on Mathlib proposes:

```text
reviewed entity coalescences              14
explicit distinct candidate pairs          8
Lean entities added                       20
Lean relations added/rewired              50
entities deleted                           0
```

All 34 source entities are accounted for as exactly fourteen coalescences plus
twenty additions. The additions expose Lean's organization, implementation
project, current development version, root modules, compiler pipeline, trusted
kernel, runtime, language server, build stages, self-hosting, CI, and Mathlib
compatibility pipeline.

The reciprocal dependency is seamless after review:

1. Lean's repository record rewires to Mathlib's existing upstream
   `leanprover/lean4` node.
2. Lean's external Mathlib record rewires to the target's existing Mathlib
   repository node.
3. Imported Lean relations now make the upstream node contain Lean's kernel,
   runtime, modules, project, and CI.
4. Lean's `tests_against` compatibility edge and Lake's Mathlib project
   template both point back to the existing Mathlib node.
5. Relations to Lake, Std, `Nat`, shared namespaces, license, and concepts are
   rewired to their reviewed target identities.

No input space is mutated. No Mathlib module is reparented under a similarly
named Lean module. The diff is a proposed target-graph transaction.

### Guarded non-edges and adversarial validation

The golden diff retains six explicit non-edges:

- v4.33.0-rc1 toolchain is not Lean master 4.34.0-pre;
- Mathlib is not the Lean implementation project;
- `Mathlib.Tactic` is not `Init.Tactics`;
- `Mathlib.Lean.Meta` is not `Lean.Meta`;
- Mathlib prime-number theory is not the core datatype module;
- Mathlib CI is not Lean CI.

Snapshot-scoped negative searches confirm that Mathlib contains no Lean
`LEAN_VERSION_MINOR 34` declaration and Lean contains no Mathlib
`theorem prime_mul_iff` definition. These are bounded absence observations,
not eternal claims.

The pre-existing seven repository-fixture mutations still fail closed. Three
new corpus-specific mutations additionally prove failure when:

- Mathlib's fixed-toolchain policy is assigned Lean master's identity;
- `Init.Tactics` is assigned the `Mathlib.Tactic` module coordinate;
- an unrelated Lean project record is injected as a second `natural numbers`
  homonym candidate.

Focused tests lock complete candidate accounting, every changeset count,
reciprocal relation rewiring, the concrete-module non-collapses, the version
guard, the 169-line golden diff, and byte-for-byte report determinism.

### Performance and final gates

Three release-mode runs executed 10,000 complete evaluations each. Every
evaluation parses and validates 64 records and 93 relations, builds exact
indexes, resolves 22 candidates, binds agent proposals to packet hashes,
routes team review, verifies accounting, and renders all 50 source relations.

| Measurement | Three-run range |
|---|---:|
| Complete merge preview mean | 492.023–494.446 µs |
| Evaluations per run | 10,000 |

This is approximately 2,020 complete previews per second on the development
machine. The deterministic merge machinery remains well below interactive
latency; extraction and human review dominate user-visible time.

Final isolated gates after E016:

```text
jq empty fixtures/github_mathlib_lean4.json          passed
31/31 commit-pinned source paths exist               passed
cargo fmt --all -- --check                           passed
cargo clippy --offline --all-targets -- -D warnings  passed
cargo test --release --offline                       passed
RUSTDOCFLAGS='-D warnings' cargo doc --offline --no-deps passed
```

The release suite now contains 79 scenario/property test functions. Production
crates remain untouched; this is isolated validation scaffolding. The GitHub
orientation workflow materially kept repository identity, source inspection,
and provenance aligned to immutable commits before modeling.

**Decision:** The design succeeds on a direct mathematics/implementation
dependency. Durable upstream identities can coalesce across spaces while
version observations remain scoped. Shared concepts require reviewed semantic
judgment; concrete project/module/workflow coordinates remain authoritative.
Reciprocal relation rewiring makes the merged graph natural to navigate without
silently changing either source graph or inventing code compatibility.

## E017 — Generic semantic merge planner redesign

**Date:** 2026-07-18

**Question:** Does the accumulated identity work actually drive a generic,
fail-closed graph merge, or do the real-world repository previews merely render
the curated fixture expectations? Can one planner handle unrelated products,
independent projects in the same technical domain, and a direct
mathematics/toolchain dependency while preserving every origin and relation?

### Audit finding: the prior prototype still cheated at the last boundary

The E014–E016 identity evaluator was conservative and correct: it discovered
candidates through exact indexes, built evidence packets, enforced trusted
identifier contradictions, routed agent proposals, and produced zero wrong
decisions on the curated corpora. However, `render_preview` reconstructed the
entity coalescence map directly from fixture `expectations`.

That shortcut meant the human-readable diff was correct, but no independent
material planner proved all of the following at once:

- every discovered candidate had exactly one current decision;
- the decision was bound to the current target/source entity revisions;
- a team-policy `same` had an explicit review receipt;
- one source entity had only one target and one target did not absorb multiple
  unnormalized source identities;
- every source entity was either merged or added exactly once;
- imported IDs could not overwrite an existing target ID;
- all relation endpoints were rewired through the final identity map;
- every entity contribution and relation assertion survived with provenance;
- source and target snapshots remained unchanged;
- the predicted result and plan were content-addressed and tamper-evident.

This was the last pair-specific seam. It was unacceptable for production and
has been removed in the POC.

### Redesigned architecture

`experiments/memory-model/src/semantic_merge.rs` is a generic immutable
knowledge-space planner. It is not aware of GitHub, Rust frameworks, Lean,
fixture expectations, labels, or model prompts.

The boundary is now:

```text
source extraction
  -> immutable scoped snapshots
  -> bounded candidate discovery
  -> proof/agent identity packets
  -> policy or human resolution receipts
  -> generic semantic merge plan
  -> independently verified materialized snapshot
  -> production-only staged persistence/promotion
```

Identity and content are deliberately separate. A canonical entity contains a
map of immutable `EntityContribution` values. Each contribution retains origin
space, origin entity, source revision, label, aliases, kind, identifiers,
description, and source references. Coalescing two entities appends the source
contribution to the target entity and gives the canonical entity a new
content-derived revision. It does not blend prose, choose a winning label, or
discard a disagreement.

Relations are a canonical `(subject, predicate, object)` key plus a set of
`RelationContribution` assertions. An assertion retains its origin space and
source reference. Relation identity and relation provenance therefore remain
separate: the same edge may gain a second source without creating an extra
semantic edge.

### Planner contract

The planner accepts four inputs only:

1. immutable target snapshot;
2. immutable source snapshot;
3. the complete discovered candidate set;
4. revision- and packet-bound identity resolutions.

A deterministic policy receipt may resolve only `different`. Under team
policy, every `same` requires a human-review receipt, even if source-verified
coordinates prove identity. An agent remains an untrusted proposal source; it
cannot construct a planner-authoritative `same` by itself.

Planning proceeds in stable order:

1. Reject identical source/target spaces.
2. Validate every resolution against the candidate set and current entity
   revisions.
3. Require exactly one resolution for every candidate and reject extras,
   duplicates, and `undetermined` decisions.
4. Reject one source resolving `same` to multiple targets.
5. Reject multiple source entities resolving `same` to one target. Those source
   duplicates must be normalized and reviewed in their own space first.
6. Assign every source entity exactly one disposition:
   - `Merge(source, target, resolution_hash)`; or
   - `Add(source, source_space::source_id)`.
7. Reject a source-qualified generated ID if it already exists in the target.
8. Rewire both endpoints of every source relation through this disposition map.
9. Classify each assertion as a new relation, added provenance, or already
   present.
10. Simulate the entire result, then compare its contribution set against the
    exact union of target plus source.
11. Prove every target assertion remains and every source assertion exists
    after rewiring.
12. Hash the predicted result and bind it, both input hashes, all resolution
    hashes, entity actions, and relation actions into one plan hash.

Materialization rechecks the plan hash and both input hashes, builds a new
snapshot, repeats complete accounting, and requires the observed result hash
to equal the prediction. It never mutates either input. Production will apply
the same plan to a staged root and reuse the previously validated old-or-new
promotion/recovery protocol.

### The “cerpheus” boundary under the new planner

The small explicit regression contains `cerpheus` in Project A and Project B.

- With a deterministic `different` receipt from conflicting project
  coordinates, the result contains both `cerpheus` and
  `project-b::cerpheus`, each with exactly one origin contribution.
- With a reviewed `same` receipt, the target `cerpheus` contains both immutable
  contributions. Project B's inbound and outbound relation endpoints rewire to
  that target, while unrelated Project B entities receive source-qualified IDs.

The string does not decide. The reviewed identity receipt decides, and the
planner proves the graph transformation implied by that receipt.

### Permanent real-world examples

The three repository evaluators now convert their fixtures into generic
snapshots, convert the already-routed policy/team outcomes into current
resolution receipts, and delegate every entity/relation action to
`plan_knowledge_merge`. The renderer reads the planner's dispositions; fixture
expectations remain test oracles only.

```text
example                 candidates  merges  distinct  additions  relations
Codex/Homebrew Tools             6       3         3         13         19
Axum/Actix Web                  20       7        13         26         43
Mathlib/Lean 4                  22      14         8         20         50
TOTAL                           48      24        24         59        112
```

All three produced zero unexpected candidates, zero wrong decisions, and zero
deletions. Their existing 169-line-or-smaller golden changesets remained
byte-identical, proving the redesign changed authority and validation rather
than presentation semantics.

The Wikipedia Solar System/Roman naming corpus, Java language/island/coffee
corpus, Python language/snake/Monty Python corpus, and all base identity tests
also still pass. They continue to validate candidate/decision behavior; the
three repository corpora exercise full graph planning and materialization.

### New adversarial and property tests

Ten deterministic semantic-planner tests now cover:

- reviewed coalescence, immutable contributions, and bidirectional endpoint
  rewiring;
- distinct same-label `cerpheus` concepts;
- missing and extra candidate resolutions;
- stale receipts and moved source snapshots;
- rejected many-to-one coalescence;
- generated imported-ID collision;
- relation evidence not bound to its subject;
- forbidden policy-only `same`;
- tampered result/plan hash;
- byte determinism under reversed entity insertion order.

Two property tests repeatedly generate spaces and prove:

- every source entity is accounted exactly once independent of insertion
  order; and
- every rewired relation endpoint exists in the materialized result.

A separate integration fixture constructs 10,000 source entities connected by
9,999 relations. All 10,000 entities are added exactly once, all 9,999 relation
assertions are imported, no endpoint dangles, and the final graph contains the
target root plus all source contributions.

The existing concurrency, SQLite transaction, identity-review crash, staged
directory-swap crash, three-way merge, audit, space isolation, and recall tests
remain green. The release suite now contains 92 scenario/property test
functions.

### Performance

`semantic-merge-scale` measures a complete plan followed by a separately
revalidated in-memory materialization. Planning already simulates the result,
performs full contribution/assertion accounting, and hashes it; materialization
intentionally repeats validation as the apply boundary must.

Three release runs:

| Source size | Relations | Iterations/run | Mean range |
|---:|---:|---:|---:|
| 100 | 99 | 1,000 | 0.583–0.655 ms |
| 1,000 | 999 | 100 | 6.587–7.073 ms |
| 10,000 | 9,999 | 10 | 86.408–93.070 ms |

The implementation is near-linear through this range. The 10,000-object case
processes roughly 107k–116k source entities and the same order of relation
assertions per second while performing two verified materializations and
several canonical hashes.

The full 64-entity/93-relation Mathlib/Lean extraction, identity resolution,
generic planning, verified materialization, and diff render now measures
1.374–1.387 ms per evaluation over three 10,000-iteration runs. E016's old
renderer-only path was about 0.493 ms. The additional ~0.89 ms buys independent
receipt validation, complete contribution/assertion accounting, two immutable
materializations, and plan/result hashing. Approximately 720 complete previews
per second remains comfortably interactive.

### Quality gates and artifacts

```text
cargo fmt --all -- --check                           passed
cargo clippy --offline --all-targets -- -D warnings  passed
cargo test --release --offline                       passed (92 functions)
RUSTDOCFLAGS='-D warnings' cargo doc --offline --no-deps passed
redesign-eval                                         passed (3/3 corpora)
semantic-merge-scale                                 passed (3 repeated runs)
```

Artifacts:

- `src/semantic_merge.rs`: generic snapshots, receipts, planner, accounting,
  hashing, and materialization.
- `tests/semantic_merge.rs`: ten deterministic adversarial cases.
- `tests/semantic_merge_properties.rs`: generated accounting and endpoint
  invariants.
- `tests/semantic_merge_scale.rs`: 10k/9,999 integration fixture.
- `src/bin/redesign-eval.rs`: combined permanent-example runner.
- `src/bin/semantic-merge-scale.rs`: repeated scale measurement.
- `src/github_merge.rs`: repository adapter now delegating to the generic
  planner.
- `plan/15-memory-spaces-audit-and-merge.md`: updated production contract and
  rollout gates.

Production crates remain untouched. The fixture harness still uses curated
expected decisions to simulate completed policy/team review; it does not claim
held-out model accuracy or reviewer usability. Production also still needs a
canonical export for 100k/1M scale, durable resolution receipt loading,
authorization checks, staged filesystem persistence, and UI studies.

**Decision:** Adopt the contribution-preserving generic planner contract. Do
not port the old fixture-driven renderer or create pair-specific merge code in
production. Identity resolution produces audited receipts; one reusable planner
turns those receipts into a content-addressed, fully accounted graph changeset;
the existing staged promotion layer makes that changeset durable and crash
safe. Keep projection/consolidation separate so merging identity never erases
disagreement.

## Production implementation ledger

### P000 — Production baseline and preserved workspace state

**Date:** 2026-07-18

**Revision:** `bcd10fd3f736290c536ac94daf5a9014ef711828`
(`feat: harden context engine for desktop production`), matching the numbered
plan's grounded revision.

**Toolchain and host:**

- `rustc 1.85.0 (4d91de4e4 2025-02-17)`
- `cargo 1.85.0 (d73d2caf9 2024-12-31)`
- Darwin 25.5.0, ARM64 T6030 (`aarch64-apple-darwin`)

**Worktree:** Tracked production files were clean. `LOGBOOK.md`, `plan/`,
`experiments/`, and prototype image assets were untracked user work. They are
preserved; production implementation will not reset, delete, or overwrite
them. This entry is appended to the existing untracked ledger as required by
the implementation specification.

**Baseline command:**

```text
cargo test -p openmemory-watch --test integration --locked --all-features
```

**Result:** 4 passed, 0 failed in 3.11 seconds. This isolated success is
consistent with E000/E009 and does not disprove the known parallel workspace
readiness flake. Inspection found that the test receives the initial-scan
notification before the native watcher is constructed and registered, so the
test can begin readiness probes while no backend exists. The production fix
will register first, then publish initial-scan/readiness state, preserving
event-driven synchronization without adding sleeps.
