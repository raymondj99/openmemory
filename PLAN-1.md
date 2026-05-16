# PLAN-1: Borrowing agentmemory's retrieval ideas without losing openmemory's identity

Status: draft proposal. Last updated: 2026-05-16.

## 0. Context

A codex review compared openmemory against agentmemory and concluded
that openmemory's biggest retrieval gap is not "missing tools" but
"thin retrieval units." agentmemory indexes compressed observations
with titles, narratives, concepts, files, importance, session
metadata, and summaries; openmemory indexes a single
`"{entity}: {content}"` string per observation and (today) does not
actually run a hybrid query on the free-text path at all.

This plan turns those findings into an executable, phased roadmap.
Each phase is independently shippable; each step lists the files it
touches, the contract it changes, and the measurable signal that
tells us the change worked. The plan deliberately keeps openmemory's
identity intact (single binary, explicit MCP contract, local storage,
predictable security, low operational complexity); we are borrowing
retrieval engineering, not adopting agentmemory's runtime surface.

## 1. Findings audit (verified against HEAD = e8e8c3f)

Every codex finding was checked against the current code. Quick
status:

| Codex finding | Status | Evidence |
|---|---|---|
| `openmemory_search` is effectively keyword-only | Confirmed | [tools/index.rs:159-164](crates/openmemory-mcp/src/tools/index.rs#L159-L164) passes `zero_vector: Vec<f32> = Vec::new()` into `engine.search`. [hybrid.rs:93-95](crates/openmemory-index/src/hybrid.rs#L93-L95) explicitly short-circuits Hybrid mode to keyword-only when the query vector is empty. |
| `openmemory_index_text` stores no vector | Confirmed | [tools/index.rs:91-98](crates/openmemory-mcp/src/tools/index.rs#L91-L98) constructs `IndexEntry::new(uri, text)` with no `.with_vector(...)` call. |
| Watcher indexing stores no vector | Confirmed | [watch/src/index.rs:144-146](crates/openmemory-watch/src/index.rs#L144-L146) builds `IndexEntry::new(uri, text)` and inserts directly. |
| `memory_tier` filter accepted but unused | Confirmed | [tools/memory.rs:213-216](crates/openmemory-mcp/src/tools/memory.rs#L213-L216) carries `pub memory_tier: Option<MemoryTierParam>` annotated `#[allow(dead_code)]`. [recall.rs:53-69](crates/openmemory-graph/src/recall.rs#L53-L69) has no tier field on `RecallFilters`. [params.rs:108-117](crates/openmemory-mcp/src/params.rs#L108-L117) marks `to_tier` itself `#[allow(dead_code)]`. |
| Observation indexed text is too narrow | Confirmed | [remember.rs:288-291](crates/openmemory-graph/src/remember.rs#L288-L291) writes only `format!("{entity_name}: {content}")` into the index. |
| Graph recall is fallback-only | Confirmed | [recall.rs:202-206](crates/openmemory-graph/src/recall.rs#L202-L206) calls `spread_activation` only when `hits.len() < top_k`. There is no graph contribution to the RRF fusion. |
| No diversification | Confirmed | [recall.rs:195-200](crates/openmemory-graph/src/recall.rs#L195-L200) just sorts by score and truncates. No per-entity, per-source, or per-tier cap. |
| No retrieval-quality benchmark | Confirmed | [bench/benches/openmemory.rs:1-15](crates/openmemory-bench/benches/openmemory.rs#L1-L15) lists six perf benchmarks (latency, throughput). None measure R@K, MRR, or NDCG. |

Bonus issues caught during the audit:

- [store.rs:575-590](crates/openmemory-graph/src/store.rs#L575-L590) `row_to_observation` does not read column 10 (`memory_tier`); the column is in the schema and populated on write, but the in-memory `Observation` struct ignores it. Plain consequence: even if recall added a tier filter, callers would never see the tier on returned rows.
- [types.rs:141-157](crates/openmemory-graph/src/types.rs#L141-L157) `Observation` has no `memory_tier` field at all. Tier exists only in SQL and in the writer.
- [tools/index.rs:158](crates/openmemory-mcp/src/tools/index.rs#L158) reproduces the `top_k * 3` over-fetch that the hybrid engine already does internally; the wrapper double-pumps the search.

## 2. Design principles for this work

These constraints apply to every step below. They are how we keep
openmemory's identity while we borrow.

- **Backwards-compatible by default.** New observation fields are
  optional. Old rows roundtrip cleanly. The MCP tool surface gains
  fields, never removes them; existing callers see no behaviour
  change unless they opt in.
- **One source of truth.** SQLite remains canonical. The hybrid
  engine is rebuilt from SQLite on demand. New schema columns flow
  through migrations; new index fields flow through the rebuild path.
- **No new runtime surface.** No new transports, no new long-running
  services, no embedded viewer. Every feature lands as: a Rust API,
  an MCP tool (when externally useful), an optional CLI subcommand.
- **Local, offline-first.** Every default path works without an LLM
  and without an internet connection. LLM-backed features live behind
  the existing `llm` feature gate (post-v0.2 backlog item).
- **Measure before tuning.** No scoring change ships without a
  R@K/MRR/NDCG eval that proves the change moves the needle on a
  task we care about.
- **Writing style.** Avoid em-dashes in prose per project preference;
  this document, MCP tool descriptions, and CHANGELOG entries follow
  that rule.

## 3. Phased plan

The phases are ordered by dependency, not by perceived value. Phase 1
(correctness gaps) and Phase 2 (retrieval evaluation harness) must
land before any scoring tuning is meaningful.

---

### Phase 1: Correctness gaps in hybrid search (must land first)

**Goal.** Make every documented hybrid path actually hybrid. Today
`openmemory_search`, `openmemory_index_text`, and the file watcher
all silently degrade to keyword-only because they never produce or
consume vectors. Fixing this is a prerequisite for every later
retrieval-quality claim.

**Steps.**

1. **Expose a document/query embedding entry point on `MemoryStore`.**
   - File: [graph/src/remember.rs:314-336](crates/openmemory-graph/src/remember.rs#L314-L336).
   - `embed_query_text` and `embed_document_text` are `pub(crate)`.
     Promote them to `pub` (or wrap in a thin `pub fn embed_query`
     and `pub fn embed_document` on `MemoryStore`). The MCP server
     and the watcher both live outside the graph crate; they need
     access to the same embedder the store uses, including the
     model's task prefixes.
   - Keep the existing behaviour: when no embedder is attached, both
     return `Vec::new()`. The hybrid engine already treats an empty
     query vector as "skip the vector arm" (see
     [hybrid.rs:90-95](crates/openmemory-index/src/hybrid.rs#L90-L95)).

2. **Vectorise `openmemory_index_text`.**
   - File: [mcp/src/tools/index.rs:77-101](crates/openmemory-mcp/src/tools/index.rs#L77-L101).
   - In `OpenMemoryIndexTextTool::call`, compute `vector =
     server.memory().embed_document(&text)`. Build
     `IndexEntry::new(uri, text).with_vector(vector)`.
   - Add a new optional input field `embed: bool` defaulting to `true`
     so callers can opt out for very long documents where they would
     rather chunk-then-index. (Multi-chunk indexing itself is a
     separate phase.)

3. **Vectorise `openmemory_search`.**
   - File: [mcp/src/tools/index.rs:150-191](crates/openmemory-mcp/src/tools/index.rs#L150-L191).
   - Replace `let zero_vector: Vec<f32> = Vec::new();` with
     `let vector = server.memory().embed_query(&req.query);`. Drop
     the over-fetch wrapper (`limit.saturating_mul(3)`); the hybrid
     engine handles the candidate inflation internally.
   - Update the tool description to drop the wording "set mode to
     keyword to skip the vector path" because the vector path now
     actually runs in hybrid mode.

4. **Vectorise the watcher.**
   - File: [watch/src/index.rs:144-159](crates/openmemory-watch/src/index.rs#L144-L159).
   - In `process_file`, compute `let vector =
     memory.embed_document(text);` before constructing the
     `IndexEntry`. Attach via `with_vector` when non-empty. No
     behaviour change when the embedder is absent.
   - For files larger than `WatchOptions::max_chunk_chars` (a new
     option, default 4 KiB), defer to the multi-chunk phase; emit a
     single entry today.

5. **Tests.**
   - In [tools/index.rs](crates/openmemory-mcp/src/tools/index.rs)
     tests, add a case that attaches a `FakeEmbedder` and asserts
     `openmemory_search` returns results in `SearchMode::VectorOnly`
     for content that has no shared keywords.
   - In [watch/src/index.rs](crates/openmemory-watch/src/index.rs)
     tests, add the same shape (FakeEmbedder + a query that only
     hits via vectors).
   - Property: with an attached embedder and identical text, every
     successful `openmemory_index_text` call produces a row with a
     non-empty stored vector. Assert via `engine.count()` against
     the FlatVectorIndex count.

**Done when.**

- `openmemory_search` returns vector-similar results without a
  keyword match present.
- `openmemory_index_text` and the watcher each cause the vector
  index count to grow when an embedder is attached.
- CI on `--no-default-features` (no `embeddings` feature) still
  passes; the empty-vector code path is the unembedded fallback.

---

### Phase 2: Retrieval evaluation harness

**Goal.** Stand up the eval surface that every later scoring change
will be measured against. Without this, scoring tuning is guesswork.

**Steps.**

1. **New crate `openmemory-eval`.**
   - Add to `[workspace.members]` alongside `openmemory-bench`.
   - Public surface: a `Dataset` trait (yields
     `(corpus_docs, queries, relevance_judgments)`), an
     `EvalRunner` that ingests into a fresh `MemoryStore`, runs
     queries through `recall`/`search`, and reports R@K, MRR, NDCG,
     plus per-query latency.
   - Two adapters to start:
     - **LongMemEval-S** subset (the smaller distribution). Adapter
       reads the public dataset format, materialises observations
       per session, and runs the standard QA-style queries.
     - **CodingMem** sift already exposes the format publicly; we
       lift the loader, not the corpus.

2. **`openmemory eval` CLI subcommand.**
   - New file: `crates/openmemory-cli/src/commands/eval.rs`.
   - Args: `--dataset <name>`, `--mode <hybrid|keyword|vector>`,
     `--alpha <f32>`, `--limit <k>`, `--profile <data-dir>`.
   - Output: a JSON report (machine-readable) plus a one-page text
     summary. Pass `--baseline <file>` to diff against a prior run.

3. **Ablation matrix in CI.**
   - GitHub Actions job runs `openmemory eval --dataset longmem-s
     --mode keyword|vector|hybrid` on every PR; uploads the JSON
     report as an artifact. The job is non-gating at first; once
     numbers stabilise, gate on regressions over a configurable
     tolerance.

4. **Document the rubric.**
   - New section in [docs/search.md](docs/search.md) called "How we
     measure retrieval quality." Defines the three metrics (R@K,
     MRR, NDCG), the datasets, and the table of current baselines.
   - Pin baselines in [CHANGELOG.md](CHANGELOG.md) on every release.

**Done when.**

- `openmemory eval --dataset longmem-s --mode hybrid` runs locally
  and prints a R@5/R@10/MRR/NDCG table.
- CI uploads the JSON report on every PR.
- The repo has a documented baseline number we can regress against.

---

### Phase 3: `memory_tier` end-to-end

**Goal.** Close the dead-code path: tier is in the schema and the
writer; expose it on read.

**Steps.**

1. **Surface tier on `Observation`.**
   - Files: [graph/src/types.rs:141-157](crates/openmemory-graph/src/types.rs#L141-L157) and [graph/src/store.rs:575-590](crates/openmemory-graph/src/store.rs#L575-L590).
   - Add `pub memory_tier: MemoryTier` to `Observation` with
     `#[serde(default = "MemoryTier::default_episodic")]` so old
     serialised rows still load. Default the in-memory value to
     `MemoryTier::Episodic` for consistency with the column default.
   - Update `row_to_observation` to read column 10 and parse via
     `MemoryTier::parse(...).unwrap_or(MemoryTier::Episodic)`.

2. **Add `tier` to `RecallFilters`.**
   - File: [graph/src/recall.rs:53-80](crates/openmemory-graph/src/recall.rs#L53-L80).
   - New `pub memory_tier: Option<MemoryTier>` field. Apply during
     the recall scan loop right alongside `entity_type` and
     `source`. Also fold tier into `cache_key` via a new
     `filters_hash` discriminant so cached results stay correct.

3. **Wire MCP tool input.**
   - File: [mcp/src/tools/memory.rs:213-262](crates/openmemory-mcp/src/tools/memory.rs#L213-L262).
   - Drop the `#[allow(dead_code)]` on `memory_tier` and convert it
     into `filters.memory_tier`.
   - Drop the `#[allow(dead_code)]` on `MemoryTierParam::to_tier`.

4. **Recall result includes tier.**
   - `RecallResult` and the MCP `openmemory_recall` JSON output gain
     `"memory_tier": "<episodic|semantic|procedural>"`. Existing
     keys are unchanged.

5. **Tests.**
   - Add `recall_filters_memory_tier_excludes_other_tiers` in
     [graph/src/recall.rs](crates/openmemory-graph/src/recall.rs).
   - Add MCP roundtrip test in
     [mcp/src/tools/memory.rs](crates/openmemory-mcp/src/tools/memory.rs)
     that stores an observation with `MemoryTier::Semantic` (after
     step 6 lets writers set it) and asserts the tier survives in
     the JSON response.

6. **(Stretch in this phase.)** Let `openmemory_remember` accept
   `memory_tier: "episodic"|"semantic"|"procedural"` on the input
   side. Default stays episodic. Wires through the existing
   `ObservationInput::memory_tier` field already on the Rust API.

**Done when.**

- `openmemory_recall { "memory_tier": "semantic" }` actually filters.
- Tier shows up on every recall result JSON.

---

### Phase 4: Fielded observation indexing

**Goal.** Replace the single `"{entity}: {content}"` indexed text
with a fielded representation so titles, concepts, source files, and
entity types contribute to retrieval. Today the entity type is
invisible to keyword search; a query for `"project"` cannot use
the fact that an observation is on a `project`-typed entity.

This is the change that most directly addresses codex's "observation
text is too narrow" finding.

**Steps.**

1. **Decide the field set.** Concrete proposal:

   | Field | Source | Indexing weight |
   |---|---|---|
   | `entity_name` | existing | high |
   | `entity_type` | existing | low |
   | `content` | existing | high |
   | `relation_labels` | existing | medium |
   | `concepts` (new optional) | caller | medium |
   | `source_files` (new optional) | caller or watcher | medium |
   | `source_kind` (new optional, free string) | caller | low |
   | `importance` (new optional, f32 in [0,1]) | caller | not indexed; used as ranking prior |
   | `title` (new optional) | caller | very high |
   | `summary` (new optional) | caller | medium |

   `concepts` and `source_files` are arrays of short strings; the
   rest are scalars. All are nullable. The MCP `openmemory_remember`
   tool accepts them as optional input keys; old callers see no
   schema break.

2. **Migration.**
   - New `V2_SQL` in [graph/src/schema.rs](crates/openmemory-graph/src/schema.rs).
   - Columns added (all nullable except `entity_type` already on
     `entities`): `observations.title`, `observations.summary`,
     `observations.importance`, `observations.source_kind`.
   - New tables: `observation_concepts(observation_id, concept)`
     and `observation_source_files(observation_id, file_path)`. Both
     keyed on `observation_id` with FK cascade. Indexed on the
     value column for keyword lookup.
   - Bump `MEMORY_SCHEMA_VERSION` to `2`. Forward-only migration
     stays compatible with v1 stores.

3. **Indexed-document construction.**
   - In `apply_search_sync_ops_with_recovery` ([remember.rs:280-309](crates/openmemory-graph/src/remember.rs#L280-L309)),
     replace the single-string formula with a weighted concatenation.
     Two backend strategies, chosen by feature gate:
     - **FTS5 path.** Add fielded columns to `chunks_fts` (title,
       text, concepts, files, source_kind, entity_type, entity_name).
       FTS5 supports per-column weights via the `bm25()` rank function,
       e.g. `bm25(chunks_fts, 5.0, 1.0, 2.0, 2.0, 0.5, 0.5, 4.0)`.
       Wire the weight vector from `Config::search.field_weights`.
     - **Pure-Rust BM25 path.** Concatenate fields with repeated
       title/entity_name tokens to simulate the weighting. Document
       that this is a coarser approximation; the FTS5 path is the
       supported default anyway.

4. **`openmemory_remember` MCP input.**
   - File: [mcp/src/tools/memory.rs:59-77](crates/openmemory-mcp/src/tools/memory.rs#L59-L77).
   - Accept optional `title`, `summary`, `importance`, `source_kind`,
     `concepts: Vec<String>`, `source_files: Vec<String>` per
     observation. Validate `importance` clamps into `[0, 1]`.

5. **Recall returns the new fields.**
   - `RecallResult` extends with the new optional fields. JSON keys
     are stable; absent on observations that did not provide them.

6. **Evaluation.**
   - Re-run the Phase 2 evals before and after the indexing change.
     Expectation: R@10 improves on LongMemEval-S, smaller swing on
     CodingMem. Pin the delta in the CHANGELOG. If R@10 regresses,
     the change does not ship.

**Done when.**

- Migration v2 lands; v1 stores upgrade cleanly on next open.
- Fielded indexing measurable in the eval harness.
- `openmemory_remember` accepts the new fields and they roundtrip
  through `openmemory_get_entity` and `openmemory_recall`.

---

### Phase 5: Graph as a third retrieval stream

**Goal.** Promote spreading activation from a fallback to a
co-equal retrieval stream that participates in the RRF fusion. This
addresses codex's "graph recall is fallback-only" finding.

**Steps.**

1. **Generalise `HybridSearchEngine`.**
   - File: [index/src/hybrid.rs:81-103](crates/openmemory-index/src/hybrid.rs#L81-L103).
   - Take an N-stream RRF (vector, keyword, plus a caller-provided
     "extra" stream) instead of the hard-coded two-stream version.
     Keep two-stream as the trivial case.
   - Per-stream weights stay configurable. Default
     `alpha_vector = 0.45`, `alpha_keyword = 0.30`, `alpha_graph =
     0.25`, tuned via Phase 2 evals.

2. **Graph stream producer in `recall`.**
   - File: [graph/src/recall.rs](crates/openmemory-graph/src/recall.rs).
   - New helper `graph_candidates(query, top_k, filters) -> Vec<SearchResult>`
     that:
     - Matches the query against entity names (case-insensitive,
       optionally fuzzy via the existing `normalize` helpers).
     - For each matched entity, walks 1-hop relations, then surfaces
       their live observations with a relation-weighted score.
   - Pass the result to the hybrid engine as the extra stream
     instead of triggering the post-hoc `spread_activation` path.
     Keep `spread_activation` available behind the existing
     `RecallFilters::spreading_activation` flag for callers who
     still want the strict-fallback semantics.

3. **Score normalisation.** Graph scores come from a different
   distribution than BM25 / cosine. The RRF design (rank-based, not
   score-based) handles this fine; we deliberately do not normalise
   raw scores. Document this in [docs/search.md](docs/search.md).

4. **Evaluation.**
   - Ablation: keyword-only, +vector, +graph, all-three.
   - Acceptance: graph stream improves MRR by at least the noise
     floor on LongMemEval-S without regressing CodingMem.

**Done when.**

- Graph candidates participate in RRF.
- Per-stream weights configurable in `[search]`.
- Eval shows non-trivial MRR lift on at least one dataset.

---

### Phase 6: Result diversification

**Goal.** Stop top-K from being dominated by one entity, one source,
or one URI prefix.

**Steps.**

1. **`Diversify` post-filter.**
   - File: new `crates/openmemory-graph/src/diversify.rs`.
   - Implements a Maximal Marginal Relevance-style cap. Caps are
     additive: `per_entity_cap`, `per_source_cap`, `per_tier_cap`,
     `per_uri_prefix_cap`. Defaults: 3, 5, no cap, no cap.
   - Invoked after recall sort/truncate but before the
     `bump_access_counts` write so we do not boost the dropped rows.

2. **Same shape on `openmemory_search`.** The MCP tool gains the
   same diversification knobs.

3. **MCP knobs.** New optional input fields on `openmemory_recall`
   and `openmemory_search`: `per_entity_cap`, `per_source_cap`,
   `per_tier_cap`. Omitted means "use config default."

4. **Evaluation.** Measure R@5 with and without diversification.
   Acceptance: R@5 unchanged or improved while subjective diversity
   on a hand-rolled query set goes up.

**Done when.**

- A `openmemory_recall` call against a corpus with 50 observations
  on one entity returns no more than `per_entity_cap` from that
  entity in top-K.

---

### Phase 7: `openmemory_context` and pinned-memory tier

**Goal.** Give agents a token-budgeted "what should I know right now"
context surface, plus a tiny pinned slot for must-not-forget items.
This is the codex "session summaries / project profiles, but
adapted" recommendation.

**Steps.**

1. **Pinned tier.**
   - Add `MemoryTier::Pinned` to [types.rs:239-272](crates/openmemory-graph/src/types.rs#L239-L272).
     Migration is additive; existing rows stay episodic.
   - `openmemory_remember` accepts `memory_tier: "pinned"`. Pinned
     observations skip decay (or apply a much smaller `lambda`) and
     are surfaced first in `openmemory_context`. A small `lambda` is
     preferable to "no decay" because it preserves the existing
     scoring math without a special case.

2. **`openmemory_context` MCP tool.**
   - New file: `crates/openmemory-mcp/src/tools/context.rs`.
   - Input: `query: String`, `token_budget: u32 = 2000`,
     `include_pinned: bool = true`, `entity_focus: Vec<String>`,
     `file_focus: Vec<String>`.
   - Output: a token-budgeted JSON bundle with three sections:
     pinned, high-confidence preferences and project facts, recent
     relevant hits. Section budgets are configurable; defaults
     follow agentmemory's pattern (small pinned, medium preferences,
     remainder for recent hits).
   - Approximates tokens via the existing tokenizer when an
     embedding model is loaded; falls back to "4 chars ≈ 1 token"
     when not.

3. **Not in this tool.** No auto-injection. The agent must
   explicitly call `openmemory_context` and feed the result back in.
   This keeps the MCP server stateless and predictable.

**Done when.**

- `openmemory_context` returns a bundle that fits the requested
  token budget within a small tolerance.
- A pinned observation survives a `consolidate` run that would
  otherwise prune it.

---

### Phase 8: Optional advanced retrieval (deterministic first, LLM later)

**Goal.** Layer in cheap query-side improvements. Anything that
needs an LLM stays behind the `llm` feature flag (post-v0.2 backlog).

**Steps.**

1. **Deterministic query expansion.** New helper
   `expand_query(query, store) -> Vec<String>`:
   - Pull entity-name aliases from the graph (e.g. SAME_AS edges
     produced by the normalisation path).
   - Preserve quoted substrings verbatim.
   - Extract file-path-like tokens (anything matching
     `[a-z_][a-z0-9_/.-]*\.[a-z]+`) and keep them as exact terms.
   - Return the expanded query list; the search caller can either
     OR-fuse them at the FTS5 layer or run multiple subqueries and
     re-RRF.

2. **Stemming and synonym hooks.** SQLite FTS5 supports a
   custom tokenizer; today we use `unicode61 remove_diacritics 2`.
   Add an opt-in Porter stemmer tokenizer behind a new config field
   `search.tokenizer = "unicode61" | "porter"`. Default unchanged.

3. **Optional local cross-encoder reranker.** Behind a new optional
   feature `rerank`. Loads a small ONNX cross-encoder (default:
   `cross-encoder/ms-marco-MiniLM-L-6-v2`), reranks the top 20
   results before truncation to top-K. Adds latency; off by default.

4. **LLM query expansion** stays in the backlog. When it lands, it
   is opt-in via `openmemory_recall { llm_expansion: true }`, gated
   on the existing `llm` feature plus a per-call cost limit.

**Done when.**

- File-path queries surface relevant observations even when the
  punctuation tokenises poorly under FTS5.
- A SAME_AS-related entity name resolves to its canonical form on
  recall.
- Cross-encoder reranker, when enabled, shows MRR lift on the eval
  harness with the latency cost reported in the same run.

---

## 4. What we are explicitly not adopting

Lifted verbatim from codex's "What I would not copy" plus three
items the audit identified.

- **No new REST/hook/viewer/runtime surface.** openmemory stays a
  single binary that speaks MCP over stdio (and optional Streamable
  HTTP). No browser viewer, no in-process compaction daemon, no
  request hooks.
- **No automatic context injection.** `openmemory_context` returns
  a bundle when asked; it does not insert itself into agent prompts.
- **No mandatory LLM dependency.** Every default path works with
  zero outbound HTTP. LLM-backed features live behind feature flags.
- **No score normalisation tricks.** RRF is rank-based on purpose;
  we will not scale BM25 vs cosine scores before fusion.
- **No silent schema changes.** Every new column flows through a
  numbered migration that the binary refuses to skip.

## 5. Risk and rollout

- **MCP tool compatibility.** All MCP additions are additive: new
  optional input fields, new optional output fields. The existing
  contract (every documented tool name, every documented field) is
  unchanged. Phase 4 introduces an MCP `tools/list` shape change
  only via a new tool (`openmemory_context`, Phase 7), which is
  itself additive.
- **Schema migrations.** v2 (Phase 4) and v3 (Phase 7) are
  forward-only. Existing v1 stores upgrade in place; downgrade is
  not supported (this is the current policy and stays).
- **Feature gates.** `embeddings`, `fts5`, `hnsw`, `mcp-http`,
  `serve` stay as is. New optional feature `rerank` (Phase 8) is
  off by default.
- **Performance.** Phase 1 (vectorising the watcher) adds embedding
  cost to every indexed file. Mitigation: respect `WatchOptions.max_size`,
  cache embeddings via the existing `EmbeddingCache` (already keyed by
  BLAKE3 of the content). Phase 5 (graph stream) adds a SQLite query
  per recall; mitigation: gate behind an `enable_graph_stream`
  config option until the eval shows it pays.
- **Eval as gate.** Phases 4, 5, 6, and 8 each require an eval-run
  that shows non-regression before merging. The harness lands in
  Phase 2 specifically so we never tune scoring without numbers.

## 6. Suggested release sequencing

| Phase | Earliest release | Notes |
|---|---|---|
| Phase 1: correctness gaps | v0.2.2 (patch) | Pure bug fix in observable behaviour. |
| Phase 2: eval harness | v0.3.0 | New crate; new CLI subcommand. |
| Phase 3: memory_tier wire-up | v0.3.0 | Ships alongside the eval baseline that proves no regression. |
| Phase 4: fielded indexing | v0.3.0 | Schema v2 migration. |
| Phase 5: graph stream | v0.3.1 | Behind config flag at first. |
| Phase 6: diversification | v0.3.1 | Defaults conservative. |
| Phase 7: context tool + pinned tier | v0.4.0 | Schema v3 migration; new MCP tool. |
| Phase 8: advanced retrieval | v0.4.x | Mostly opt-in. |

## 7. Open questions

These are decision points the implementer should surface to the
maintainer before committing code.

1. **Title and summary as first-class columns vs `observations.metadata` JSON blob?** A blob is cheaper to schema-evolve but loses the FTS5 per-column weighting trick. Proposal: dedicated columns for title/summary/importance/source_kind; concepts and source_files as side tables.
2. **Graph stream weight (`alpha_graph`).** Pick a default after Phase 5 eval; 0.25 is a placeholder.
3. **Diversification defaults.** `per_entity_cap = 3` and
   `per_source_cap = 5` are agentmemory-flavoured guesses.
   Confirm via the eval harness on real corpora.
4. **`openmemory_context` token budget.** Defaulting to 2000 is
   conservative. Larger budgets (8k, 32k) are reasonable for
   long-context models; consider deriving the default from the
   active embedding model's `max_tokens` or making it explicitly
   required.
5. **Pinned tier vs core-memory slot.** A dedicated `MemoryTier::Pinned`
   is simpler than a separate "core memory" table; the tradeoff is
   that pinned observations still flow through the same recall
   pipeline. Acceptable for now; revisit if pinned-vs-other
   contention shows up in eval.

## 8. References

- Codex review: see the original chat transcript that produced this
  plan.
- Local files (verified): [recall.rs](crates/openmemory-graph/src/recall.rs),
  [remember.rs](crates/openmemory-graph/src/remember.rs),
  [hybrid.rs](crates/openmemory-index/src/hybrid.rs),
  [tools/index.rs](crates/openmemory-mcp/src/tools/index.rs),
  [tools/memory.rs](crates/openmemory-mcp/src/tools/memory.rs),
  [watch/src/index.rs](crates/openmemory-watch/src/index.rs),
  [params.rs](crates/openmemory-mcp/src/params.rs),
  [types.rs](crates/openmemory-graph/src/types.rs),
  [schema.rs](crates/openmemory-graph/src/schema.rs),
  [bench/benches/openmemory.rs](crates/openmemory-bench/benches/openmemory.rs).
- agentmemory: `hybrid-search.ts`, `search-index.ts`, `observe.ts`,
  `compress.ts`, `context.ts`, LongMemEval-S results table.
