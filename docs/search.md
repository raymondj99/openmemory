# Search and recall

Hybrid search is the engine that powers both `openmemory_recall`
(graph) and `openmemory_search` (free-text URI store). It combines
a vector backend with a keyword backend, fuses their rankings via
Reciprocal Rank Fusion, and (for graph recall) re-scores with
Ebbinghaus forgetting-curve decay.

This document explains the math and the moving parts.

Every ingestion path (the `openmemory_remember` write path, the
`openmemory_index_text` tool, and the file watcher) calls the same
`embed_document` helper, so the keyword and vector backends always
hold the same set of URIs.

## The hybrid engine

`HybridSearchEngine<V, F>` (in
[`crates/openmemory-index/src/hybrid.rs`](../crates/openmemory-index/src/hybrid.rs))
is generic over a `VectorStore` and a `FullTextStore`. The active
backend pair is chosen at compile time by feature flags:

| Vector backend | When |
|----------------|------|
| `AdaptiveVectorIndex` | Default CLI backend. Exact flat search below 4,096 vectors, then one-time migration to approximate usearch HNSW. |
| `FlatVectorIndex` (brute-force cosine) | Builds without `hnsw`. Exact, O(n) per query. |
| `HnswIndex` (usearch HNSW) | Large-corpus backend used by the adaptive index. Approximate, sub-linear per query. |

| Keyword backend | When |
|-----------------|------|
| `Fts5Store` (SQLite FTS5 with BM25) | Default. |
| `Bm25Store` (pure-Rust BM25, JSON-backed) | `--no-default-features`. Snapshot-loaded; no SQL dependency. |

The two backends are queried independently; their result lists are
fused via RRF.

## Reciprocal Rank Fusion (RRF)

For each backend, every result is assigned a fusion score:

```text
rrf_score(result) = 1 / (k + rank(result))
```

where `rank(result)` is the 1-indexed position in that backend's
ranked list, and `k` is the RRF dampening constant (default
`rrf_k = 60`, configurable in `[search]` via `rrf_k`).

The hybrid engine then:

1. Runs the vector backend to retrieve `top_k * 3` candidates.
2. Runs the keyword backend to retrieve `top_k * 3` candidates.
3. Sums the per-backend RRF scores, weighted by `alpha`:

```text
final_score(uri) = alpha * vector_rrf(uri) + (1 - alpha) * keyword_rrf(uri)
```

4. Sorts descending and trims to `top_k`.

The default `alpha = 0.7` favours vector hits; tune via
`Config::search.hybrid_alpha`. `SearchMode::VectorOnly` short-circuits
to the vector backend (alpha = 1.0); `SearchMode::KeywordOnly`
short-circuits to keyword only (alpha = 0.0).

## Caching

`CachedSearchEngine` (in
[`src/cache.rs`](../crates/openmemory-index/src/cache.rs)) wraps
the hybrid engine with an LRU + TTL cache:

- `DEFAULT_CACHE_CAPACITY = 50` entries.
- `DEFAULT_CACHE_TTL = 60` seconds.
- Cache key: `(query, top_k, mode, filter_hash)`.

Every `CachedSearchEngine::insert` and `delete_by_uri` calls
`invalidate()` on the LRU, so a `MemoryStore` write automatically
drops stale cached results. The `RwLock<()>` rebuild barrier on
`MemoryStore` is a separate mechanism guarding vector-index
visibility (see [architecture.md](architecture.md#threading-model)).

## Embeddings

Vector search needs vectors. `openmemory-embed` provides them via
ONNX Runtime running locally on CPU.

### Model registry

Two models ship in
[`crates/openmemory-embed/src/models.rs`](../crates/openmemory-embed/src/models.rs):

| Constant | Dimensions | Pooling | Notes |
|----------|------------|---------|-------|
| `NOMIC_EMBED_TEXT_V1_5` | 768 | Mean | Default. The most-used local embedding model on Hugging Face for English text. |
| `SNOWFLAKE_ARCTIC_EMBED_L_V2` | 1024 | CLS | Alternate. Higher dim, better for cross-lingual or longer-doc retrieval. |

Each `Model` carries: name, aliases, repo id, dimension, max-token
limit, pooling strategy, output tensor name, query and document
prefix templates, ONNX file URL, tokenizer URL, and SHA-256 hashes
for both files.

Model files live in the shared cache at
`~/.openmemory/models/<model-name>/`. They are downloaded only when
the user explicitly runs `openmemory model download [MODEL]`; server
startup never performs outbound HTTP. At runtime, graph writes use
the model's document prefix and recall queries use its query prefix
before embedding.

### Integrity verification

`OnnxEmbedder::load_for_model` calls
`integrity::verify_sha256(file, expected)` on both the `model.onnx`
and `tokenizer.json` files before handing them to the runtime.
Mismatches surface as `EmbedError::ChecksumMismatch` with a
"refusing to load" message.

The verification implementation streams files in 64 KiB
heap-allocated blocks, normalises expected hex case-insensitively,
and treats empty hashes as `VerificationOutcome::Skipped`. Both
shipped models (Nomic v1.5, Snowflake Arctic Embed L v2) carry
real `onnx_sha256` and `tokenizer_sha256` values in
[`models.rs`](../crates/openmemory-embed/src/models.rs), so loads
always run a real check. A future registry entry that ships with
empty hashes still surfaces as `Skipped` (warns and loads) rather
than blocking startup.

### Cache

The `EmbeddingCache` (SQLite-backed by default; JSON fallback when
the `sqlite` feature is off) is keyed by `BLAKE3(content)` (hex).
A second `embed()` call on the same text is a cache hit, no model
inference.

### When the vector arm runs

The vector arm runs whenever an embedding model is loaded, on every
hybrid-mode call into `openmemory_recall`, `openmemory_search`, and
the file watcher. When no model is loaded (the `embeddings` feature
off, or the cached model files absent), the same calls fall through
to keyword-only without raising an error. Hybrid mode is therefore
safe to ask for unconditionally. `VectorOnly` mode returns no results
without a loaded model because there is no query vector to search.

## Recall scoring (graph only)

Once the hybrid engine returns a list of candidate observations,
`MemoryStore::recall` (in
[`crates/openmemory-graph/src/recall.rs`](../crates/openmemory-graph/src/recall.rs))
re-scores each with the Ebbinghaus forgetting curve plus
retrieval, correction, importance, and confidence boosts:

```text
base_decay     = exp(-lambda * days_since_observed)
retrieval      = 1 + 0.15 * ln(1 + access_count)
correction     = source in {"correction", "cortex:correction"} ? 1.3 : 1.0
importance     = 1 + 0.25 * importance
final_score    = search_score * base_decay * retrieval * correction * importance * confidence
```

- `lambda` is the per-store decay rate. Default
  `Config::memory.decay_rate = 0.01` per day. Override per-store
  via `MemoryStore::with_decay_rate(rate)` for tests.
- `access_count` increments after each successful recall via the
  background `bump_access_counts` write. Frequently-recalled
  observations decay slower because the retrieval boost grows
  logarithmically with access count.
- `correction` flags observations the agent should not repeat; the
  `1.3` boost (`CORRECTION_RETRIEVAL_BOOST` constant) ensures they
  surface ahead of competing facts.
- `importance` is the optional per-observation prior in `[0.0, 1.0]`.
  It contributes up to a 1.25x multiplier and is not indexed as text.
- `confidence` is the per-observation `confidence` field (default
  1.0; the `remember` API lets the caller drop it for uncertain
  facts).

`RECALL_MIN_SCORE = 0.05` is the floor; raw search scores below
that drop out before re-scoring, so HNSW noise does not muddy the
final list.

## Spreading activation

When direct hits underflow the requested `top_k`, recall walks
1-hop relations and surfaces observations from neighbouring
entities. Spread results carry an extra `SPREADING_DISTANCE_DECAY
= 0.5` multiplier so they rank below direct hits even when the
direct hit's raw search score is low.

Spreading activation is on by default; turn it off via
`RecallFilters::spreading_activation = false`.

All recall filters still apply to spread results. That includes
`entity_type`, `source`, `min_confidence`, `memory_tier`, temporal
validity, and case-insensitive `entity_names`; spreading activation
expands the candidate source, not the caller's visibility scope.

## Temporal validity

Observations carry three timestamps:

- `observed_at` (always set): when the observation entered the
  store.
- `valid_from` (optional): when the fact began being true.
- `valid_until` (optional): when the fact stopped being true.

Recall filters out observations whose validity window does not
include `RecallFilters::valid_at` (defaults to "now" via the
store's `Clock`). This makes it safe to leave outdated facts in
the store: they survive but stop surfacing once `valid_until`
passes.

## Consolidation

`MemoryStore::consolidate(config)` (in
[`crates/openmemory-graph/src/consolidate.rs`](../crates/openmemory-graph/src/consolidate.rs))
runs two phases:

1. **Dedup.** For each entity, compute Jaccard text similarity
   (character n-gram based) between every observation pair newer
   than `min_age_secs`. Pairs above `dedup_text_threshold`
   (default 0.95) are merged: keep the older row, sum
   `access_count`, drop the duplicate.
2. **Decay-prune.** Score every observation with the Ebbinghaus
   formula above. Tombstone every observation below `prune_floor`.

Consolidation is **idempotent**: a second call right after the
first reports zero work. Run it on a schedule via the
`openmemory_consolidate` MCP tool or the `openmemory consolidate`
CLI subcommand.

## SearchMode reference

`SearchMode` is the public enum exposed via the index crate and
re-exported from the graph crate:

- `Hybrid` (default). RRF-fused vector + keyword. Best general-
  purpose mode.
- `VectorOnly`: alpha forced to 1.0. Best for "find me anything
  semantically similar even if the words don't match."
- `KeywordOnly`: alpha forced to 0.0. Best for exact-term recall
  ("find every observation that mentions `foo_bar()`").

The MCP tools accept `SearchModeParam { hybrid, vector_only,
keyword_only }` (snake_case wire shape).

## Fielded indexing (v0.3)

`openmemory_remember` accepts a fielded observation shape: callers can
attach an optional `title`, `summary`, `importance` (a ranking prior in
`[0.0, 1.0]`), `source_kind`, `concepts` (string array), and
`source_files` (string array) per observation. The FTS5 keyword
backend folds the fielded inputs into its single `text` column at index
time, repeating high-weight fields per the
`[search.field_weights]` config. Defaults bias `title` and
`entity_name`, and give `summary`, `concepts`, and `source_files`
medium weight:

```toml
[search.field_weights]
title = 5.0
text = 1.0
summary = 2.0
concepts = 2.0
source_files = 2.0
source_kind = 0.5
entity_type = 0.5
entity_name = 4.0
```

The vector backend ignores the fielded inputs and embeds the
`{entity_name}: {content}` body the way it did in v0.2.

## Measuring retrieval quality

`openmemory-eval` is the crate that runs canonical retrieval-quality
benchmarks against a fresh `MemoryStore`. It reports three metrics
on every dataset:

- **R@K (recall at K).** Fraction of relevant documents that appear
  in the top K results.
- **MRR (mean reciprocal rank).** Mean over queries of `1 / rank` of
  the first relevant hit (0 if none).
- **NDCG@K (normalised discounted cumulative gain at K).** Graded
  relevance weighted by position; perfect ranking is 1.0.

Run it via the `openmemory eval` subcommand (behind the `eval` build
feature). Hybrid and vector evals require the `embeddings` feature and
a downloaded model; run `openmemory model download` first. Keyword evals
do not need a model.

```bash
cargo build --release --features eval -p openmemory-cli
./target/release/openmemory eval \
    --dataset longmem-s \
    --dataset-path tests/fixtures/longmem-s \
    --mode hybrid \
    --report /tmp/report.json
```

Datasets are read as three JSONL files under `--dataset-path`:
`corpus.jsonl` (documents), `queries.jsonl` (queries), and
`judgments.jsonl` (one `{query_id, uri, relevance}` per line).

The CHANGELOG pins the current baseline on every release; an
ablation that regresses any metric by more than the noise floor
(documented inline with each release) is a hard stop.
