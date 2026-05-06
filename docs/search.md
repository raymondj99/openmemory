# Search and recall

Hybrid search is the engine that powers both `openmemory_recall`
(graph) and `openmemory_search` (free-text URI store). It combines
a vector backend with a keyword backend, fuses their rankings via
Reciprocal Rank Fusion, and (for graph recall) re-scores with
Ebbinghaus forgetting-curve decay.

This document explains the math and the moving parts.

## The hybrid engine

`HybridSearchEngine<V, F>` (in
[`crates/openmemory-index/src/hybrid.rs`](../crates/openmemory-index/src/hybrid.rs))
is generic over a `VectorStore` and a `FullTextStore`. The active
backend pair is chosen at compile time by feature flags:

| Vector backend | When |
|----------------|------|
| `FlatVectorIndex` (brute-force cosine) | Default. Exact, O(n) per query. Sufficient for graph-sized data (<10⁶ vectors). |
| `HnswIndex` (usearch HNSW) | `--features hnsw`. Approximate, sub-linear per query. Adds a C++ build dep. |

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

The default `alpha = 0.6` slightly favours vector hits; tune via
`Config::search.hybrid_alpha`. `SearchMode::VectorOnly` short-circuits
to the vector backend (alpha = 1.0); `SearchMode::KeywordOnly`
short-circuits to keyword only (alpha = 0.0).

## Caching

`CachedSearchEngine` (in
[`src/cache.rs`](../crates/openmemory-index/src/cache.rs)) wraps
the hybrid engine with an LRU + TTL cache:

- `DEFAULT_CACHE_CAPACITY = 1000` entries.
- `DEFAULT_CACHE_TTL = 300` seconds.
- Cache key: `(query, mode, alpha, top_k, filter_hash)`.

A `MemoryStore` write invalidates the cache automatically (the
hybrid engine is rebuilt under the `RwLock<()>` rebuild barrier).

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

### Integrity verification

`OnnxEmbedder::load_for_model` calls
`integrity::verify_sha256(file, expected)` on both the `model.onnx`
and `tokenizer.json` files before handing them to the runtime.
Mismatches surface as `EmbedError::ChecksumMismatch` with a
"refusing to load" message.

The verification implementation streams files in 64 KiB
heap-allocated blocks, normalises expected hex case-insensitively,
and treats empty hashes as `VerificationOutcome::Skipped`. The
v0.2.0 registry uses empty hashes as a placeholder; populating
real hashes is tracked for v0.3 and tightens to "always verified"
with no further code changes.

### Cache

The `EmbeddingCache` (SQLite-backed by default; JSON fallback when
the `sqlite` feature is off) is keyed by `BLAKE3(content)` (hex).
A second `embed()` call on the same text is a cache hit, no model
inference.

### Recall fallback

When the `embeddings` feature is off (or the model has not finished
downloading on first run), `MemoryStore::recall` skips the vector
backend and runs keyword-only. The hybrid engine's `alpha` parameter
is overridden to 0.0 in that mode. Recall still works; it just
loses the semantic-similarity contribution.

## Recall scoring (graph only)

Once the hybrid engine returns a list of candidate observations,
`MemoryStore::recall` (in
[`crates/openmemory-graph/src/recall.rs`](../crates/openmemory-graph/src/recall.rs))
re-scores each with the Ebbinghaus forgetting curve plus
retrieval, correction, and confidence boosts:

```text
base_decay     = exp(-lambda * days_since_observed)
retrieval      = 1 + 0.15 * ln(1 + access_count)
correction     = source in {"correction", "cortex:correction"} ? 1.3 : 1.0
final_score    = search_score * base_decay * retrieval * correction * confidence
```

- `lambda` is the per-store decay rate. Default
  `Config::memory.decay_rate = 0.05` per day. Override per-store
  via `MemoryStore::with_decay_rate(rate)` for tests.
- `access_count` increments after each successful recall via the
  background `bump_access_counts` write. Frequently-recalled
  observations decay slower because the retrieval boost grows
  logarithmically with access count.
- `correction` flags observations the agent should not repeat; the
  `1.3` boost (`CORRECTION_RETRIEVAL_BOOST` constant) ensures they
  surface ahead of competing facts.
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
first reports zero work. Run it on a schedule (the Unreleased
`openmemory_consolidate` MCP tool exists for that).

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
