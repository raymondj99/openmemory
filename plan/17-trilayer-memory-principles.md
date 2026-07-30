# Tri-layer memory: design principles

Status: brainstorm output, 2026-07-29. Derived from first principles and
then revised against the recorded evidence in `LOGBOOK.md`,
`experiments/memory-model/NOTEBOOK.md`, `TODO-CLAUDE.md`, and
`plan/16-production-memory-spaces/`. Every principle cites the finding
that forced or supports it. Principles marked **(untested)** are design
commitments awaiting evidence; the rest rest on measurements this
project has already made.

The one-sentence version: keep the three-layer instinct (graph,
embedding space, text index), but let the graph own identity, time, and
truth rather than ranking; let embeddings own confidence and
sense-disambiguation rather than primary retrieval; and make
supersession chains the single structure that serves both user
corrections and the within-fact recency that no global prior can
express.

## 1. Three layers are three kinds of knowledge, not three stores

The knowledge graph, the embedding space, and the text index hold
different kinds of knowledge with different dynamics, not copies of the
same content.

| Layer | Kind of knowledge | Cardinality | Change rate | Who edits it |
|---|---|---|---|---|
| Knowledge graph | Conceptual structure: what exists, what it is, how it relates | small | slow, high-stakes | user + agent, reviewable |
| Embedding space | Detail: dates, functions, observations, nuances | large | fast, low-stakes each | agent, automatic |
| Text index | Ground truth: files, docs, transcripts, addressed by URI | unbounded | external | nobody (it is the world) |

Library analogy: catalog, shelves, books. A correction to the catalog
never rewrites a book; shelving a new book never convenes a catalog
committee.

The graph points at indexed content via `references` edges to URIs. It
never copies file content into memory. (The corpus experiments showed
6x storage amplification per copied observation; pointing is free.)

A concept is a name the user would recognize and might want to correct.
If the user would never say "no, that is wrong" about it as a unit, it
is a detail, not a concept.

## 2. Identity is immutable; names are assertions

- Every concept carries an immutable internal id. Names, including the
  primary name, are provenance-bearing assertions resolved through a
  directory to a bounded candidate set. Evidence: T4a (two people
  silently merged into one row, unrecoverably; the fact that they were
  ever distinct was never recorded), T11 (the defect exists in the live
  store today: `ProjectAlpha` exists as both `concept` and `project`,
  and one of the two is unreachable by name).
- A rename is one domain-local audited mutation (alias assertion), never
  a delete-and-recreate and never a physical re-home. Evidence: T11
  (id routing moves 0 of 58 entities on rename; name routing would move
  46 of 58 at 4 domains).
- Shared labels are discovery context, never proof of identity. Identity
  is established only by lineage, source-verified identifiers in a
  configured namespace, or a human decision bound to both revisions.
  Evidence: memory-model identity findings; Java/Python/Wikipedia
  homonym corpora with zero wrong proof-gated decisions.
- Every name-addressed surface (get, forget, relate, correct) must have
  an answer for "the name matched three things." Returning a candidate
  set is not a drop-in for a tool that returns one entity; until the
  surfaces handle ambiguity, relaxing the schema changes nothing a user
  can observe. Evidence: T11's closing finding. **(unsolved)**

## 3. The graph owns identity, time, and truth. It never injects scores into ranking

The graph's retrieval-adjacent jobs:

- **Typed traversal as a planner step.** When the question is
  relational ("what depends on X", "who maintains Y"), walk edges
  deterministically and return structured answers. **(untested as a
  planner; tested and rejected as a fused channel)**
- **Assembly and framing.** Group retrieved details under their
  concepts; prepend concept glosses; show the neighborhood. This never
  touches ranking, so it inherits none of rank fusion's pathologies.
- **Edge coverage is the investment, not the algorithm.** Adding 135
  commit-to-file edges tripled recall on the failing category; no
  amount of diffusion tuning did anything. Record the relations users
  actually ask about. Evidence: T6 round 2.

What the graph must not do: act as a diffusion channel fused into
ranking. Evidence: T6 oracle result, the most thorough negative in the
logbook: on a 574-query mix, 2 queries had any headroom from graph
expansion; the best global weight gained +0.027 R@10 on 18 queries and
lost 0.696 on 454. Additionally, RRF structurally mutes a
below-primary-weight channel exactly when the primary has failed
(`displaces_from_rank`). The negative is scoped to that query mix and
to fused channels, not to graph retrieval generally.

## 4. No post-fusion multiplicative priors, ever

Four independent failures traced to one mechanism. RRF compresses
scores into a narrow band by construction; any multiplicative prior
applied after fusion overwhelms the fused signal.

- Recency decay: costs 17-20 R@10 points at the shipped lambda; loses
  even on the temporal corpus built for it to win (F9, T7).
- Access-count boost: 67% top-1 churn, no evidence of benefit, write
  lock on every read, popularity feedback loop (T3).
- Channel weighting: no global weight and no adaptive gate is
  net-positive (T6).
- Cross-space score comparison: an uncalibrated prior zeroed an entire
  space, 0.0% of fused positions across 839 queries (T5/F10).

Do not re-tune these knobs. The position is wrong, not the value.

Recency is a within-fact signal: versions of one fact chain compete
only with each other, newest-valid wins, and the rest of the corpus is
untouched. Supersession chains are the structure that makes this
expressible. Evidence: T7 (decay gains +0.07 to +0.08 MRR when a fact
competes only against its own history; the sign flips the moment the
surrounding corpus is added).

Time questions are answered by validity pinning, not decay:
`valid_at` is worth +0.11 MRR at zero decay and is flat in lambda while
the unpinned arm collapses 87%. Caveat: that number is an upper bound
measured by handing the probe the instant; extracting "as of last
March" from a natural question is unmeasured and unbuilt. **(gap)**

## 5. Corrections are supersessions, never edits

- A correction writes a new assertion with a `supersedes` link; the old
  assertion gets `valid_until` stamped. History survives; as-of queries
  work; a bad correction is itself revertible by the same mechanism.
- Authority ordering resolves conflicts: user > agent-verified >
  agent-inferred. A user correction pins the new fact and
  retrieval-masks contradicted ones. It deletes nothing.
- User-asserted structure is pinned: never decayed, never auto-merged,
  never pruned by consolidation. Automation may only revise inferred
  structure. This is the rule that makes malleability safe: automation
  and the user never fight over the same cells.
- Propagation is scoped and lazy. At correction time, search for
  details near the corrected assertion within the affected concept's
  anchors and flag the top contradiction candidates for review or
  cold-path adjudication. Full truth-maintenance is intractable;
  bounded best-effort is the contract. Note this depends on calibrated
  similarity (principle 7). **(untested)**
- Structural corrections are first-class, cheap, and reversible:
  rename (alias add), retype, merge (alias union plus lazy re-anchor),
  split (cold path, re-anchor by similarity to the new glosses), edge
  rewiring. Merge semantics follow the memory-model contract: explicit
  direction, immutable source, one disposition per source entity, never
  last-writer-wins, provenance on every rewired edge.

## 6. Details live in one embedding space, anchored to concepts by metadata

- One global vector+keyword index per space; every detail carries
  concept anchors as metadata. "Within a concept" is a query-time
  filter or grouping, never a physical partition and never a
  multiplicative score boost (principle 4 forbids the boost form).
- Reasons against physical partitioning: details belong to multiple
  concepts; per-concept indexes prevent cross-concept similarity
  discovery; merge and split become index rebuilds instead of metadata
  updates. Malleability requires membership to be cheap to revise.
- Match queries against concept names, aliases, and glosses, not
  member centroids: a centroid of diverse details is mush, and the
  abandoned EMA-centroid classifier (26% accuracy) showed embeddings
  cluster by topic, not by role or intent.

## 7. Embeddings are the confidence channel first, the retrieval channel second

On the measured corpora, keyword retrieval beats hybrid on nearly every
quality metric (0.92 vs 0.69 R@10) at a fraction of the cost, except
homonyms, where vector wins 6x (0.77 vs 0.125), the largest mode gap
measured anywhere in the project. Meanwhile the shape of a dense result
set (margin, lift, dispersion) separates answerable from unanswerable
queries at AUC ~0.81, stable across two unrelated corpora, while
keyword shape cannot tell "plausible but absent" from "found" and
hybrid shape is destroyed by RRF compression.

Roles for the embedding space, ordered by evidence strength:

1. Confidence and calibration (abstention, mode routing, cross-space
   comparability, correction targeting).
2. Homonym and sense disambiguation.
3. Concept formation in the cold path (topic clustering is what
   embeddings do well).
4. Semantic-gap fallback retrieval when keyword confidence is low.

Keyword search is the retrieval workhorse. Route per query with
calibrated confidence instead of fusing everything through a fixed
global alpha; a fixed alpha provably cannot tell a lexical query from a
semantic one (Finding 5).

Calibration is a precondition, not an enhancement. The system today
cannot tell a good result from a bad one, and every adaptive behavior
(abstention, fusion, routing, weighting) is downstream of that (F8,
F10, T6 convergence).

## 8. Lifecycle: conservative hot path, proposing cold path

- Hot path (in conversation): anchor new details to the best existing
  concept via alias lookup plus embedding similarity; on ambiguity,
  anchor to nothing rather than anchor wrongly.
- Cold path (consolidation): propose concept birth from dense clusters
  of unanchored details; detect drift (split candidates) and
  near-duplicate glosses (merge candidates). Proposals, never silent
  mutations. Agent output is proposal-only; prompt injection cannot
  mutate canonical state (memory-model identity findings).
- Retention decay prunes only inferred details. Concepts are never
  deleted by automation; they go dormant (excluded from seeding, still
  reachable by traversal).
- The ranking prior and the retention knob are separate configuration
  values and must never share one field. Evidence: F9's knob split;
  setting one to zero silently disabled the other's job.
- Extraction: the job is finding the ~7% of a transcript worth
  remembering, not summarizing the transcript (T12 corpus work:
  317 memorable observations from a 6 MB tree; role-specific length
  floors; run-wide dedup).

## 9. Consistency: one source of truth, one writer per derived state

- SQLite rows plus immutable revisions are semantic truth. The vector
  index, FTS, caches, mirrors, and stubs are derived and rebuildable.
  Anything that exists only in an index is a bug.
- Derived state has exactly one correct writer; a transformation that
  bypasses it is wrong. Evidence: three separate invalid experimental
  results from in-place SQL corpus transformations (backdating left
  317/317 observations disagreeing with their revisions; a
  "size-matched" arm searched its parent's entire corpus).
- Spaces are authorization silos; domains are storage shards inside a
  space and never an authorization scope. Cross-space identity and
  merge are reviewable semantic operations, never incidental
  consequences of recall (memory-model core conclusions).

## 10. Trust failures first, then quality, then latency

Every catastrophic failure found so far is a silent-wrong-answer
failure invisible to latency and crash gates: year-old memories
structurally unrecallable, confident answers from empty memory,
silently merged people, a whole space contributing zero results with
no signal. The malleable-graph design targets that column directly:
identity that cannot silently merge, corrections that cannot be lost,
abstention that can say "I have nothing," provenance on every edge.

## 11. Measurement discipline (how any of this gets validated)

- Author queries by reading; do not mine them from structure. Mined
  queries restate their own answers (50 of 50 sampled positives were
  verbatim), and nearly every number published before the authored
  sets describes a system answering questions that contain their own
  answers. The authored multi-hop frontier is R@10 0.40.
- Judge file-level questions at file granularity; exclude each query's
  own source document from the ranking (the commit-to-file
  retractions).
- Build corpora from pinned revisions by provenance, never the working
  tree; event time is commit time, never mtime (71% of one corpus read
  as under a week old from checkout mtimes).
- Declare every cap and every non-exhaustive judgment set beside the
  number it bounds. A mean is a budget that unrelated easy queries pay
  into; gate the worst query and the capped count, not the mean.
- Report paired per-category deltas with cluster resampling; the
  chain or subject is the sampling unit, not the row.
- Keep provenance tiers separate: generated, model-adjudicated, and
  human-reviewed judgments never compare equal. An adjudicator shares
  the generator's blind spots.
