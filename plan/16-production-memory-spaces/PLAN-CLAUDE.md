# PLAN-CLAUDE: Design Review and Memory-Intelligence Extension Proposal

Status: proposal for review. Nothing in this document is binding until it is
validated and folded into the numbered plan documents through the amendment
process in `INDEX.md` (stop, record, update the affected plan document, obtain
review).

Reviewed: 2026-07-27, against the full plan set (`INDEX.md`, `PROMPT.md`,
`00`–`10`), `IMPLEMENTATION_LOG.md`, and the production tree at the current
HEAD (`openmemory-index` hybrid/HNSW/BM25, `openmemory-graph/src/recall.rs`).

Implementation status at review time: Phases 0–2 complete; Phase 3 green but
withheld pending the controllable-VFS fault matrix and the audited-write
shadow benchmark; Phase 4 not started. Phase 4 freezes layered-recall and
fusion semantics, which makes several decisions below time-sensitive.

## 1. Verdict

The product goal is the most robust memory system for agents: fast,
efficient (SOTA retrieval), inspectable, and sandboxed. Scored against those
four goals:

| Goal | Coverage in current plan | Grade |
|---|---|---|
| Sandboxed | Physical isolation, overlay-first composition, staged directional merge, crash matrices | Excellent; possibly over-provisioned (teams) |
| Inspectable | Changesets, immutable revisions, history/diff, provenance everywhere | Excellent substrate; thin human UX |
| Fast | Shared executor, all-or-nothing admission, p95 budgets, facade-cache parity | Strong |
| Efficient (SOTA retrieval) | Per-space RRF hybrid exists in code; cross-space fusion is fixed prior 1.0 + score sort; no relevance evaluation anywhere | The gap; essentially unaddressed |

Summary: the plan is a world-class specification for the storage and control
plane of a memory system. It is close to best-in-class on inspectability and
sandboxing. It is silent on the memory-intelligence layer (retrieval quality,
consolidation semantics, context assembly, relevance evaluation), and that
layer is where "world's best memory system for agents" is actually decided.
Competitors (Letta/MemGPT, Zep/Graphiti, Mem0, HippoRAG-style systems) are
weak where this plan is strong and strong where it is silent.

## 2. Preserve without rethinking

These decisions are correct and differentiated. Do not reopen them.

- Physical isolation per space rather than logical row filtering. Eliminates
  the cross-tenant leakage bug class entirely.
- Overlay → cherry-pick → material merge as an escalation ladder, with source
  immutability.
- Conservative identity: labels, embeddings, timestamps, and agent output are
  never proof; human receipts establish identity. Auto-merging competitors
  eventually corrupt graphs; this is a durable differentiator.
- Canonical truth vs derived state; derived state is rebuilt, never merged.
- One shared executor with all-or-nothing admission; no nested fan-out.
- No LLM or network call on hot paths (see 3.3 for a wording fix).
- Capability readiness gating; crash matrices; staged fsynced promotion.

## 3. Thread 1: Retrieval layer

Grounding (verified in code): within-space hybrid already fuses vector and
keyword lists with RRF (`hybrid.rs`, k=60, alpha weighting, 3x over-fetch).
Graph recall then multiplies by Ebbinghaus decay, retrieval boost,
correction, importance, and confidence. Spreading activation is a 1-hop
fallback that fires only when direct hits are sparse. The open questions are
all in the layer above the per-space engine.

### 3.1 Cross-space fusion semantics (decide before Phase 4)

Problem: per-space scores are RRF-derived then boost-multiplied, so they are
rank-shaped but distorted, and corpus density skews them. A mediocre #1 hit
in a 10-observation personal space scores like a strong #1 in a 100k-record
team space.

| Option | Mechanism | Trusts | Risk |
|---|---|---|---|
| A: second-level RRF | Re-rank per-space lists; fuse by rank; layer prior = list weight | Relative order within each space | Discards absolute relevance; sparse spaces get inflated say |
| B: adjusted-score sort (current plan) | Compare boosted scores directly across spaces | Cross-space score comparability | Density/boost skew; sparse-space starvation or dominance |
| B+norm | Option B with per-space normalization (z-score or max-norm) over the candidate pool | Magnitude minus corpus offset | Normalization constants are another heuristic to version |

Position: instinct favors A for robustness, but this is exactly a question
the evaluation harness (Thread 2) must answer, not intuition.

Validation: judged multi-space fixtures where the correct answer alternates
between sparse and dense spaces; measure NDCG@10 under A, B, and B+norm.
Pure-function experiment in the `experiments/` pattern; roughly one day.

### 3.2 Graph diffusion as a first-class retrieval signal

Upgrade spreading activation from a sparse-results fallback to a standing
signal: a bounded, deterministic personalized-PageRank-style diffusion
(2 hops, bounded frontier, weight-decayed), seeded by direct hits, fed into
RRF as a third ranked list alongside vector and keyword. This is the
HippoRAG insight adapted to a store that already has typed, weighted,
provenance-bearing relations. Deterministic, no model, bounded work; fits
every existing invariant and enters through the existing hybrid seam.

Validation: multi-hop judgment sets (query names X; answer stored under Y
related to X); recall@10 delta; p95 check that bounded 2-hop diffusion stays
within budget (target <1 ms on representative graphs).

### 3.3 Invariant 15 wording fix

"No model call on recall critical paths" is already violated in spirit:
query embedding is local model inference on the recall path. Reword to:

> No LLM or network call is on recall, ordinary remember, approval, snapshot,
> or promotion critical paths. Local bounded inference (embeddings, optional
> rerankers) enters only through optional adapter seams with a deterministic
> fallback and forced-fallback equivalence tests.

This preserves the intent (no latency cliffs, no network dependency, no
agent authority) while legalizing 3.4.

### 3.4 Optional local reranker seam

A MiniLM-class cross-encoder (~25 MB ONNX) reranking the top 64 fused
candidates is the highest-leverage relevance upgrade available and is the
same dependency category as the existing optional ONNX embeddings.

- Latency class: 10–30 ms CPU; it must not sit on the default recall path.
  Expose as a per-call quality mode (`fast|thorough`); the context-assembly
  operation (5.3) is its natural consumer.
- Seam: identical shape to optional embeddings; eligibility = configured and
  ready; fallback = current ordering; forced-off equivalence tests per the
  existing specialization doctrine.

Validation: NDCG delta on the harness; exact fallback equivalence.

### 3.5 Learned layer priors

The plan already reserves a versioned layer prior (fixed 1.0). Feed it from
usage: which layer's results are actually consumed in assembled context.
Bounded, versioned, revisable per principle 14. Blocked on the feedback
signal from 5.3; defer until then.

### 3.6 Temporal retrieval semantics

Validity windows and supersedes relations exist; query semantics do not.
Add: as-of queries, current-truth boost over superseded facts, and
"when did this change" answers. Mostly scoring policy plus a filter; no
schema. Validation: superseded-fact-chain fixtures; superseded facts rank
below current unless as-of is requested.

### 3.7 Model-free query expansion

Entity-link the query against the store's own alias/identifier tables
(being built in graph v4) before search; expand acronyms/aliases
deterministically. Uniquely available to a KG-backed system. Validation:
alias/acronym query categories on the harness.

## 4. Thread 2: Relevance evaluation harness

The keystone. Every idea in Thread 1 becomes an experiment instead of a
debate. The plan currently gates latency, RSS, disk, and crash recovery
exhaustively but has no retrieval-relevance gate anywhere; SOTA retrieval
cannot be claimed without an instrument that measures it.

### 4.1 Corpus classes

1. Synthetic-deterministic: generated KGs with planted relevant facts,
   distractors, multi-hop chains, cross-space homonyms, superseded facts.
   Seeded, versioned, CI-runnable.
2. Sanitized real fixtures with judgments: the permanent merge corpora
   (Codex/Homebrew, Axum/Actix, Mathlib/Lean) gain relevance judgment files
   (question, relevant observation IDs, category).
3. Public offline benches: LongMemEval, LoCoMo, plus the existing CodingMem
   suite. Release-gated, not per-commit; LLM-judged benches stay out of CI.

### 4.2 Metrics and categories

Recall@k, NDCG@10, MRR per category: direct, multi-hop, temporal,
cross-space, abstention. Abstention is a scored category, not an
afterthought: "no relevant memory exists → nothing above threshold" matters
because confident garbage poisons agent contexts.

### 4.3 Gates

Mirror the performance-budget table: for example, planted-fact recall@10
≥ 0.95; multi-hop recall@10 ≥ recorded baseline; no NDCG regression > 1
point vs the recorded baseline; deterministic seeds. Add a release blocker:
any search-scoring change without a recorded harness comparison.

### 4.4 Timing

No dependency on Phase 4; must land before it so fusion semantics (3.1) are
decided with evidence. Does not touch production correctness gates, so it
can proceed while the Phase 3 VFS matrix work completes.

## 5. Thread 3: Consolidation and salience (the librarian)

Central insight: the Phase 3 changeset machinery is the ideal substrate for
memory cognition, and nothing in the current plan exploits it. Consolidation
outputs should be changesets.

### 5.1 Consolidation as a proposal-producing pipeline

Extend cold-path consolidation (currently dedup + decay-prune) with
LLM-powered passes that emit `SubmitMode::Propose` changesets:

- episodic→semantic summarization: cluster related observations; propose a
  higher-tier synthesis observation with provenance links to originals;
- contradiction detection: propose supersedes relations and validity-window
  closes;
- staleness review queues.

Low-risk classes (exact dedup) may auto-apply under configured policy;
everything else lands in the existing review queue. LLMs remain strictly on
the cold path (consistent with invariant 15 as reworded in 3.3), every
consolidation act is audited and revertible, and the approach matches the
recorded Cortex lesson that LLM-only extraction decisively beat heuristics.
Home: the v4 `maintenance_tasks` table with its resumable cursors.

### 5.2 Tier semantics specification

Tiers and `promote_observation` exist without a written policy for what
tiers mean at recall (working-set boost, faster episodic decay,
semantic-tier stability) or what drives promotion (retrieval usage plus
consolidation). One page of versioned policy, evaluated on the harness.

### 5.3 Context assembly operation

`openmemory_compose_context(query, token_budget, mode)` → ordered, deduped,
provenance-cited context block plus the memory IDs used. This is:

- the surface where "efficient" cashes out for agents, measured as
  relevant-tokens per total-tokens;
- the consumer for the `thorough`/reranker mode (3.4);
- the feedback signal source for learned layer priors (3.5);
- the natural site for read-time entity unification via identity receipts
  (6.3).

Validation: end-to-end answer accuracy on LongMemEval with composed context
vs raw top-k at equal token budgets.

### 5.4 Forgetting is retirement

Decay-prune retires (audited, reversible, absent from default recall); it
never destroys. The lifecycle model already supports this; make it the
specced behavior.

### 5.5 Deliverable

A new plan document `11-memory-intelligence.md` with the same rigor as the
existing set: tier policy, consolidation passes, assembly contract, eval
gates. Runs as daemon maintenance plus admin surfaces; parallelizes with
Phases 4–5 rather than blocking them.

## 6. Thread 4: Scope and inspectability

### 6.1 Team apparatus: defer activation, keep schema

The capability ladder already ships features dark until proven. Proposal:
v1 ships with `team_spaces` off and Phase 6's team-review surfaces deferred;
the Phase 2 schema stays. Critical nuance: merge must not go down with
teams. Combining project KGs is a single-user need; single-user merge
requires only Maintainer-of-own-spaces, which is trivial locally. The trim
is therefore: keep identity and merge for personal spaces; defer roles,
review UI, and membership management. This is a product-contract amendment
(PROMPT.md deliverable 2) and requires explicit review sign-off.

### 6.2 Disjoint-merge fast path

Merging two KGs with zero identity candidates currently pays the full
ceremony. Add a specialization per existing doctrine: conservative
eligibility fact = discovery completed untruncated with zero candidates;
the plan is pure AddDistinct; one confirmation step. Equivalence-tested
against the general path. Makes the common personal case pleasant without
touching the conservative core.

### 6.3 Decouple identity receipts from merge jobs

`identity_candidates` are currently job-bound. Making receipts durable
space-pair facts (nullable job binding, pair-scoped uniqueness; a small
product schema step) enables:

- overlay-time presentation unification: one card, two origins, zero
  mutation; the missing middle step between "overlay shows both" and
  "material merge";
- pre-resolved future merges.

Cheap now; painful to retrofit after Phase 6 builds on job-bound receipts.

### 6.4 Editable text projection

Deterministic export of a space to a Markdown tree: one file per entity with
front-matter (logical ID, revision hash, tier, validity), observations as
annotated list items, a relations section. Re-import parses the tree, diffs
against the embedded revision hashes, and emits changeset drafts where the
embedded hash is the expected head; stale edits conflict exactly per
existing semantics; new files become remembers. Doctrine-clean (a derived
projection), git-diffable, greppable. This substitutes for a desktop app for
a long time and no competitor has it. Deliverable: plan document
`12-projection.md`. Validation: export→import round-trip is a no-op
(property test); a single edit produces exactly one changeset operation.

### 6.5 Space packs

A published snapshot is already an immutable, hashed, manifest-bound
artifact; restore-as-new-space exists; imports never carry authority.
Naming this a product feature (export a space as a shareable knowledge
pack; import as a read-only overlay) turns sandboxing into a distribution
story at near-zero engineering cost.

### 6.6 Read-set cap of 4 is policy, not architecture

Arbitrary overlays (several project packs at once) hit the cap immediately.
If Phase 4's flattened executor holds its global bounds, the 4 should be a
default policy value. Action: confirm nothing structural (fusion arrays,
precedence encoding) hard-codes it.

### 6.7 Sync-shaped door (no work now)

Hosted sync remains a non-goal. The changeset log with globally unique IDs
and deterministic apply is most of a shippable replication log; avoid
decisions that preclude multi-device later. Current design already
complies; this is a standing constraint, not a task.

## 7. Sequencing

1. Now, before Phase 4 (frozen otherwise):
   - evaluation harness skeleton plus the 3.1 fusion experiment;
   - invariant 15 rewording (3.3);
   - identity receipt/job decoupling decision (6.3);
   - read-set cap as policy confirmation (6.6).
2. Phase 4 additions: graph diffusion as a third RRF list (3.2); reranker
   seam defined but dark (3.4).
3. Parallel plan documents: `11-memory-intelligence.md` (Thread 3);
   `12-projection.md` (6.4).
4. Scope amendment: teams dark in v1; merge stays; disjoint fast path (6.1,
   6.2).

Highest-conviction first step: build the harness and run the 3.1 fusion
experiment. It is cheap, it unblocks Phase 4's largest open semantic
decision with evidence, and it establishes the instrument every other
retrieval idea requires.

## 8. Decisions requiring explicit sign-off

| # | Decision | Default recommendation |
|---|---|---|
| D1 | Cross-space fusion rule (A / B / B+norm) | Decide from harness evidence; instinct: A |
| D2 | Reword invariant 15 to "no LLM/network" | Yes |
| D3 | Reranker as optional `thorough` mode | Yes, dark until harness proves it |
| D4 | Identity receipts decoupled from merge jobs | Yes, schema step before Phase 6 |
| D5 | Teams dark in v1; personal merge retained | Yes; requires PROMPT.md amendment review |
| D6 | Disjoint-merge fast path | Yes, behind conservative eligibility |
| D7 | Read-set cap becomes default policy | Yes, pending 6.6 confirmation |
| D8 | New plan docs 11 (memory intelligence) and 12 (projection) | Yes |
| D9 | Relevance gates added to release blockers | Yes |

Per `INDEX.md`, none of these take effect until the affected numbered plan
documents are updated and reviewed. This document records the proposal and
its rationale; it is not itself authority.
