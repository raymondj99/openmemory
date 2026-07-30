# Test, Performance, and Release Plan

## Proof standard

Each invariant needs a test that fails under its obvious shortcut. Use real
SQLite/filesystems for durability, small reference models for pure semantics,
subprocess aborts for crash boundaries, and release builds for performance.
No timing sleeps, fixture-name branches, ignored blockers, or error-only
simulation where process death/disk failure is the risk.

New crates/modules use `#![forbid(unsafe_code)]`. All externally supplied text,
collections, scores, paths, and artifacts have hard bounds and arbitrary-input
tests.

Phase 0 first stabilizes the existing FSEvents readiness flake causally on the
supported macOS/Linux matrix.

## Stage verification contract

Every phase must ship its tests with its implementation and pass all applicable
layers before review:

1. **Unit/property**: validated types, pure algorithms, state transitions,
   bounds, canonical ordering/hashing, and minimized regressions.
2. **Production integration**: real repository services with actual SQLite,
   indexes, manifests, process locks, filesystem operations, and public
   crate/service boundaries. Mocks may isolate an external model, never the
   persistence or publication mechanism being proved.
3. **Real-world scenario**: at least one permanent, sanitized, provenance-
   documented fixture executed through the production implementation path.
   Fixture-name branches and prototype adapters are forbidden.
4. **Compatibility regression**: affected legacy profile, Rust wrapper, admin,
   CLI, MCP, watch/ingest, backup, or domain-migration behavior.
5. **Failure/concurrency/security**: every newly reachable failure boundary,
   race, malformed input, authorization boundary, and recovery transition.
6. **Performance/resource**: relevant control comparison and hard worker,
   queue, handle, memory, disk, and latency budgets.

A layer may be marked not applicable only with a written reason approved during
phase review. “Covered later” is not valid for behavior already reachable in
that phase. A phase cannot pass solely on unit tests, mocks, the isolated
prototype, or a happy-path integration test.

Each implementation-log verification packet records:

- phase, invariant/failure-matrix IDs, owning test names, and fixture revision;
- exact command, features, seed/case/repetition counts, and pass/fail/ignored
  counts;
- OS, filesystem, toolchain, commit, and benchmark controls where relevant;
- failure output, minimized regression, and corrective change;
- confirmation that the production path—not prototype code or a fixture-
  specific branch—was exercised.

Real-world means a versioned representative workload, not an uncontrolled
network dependency. Required fixtures include populated legacy profiles,
personal/project/team stores, actual multi-domain SQLite/index families, public
daemon/CLI/MCP process flows, and the sanitized repository corpora.

## Engineering-principle conformance

Every slice completes the checklist in `10-engineering-principles.md` and adds
causal evidence for the seams it reaches:

- **Policy normalization:** mutate ambient config, environment, current
  directory, platform facts, and workspace mappings after resolution and prove
  the in-flight operation is unchanged; invalid explicit intent must fail
  rather than fall back.
- **General/specialized equivalence:** force the general implementation and
  every eligible fast path, cache hit, direct-domain path, indexed discovery,
  streaming planner, optional adapter, and platform strategy over the same
  corpus. Compare values, order, stable errors, provenance, telemetry,
  publication, and cleanup; prove ineligible paths cannot be selected.
- **Representation boundaries:** round-trip every canonical wire/hash/path
  encoding and reject non-reversible or invalid input without lossy conversion,
  aliasing, panic, or accidental authority.
- **Closed worlds:** exhaustively cover every enum/state/tag/version transition
  and verify generated descriptors, serde forms, SQL constraints, and
  compatibility tables come from one owner.
- **Caches/publication:** vary each shaping input and generation independently
  to catch false hits; race readers at every construction boundary and prove
  only a fully verified atomically published value becomes visible.
- **Ownership/protocols:** use leak/resource counters and injected early return,
  error, cancel, timeout, panic, disconnect, and shutdown at every acquisition
  and stream phase; cleanup is armed before fallible work and runs once.
- **Portability:** compile supported targets, test centralized capability
  selection/probes and incompatible filesystems, and give every workaround an
  owner, regression, and removal condition.
- **Optional seams:** run the same semantic suite with optional features
  absent, present, unavailable, and explicitly required.

## Required tests by owner

### Core, catalog, and authority

- Canonical ID/string/serde round trips; arbitrary Unicode/bytes never panic;
  accepted path components are portable and non-reserved.
- Reversible workspace-path key round trips; non-UTF-8 or incompatible
  normalization fails before uniqueness, authorization, persistence, or cache
  lookup and never aliases a lossy display string.
- Resolved policy/selection provenance is immutable; explicit invalid choices
  do not silently fall through and ambient mutation cannot reroute an admitted
  operation.
- Domain counts reject zero, >64, shard-incompatible, or changed pinned values
  before opening stores or allocating pools.
- Read sets reject empty/duplicate/unauthorized/>4 and preserve precedence.
- Config defaults, hard caps, invalid combinations, and no NaN/Infinity.
- Product v1→v4 migration, failed-step rollback, exact indexes/constraints,
  idempotent reopen, future refusal.
- Legacy personal-global binding at every manifest/catalog crash combination
  creates exactly one ID/root.
- Managed-root uniqueness across active/closed rows; path traversal, symlink,
  hard-link, case/device-name, and path-swap harnesses.
- Shared lifetime process lock blocks snapshot/migration/promotion while a
  daemon-less store is open; exclusive acquisition excludes new opens and
  releases cleanly after crash/recovery.
- Team role/self-review matrix; expiry, disable/archive, credential rotation,
  revocation racing reads/writes; revocation waits old leases and invalidates
  the next use.
- Capability plaintext never persists; wrong bearer/principal/actor/profile,
  substitution, replay, expiry, rotation, and forged payload fail.
- Full restore rotates authority and makes every pre-backup capability/token
  and merge confirmation unusable.
- Registry one-open, lease/eviction, error recovery, close-for-promotion, and
  domain/connection/index/context-engine/flusher cap under churn and 10k
  catalog rows. Opening over budget allocates nothing and fails/evicts
  deterministically.

### Graph audit and derived state

- Fresh/v1/v2→v4 graph migration, populated fixtures, failed-step rollback,
  exact singleton metadata, future/request/hash-version refusal.
- Immediate canonical/revision/event/generation/outbox all-or-nothing.
- Proposal invisibility and complete transition/idempotency table, including
  submit-mode binding and exact affected-row checks.
- Lazy baseline, edit/retire/restore/revert/destruction confirmation, and
  deterministic diff.
- Cross-domain draft rejected before insertion; non-atomic child receipts retain
  caller order and successful siblings.
- Index commit/crash/apply/generation repair; recall refuses persistent stale
  derived state.
- Canonical relation/mirror outbox at-least-once repair and readiness.
- Canonical export/import/hash round trip for all lifecycle/history/
  contribution/relation cases.
- Controllable SQLite VFS injects `SQLITE_FULL`, short write, sync, checkpoint,
  WAL, corrupt page, and reopen errors; no premature success or lost
  acknowledged data.

### Shared execution and recall

- Force the general path and every direct/cache specialization over identical
  inputs; eligibility false positives are rejected or fall back, and observable
  values, ordering, errors, provenance, access telemetry, and cleanup match.
- Four or more concurrent coordinators across every 1/4-space × 1/4-domain
  shape never exceed the one runtime worker/task/coordinator/byte bounds.
- Executor tasks, direct single-domain calls, and context-engine drains share
  one active domain-I/O bound; direct fast paths retain result/performance
  parity.
- Context-engine submissions across multiple spaces cannot exceed global
  queued-write count/byte limits; reservation, journal failure, commit, replay,
  and cancellation release permits exactly once.
- All-or-nothing admission allocates no partial operation; queue saturation and
  byte/cost rejection are typed and observable.
- Re-entrant submission fails without deadlock; panic becomes the lowest
  indexed task error and the pool remains usable.
- Completion order cannot change results/errors; the barrier waits every task
  to a terminal state, except an explicit stuck-task deadline which publishes
  nothing and degrades the operation; non-atomic outcomes retain numeric order.
- Maintenance max-size backlog cannot occupy foreground reserve; round-robin
  operation fairness, FIFO same-class admission, large-request starvation, and
  bounded chunk yield are deterministic.
- Queue/execution deadline cancels queued work, cooperatively interrupts active
  work, prevents publication, and marks a non-stopping task/space degraded.
- Writer deadline before commit rolls back; a deadline racing commit returns an
  idempotently reconcilable outcome-unknown receipt, never a false rollback.
- Startup/quiesce/shutdown are bounded; a stuck worker returns
  `shutdown_incomplete` rather than hanging.
- One-space/single-domain result and allocation fast-path parity.
- Flattened vs existing facade parity for cold/warm/TTL/capacity, every filter
  and keyword/hybrid/vector mode, spreading activation, write races, every
  write invalidator, non-finite output, errors, and access recording on/off.
- Layered provenance/fusion exact across completion orders; only semantic-hash
  dedup; catalog size independent; authorization/cache generation invalidation.

### Identity and pure merge

- Canonical hashes change for every semantic field and ignore insertion order/
  telemetry/serialization; framing, version, tamper, and collision regressions.
- Lineage, verified/claimed IDs, resolver/ontology generations, simultaneous
  proof/contradiction, unknown kind, and context-only evidence.
- Agent timeout/invalid schema/injection/unknown evidence/hard-proof reversal
  produces no decision or graph mutation.
- Candidate index is target-space scoped, prioritizes strong evidence, rejects
  stale duplicate insertion, and marks truncation without inferring different.
- Indexed discovery matches a brute-force reference on generated bounded
  corpora; every strong-evidence pair appears before contextual truncation and
  discovery receipts account every consulted index generation/page.
- Indexed discovery forced off/on has exact receipt and decision equivalence;
  stale or unsupported generations fall back or fail typed and never claim
  completeness.
- Duplicate values observed in a configured unique namespace revoke proof and
  force policy/resolver revalidation.
- Review packet/revision/generation rebinding; concurrent reviewers; new
  evidence reopens; `undetermined` is not a receipt.
- Planner rejects missing/extra/stale/duplicate resolution, many-to-one,
  qualified-ID collision, duplicate assertion, dangling endpoint, silent
  deletion, and forged action structure.
- Every emitted candidate is consumed exactly once; keep-distinct receipts bind
  the complete page without asserting `different`, and truncated discovery can
  never coalesce.
- Every source/target entity, observation, relation, receipt, and contribution
  is accounted exactly once.
- Three-way unchanged/source/target/same/disjoint/conflict/delete-vs-edit.
- Streaming predicted hash equals independently materialized hash and is
  invariant to input chunk/allocation order.
- Materialized-reference and streaming planners emit identical actions,
  accounting, hashes, stable errors, and terminal outcomes; every injected
  source/sink failure aborts once and publishes no finished plan.

### Snapshot, materialization, and recovery

- One pause drains accepted writers, index/mirror/outbox/journal work before
  generation capture; concurrent rejected/new writes cannot enter.
- Domain export/checkpoint failure at every index publishes no manifest.
- Barrier verifies numeric generations, canonical/file hashes, schemas,
  indexes/mirrors, and manifest; completion order cannot change publication.
- Snapshot deadline leaves either resumed known-good space or explicitly closed
  recovery state.
- Per-domain material import/rebuild/checkpoint/verify failure publishes
  nothing; global accounting and predicted hash are independently checked.
- Target applied/rejected audit and idempotency receipts survive byte-
  equivalently; pending proposals remain only when expected heads/lifecycle are
  unchanged and otherwise conflict without rebasing. Source proposals never
  import.
- Foreground latency and worker reserve remain within budget during snapshot,
  backfill, and materialization.
- Final target admission guard covers every production writer from fresh target
  recheck through rename/open verification, including cross-process legacy
  opens through the stable exclusive space lock.
- Existing domain migration passes after primitive extraction.
- Source snapshot remains byte/hash unchanged after success, failure, cancel,
  and recovery.

### Surface and compatibility contracts

- Spawned-process tests concurrently drain bounded stdout/stderr and arm child
  cleanup before assertions; early failure, timeout, and client disconnect
  leave no child, socket, lock, temp artifact, or background task.
- Admin DTO old/new serde goldens, stable codes/status/redaction, pagination,
  job/SSE restart ordering, and capability readiness.
- Existing MCP initialize/list/call corpus for every old tool; fixed and
  resolved contexts agree; raw targets cannot broaden capability.
- Legacy forget keeps its wire response and removes the object from ordinary
  recall through retirement, preserves history, and exposes no hard-destroy
  agent path.
- CLI parser/completions/process output/JSON/exit/confirmation/daemon-absent
  cases for every new command.
- Watch/ingest retain one target and provenance for a complete run.
- Existing profile, daemon-less stdio, backup/restore/status/consolidate/prune,
  and domain migration behavior with personal-global only.
- Default/all/no-default feature combinations; embeddings remain optional.

## Property, concurrency, and crash matrices

Run at least 256 CI cases for each cheap property:

- arbitrary ID/read-set/fusion/hash input;
- random revision operations against a reference state machine;
- candidate symmetry and “context never proves same”;
- three-way disjoint-change commutativity and explicit direction asymmetry;
- planner accounting/no-overwrite/no-dangling/contribution-order;
- streamed plan/result equivalence and semantic mutation sensitivity.

Concurrency tests use barriers/channels/fixed clocks and run 100 iterations in
reference CI:

- competing reviews/proposals/edits/lifecycle/revocation;
- recall vs write/cache publication/index repair;
- registry eviction vs recall/promotion;
- context resolution vs team/space closure;
- two merge jobs on one target;
- snapshot/confirmation vs source/target writes;
- concurrent daemon/process promotion-intent ownership.

Crash workers call `abort()` after every durable boundary:

- changeset insert, projection/revisions/outbox, graph commit, index/mirror
  apply and generation update;
- catalog create, manifest file/parent sync, activation, backfill cursor;
- snapshot pause, each domain export/checkpoint/verification, manifest sync,
  publish;
- intent phases, staging import/rebuild/verification, target→backup rename/
  parent sync, staging→target rename/parent sync, promoted open, catalog/
  lineage commit, intent clear, cleanup.

Run every boundary at least 10 times per supported macOS and Linux release
runner. Recovery must be idempotent and yield exact verified old/new state,
unchanged source, no duplicate audit/contribution, and no ambiguous open.

## Permanent fixtures and security

Commit small inspectable v1/v2 schema fixtures, legacy audit rows,
personal/project/team isolation in 1/4-domain forms, adversarial homonyms and
identifiers, and sanitized/provenanced Codex/Homebrew, Axum/Actix, and
Mathlib/Lean corpora.

Security tests cover:

- raw IDs/paths/repo URLs/source/target fields cannot escalate authority;
- manifest/import/intent traversal, symlink/hard-link/path swap, cross-device
  rename, near-full and unsupported filesystems;
- common-label/candidate explosion, oversized/deep JSON, invalid UTF-8,
  duplicate evidence, non-finite scores, corrupt/truncated/oversized artifacts;
- imported authority is ignored;
- logs/SSE/errors/Debug omit tokens, content, absolute paths, and secrets;
- optional outbound agent use is disabled without explicit configuration and
  data consent.

## Benchmarks

Use fixed-seed interleaved control/feature samples in release mode. Verify
results outside timed loops and record toolchain, revision, features,
filesystem, CPU, memory, sample count, p50/p95/p99, throughput, peak RSS, disk,
and foreground-load impact.

Required groups:

1. context resolution with 1/1k/10k catalog rows;
2. layered recall 1/2/4 spaces × 1/4 domains, cold/warm, all modes;
3. fusion 64/256/512/1,024 candidates;
4. executor admission/queue under concurrent foreground and maintenance;
5. legacy vs audited write, proposal, approval, and outbox repair;
6. audit storage amplification at 1/16/256 KiB payloads;
7. identity packet/candidate index at maximum evidence and 100k homonyms;
8. merge planning at 100/1k/10k/100k source and target objects;
9. snapshot and staged materialization sequential vs 1/2/4 concurrency on
   representative SSD, slow/throttled, near-full, and foreground-loaded storage;
10. daemon list/context/proposal/diff/merge-status routes.

If an agent adapter is proposed for default enablement, publish a versioned
offline evaluation: pre-agent candidate coverage, deterministic proof/
contradiction rate, same/different/undetermined/invalid/timeout rates, false
same/false different, human review rate/agreement/time, and model/prompt/cost/
latency. False proof overrides are always zero-tolerance. Ordinary CI uses a
deterministic fake and requires no network; production remains safe with no
model configured.

Snapshot remains concurrency 1 unless a platform/filesystem profile improves
p95 at least 10% with no correctness failure and ≤5% foreground p95 regression.
Material construction remains 2 under the same gate. Do not auto-tune from one
request; ship reviewed per-platform defaults and operator metrics.

## Graph diffusion: measured and rejected for this workload

A bounded deterministic diffusion channel was built and measured against
the existing 1-hop `spread_activation` fallback. It is **not adopted**,
and the reason is quantitative rather than a design preference.

Two mechanisms were established along the way and both are worth
keeping:

- **Edge coverage decides the channel's value.** Without commit→file
  edges the channel scored −0.0128 on the target category; with them,
  +0.0272 — same algorithm, same weights. Effort spent recording the
  relationships users ask about dominates effort spent tuning
  propagation.
- **Confidence-driven weighting works as designed.** Reading confidence
  from dense scores yields mean channel weight 1.46 on a category where
  retrieval fails versus 0.84 where it succeeds, cutting the collateral
  damage by 71%.

The decisive measurement is a **selector oracle**: a router that expands
only when expansion actually improves that query. It is unachievable, so
it bounds what a selector choosing between direct retrieval and a given
diffusion arm could deliver.

Expansion improves **2 queries out of 574**; the other **572 have zero
positive headroom**, and doc-heading is not a trade-off but pure loss
(0 helped, 346 harmed).

Because a single arm's oracle bounds only that arm, six configurations
were crossed — hops 1/2/3, damping 0.3/0.5/0.7, per-entity 1/3/5, weight
1.0/2.0/4.0 — and the per-query best taken:

| category | queries | one arm | 6-arm envelope |
|---|---:|---:|---:|
| commit-to-file | 18 | +0.0400 | +0.0995 |
| doc-heading | 454 | +0.0000 | **+0.0000** |
| commit-lookup | 102 | +0.0000 | **+0.0000** |

Micro-average headroom: **+0.0013** for one arm, **+0.0031** across the
envelope. **556 queries have zero headroom under every arm crossed.**

That is sufficient to decline promoting this channel and is explicitly
*not* a claim about graph retrieval in general, a typed query planner,
or configurations outside the crossed set. The falsification is
straightforward: a query mix with more genuine cross-entity questions
moves the headroom, and two categories sitting at exactly zero indicate
the current mix contains almost none.

This is a property of the query mix, not of graph retrieval, and should
be re-measured if the mix changes materially. The existing 1-hop
fallback should not be described as a semantic-retrieval mechanism in
the meantime; it fires only when direct hits underflow `top_k` and
contributes nothing on ordinary queries.

## Retrieval confidence is a required capability

The system has no calibrated notion of result quality, and three
separate failures trace to that single gap:

| Capability | Needs to know | Fails today because |
|---|---|---|
| Abstention | is anything here relevant? | no score separates a good hit from a bad one |
| Cross-space composition | are these spaces' scores comparable? | uncalibrated; one space silently contributes zero |
| Adaptive channel weighting | did direct retrieval succeed? | a channel strong enough to rescue a failing query destroys working ones |

Measured evidence that a usable signal exists: the **shape** of a result
set (top margin, top lift, dispersion, head/tail ratio — all scale-free)
separates answerable from unanswerable queries with AUC **0.809** on one
corpus and **0.796** on a second, in vector mode.

Two constraints follow, both measured rather than assumed:

- **Calibration must be mode-aware, and the reason is specific.** With
  negatives split by how close they sit to the corpus:

  | mode | out-of-domain negatives | corpus-adjacent negatives |
  |---|---|---|
  | keyword | AUC 0.838 / 0.826 | **0.078 / 0.811** |
  | vector | 0.795 / 0.885 | 0.809 / 0.798 |

  Keyword confidence reliably distinguishes *unrelated* from *found*,
  and fails to distinguish *plausible but absent* from *found* — an
  adjacent query shares real vocabulary, so BM25's distribution looks
  like a successful retrieval. Since "plausible but absent" is the
  everyday case a user hits, keyword shape must not be the basis of an
  abstention policy. Dense confidence handles both.
- **Hybrid carries no shape signal** (AUC 0.427 and 0.469). RRF
  compresses scores into a narrow band, which destroys the distribution
  the signal reads. This is the third distinct defect traced to that
  compression, after priors overwhelming the fused signal and cross-space
  incomparability.

This assigns the embedding model a role the plan does not currently give
it: dense retrieval underperforms keyword on ranking quality (R@10 0.66
versus 0.92) but is the only reliable source of *confidence*. Any
abstention policy, cross-space calibration, or adaptive channel weight
should be built on the dense signal.

**A fixed abstention threshold generalises, at a useful but partial
rate.** With 240 generated corpus-neutral negatives per corpus, split by
subject so near-duplicate rows cannot straddle the boundary, and the
threshold chosen on the fit half and evaluated once on the held-out
half at 80% answer retention:

| | fit | held-out | held-out 95% CI | independent clusters |
|---|---:|---:|---|---:|
| Space A | 57% | **60%** | 51–68% | 15 |
| Space B | 64% | **66%** | 57–73% | 15 |

Held-out slightly exceeds fit on both corpora, which is what a
generalising threshold looks like.

Intervals are computed with the **cluster** as the sampling unit, not
the row: generated negatives come in groups of eight phrasings per
subject, and rows within a group share vocabulary, template, and author.
Any evaluation set built by templating must carry its group through to
the evaluator and report both row and cluster counts; a row-level
interval over clustered data is too narrow by roughly the square root of
the group size.

An earlier pilot at twelve negatives per split appeared to show a large
fit→held-out collapse (67→42, 92→50). That was sampling noise: the
held-out interval was 0.49 wide, and Fisher tests of the difference gave
p = 0.414 and p = 0.069. It is superseded by the figures above and
should not be cited.

Against a current baseline of **0%** — the system today answers every
query — silencing 60–66% of unanswerable ones at 80% answer retention is
a substantial improvement. It is not complete: roughly one unanswerable
query in three still receives a confident answer, so whether this clears
the shipping bar is a product judgement.

The signal itself is sound: AUC ≈ 0.80 reproduces, and the *ranking*
separates answerable from unanswerable. Converting that ranking into a
fixed decision boundary is the part that fails.

No abstention policy ships on a scalar threshold derived from this
sample. Revisit with an order of magnitude more negatives, per-corpus
calibration rather than one global constant, or a decision rule richer
than a single cut-off.

## Retrieval-quality gates

Latency budgets alone cannot tell a faster system from a better one. A
change that halves p95 while returning the wrong memories is a
regression, and until these gates existed the plan had no way to say so.

Quality is measured through `MemoryStore::recall` — not the index search
path — because decay, the access-count boost, the correction boost,
importance, temporal validity, and spreading activation all live in
`recall` and are invisible below it. `openmemory-eval`'s recall runner
is the owner. Evaluation always runs with `record_access = false`; a run
that records access mutates the ranking term it is measuring.

Every corpus backing a gate carries a completion receipt recording
source revision and dirty state, chunker policy, embedding model
identity and dimension, and verified counts. A run against a corpus
without a receipt fails rather than reporting numbers nobody can
reproduce.

Required reporting, per run:

- R@5, R@10, MRR, NDCG@5, NDCG@10, **split by category**, because one
  aggregate routinely hides a category moving the opposite way;
- generated and human-reviewed judgments rolled up **separately**, so
  mechanically mined judgments cannot decide a design question alone;
- abstention as a risk/coverage curve over score thresholds, not a
  single rate at an arbitrary floor;
- latency p50/p95/p99, with cold and warm query-embedding runs
  distinguished.

### Comparability precedes scoring

Two reports are a regression signal only if they describe the same
experiment. A baseline from another corpus, or the same queries with
easier answers, keeps its dataset and strategy strings and would
otherwise be compared as though nothing had changed.

Every report therefore carries an **evaluation contract**: metric
version, a digest of the complete query set (ids, text, category,
abstention flag, group, and graded judgments, length-framed so field
boundaries cannot shift), query count, corpus, mode, K, spreading flag,
`record_access`, and decay rate. The comparability gate runs **first**
and returns immediately on a mismatch, so an inadmissible pair yields a
named error rather than a number.

A consequence, and it is intended: a deliberate configuration change is
refused rather than scored. Comparing decay 0.01 against 0.02 is a design
question, not a regression measurement, and belongs in an experiment
report with cluster-aware intervals. The gate measures undeclared
changes — the system moving under a fixed configuration.

### Gates

| Gate | Threshold |
|---|---|
| Comparable experiment | baseline and current contracts match exactly; evaluated before any score |
| Category regression | no category's NDCG@10 falls more than 1 point below its recorded baseline |
| Aggregate regression | no aggregate metric falls below baseline |
| Judgment provenance | no judgment-source slice present in the baseline vanishes or regresses past the category limit |
| Abstention quality | the no-answer slice does not vanish, and its rate does not fall more than 5 points |
| Recall ceiling | the query set admits `R@10 = 1.0`; a lower ceiling must be set deliberately and is reported |
| Small-space contribution | in a multi-space read set, every space's own queries retain non-zero R@10 |
| Determinism | repeated runs on one corpus are bit-identical |
| Corpus validity | manifest/store agreement, key uniqueness, canonical-vs-revision consistency, SQLite integrity |

Two of these need their reasoning stated, because both replaced a gate
that read plausibly and checked nothing.

**Recall ceiling, not "judgment reachability".** More than K judged
targets does not make judgments unreachable: the documents are findable
and MRR and NDCG@K stay well-defined. What it makes impossible is raw
`R@K = 1.0`. The honest statement is a ceiling — the mean over answerable
queries of `min(|relevant|, K) / |relevant|` — measured at K=10 by the
runner and carried in the report, so the gate can check it without being
handed the query set again. The earlier formulation lived in a helper the
gate had no way to call and so never ran.

**Abstention is gated in a pair, never alone.** Abstention queries are
excluded from every quality rollup by construction, so their categories
report zero queries and the per-category gates skip them: a change that
destroyed no-answer behaviour moved nothing any gate looked at. Gating
the abstention rate alone is equally unsafe in the other direction —
silencing the system would ace it. The abstention gate and the aggregate
gate must both pass, and each blocks the other's evasion. Failure detail
quotes independent clusters, not rows: negatives are generated as many
surface forms of one subject, and the row count overstates the sample by
the family size.

Provenance is gated for what exists rather than for what should exist. A
slice present in the baseline must survive and must not regress; no gate
demands hand-reviewed judgments the corpora do not yet have, because a
gate that demands the unavailable is a gate somebody switches off. The
protection becomes live the moment those judgments land.

Provenance has three tiers, not two:

| tier | meaning |
|---|---|
| `generated` | mined mechanically from corpus structure |
| `model-adjudicated` | independently checked against written criteria by a model that did not generate the judgment, with the verdict recorded per query |
| `human-reviewed` | inspected and confirmed by a person |

The middle tier exists because mined judgments have a specific failure
mode — a document that genuinely answers the question but was never
linked by the miner is graded 0, and a false negative is indistinguishable
from a retrieval failure. That is not hypothetical: it was found in the
temporal doc-section chains, where a live documentation chunk is a correct
answer to "what does this section say now?" and scored zero.

It is a separate tier rather than a promotion because an adjudicator
shares the generator's blind spots: both read the same text with the same
priors, so a systematically wrong notion of relevance gets confirmed
rather than caught. The independence is procedural, not statistical.
Adjudicated judgments are a stronger prior and never ground truth, and no
design question that turns on the answer may be settled without the human
pass.

Query sets must not leak their own answers: a generated query containing
its target's filename stem or a unique symbol measures string matching
rather than retrieval and is rejected at generation time.

### The verdict is an artifact

A build log saying PASS records a conclusion with no argument. The gate
report carries its schema version, the thresholds in force, the dataset
and strategy, and both contracts, and is published by write-and-rename so
a reader never observes a partial verdict and a crash leaves the previous
one intact rather than a truncated file that parses as "no failures". It
is written before the failure check, because a FAIL is the verdict most
worth keeping.

### Proving the instrument still works

A gate that has silently stopped detecting regressions is worse than no
gate: it reports green and is believed. CI therefore runs the
instrument's own **sensitivity** and **specificity** on every push,
through the production `MemoryStore::recall` path against an in-memory
store — not the real corpora, which are personal data.

Sensitivity is demonstrated against a *system* regression inside an
admissible comparison: identical query set, mode, K, spreading flag, and
corpus, ingested by a system whose extractor stored a placeholder instead
of the body for part of the corpus. The test asserts the two contracts
are comparable before asserting the gate fires, so the failure is
evidence about quality rather than about the contract. The degradation is
partial by design; a total collapse would show only that the gate fires
on catastrophe.

Because that suite is feature-gated, a dropped feature would compile it
away and leave the job green over zero tests. The CI step therefore
asserts that tests actually ran, rather than only that none failed — the
guard has to hold whatever the cause of their absence, and "compiled out"
and "all passed" are otherwise the same exit code.

A gate that fires on a NaN is the same argument once more. Every quality
gate asks whether a drop exceeds a limit; a non-finite value on either
side makes that question answer "no" whatever happened, so a report of
NaNs would clear every gate. Metrics on both sides, and the thresholds
themselves, are checked for finiteness and reported as a vacuous
comparison rather than a pass.

## Performance budgets

Absolute budgets are enforced on documented Apple Silicon and Linux x86_64
reference runners; shared CI uses interleaved regression ratios.

| Path | Gate |
|---|---|
| Legacy one-space orchestration | <1 ms added warm p95; no catalog-size trend |
| Flattened 4×4 cold recall | no slower than existing nested p95 by >5%, with global bound |
| Two-space warm recall | ≤1.25× slower component p95 + 5 ms |
| Fusion 512→128 | <2 ms p95 |
| Warm context, 10k catalog rows | <2 ms p95, bounded indexed rows |
| Unsaturated executor queue/admission | <1 ms p95 overhead |
| Audited small write | ≤25% p95 and <2 ms absolute overhead vs canonical control |
| Proposal/approval bookkeeping | <2 ms p95 each, excluding embedding |
| Identity analysis at max evidence | <100 µs p95 |
| Plan 10k entities + 10k relations | <150 ms p95 |
| Plan 100k + 100k | <1.5 s p95; incremental RSS ≤1.5× canonical bytes and ≤256 MiB |
| Materialization | streaming; process incremental RSS ≤256 MiB at 100k fixture |
| Canonical import | ≥70% equivalent existing migration throughput |
| Final target admission pause | <250 ms p95, excluding moved-target abort |
| Execution/registry | all configured bounds hold; idle CPU effectively zero |

Disk gate uses measured predicted result, scratch, WAL, and retained backups.
Peak allocated bytes may not exceed preflight estimate plus 10%; no fixed
multiple of the old target substitutes for measuring a larger merged result.

If a budget misses, profile first. Prefer sorted streams, batched SQL,
precomputed hashes/embeddings, bounded scratch, and fewer copies. Add no cache
without complete invalidation and telemetry evidence.

## CI and release gates

Per slice, run owning tests plus relevant feature combinations. Final gate:

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

Also require production corpora, repeated concurrency/crash matrices,
disk-full/corruption suite, backup/restore/domain-migration suite, MCP/CLI/admin
process contracts, and recorded benchmark comparison. Miri/sanitizer evidence
is recorded when available for the pinned toolchain; absence is explicit, never
reported as a pass.

## Open defect: the decay parameter costs ~20 points of recall

Measured on 529 ranked queries against a real corpus with true event
times, hybrid mode, sweeping only `memory.decay_rate`:

| λ | R@10 | MRR | NDCG@10 | half-life |
|---:|---:|---:|---:|---:|
| 0 | **0.8846** | **0.9003** | **0.8606** | — |
| 0.001 | 0.8837 | 0.8353 | 0.8123 | 693 d |
| 0.0025 | 0.8783 | 0.7727 | 0.7635 | 277 d |
| 0.005 | 0.8065 | 0.7174 | 0.7043 | 139 d |
| **0.01 (shipped default)** | **0.6873** | 0.6755 | 0.6465 | **69 d** |
| 0.02 | 0.6471 | 0.6470 | 0.6168 | 35 d |

The shipped default costs **19.7 points of R@10** against no decay — a
22% relative loss. λ = 0.001 costs 0.1 point while still expressing a
recency preference, so the shipped value is roughly two orders of
magnitude more aggressive than the data supports.

**What this does and does not establish.** It measures decay's *cost*,
not its net value. This query set asks "find this specific document";
none of its categories has a recency-sensitive correct answer, so decay
can only lose here. A corpus with genuinely time-varying facts — where
the current value of something supersedes an older one — is required to
measure the benefit side, and that is what the temporal retrieval work
exists to build. The honest reading is that the shipped λ is unjustified,
not that λ should be zero.

Two candidate remedies, neither yet chosen:

1. recency as an additional ranked channel *inside* fusion, so it
   competes with lexical and dense evidence rather than scaling them;
2. a bounded post-fusion prior with λ set from measured retrieval value
   on a temporally sensitive query set.

Until one is chosen and measured, λ must not be changed on intuition,
and any recall change ships with the sweep above re-run.

## Open defect: multiplicative priors overwhelm fused scores

Recorded here because it gates any ranking change and is not yet fixed.

`compute_score` multiplies the search score by `exp(-λ · days)` with
λ = 0.01/day. Measured on a corpus with real event times, hybrid recall
of the oldest content (git commits, up to 84 days old) collapsed to
**R@10 = 0.069**, against 0.784 for the same queries in keyword mode.

The mechanism is a scale mismatch, not a tuning error. RRF compresses
fused scores into a narrow band by construction, so a multiplicative
prior of 0.43 dominates the entire fused signal; BM25 scores span
roughly 15–139, so the same multiplier leaves ordering largely intact.
Any prior applied *after* rank fusion has this property.

At λ = 0.01/day a one-year-old memory is multiplied by 0.026 and a
two-year-old memory by 0.0007. A memory system with that curve cannot
retrieve old memories at any relevance.

Two directions, neither yet chosen:

1. apply recency as an additional ranked channel *inside* fusion, so it
   competes with lexical and dense evidence instead of scaling them;
2. keep a post-fusion prior but bound its dynamic range and set λ from
   measured retrieval value rather than an assumed forgetting curve.

Either way, the decay parameter must be justified against the quality
gates above before any recall change ships.

## Release blockers

- Any semantic behavior implemented through a second pipeline instead of the
  general contract plus a proved specialization/fallback seam.
- Any hot-path reread of ambient policy/intent, silent fallback from invalid
  explicit intent, or lossy representation used for identity, authorization,
  hashing, persistence, or caching.
- Any cache with an incomplete shaping-input/generation key or any reader able
  to observe a partially constructed value.
- Any resource without one visible owner and terminal cleanup across success,
  error, cancellation, timeout, panic, disconnect, and shutdown.
- Any scattered platform condition or workaround without centralized
  capability data, a regression test, and a removal condition.
- Any unauthorized cross-space read/write/cache/traversal/backup/restore.
- Any automatic same identity from context evidence or agent output; any agent
  approval/promotion/destruction authority.
- Canonical mutation without its audit or audit without canonical mutation.
- Silent stale index, lost update, non-idempotent retry, partial same-domain
  changeset, or false cross-database atomicity.
- Unbounded worker, queue, coordinator, captured/result bytes, handle,
  candidate, payload, event, memory, disk, or retry behavior.
- Nested fan-out, facade-cache/access parity failure, maintenance starvation,
  stuck-worker hang, or incomplete shutdown.
- Partial snapshot generation vector or staged domain advertised as ready.
- Incomplete merge accounting, forged plan acceptance, dangling endpoint, or
  silent deletion.
- Promotion without current authority/confirmation, source immutability, target
  recheck, writer exclusion, same-filesystem/disk preflight, predicted hash,
  fsynced intent/parents, backup, and old-or-new recovery.
- Startup/backfill proportional to corpus size.
- Capability advertised before migration/backfill/repair/backup/platform and
  compatibility readiness.
- Any required gate or known watcher baseline failure.
- Any search-scoring, fusion, chunking, or ranking-prior change without a
  recorded retrieval-quality comparison against its baseline.
- Any cross-space composition in which a space in the authorized read set
  cannot contribute a result its own judged queries expect.
- Any evaluation run against a corpus lacking a verified completion
  receipt, or any evaluation that records access telemetry.
- A mutable label used as an identity key or as a physical routing input.
