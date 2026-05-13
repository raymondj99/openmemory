# Crates reference

This document is the per-crate reference for the seven workspace
members. Each section describes the crate's purpose, feature flags,
public API surface, and source-file map.

The Rust crate API is **not** part of the public-stability contract;
see [architecture.md](architecture.md#public-api-stability). The
file is here so that an agent (or a human) reading the source can
orient quickly.

Workspace conventions:

- All crates use Rust edition 2021 with MSRV 1.85.0.
- All crates share the workspace `version = "0.2.0"`.
- All crates inherit the workspace lints (clippy pedantic with a
  pragmatic allow-list, `unsafe_code = "warn"`).

## `openmemory-core`

**Purpose.** The thinnest possible foundation: trait abstractions
(clock, embedder), the shared error type, the config loader/saver,
and the `Migrator` schema-versioning helper used by every store
crate.

**Cargo.toml summary.**

- Description: "openmemory: shared foundations (clock, config,
  error, schema migrations)"
- Dependencies: `thiserror`, `rusqlite`, `serde`, `toml`, `rand`.
- Features: `testing` (gates the test doubles).

**Source files.**

- [`src/lib.rs`](../crates/openmemory-core/src/lib.rs): module
  declarations and re-exports.
- [`src/clock.rs`](../crates/openmemory-core/src/clock.rs):
  `Clock` trait, `SystemClock`, `FixedClock`.
- [`src/config.rs`](../crates/openmemory-core/src/config.rs):
  `Config` plus the section structs and load/save logic.
- [`src/error.rs`](../crates/openmemory-core/src/error.rs):
  `OmError`, `OmResult`.
- [`src/migrations.rs`](../crates/openmemory-core/src/migrations.rs): `Migrator`, the `Migration` trait.
- [`src/retry.rs`](../crates/openmemory-core/src/retry.rs):
  `with_retry`, `RetryConfig`.
- [`src/testing.rs`](../crates/openmemory-core/src/testing.rs):
  `Embedder` trait, `FakeEmbedder` (gated on `testing`).

**Key types.**

- `pub trait Clock: Send + Sync + 'static` with `now_secs()` and
  `now_millis()`.
- `pub struct SystemClock` (Copy) and `SystemClock::shared() ->
  Arc<dyn Clock>`.
- `pub struct FixedClock` for deterministic tests, with `new`,
  `advance`, `set`.
- `pub struct Config` with section structs `DefaultSection`,
  `SearchSection`, `MemorySection`, `IndexSection`, `WatchSection`.
  Methods include `home_dir()`, `config_path()`, `data_dir(profile)`,
  `load()`, `load_from(path)`, `save(path)`, `num_jobs()`.
- `pub enum OmError` with variants `Io`, `Sqlite`, `Config`,
  `InvalidInput`, `SchemaTooNew { current, max }`, `Migration { version, reason }`.
- `pub struct Migrator` with `new`, `apply`, `test`. `pub trait
  Migration { fn version() -> u32; fn up(&self, conn) -> OmResult<()>; fn down(&self, conn) -> OmResult<()>; }`.
- `pub async fn with_retry<F, Fut>(max_retries, base_delay_ms, max_delay_ms, f)`
  for exponential backoff with jitter.
- `pub trait Embedder: Send + Sync` with
  `embed(&self, &[&str]) -> Result<Vec<Vec<f32>>>`. Behind the
  `testing` feature on this crate; re-exported by
  `openmemory-embed`.

## `openmemory-index`

**Purpose.** The hybrid (vector + keyword) search engine. Text in
by URI, hybrid results out, ranked by Reciprocal Rank Fusion.
Pluggable backends behind feature flags but only one of each is
compiled at a time.

**Cargo.toml summary.**

- Description: "openmemory: hybrid (vector + FTS5) search backend"
- Default features: `fts5`, `sqlite`.
- Features:
  - `sqlite`: enables the SQLite metadata store.
  - `fts5`: SQLite FTS5 backend (requires `sqlite`).
  - `hnsw`: usearch-backed HNSW vector index.
  - `simd`: reserved.
  - `testing`: test helpers.
- Dependencies: `openmemory-core`, `thiserror`, `serde`,
  `serde_json`, `rusqlite` (bundled), `lru`, `usearch` (optional),
  `tracing`.
- Benches: `benches/vector_search.rs`,
  `benches/hybrid_search.rs` (criterion).

**Source files.**

- [`src/lib.rs`](../crates/openmemory-index/src/lib.rs): module
  declarations, type aliases that pick the active backend by feature.
- [`src/traits.rs`](../crates/openmemory-index/src/traits.rs):
  `IndexEntry`, `SearchResult`, `SearchMode`, `ExportEntry`, the
  `VectorStore` / `VectorIndex` / `FullTextStore` traits.
- [`src/flat.rs`](../crates/openmemory-index/src/flat.rs):
  `FlatVectorIndex` (brute-force cosine).
- [`src/hnsw.rs`](../crates/openmemory-index/src/hnsw.rs):
  `HnswIndex` (gated on `hnsw`).
- [`src/fts5.rs`](../crates/openmemory-index/src/fts5.rs):
  `Fts5Store` (gated on `fts5`).
- [`src/bm25.rs`](../crates/openmemory-index/src/bm25.rs):
  `Bm25Store` (used when `fts5` is off).
- [`src/hybrid.rs`](../crates/openmemory-index/src/hybrid.rs):
  `HybridSearchEngine` with RRF fusion.
- [`src/cache.rs`](../crates/openmemory-index/src/cache.rs):
  `CachedSearchEngine` with LRU + TTL.
- [`src/metadata.rs`](../crates/openmemory-index/src/metadata.rs): `MetadataStore` (URI + source tracking, gated on `sqlite`).
- [`src/engine.rs`](../crates/openmemory-index/src/engine.rs):
  `OpenEngine` bundle and `open_engine` factory.
- [`src/error.rs`](../crates/openmemory-index/src/error.rs):
  `IndexError`, `IndexResult`.

**Key types.**

- `pub struct IndexEntry { uri, text, chunk_index, vector }` with
  builder methods `with_vector`, `with_chunk_index`.
- `pub struct SearchResult { uri, text, chunk_index, score }`.
- `pub enum SearchMode { Hybrid, VectorOnly, KeywordOnly }`. Default
  is `Hybrid`.
- `pub trait VectorStore: Send + Sync { insert(...); search(query_vec, top_k); delete_by_uri(uri); count(); }`.
- `pub trait VectorIndex: VectorStore { save(path); export_all() -> Vec<ExportEntry>; }`.
- `pub trait FullTextStore: Send + Sync { insert(...); search(query, top_k); delete_by_uri(uri); flush(); }`.
- `pub struct HybridSearchEngine<V, F>` parameterised over vector
  and fulltext backends. Method:
  `search(query_vec, query_text, top_k, mode, alpha, rrf_k) -> Vec<SearchResult>`.
- `pub struct CachedSearchEngine<V, F>` wraps the hybrid engine
  with an LRU TTL cache. `DEFAULT_CACHE_CAPACITY = 1000`,
  `DEFAULT_CACHE_TTL = 300 s`.
- `pub fn open_engine(config, data_dir) -> IndexResult<OpenEngine>`
  wires metadata + vector + fulltext + cache from feature flags.
- `pub fn flush(engine) -> IndexResult<()>` persists vector and
  BM25 backends to disk. (FTS5 flushes via SQLite WAL checkpoint.)
- `pub struct MetadataStore` with `open`, `insert_entry`,
  `get_by_uri`, `list_by_source`, `checkpoint`. Tracks BLAKE3
  content hashes; the watcher uses these to skip unchanged files.

The on-disk layout under the data directory is documented in
[storage.md](storage.md):

## `openmemory-embed`

**Purpose.** Optional. Loads ONNX Runtime, runs Nomic Embed Text
v1.5 or Snowflake Arctic Embed L v2.0, and caches embeddings in
SQLite by content hash. When disabled at the call site, `recall`
falls back to keyword-only.

**Cargo.toml summary.**

- Description: "openmemory: ONNX Runtime text embeddings (optional)"
- Default features: `sqlite`.
- Features: `sqlite` (SQLite-backed cache), `testing`.
- Dependencies: `openmemory-core` (with `testing` for the
  `Embedder` trait), `thiserror`, `blake3`, `sha2`, `ort`,
  `tokenizers`, `ndarray`, `tracing`, `serde`, `serde_json`,
  `rusqlite` (optional).
- Tests: `tests/onnx_smoke.rs` exercises a real model load.

**Source files.**

- [`src/lib.rs`](../crates/openmemory-embed/src/lib.rs): module
  declarations and re-exports.
- [`src/traits.rs`](../crates/openmemory-embed/src/traits.rs):
  re-exports `Embedder` from `openmemory_core::testing`.
- [`src/models.rs`](../crates/openmemory-embed/src/models.rs):
  `Model`, `ModelRegistry`, plus the constants
  `NOMIC_EMBED_TEXT_V1_5` and `SNOWFLAKE_ARCTIC_EMBED_L_V2`.
- [`src/onnx.rs`](../crates/openmemory-embed/src/onnx.rs):
  `OnnxEmbedder`, `OnnxOptions`, `PoolingStrategy`.
- [`src/integrity.rs`](../crates/openmemory-embed/src/integrity.rs): `verify_sha256`, `VerificationOutcome`.
- [`src/cache.rs`](../crates/openmemory-embed/src/cache.rs):
  SQLite-backed `EmbeddingCache`.
- [`src/json_cache.rs`](../crates/openmemory-embed/src/json_cache.rs): JSON fallback `EmbeddingCache`.
- [`src/error.rs`](../crates/openmemory-embed/src/error.rs):
  `EmbedError`, `EmbedResult`.
- [`src/testing.rs`](../crates/openmemory-embed/src/testing.rs):
  `StubEmbedder`.

**Key types.**

- `pub struct Model { name, aliases, repo_id, dimensions, max_tokens, pooling, output_tensor, search_prefix, document_prefix, onnx_url, tokenizer_url, onnx_sha256, tokenizer_sha256 }`.
- `NOMIC_EMBED_TEXT_V1_5` (768-dim, mean-pooled, default).
- `SNOWFLAKE_ARCTIC_EMBED_L_V2` (1024-dim, CLS-pooled, alternate).
- `pub enum PoolingStrategy { MeanPooling, ClsPooling }`.
- `pub struct OnnxOptions { max_tokens, pooling, output_tensor }`.
- `pub struct OnnxEmbedder` with `load_for_model(model, cache_dir)`
  and `embed(&[&str])`. Loads the model file, verifies SHA-256 against
  the registered hash, refuses to load on a mismatch
  (`EmbedError::ChecksumMismatch`). The empty-hash placeholder used
  by v0.2.0 surfaces as `VerificationOutcome::Skipped` and gets
  promoted to a real check when registry hashes are populated in a
  future release.
- `pub struct EmbeddingCache` (sqlite or JSON) keyed by
  `BLAKE3(content)` (hex). Methods: `open(path)`, `get(hash)`,
  `insert(hash, embedding)`, `checkpoint()`.
- `pub fn verify_sha256(path, expected_hex) -> VerificationOutcome`,
  with variants `Ok`, `Mismatch { expected, actual }`, `Skipped`,
  `IoError(io::Error)`. Streams in 64 KiB blocks; case-insensitive
  hex comparison.
- `pub struct StubEmbedder` for tests (deterministic vectors).

## `openmemory-graph`

**Purpose.** The knowledge graph. Entities, observations, and
relations on top of SQLite, plus the hybrid recall engine kept in
lockstep by `MemoryStore`. This is the heart of the project.

**Cargo.toml summary.**

- Description: "openmemory: knowledge graph (entities, observations, relations)"
- Default features: `fts5`.
- Features: `sqlite`, `fts5` (requires `sqlite`), `hnsw`,
  `embeddings`, `testing`.
- Dependencies: `openmemory-core`, `openmemory-index`
  (no-defaults), `openmemory-embed` (optional), `serde`,
  `serde_json`, `thiserror`, `tracing`, `uuid` (v7), `rusqlite`,
  `blake3`, `tempfile`.
- Tests: `tests/integration.rs`.

**Source files.**

- [`src/lib.rs`](../crates/openmemory-graph/src/lib.rs): module
  declarations and re-exports (including `SearchMode` from the
  index crate).
- [`src/types.rs`](../crates/openmemory-graph/src/types.rs):
  `Entity`, `EntityType`, `Observation`, `Relation`, `MemoryTier`,
  `new_id()`.
- [`src/schema.rs`](../crates/openmemory-graph/src/schema.rs):
  `MEMORY_SCHEMA_VERSION`, the migration list.
- [`src/store.rs`](../crates/openmemory-graph/src/store.rs):
  `MemoryStore`, `MemoryStatus`, `EntityListRow`, `MEMORY_DB_FILE`.
- [`src/pool.rs`](../crates/openmemory-graph/src/pool.rs):
  `ReadPool`, the read-only WAL connection pool.
- [`src/normalize.rs`](../crates/openmemory-graph/src/normalize.rs): `NormalizeMatch`, `similarity()`, `find_best_match()`.
- [`src/remember.rs`](../crates/openmemory-graph/src/remember.rs): `ObservationInput`, `RelationInput`, `RememberOutcome`.
- [`src/recall.rs`](../crates/openmemory-graph/src/recall.rs):
  `RecallFilters`, `RecallResult`, decay constants.
- [`src/forget.rs`](../crates/openmemory-graph/src/forget.rs):
  `PruneReport`, `DEFAULT_TOMBSTONE_TTL_SECS`.
- [`src/consolidate.rs`](../crates/openmemory-graph/src/consolidate.rs): `ConsolidateConfig`, `ConsolidateReport`.
- [`src/error.rs`](../crates/openmemory-graph/src/error.rs):
  `MemoryError`, `MemoryResult`.

**Key types.**

- `pub fn new_id() -> String`. UUIDv7 (sortable by creation time).
- `pub struct Entity { id, name, entity_type, created_at, updated_at, confidence, source }`.
- `pub enum EntityType { Person, Project, Concept, Tool, Preference, Fact, Event, Location, Organization }`
  with `as_str()`, `parse(s)`, `all()`.
- `pub struct Observation { id, entity_id, content, observed_at, valid_from, valid_until, tombstoned, access_count, confidence, source, memory_tier }`.
- `pub enum MemoryTier { Episodic, Semantic, Procedural }`.
- `pub struct Relation { id, source_entity_id, target_entity_id, relation_type, created_at, confidence }`.
- `pub struct ObservationInput` with `new(content)`, `with_confidence`,
  `with_source`, plus optional `valid_from`, `valid_until`, `memory_tier`.
- `pub struct RelationInput { relation_type, target_name, target_type }`.
- `pub struct RememberOutcome { entity_id, entity_existed, observation_ids, relation_ids, normalized }`.
- `pub enum NormalizeMatch { AutoMerge { entity_id, score }, Flag { entity_id, score } }`.
- `pub struct RecallFilters` with optional `entity_type`,
  `valid_at`, `source`, `min_confidence`, `entity_names`, `mode`,
  `spreading_activation`.
- `pub struct RecallResult { observation, entity_name, entity_type, raw_score, score }`.
- `pub struct ConsolidateConfig { dedup_text_threshold, prune_floor, min_age_secs, decay_rate }`.
- `pub struct ConsolidateReport { duplicates_merged, observations_pruned, entities_removed }`.
- `pub struct MemoryStatus` (full counts plus
  `reader_pool_size`, `vector_count`, schema version, etc.).
- `pub struct ReadPool` with `open(db_path, num_slots)`,
  `shared_with_writer(Arc<Mutex<Connection>>)`, and a
  `with_reader<F>(f) -> R` closure entry point.

**`MemoryStore` API.** The full set of methods on the public store.

- `open(config, data_dir) -> MemoryResult<Self>`: on-disk store.
- `open_in_memory(config) -> MemoryResult<Self>`: ephemeral
  (tempdir, the read pool proxies the writer).
- `with_clock`, `with_decay_rate`, `with_embedder`: test hooks.
- `clock()`, `memory()`, `router()`: accessors.
- `remember(name, entity_type, &[ObservationInput], &[RelationInput], source) -> RememberOutcome`.
- `recall(query, top_k, filters) -> Vec<RecallResult>`.
- `forget(observation_id)` (soft-delete) and
  `forget_entity(entity_id)` (hard cascade).
- `consolidate(config) -> ConsolidateReport` (idempotent).
- `prune() -> PruneReport` (sweeps tombstones older than
  `DEFAULT_TOMBSTONE_TTL_SECS = 2_592_000` seconds, 30 days).
- `get_entity(name|id)`, `list_entities(filter, limit, offset)`,
  `get_entity_observations`, `get_entity_relations`.
- `status() -> MemoryStatus`.
- `bump_access_counts(...)`: post-recall update used by the
  retrieval-boost calculation.

The decay scoring math and the spreading-activation algorithm are
documented in [search.md](search.md):

## `openmemory-mcp`

**Purpose.** The MCP server. Eleven `openmemory_*` tools served
over stdio (always) and Streamable HTTP (behind the `mcp-http`
feature). The `Tool` trait colocates the JSON-Schema descriptor
and the dispatch handler so what `tools/list` advertises cannot
drift from what the router actually answers.

**Cargo.toml summary.**

- Description: "openmemory: MCP server exposing memory + index tools"
- Default features: `fts5`.
- Features: `sqlite`, `fts5`, `hnsw`, `embeddings`, `mcp-http`,
  `testing`.
- Dependencies: `openmemory-core`, `openmemory-index`,
  `openmemory-graph`, `schemars` (no `rmcp` dependency; see
  [architecture.md](architecture.md#crate-dependency-graph)),
  `tokio` (macros, rt, sync, io-*), `serde`, `serde_json`,
  `tracing`, `anyhow`, `thiserror`, `axum`/`tower`/`tower-http`
  (optional).

**Source files.**

- [`src/lib.rs`](../crates/openmemory-mcp/src/lib.rs):
  `OpenMemoryMcpServer`, the public re-exports, `PROTOCOL_VERSION`.
- [`src/protocol.rs`](../crates/openmemory-mcp/src/protocol.rs):
  JSON-RPC 2.0 framing: `JsonRpcRequest`, `JsonRpcResponse`,
  `JsonRpcError`, `Content`, `ServerCapabilities`, `ServerInfo`,
  `ToolDescriptor`, `ToolAnnotations`.
- [`src/params.rs`](../crates/openmemory-mcp/src/params.rs):
  wire-shape param enums (`EntityTypeParam`, `MemoryTierParam`,
  `SearchModeParam`).
- [`src/stdio.rs`](../crates/openmemory-mcp/src/stdio.rs): the
  `run_stdio_server` event loop.
- [`src/http.rs`](../crates/openmemory-mcp/src/http.rs): the
  Streamable HTTP transport, `BearerToken`, `BEARER_TOKEN_ENV =
  "OPENMEMORY_HTTP_TOKEN"`.
- [`src/tools/mod.rs`](../crates/openmemory-mcp/src/tools/mod.rs): `Tool` trait, `ToolGroup`, `ToolRouter`, `build_router`,
  `server_instructions`, plus shared annotation helpers.
- [`src/tools/memory.rs`](../crates/openmemory-mcp/src/tools/memory.rs): the seven memory tools.
- [`src/tools/index.rs`](../crates/openmemory-mcp/src/tools/index.rs): the three index tools.
- [`src/tools/maintenance.rs`](../crates/openmemory-mcp/src/tools/maintenance.rs): `openmemory_consolidate`.

**Key types.**

- `pub trait Tool` with associated consts `NAME`, `SUMMARY`, `GROUP`
  and methods `descriptor() -> ToolDescriptor`, `call(server, args) -> Result<CallToolResult, JsonRpcError>`.
- `pub enum ToolGroup { Memory, Index, Maintenance }`.
- `pub struct ToolRouter` with `len`, `is_empty`, `list_descriptors`,
  `names`, `call(server, params)`. Built via `pub fn build_router() -> ToolRouter`.
- `pub fn server_instructions() -> String` renders the human-readable
  tool index used by `initialize`.
- `pub struct OpenMemoryMcpServer` with `from_memory`, `open`,
  `config`, `memory`, `router`, `initialize_result`, `handle(req) -> Option<JsonRpcResponse>`.
- `pub const PROTOCOL_VERSION: &str = "2024-11-05"`.
- `pub async fn run_stdio_server(server)`.
- `pub struct BearerToken` (constant-time comparison; redacting `Debug` impl).
- `pub const BEARER_TOKEN_ENV: &str = "OPENMEMORY_HTTP_TOKEN"`.

The full MCP tool reference (names, schemas, error codes,
transports) lives in [mcp.md](mcp.md):

## `openmemory-cli`

**Purpose.** The `openmemory` binary. Tiny `clap` surface; no
business logic. Each subcommand is a small adapter to a function in
`commands/*` that does the real work.

**Cargo.toml summary.**

- Description: "openmemory: command-line interface"
- Binary name: `openmemory`.
- Default features: `fts5`, `embeddings`, `completions`, `watch`,
  `mcp-http`.
- Features: `sqlite`, `fts5`, `hnsw`, `embeddings`, `mcp-http`,
  `completions`, `watch`.
- Dependencies: every other workspace crate plus `clap`,
  `clap_complete` (optional), `serde`, `serde_json`, `anyhow`,
  `tracing`, `tracing-subscriber`, `json5`, `tokio`.
- Tests: `tests/mcp_e2e.rs` (end-to-end MCP server smoke).

**Source files.**

- [`src/main.rs`](../crates/openmemory-cli/src/main.rs):
  `fn main()` plus tracing init.
- [`src/cli.rs`](../crates/openmemory-cli/src/cli.rs): the clap
  command tree.
- [`src/commands/mod.rs`](../crates/openmemory-cli/src/commands/mod.rs): module wiring.
- [`src/commands/init.rs`](../crates/openmemory-cli/src/commands/init.rs): creates the data directory and config skeleton.
- [`src/commands/status.rs`](../crates/openmemory-cli/src/commands/status.rs): `MemoryStore::status` printer.
- [`src/commands/mcp.rs`](../crates/openmemory-cli/src/commands/mcp.rs): stdio or HTTP transport launcher.
- [`src/commands/consolidate.rs`](../crates/openmemory-cli/src/commands/consolidate.rs): one-shot consolidation.
- [`src/commands/integrate.rs`](../crates/openmemory-cli/src/commands/integrate.rs): JSON5 OpenClaw config writer.
- [`src/commands/scriptable.rs`](../crates/openmemory-cli/src/commands/scriptable.rs): `remember`, `recall`, `list-entities`, `forget-entity` (with
  `--json` for scripting).
- [`src/commands/completions.rs`](../crates/openmemory-cli/src/commands/completions.rs): shell completion generator (gated on `completions`).
- [`src/commands/watch.rs`](../crates/openmemory-cli/src/commands/watch.rs): watcher launcher (gated on `watch`).

The full per-subcommand flag reference lives in [cli.md](cli.md):

## `openmemory-watch`

**Purpose.** Filesystem watcher with incremental re-indexing. Walks
the tree once on startup (BLAKE3-deduped against the existing
metadata store), then tails `notify-debouncer-full` events to
re-index only what changed.

**Cargo.toml summary.**

- Description: "openmemory: filesystem watcher with incremental
  re-indexing"
- No optional features at the crate level. The watcher requires
  FTS5 and SQLite directly via the index/graph crates; higher-level
  crates gate inclusion behind their own `watch` feature.
- Dependencies: `openmemory-core`, `openmemory-index` (with
  `fts5`), `openmemory-graph` (with `fts5`), `notify`,
  `notify-debouncer-full`, `ignore`, `walkdir`, `blake3`,
  `thiserror`, `tracing`.
- Tests: `tests/integration.rs` (create/modify/delete, ignore
  rules, dedup-on-restart, p50/p99 latency smoke).

**Source files.**

- [`src/lib.rs`](../crates/openmemory-watch/src/lib.rs). `Watcher`,
  `WatchOptions`, public constants.
- [`src/scan.rs`](../crates/openmemory-watch/src/scan.rs): initial
  tree walk.
- [`src/events.rs`](../crates/openmemory-watch/src/events.rs):
  event mapping.
- [`src/index.rs`](../crates/openmemory-watch/src/index.rs):
  per-file processing: read, hash, dedup, index.
- [`src/runtime.rs`](../crates/openmemory-watch/src/runtime.rs):
  the debounced event loop.
- [`src/error.rs`](../crates/openmemory-watch/src/error.rs):
  `WatchError`, `WatchResult`.

**Key types.**

- `pub struct Watcher` with `new(memory, root, options)` and
  `run() -> WatchResult<()>`.
- `pub struct WatchOptions { debounce: Duration, extensions: Vec<String>, max_size: u64, initial_scan: bool }`,
  `from_config(config) -> Self`.
- Constants:
  - `pub const DEFAULT_EXTENSIONS: &[&str]`. `md`, `markdown`,
    `mdx`, `txt`, `org`, `rst`, `rs`, `py`, `js`, `ts`, `tsx`,
    `jsx`, `go`, `java`, `c`, `h`, `cpp`, `hpp`, `toml`, `yaml`,
    `yml`, `json`.
  - `pub const ALWAYS_IGNORE_DIRS: &[&str]`. `.git`, `target`,
    `node_modules`, `.venv`, `__pycache__`.
  - `pub const ALWAYS_IGNORE_GLOBS: &[&str]`. `*.lock`,
    `*.lockb`.
  - `pub const IGNORE_FILE_NAME: &str = ".openmemory-ignore"`.
  - `pub const SOURCE_TYPE_FILE_WATCHER: &str = "file_watcher"`
    (the source tag stamped on indexed entries).
- `pub fn path_to_uri(root: &Path, path: &Path) -> String`.
  `file://<canonical-absolute-path>`.
- `pub fn process_file(memory, root, path, options) -> ProcessOutcome`,
  `pub fn remove_path(memory, root, path) -> ProcessOutcome`.
- `pub enum ProcessOutcome { Indexed, Skipped(SkipReason), Error(WatchError) }`.
- `pub enum SkipReason { TooLarge, WrongExtension, Ignored }`.
- `pub struct ScanReport { files_indexed, files_skipped, files_errored }`.
- `pub struct BatchSummary { duration, events_processed, files_indexed, files_removed }`.

The watcher reuses the parent process's `Arc<MemoryStore>`, so a
future `openmemory mcp --watch DIR` mode can share the MCP
server's handle without opening a second SQLite connection. See
[watcher.md](watcher.md).
