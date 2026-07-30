# T15 results — tri-layer retrieval on codex + clap + anyhow

Date: 2026-07-29. Store: 517 files, 1,979 chunks, 534 entities, 618
relations, 2,513 vectors; one shared graph, one profile. Ingest wall
time 949.7 s (0.54 files/s, embedding-bound; `index_text` p50
467 ms/chunk). Model: nomic-embed-text-v1.5. `recall_decay_rate = 0`,
normalization disabled (see "identity notes" below). Query set: 55
authored queries (45 answerable, 10 abstention), file-granularity
judgments, single-author, unadjudicated: absolute levels are
provisional; paired deltas between arms are the result.

## Aggregate (45 answerable queries)

| arm | R@5 | R@10 | MRR | p50 | p95 |
|---|---|---|---|---|---|
| index-keyword | 0.689 | 0.804 | 0.529 | 18.5 ms | 30.6 ms |
| index-vector | 0.737 | 0.793 | 0.618 | 1.7 ms | 1.9 ms |
| index-hybrid | 0.793 | 0.859 | **0.689** | 4.7 ms | 7.4 ms |
| graph-keyword | 0.681 | 0.741 | 0.577 | 3.2 ms | 5.9 ms |
| graph-vector | 0.752 | 0.819 | 0.562 | 2.3 ms | 2.7 ms |
| graph-hybrid | **0.815** | 0.837 | 0.684 | 5.4 ms | 9.0 ms |
| tri-keyword | 0.700 | 0.796 | 0.482 | 8.3 ms | 13.9 ms |
| tri-vector | 0.774 | 0.822 | 0.586 | 4.5 ms | 5.2 ms |
| tri-hybrid | 0.789 | **0.867** | 0.624 | 13.4 ms | 20.4 ms |

Latencies are warm (model loads at server start; query-embedding
cache shared across arms, so later arms hit cached query vectors).

## Per category (R@10 / MRR)

| arm | direct-lexical (12) | semantic (12) | homonym (11) | relational (10) |
|---|---|---|---|---|
| index-keyword | 1.00 / 0.89 | 0.74 / 0.53 | 0.85 / 0.35 | 0.60 / 0.29 |
| index-vector | 0.83 / 0.68 | 0.79 / 0.63 | 0.88 / 0.71 | 0.65 / 0.43 |
| index-hybrid | 1.00 / 0.88 | 0.85 / 0.68 | 0.91 / 0.73 | 0.65 / 0.43 |
| graph-keyword | 1.00 / 0.90 | 0.60 / 0.49 | 0.79 / 0.47 | 0.55 / 0.41 |
| graph-vector | 1.00 / 0.76 | 0.61 / 0.39 | 0.91 / 0.60 | 0.75 / 0.48 |
| graph-hybrid | 1.00 / **1.00** | 0.76 / 0.56 | 0.91 / 0.68 | 0.65 / 0.46 |
| tri-hybrid | 1.00 / 0.76 | **0.88** / 0.60 | 0.91 / 0.68 | 0.65 / 0.42 |

## Findings

**F-T15-1: The homonym mode gap replicates on a shared graph.**
Keyword MRR halves on homonym queries (0.35) while vector holds
(0.71): every sense of "error"/"README"/"lib.rs" matches lexically and
BM25 has nothing left to separate repos. Same direction as the A2
finding (0.125 vs 0.767 R@10 there); here the gap is MRR-shaped
because filename-bearing chunks keep the right file inside the top 10.

**F-T15-2: Hybrid beats keyword on authored queries.** Aggregate MRR
0.689 (hybrid) vs 0.529 (keyword) on the index route. This is the
opposite of the mined-corpora baselines (keyword 0.85 vs hybrid 0.68
MRR) and consistent with the A2 lesson: mined queries restate their
answers, authored ones do not. Query construction, not engine change,
flips the mode ranking.

**F-T15-3: The graph gloss route is the best identifier resolver and
the best precision arm.** graph-hybrid: perfect MRR 1.00 on
direct-lexical, best overall R@5 (0.815), from one gloss observation
per file (doc comments + pub item names, title-weighted). The graph
layer earns its keep on name-shaped queries without touching file
content. Its weakness is symmetric: semantic content questions (0.76 /
0.56) where the gloss compressed away the answer.

**F-T15-4: Ungated scope fusion taxes healthy queries more than it
helps failing ones (T6's lesson, new mechanism).** Concept
localization chose a correct neighborhood for 34/45 queries, wrong for
10, unscoped for 1. Each wrong scope cost ~0.5 MRR (right answer
pushed from rank 1 to rank 2 by an interleaved wrong-scope list); the
one win (+0.67, textwrap) does not pay for nine losses. tri-hybrid
still posts the best R@10 (0.867, +0.008 over flat) via semantic
gains (0.88 vs 0.85), but MRR drops 0.065. A secondary signal fused at
equal priority hurts exactly when the primary was already right.
Wrong-scope examples worth keeping: `generate_pkce` localized to
`clap`; anyhow's README scoped to `anyhow/src`, which excludes the
target (deepest-agreed-prefix overreach).

**F-T15-5: Routing headroom is real here, unlike T6.** Oracle
per-query choice between index-hybrid and graph-hybrid reaches MRR
0.796 / R@10 0.881 vs 0.689 / 0.859 for the best single arm (+0.107
MRR); all-three oracle reaches 0.807 / 0.889. T6's oracle found 2/574
queries with headroom because its channels were redundant; these two
routes are complementary (glosses win names, content wins semantics),
so a calibrated router has ~0.11 MRR to collect. This is the
strongest evidence yet, on this project's own data, for the
principles-doc claim that calibration/routing is the precondition
capability.

**F-T15-6: Relational queries are the frontier again.** Best
relational cell: graph-vector 0.75 / 0.48; most arms sit at 0.55-0.65
R@10, MRR 0.27-0.49. Scope fusion does not fix it (0.65 / 0.42). These
questions ("what does codex parse CLI args with") want typed traversal
of the `depends_on`/`references` edges that are already in the graph,
not better ranking. Matches the authored multi-hop 0.40 frontier and
the T6 conclusion that edge coverage, not diffusion, is where graph
value lives. A planner arm is the natural T15 follow-up.

**F-T15-7: Score shapes point the same way as F8.** Index-route top-1
and margin means (answerable vs abstention): vector margin 0.019 vs
0.008 (separates, direction consistent with the AUC 0.81 finding);
hybrid margin inverted (0.032 vs 0.077, RRF compression destroys the
shape); keyword unbounded scale (31.2 vs 23.0 top-1). No arm abstains:
all ten no-answer queries received ten confident results each.

## Identity notes (graph-layer stress observations)

- Fuzzy normalization (`auto_merge_threshold = 0.95`) had to be
  disabled: path-named entities like `.../tools/mod.rs` and
  `.../state/mod.rs` are lexically near-identical, and auto-merge
  would have silently coalesced distinct files: the T4a defect
  reachable from config defaults on any path-shaped corpus.
- Entity names carry a repo prefix purely because of the UNIQUE
  `(name, entity_type)` constraint; three `README.md` entities would
  otherwise merge. Under the plan/17 identity contract (immutable ids,
  names as assertions) the prefix would be unnecessary.
- `add_relation` defaults both endpoints to `concept`; relating two
  `project` entities fails unless the caller states types explicitly.
  A candidate-set lookup (T11) would remove this foot-gun.

## Caveats

- Single-author judgments, no adjudication pass, no human-review tier;
  multi-target queries judged with every acceptable file, but
  `judgments_complete` should be treated as false throughout.
- 45 answerable queries across 4 categories: direction-grade evidence,
  not magnitude-grade. No cluster bootstrap was run.
- Chunk cap 6/file and per-crate file caps (declared in README) bound
  what is recallable; semantic targets were authored inside the caps.
- Each chunk is prefixed with its relpath line, which aids
  filename-lexical matching in all index arms equally.
- Latency is warm-path; cold model load (~1.6 s process start) and the
  cross-arm query-embedding cache are declared above.

# V2 — calibrated router and typed traversal planner (same day)

Three new arms, all twelve re-run in one pass (`scripts/eval_v2.py`,
`results/eval2.json`). Router rules were fixed a priori in the script
header and not tuned against the judgments.

## Aggregate (45 answerable queries)

| arm | R@5 | R@10 | MRR | p50 | routing |
|---|---|---|---|---|---|
| index-hybrid (baseline) | 0.793 | 0.859 | 0.689 | 4.2 ms | |
| graph-hybrid | **0.815** | 0.837 | 0.693 | 4.9 ms | |
| tri-hybrid (v1 scoping) | 0.789 | 0.867 | 0.624 | 13.4 ms | |
| plan-hybrid (typed traversal) | 0.811 | **0.889** | 0.714 | 15.2 ms | |
| router-hybrid (2-way) | 0.793 | 0.859 | 0.704 | 12.0 ms | graph 4 / index 51 |
| router-full (3-way) | **0.815** | 0.881 | **0.754** | 11.6 ms | plan 8 / graph 4 / index 43 |

Oracle ceilings on this run: index+graph 0.804 MRR; index+graph+plan
0.811 MRR / 0.911 R@10.

## Findings

**F-T15-8: The typed traversal planner fixes what ranking could not.**
plan-hybrid posts the best relational cell measured in this project:
R@10 0.75 / MRR 0.60 vs 0.65 / 0.43 for flat hybrid (+0.17 MRR), plus
perfect homonym R@10 (1.00) and perfect direct-lexical MRR (1.00). The
mechanism is the one the principles doc predicted: walk
`references`/`depends_on`/`part_of` edges from recall seeds, map
entity answers to their defining files, scope content search to the
neighborhood. Deterministic, no score fusion, ~15 ms.

**F-T15-9: The calibrated router collects most of the routing
headroom.** router-full reaches MRR 0.754 (+0.065 over the best single
arm), 61% of the two-route oracle gap, with sane dispatch (relational
cues to the planner 7/10, homonyms and semantics to the index,
identifiers plus dense-margin override to the graph). Secondary-route
fill (append, never interleave) means the router never scores below
its primary; contrast tri-hybrid's -0.065 MRR from equal-priority
interleaving. Routing beats fusing, measured twice in one experiment.

**F-T15-10: Remaining gap is classifier misses, not retrieval.** Eight
queries account for the distance to the 0.811 three-route oracle; the
largest single miss is `ValueHint` routed to index because the a
priori identifier regex only matches camelCase with a lowercase head,
not PascalCase. Left unfixed rather than patched post hoc; a fitted
or learned router (or an LLM query classifier) is the obvious next
increment, and needs a held-out set before its numbers count.

**F-T15-11: T3's irreproducibility observed live.** Between the v1 and
v2 passes, every index arm reproduced to the fourth decimal and every
graph arm moved (graph-vector MRR 0.562 to 0.637) because v1's recall
calls incremented `access_count`, which feeds the retrieval boost.
Same store, same queries, different ranking. The v2 numbers are the
citable ones (single consistent pass); the defect is the point:
default ranking should not mutate under measurement, exactly as T3
concluded.

## V2 caveats

- Router rules are a priori but were authored by the same person who
  authored the queries; treat router gains as in-sample until run
  against a held-out query set.
- The planner's representative-file convention (an entity answer is
  proxied by its `src/lib.rs` or `README.md`) matches how the
  relational qrels were authored; the two were written independently
  but by one author. An adjudication pass would firm this up.
- Access-count drift means arms within one pass share state; arm
  order is fixed in the script, so within-pass comparisons are stable.

# V3 — non-code scenarios: researcher folders and student classes

Two fictional but structurally grounded corpora (generator:
`scripts/make_scenarios.py`), each in its own isolated store:

- **researcher**: three related project folders modeled on the
  Stanford NLP group's real project mix (a Stanza-like multilingual
  pipeline, a DSPy-like LLM-programming framework, a HELM-like eval
  harness); 18 documents, homonym filenames (proposal.md,
  meeting-notes.md, results.md in every folder), real cross-project
  references and `uses` edges.
- **student**: three closely related classes modeled on CS229 / CS230
  / CS224N with real syllabus topics; 21 documents, three
  syllabus.md homonyms, `prerequisite_of`/`related_to` edges,
  cross-class references (softmax derivation, attention lineage).

24 authored queries each (20 answerable, 4 abstention), same
categories, same arms, router rules deliberately unchanged from V2.

## Aggregate (20 answerable queries per scenario)

| arm | researcher R@10 / MRR | student R@10 / MRR |
|---|---|---|
| index-keyword | 1.000 / 0.879 | 1.000 / 0.885 |
| index-hybrid | 1.000 / 0.860 | 0.975 / **0.933** |
| graph-hybrid | 0.825 / 0.716 | 0.975 / 0.912 |
| plan-hybrid | 0.825 / 0.665 | 0.975 / 0.912 |
| router-hybrid | 1.000 / 0.860 | 0.975 / 0.933 |
| router-full | 0.950 / 0.802 | 0.975 / 0.908 |

## Findings

**F-T15-12: At personal-corpus scale, flat search is at the ceiling
and structure has no ranking headroom.** With ~20 distinctive
documents, index-hybrid hits R@10 1.00 / MRR 0.86-0.93 and nothing
beats it. The graph layers cannot add ranking value where there is no
ambiguity left to resolve; the researcher gloss route is actively
worse on semantic queries (0.58 R@10) because a meeting-note gloss
compresses away the content being asked about. The tri-layer's
ranking payoff is a function of corpus size and ambiguity: +0.065 MRR
at 517 files of overlapping code, ~0 at 20 personal documents.

**F-T15-13: "Never worse than the primary" held; the damage came
from cue-based dispatch.** router-hybrid (shape rules + dense
override + append-only fill) matched index-hybrid exactly on both
scenarios: the calibration override correctly kept every query on the
index route. router-full lost ground only where the a priori word-cue
list forced the planner as primary: "the deep learning class
syllabus" matched the "class " cue and the planner put the wrong
class's syllabus at rank 1 (-0.5 MRR); a "what ..." phrasing sent a
lookup query to traversal (-1.0 MRR on rel-05). Meanwhile the same
planner *improved* student relational MRR to 1.00. Traversal is fine;
keyword cues are not a query classifier. Dispatch must be calibrated
or learned, and must default to the flat route when unsure.

**F-T15-14: Graph value at this scale is navigational, not
rank-based.** The researcher graph route posted perfect homonym MRR
(1.00 vs 0.90 index) by resolving "the multiparse proposal" through
title-weighted glosses, and the edges still answer provenance
questions ranking never sees ("promptlib uses evalbench"). The
correction/identity/assembly roles of the graph are unaffected by
this scenario's ranking ceiling; only the retrieval-ranking role is
scale-gated.

## Scenario caveats

Corpora are author-written (structure grounded in real Stanford
examples, text synthetic), and small by construction: 20 answerable
queries per scenario is direction-grade only. The shared-author
caveat from V2 applies doubly: corpus, queries, and judgments have
one author. The scale claim (F-T15-12) should be tested by growing a
scenario corpus past a few hundred documents with genuinely
overlapping content.

# V4 — three more scenarios: novelist, freelancer, and corrections

Corpora: `scripts/make_scenarios2.py`. novelist (three-book series
sharing characters and worldbuilding, 13 docs), freelancer (three
clients with identical doc types per folder, 14 docs), teamwiki (11
docs, four decisions superseded by later decisions, evaluated before
and after applying corrections through today's MCP surfaces:
forget the stale gloss, remember an OUTDATED marker with
source=correction, add a `supersedes` relation).

## novelist / freelancer aggregates (16-17 answerable queries each)

| arm | novelist R@10 / MRR | freelancer R@10 / MRR |
|---|---|---|
| index-hybrid | 1.000 / 0.908 | 0.980 / 0.961 |
| graph-hybrid | 1.000 / 0.887 | 1.000 / 0.931 |
| plan-hybrid | 1.000 / 0.846 | 0.980 / 0.926 |
| router-hybrid | 1.000 / 0.908 | 0.980 / 0.961 |
| router-full | 1.000 / 0.856 | 0.980 / 0.971 |

F-T15-12 (flat search at ceiling at personal scale) and F-T15-13
(router-hybrid never worse than its primary; only cue-forced planner
dispatch loses) both replicate on both corpora. Even the
maximal-homonym freelancer corpus resolves lexically because client
names appear in queries and filenames. Nothing new; the scale claim
now stands on four small corpora.

## teamwiki: what correction actually does today (the V4 result)

Current-truth and history MRR, before -> after corrections:

| arm | current | history |
|---|---|---|
| index-hybrid | 0.80 -> 0.70 | 1.00 -> 1.00 |
| graph-hybrid | 0.80 -> **1.00** | 1.00 -> **0.38** |
| plan-hybrid | 0.80 -> 0.57 | 1.00 -> 0.71 |
| router-full | 0.80 -> 0.67 | 1.00 -> 0.71 |

**F-T15-15: Correction-as-forget trades history for truth.** The
graph route got current-truth exactly right after correction (MRR
1.00: the stale gloss no longer competes) and simultaneously lost
history access (1.00 -> 0.38: "what did we originally choose and
why" can no longer find the February decision, whose only remaining
observation is the OUTDATED marker). Today's surfaces offer forget,
not supersede; this is the measured cost of that gap, and the
sharpest evidence yet for the plan/17 supersession contract
(valid_until instead of tombstone: current queries filter the old
fact, as-of queries still reach it).

**F-T15-16: The correction boost can rank the tombstone above the
truth.** In the planner route, post-correction current queries came
back with the *stale* doc at rank 1 (rotation-q1 above rotation-q3,
versioning-v1 above v2). The OUTDATED marker carries the old doc's
title at 5x field weight plus the 1.3x source=correction boost, so
the marker outranks the superseding document itself. A boost designed
to surface corrections surfaces the pointer instead of the
destination. Under the principles doc this is another post-fusion
multiplicative prior defect; the supersession *edge* should reroute
retrieval to the new fact, not a boosted marker observation.

**F-T15-17: The layers share one physical index namespace, and
corrections leak across it.** index-hybrid current-truth dropped
0.80 -> 0.70 even though no omem:// content was touched: graph
observations and indexed text live in the same FTS/vector backend
(observations under reserved memory:// URIs), so the four new
OUTDATED markers entered the shared top-30 candidate pool and
displaced content chunks by rank. Layer separation in the design must
be physical (or filtered at the backend), not a naming convention.

**F-T15-18: Nothing masks the stale content layer.** The index route
keeps serving superseded documents for current-truth queries in both
conditions (0.70-0.80 MRR ceiling); with the graph knowing exactly
which doc supersedes which, no retrieval path uses that edge to
demote the stale chunk. Graph-informed masking of superseded URIs
(or validity metadata on indexed content) is a concrete, testable
re-architecture item.

## V4 caveats

Same single-author caveats as V3; teamwiki has 8 current/history
queries, so treat the pre/post deltas as mechanism demonstrations,
not magnitudes. Arms within a pass share access-count state (the T3
defect), which contributes noise to between-arm comparisons on these
small stores.
