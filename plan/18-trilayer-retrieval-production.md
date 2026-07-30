# Plan 18: Productionizing tri-layer retrieval, routing, and supersession

Status: draft for review, 2026-07-29. Companion to
`17-trilayer-memory-principles.md` (the design principles) and
`16-production-memory-spaces/` (spaces, identity, changesets,
executor). This plan turns the T15 v1-v6 experiment results into a
production delivery sequence. Every feature cites the finding that
justifies it; features without evidence are marked as gated
experiments, not commitments.

Baseline: the working tree after T15 v6 (valid_from/valid_until on
`openmemory_remember`, valid_at on `openmemory_recall`, already
implemented and tested, not yet committed).

## 1. Product statement

One retrieval surface that answers four kinds of questions well, on
one shared knowledge graph, without silent wrong answers:

1. Name-shaped lookups ("generate_pkce") through the concept/gloss
   layer.
2. Content questions ("how does X handle Y") through the text index.
3. Relational questions ("what does A use for B") through typed edge
   traversal.
4. Time questions ("what do we use now" / "what did we use in March")
   through supersession and validity, with corrections that never
   destroy history.

And one correction surface that revises the graph the way plan/17
specifies: supersession, never deletion; authority-ranked; reversible.

## 2. Evidence ledger (what is proven, what is not)

| Capability | Evidence | Standing |
|---|---|---|
| Gloss route resolves names (MRR 1.00 lexical) | T15 F-3 | proven on 1 corpus |
| Routing between routes has real headroom (+0.107 MRR oracle) | T15 F-5 | proven on 1 corpus |
| Calibrated router collects ~61% of that (MRR 0.754 vs 0.689) | T15 F-9 | in-sample rules |
| Append-only fill never scores below its primary | T15 F-9, F-13; 6 corpora | proven |
| Equal-priority fusion of a secondary signal is net-negative | T6, T15 F-4 | proven twice |
| Typed traversal fixes relational queries (0.75/0.60 vs 0.65/0.43) | T15 F-8 | proven on 1 corpus |
| Flat search is at ceiling below a few hundred docs | T15 F-12, 4 corpora | proven at small scale; bend point unmeasured |
| Correction-as-forget trades history for truth (1.00/0.38) | T15 F-15 | proven |
| Correction boost ranks tombstone above truth | T15 F-16 | proven |
| Graph obs and indexed text share one index namespace and leak | T15 F-17 | proven |
| Validity + pinning closes the correction trade (1.00/1.00) | T15 F-21 | proven, oracle instant |
| Unpinned validity zeroes history (correctness cliff) | T15 F-22 | proven |
| Successor promotion is the bounded no-intent fallback (1.00/0.50) | T15 F-19 | proven |
| Dense shape as confidence signal (AUC ~0.81 cross-corpus) | F8 | proven pre-T15 |
| Access-count boost harms defaults | T3 | accepted for removal |
| Query classifier quality decides router value | T15 F-10, F-13 | the open risk |

Not proven and therefore gated: the scale bend point, classifier
generalization to unseen query styles, transitive supersession
chains, behavior above ~10k documents, composition with multi-space
layered recall.

## 3. Architecture

### 3.1 One new read surface: `openmemory_retrieve`

A server-side orchestration tool. Existing `openmemory_recall` and
`openmemory_search` remain unchanged (stable-surface commitment);
`retrieve` composes them behind one call.

Input: `query`, optional `limit`, optional `as_of` (Unix seconds),
optional `intent` override (`lookup | content | relational | auto`),
optional `budget` knobs. Output: ranked file/entity results, each
carrying route provenance, confidence, and supersession annotations
("superseded_by: X" on any stale result that survives ranking), plus
a `retrieval_trace` (route chosen, why, fallbacks consulted).

Pipeline (each stage cites its evidence):

1. **Classify** the query: identifier-shaped (incl. PascalCase, the
   T15 F-10 miss), relational, temporal, else content. Deterministic
   features first; see 3.5 for the classifier contract.
2. **Route** to exactly one primary: gloss recall, index search, or
   typed traversal (F-5, F-8).
3. **Calibrate**: dense-shape confidence on the primary result set;
   below floor, consult the secondary and, if both floor out, return
   an explicit "low confidence / possibly no answer" envelope (F8;
   the abstention product gap).
4. **Fill, never fuse**: secondary routes append after the primary,
   deduplicated (F-9, F-13). No score mixing, no multiplicative
   boosts, ever (principles 4; four independent proofs).
5. **Supersession post-pass**: successor promotion by default;
   validity pinning when `as_of` is present (F-19, F-21, F-22).
6. **Assemble**: group results under their concepts with glosses and
   one-hop typed edges as framing (T15 F-3; T13's EvidenceBundle
   direction, unmeasured, kept minimal here).

**Scale gate**: stages 1-2 and 6 engage only when the store crosses
an engagement threshold (default: >= 500 indexed documents OR
homonym rate >= threshold, both computable from status counts).
Below it, `retrieve` is exactly index-hybrid plus the supersession
post-pass (F-12: four corpora show structure adds only risk at small
scale).

**The agent is the first classifier.** In MCP deployments the caller
is an LLM; the tool schema documents `as_of` and `intent` so the
calling model supplies temporal instants and intent directly. The
server-side classifier is the fallback for bare queries and non-agent
surfaces (CLI, daemon), not the primary mechanism. This converts F-22
(intent is a correctness requirement) from "build a classifier" into
"design the tool schema", with the classifier as defense in depth.

### 3.2 One new write surface: `openmemory_supersede`

Input: the superseded target (observation id, or entity + match
text), the superseding fact (entity + observation body or existing
observation id), optional `superseded_at` (defaults to now), reason,
actor. In one transaction (plan/16 invariant 6: one changeset, one
revision set, one semantic generation, one outbox batch):

1. Stamp `valid_until = superseded_at` on the old observation in
   place (no forget, no id churn; fixes the V6 caveat).
2. Ensure the new observation exists with `valid_from`.
3. Write the `supersedes` relation with provenance and authority.
4. Record the changeset revision (audit schema v3+ already holds the
   machinery).

Delete the 1.3x correction source boost from default scoring in the
same release (F-16: it ranks the tombstone above the truth; its job
is replaced by supersession rerouting). `source=correction` remains a
queryable provenance tag.

Authority: user-asserted supersessions are pinned (never decayed,
never auto-merged, never pruned); agent-inferred ones are proposals
under the plan/16 changeset review flow when a reviewer is
configured, immediate otherwise (principles 5).

### 3.3 Physical layer separation

Graph observations and URI-indexed text currently share one FTS/
vector namespace; correction markers measurably displaced content
results with zero content changes (F-17). Fix: partition the index
by document class (separate FTS tables and vector segments, or a
class column filtered in every backend query; decide by benchmark,
the contract is "no cross-class candidate displacement").
`openmemory_search` sees only URI-class rows; gloss recall sees only
observation-class rows. Migration: rebuild derived indexes from
canonical SQLite (they are declared rebuildable; storage.md).

Add validity to the index layer: indexed URIs get optional
`valid_until` metadata stamped by supersession (when the graph knows
`omem://old` is superseded, the index row is annotated, not deleted).
Search applies the same as-of semantics as recall. Until that lands,
successor promotion in `retrieve`'s post-pass covers the content
layer (F-18, F-19).

### 3.4 Ranking hygiene (deletions, not additions)

- Remove `access_count` from default scoring; keep async outcome
  telemetry for offline policies (T3; also shrinks plan/16 Phase 4
  cache-parity scope). Provide `ranking=audit` deterministic mode.
- `recall_decay_rate` stays 0 and is not re-tuned (F9/T7).
- No new multiplicative post-fusion terms of any kind. Recency,
  scope, channel, and correction preferences are expressed as
  filters, rank interleaves, append fills, or supersession
  rerouting. This is a review-blocking invariant.

### 3.5 The classifier contract

The router's value is bounded by classification quality (F-10: the
remaining oracle gap is misses; F-13: cue lists misfire across
domains). Contract:

- Deterministic feature rules for identifier shapes (snake_case,
  camelCase, PascalCase, `::`, `!`, path-likes, quoted strings).
- Relational and temporal intent: default to the agent-supplied
  `intent`/`as_of` parameters; server-side fallback is a small local
  model or rules, but ships only after beating the "always index"
  baseline on a held-out authored set it was never fit to. No LLM or
  network call on the retrieve path (plan/16 invariant; T8's local
  bounded-inference seam is the only permitted escape hatch).
- Unknown or low-confidence classification routes to index-hybrid
  (F-13: the router must never be worse than flat search; append
  fills make that structural, classification only adds upside).

### 3.6 Identity prerequisites (owned by plan/16, sequenced here)

The correction UX cannot ship on name-keyed identity: `supersede` by
entity name with two candidates is exactly T4a/T11 territory, and
`add_relation`'s type-defaulting foot-gun bit twice during T15.
Dependencies taken from plan/16 doc 06 and T11: immutable `EntityId`,
name/alias directory with bounded candidate sets, candidate-set
answers on every name-addressed tool. Phase T2 below consumes these;
if plan/16 Phase 4+ has not delivered them, T2 ships id-addressed
variants only (`observation_id`, `entity_id`) and defers name
addressing.

### 3.7 Composition with memory spaces

Per-space `retrieve` runs the full pipeline inside each authorized
space; cross-space composition stays deterministic rank interleaving
of per-space outputs (plan/16 provisional policy). Confidence values
are comparable across spaces only if the calibrator is shared and
versioned with the embedding model; until measured, confidence is
per-space advisory and never a cross-space sort key (F10 invariant:
no uncalibrated cross-space comparison).

## 4. Delivery phases

Each phase has an exit gate tied to the instrument (openmemory-eval
plus the T15 harness promoted into CI fixtures). No phase advances
with its gate red. Trust gates precede quality gates precede latency
gates (principles 10).

### T1: Ranking hygiene and layer separation (small)

Remove access-count from default scoring; remove the correction
boost; physical index-class separation; commit the v6 validity
plumbing. Exit gates: T15 grids reproduce with index arms unchanged
across two identical passes (determinism restored); the F-17 leak
test (write markers, assert zero candidate displacement) passes;
existing eval baselines do not regress beyond noise.

### T2: The supersession contract (medium)

`openmemory_supersede` (one transaction, in-place valid_until, edge,
revision); successor promotion inside recall/search post-processing;
index-layer validity annotation; transitive chain resolution (A->B->C
resolves to the newest valid successor; cycle-safe, depth-capped).
Exit gates: teamwiki grid on production surfaces scores current 1.00
/ history 1.00 (pinned) and 1.00 / >= 0.50 (unpinned); supersession
chain fixture passes; no marker observations exist anywhere in the
flow; history remains reachable after arbitrary correction sequences
(property test: supersede is information-preserving).

### T3: Calibration subsystem (medium)

Dense-shape confidence (margin, lift, dispersion) computed per
result set; per-mode calibrators, versioned with the embedding
model; confidence on every result envelope; low-confidence explicit
abstention envelope. Exit gates: cross-corpus AUC >= 0.75 on
negatives from a corpus the weights never saw (F8 floor was 0.79/
0.81); at 80% answer retention, >= 55% of held-out unanswerable
queries receive the abstention envelope (F8 measured 60-66%);
"always answers confidently from empty memory" product defect
demonstrably closed.

### T4: Router and planner inside `openmemory_retrieve` (large)

The 3.1 pipeline: classifier (3.5), route, calibrate, fill,
supersession post-pass, scale gate, retrieval trace. Typed traversal
planner productionized: seeds from gloss recall, edge walk with
type priorities, entity answers with representative-file projection,
neighborhood-scoped search, global fill; depth/fan-out caps and
deterministic order (memory-model executor discipline). Exit gates:
on a NEW held-out authored query set (different author or documented
authorship split) retrieve MRR beats index-hybrid by >= 0.03 with
zero per-category regression beyond noise; on the four small-scale
scenario corpora retrieve equals index-hybrid exactly (scale gate
proof); p95 <= 25 ms warm on canonical hardware (measured components:
router 12 ms, planner 15 ms); the trace explains every routing
decision.

### T5: Temporal intent end to end (medium, partly external)

Tool-schema-first: `as_of` documented so calling agents extract
instants (the agent is the classifier); server-side high-precision
temporal cue fallback that only ever loosens toward promotion, never
toward unpinned filtering (F-22 cliff). Exit gates: teamwiki-style
current/history sets pass with agent-supplied `as_of` through a real
MCP client; with no intent signal at all, history >= 0.50 (promotion
floor) and current = 1.00.

### T6: Scale-bend measurement (experiment, gates future tuning)

Grow one scenario corpus stepwise (10^2 to 10^4 docs with genuinely
overlapping content, mined from real wikis or monorepos under the
provenance rules) and measure where flat search leaves the ceiling
and routing headroom appears. Output: engagement-threshold defaults
with evidence instead of the current guess (500). Not a ship gate;
a tuning input.

### Explicitly out of scope for this plan

Consolidation/concept-birth automation (T12 evidence pending), the
markdown projection surface (T14), cross-space identity/merge
(plan/16 owns it), EvidenceBundle token budgeting beyond minimal
assembly (T13), any re-tuning of decay or fusion constants.

## 5. Testing and instrumentation

- Promote the T15 harness into `openmemory-eval` fixtures: the six
  corpora become versioned eval datasets (code corpus via pinned
  clones at CI time or a derived sanitized subset per the privacy
  rules; scenario corpora committed as-is since they are synthetic).
- Query-set discipline (principles 11): authored sets with declared
  authorship; judgment tiers (generated / model-adjudicated /
  human-reviewed) never compare equal; per-category gates, worst-
  query ceilings, no mean-only gates; contract digests so an
  inadmissible comparison errors instead of producing a number.
- New CI gates: determinism (two identical passes byte-equal),
  layer-leak (F-17), supersession property tests
  (information-preservation, chain resolution, inverted windows),
  router-never-below-primary (structural, per query, not aggregate),
  abstention wellformedness.
- Every capability ships with its negative-control test: the
  configuration in which it must do nothing (scale gate below
  threshold, promotion with no supersession edges, classifier on
  unclassifiable queries).

## 6. Performance and size budgets

- `retrieve` p95 <= 25 ms warm, single space, canonical hardware;
  cold model load stays off the request path (loaded at open).
- No outbound network on any retrieve/supersede path; no LLM calls
  server-side (plan/16 invariant 15 as reworded by T8).
- Index-class separation must not regress `openmemory_search` p95 by
  more than 20%.
- Binary stays under the 25 MB release threshold (current 15.99 MB;
  no new heavyweight deps are anticipated; a local classifier model,
  if T5's fallback ever needs one, ships as an optional download like
  embeddings, never in-binary).

## 7. Risks and open questions

1. **Classifier generalization** is the load-bearing unknown. The
   structural mitigation (append fill + default-to-index) caps the
   downside at "no better than today"; the held-out gate in T4 caps
   self-deception.
2. **Judgment provenance**: every T15 number is single-author. T4's
   gate requires an authorship split; a human-review pass on a
   sample is the honest next step before any external claim.
3. **Small-scale regression risk** is handled structurally (scale
   gate, negative-control tests), but the bend point is a guess
   until T6.
4. **Interaction with plan/16 Phase 4** (shared executor, flattened
   recall): retrieve's multi-route calls must go through the same
   bounded executor, not spawn their own fan-out. Sequencing: T4
   lands either behind Phase 4's executor or with a single-threaded
   route sequence (measured: sequential route calls fit the latency
   budget; parallelism is an optimization, not a requirement).
5. **Supersede on name-keyed identity** is deliberately deferred to
   id-addressed inputs if plan/16 identity work has not landed;
   shipping name-addressed supersede on the current schema would
   build correction UX on the T4a defect.

## 8. Definition of done

A user or agent can: ask any of the four question kinds through one
tool and get route-transparent, confidence-annotated, supersession-
aware answers; correct any fact without losing history; pin any past
instant and get the past truth; receive "I have nothing on that"
when memory is empty; and audit every one of those behaviors through
a deterministic ranking mode and a retrieval trace. All of it gated
by eval fixtures that fail CI when any of the above regresses.

## Appendix A: field-complaint validation (2026-07-30)

Thirty user-complaint themes about shipping memory systems, collected
from GitHub issues (mem0, Zep/Graphiti, Letta/MemGPT), Hacker News,
the OpenAI forum, and Product Hunt (research transcripts: two agent
reports, sources verified by direct fetch). Each mapped to this
architecture's answer. Status legend: **measured** (we have numbers),
**by-design** (structural, tested but not benchmarked), **partial**,
**roadmap** (designed, not landed, owner named), **out-of-scope**.

### Where the architecture already answers (OSS themes)

| Field complaint (system) | Our answer | Status |
|---|---|---|
| Contradictions accumulate, no recency signal; stale fact ranks top (mem0 #5867) | supersedes edges + validity + successor promotion; current/history 1.00/1.00 | **measured** |
| Correction deletes the old fact without adding the new; "love then hate = empty store" (mem0 #4536) | supersede never deletes; we measured forget's cost (history 1.00 to 0.38) and rejected it | **measured** |
| Superseded facts returned mixed and ranked equal with current (graphiti #1645) | successor promotion + `superseded_by` annotation + `as_of` pinning | **measured** |
| Extraction noise: 97.8% junk after 32 days (mem0 #4573) | no server-side extraction at all; ground truth lives in the index layer, distilled facts are caller-written; consolidation dedups; retention decay prunes | by-design |
| Over-extraction prompt cannot be tuned down (mem0 #5730) | there is no extraction prompt; read-time intelligence is the design (tri-layer routing) | by-design |
| Concurrent adds create permanent duplicates, drop links, corrupt HNSW (mem0 #6515) | single-writer SQLite transactions, WAL read pool, journaled engine with exactly-once replay; stress: no silent dupes; multi-process contention fails closed, never silently corrupts | by-design |
| Silent write loss with a normal response (mem0 #5245) | durable-ack watermarks; fail-closed index-repair marker; write errors surface | by-design |
| Search silently empty / episodes not persisted (mem0 #2672, graphiti #566) | read-your-writes durable ack; explicit failure over empty success | by-design |
| $0.80 per 40 chats; $1-3k/month extraction bills (graphiti #467, HN) | zero LLM and zero network calls on every path, invariant; local ONNX embeddings, 56x cached rebuild | by-design |
| 20 s per add; 1,000+ API calls per document (mem0 #2813, graphiti #290) | remember p50 107 ms embedding-inclusive; engine acks at 0.4 us, 32k obs/s | **measured** |
| No forgetting, unbounded growth (graphiti #864) | retention decay-prune, tiers, tombstones; ranking and retention knobs split by measurement | by-design |
| Broken relevance scores, unranked injection (mem0 #4999) | deterministic ranking, eval-gated in CI; calibrated confidence is roadmap T3 with cross-corpus AUC 0.81 evidence | partial |
| Compaction wipes history (letta #3270) | memory is an out-of-context store; append-only revisions + supersession; nothing wipes | by-design |
| Entity dedup silently fails, duplicates everywhere (graphiti #875) | our live defect is the inverse (silent merge via UNIQUE name constraint, T4a/T11); immutable EntityId + candidate directory prototyped, 23 checks green | **roadmap: plan/16** |
| Memory poisoning, cross-session leakage (letta #3388) | provenance on every row, user>agent authority, injected text inert server-side (stress-verified); spaces as authorization silos | partial; spaces **roadmap: plan/16** |

### Where the architecture already answers (consumer themes)

| Field complaint (product) | Our answer | Status |
|---|---|---|
| Stale constraints resurrected; "relates every K8s question to that one VM deploy" (Claude/ChatGPT, HN) | the decay/recency findings verbatim: recency priors measured net-negative and removed; within-fact recency via supersession instead | **measured** |
| Black-box opacity; "memory lowers the floor" (Packer) | every retrieve carries a routing trace; deterministic audit mode; local SQLite the user can open; markdown projection planned (T14) | by-design |
| Deletion doesn't delete (ChatGPT) | verified live: forget removes graph row and index copy immediately; forget_entity cascades | **verified** |
| Toggle doesn't mean off; shadow retrieval paths (ChatGPT) | one store, one surface, no shadow paths; local process the user controls | by-design |
| Hard capacity ceiling, delete-one-by-one UX (ChatGPT) | unbounded local store; consolidation instead of caps | by-design |
| Silent memory loss, no export (ChatGPT) | local files, raw export/import APIs, daemon backup/restore, crash-durable journal | by-design |
| Server-side profile; "data harvesting is the value prop" (HN) | single local binary, loopback-only daemon, no outbound network, invariant | by-design |
| Lock-in, no portability (HN) | user-owned SQLite + export; MCP standard surface across 9 clients | by-design |
| "Just a vector DB with better branding": no reconciliation, no decay policy, no read/write split (HN) | the tri-layer thesis point by point, each mechanism with a measurement behind it | **measured** |
| No agreed evaluation; can't show it beats grep (HN) | openmemory-eval with category gates in CI, authored query sets, contamination discipline, retractions ledger | by-design |
| Context pollution / worse-than-clean-slate (ChatGPT/Claude, HN) | scale gate keeps small stores on flat search; deterministic traces; but abstention/confidence not shipped, so the system still always answers | partial, **roadmap: T3** |
| Cross-context bleed between clients/projects (ChatGPT) | memory spaces with explicit bounded read sets, designed exactly for this | **roadmap: plan/16** |
| Injection persists as durable compromise (ChatGPT/Claude) | provenance surfaces origin; local-only limits exfiltration; poisoned content still reaches the reading agent as data — inherent to the category; spaces bound blast radius | partial |
| Unwanted intent inference from stored facts (ChatGPT/Claude) | server stores typed facts with provenance, synthesizes no profile; the reading model's inference behavior is out of server scope | out-of-scope |
| Second-brain product-quality tax (Mem, Rewind) | execution risk, not architecture; noted | out-of-scope |

### The honest gap list

1. Calibrated confidence and abstention (T3): the one partial that
   recurs on both lists (context pollution, broken scores, always
   answers). Highest-leverage unshipped piece; evidence in hand.
2. Immutable entity identity (plan/16): our silent-merge defect is
   the mirror image of Graphiti's silent-duplicate defect; neither
   camp has shipped conservative identity. The T11 prototype is the
   most complete evidence base either side has.
3. Memory spaces (plan/16): cross-context bleed is a top-3 consumer
   complaint and our design answers it precisely, unlanded.
4. Transitive supersession chains (T2): stress-confirmed one-hop
   limitation.
5. Contradiction *detection* remains explicit: we surface honest
   ties; mem0-style auto-resolution measurably destroys data, and we
   will not auto-resolve without the calibration precondition.
6. Multi-process bare-stdio contention fails closed but names no
   remedy; daemon-as-owner is the deployment answer and the error
   message should say so.
