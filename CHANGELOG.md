# Changelog

All notable changes to `openmemory` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
(pre-1.0: minor bumps for breaking changes to the MCP tool surface,
SQLite schema, or public Rust API; patch bumps for fixes).

## [Unreleased]

Production-hardening pass on top of v0.2.0.

### Added

- **HTTP-transport bearer-token auth.** `openmemory_mcp::http::serve`
  now reads `OPENMEMORY_HTTP_TOKEN` from the environment; when set,
  every `POST /mcp` request must carry a matching `Authorization:
  Bearer <token>` header or it gets a 401 response with
  `WWW-Authenticate: Bearer` and a JSON-RPC `-32600` envelope. The
  comparison is constant-time over the byte payload; the new
  `BearerToken` type's `Debug` impl redacts the secret. `/healthz`
  remains exempt so external liveness probes keep working without
  the token. With the env var unset, the server logs a warning and
  serves unauthenticated, preserving the local-dev workflow.
- **SHA-256 model integrity verification.**
  `openmemory_embed::OnnxEmbedder::load_for_model` hashes the
  on-disk `model.onnx` and `tokenizer.json` files before handing
  them to the ONNX runtime; mismatches surface as
  `EmbedError::ChecksumMismatch` with a "refusing to load" message.
  The new `integrity` module + `verify_sha256` function stream
  files in 64 KiB heap-allocated blocks, normalise expected hex
  case-insensitively, and treat empty hashes as
  `VerificationOutcome::Skipped` (warns, loads anyway).
  Both shipped models have recorded hashes.
- **Entity normalization on the remember write path.** Fuzzy-matches
  incoming entity names against existing entities of the same type
  before creating duplicates. Three configurable thresholds
  in `[normalization]`: auto-merge (>= 0.95), flag with `SAME_AS`
  relation (0.85-0.95), or create new entity (< 0.85). Enabled by
  default; disable with `normalization.enabled = false`.
- **CI gates.** `cargo test --workspace --no-default-features`,
  `cargo clippy --workspace --no-default-features --all-targets --
  -D warnings`, and `cargo doc --workspace --no-deps --all-features`
  now run on every push. The first two would have caught
  `openmemory-watch`'s feature-gated import bug; the third catches
  intra-doc links that resolve only when an optional module
  compiles.
- **Explicit embedding-model management.** `openmemory model list`
  reports every registry model and cache status; `openmemory model download [MODEL]`
  downloads `model.onnx`, `tokenizer.json`, and (when the model uses
  ONNX external data format) `model.onnx_data` into the shared
  `~/.openmemory/models/<model>/` cache. The MCP server and
  scriptable `remember` / `recall` commands load the cached default
  model when present, and otherwise run keyword-only without touching
  the network. Downloads stream to disk in 64 KiB chunks (no
  full-model heap allocation), enforce per-file size caps (500 MB for
  graph/tokenizer, 3 GB for external data), verify `Content-Length`
  before streaming, check SHA-256 against the registry hash after
  writing, retry transient failures, and write through a sibling
  `.part` file before the final rename.
- **`openmemory model use <name>`.** Switch the active embedding
  model. Writes `default.model` to `config.toml`; takes effect on
  the next process. Priority chain: `OPENMEMORY_MODEL` env var >
  `default.model` in config > registry default (nomic-embed-text).
- **Adaptive ONNX input tensors.** The embedder queries the model's
  declared inputs at load time and only passes `token_type_ids` when
  the model expects it. This lets the snowflake model (which omits
  that input) run without a hard-coded exception.

### Changed

- `mcp-http` is now a default feature in `openmemory-cli`, matching
  the sift binary's feature set. Builds that do not need HTTP
  transport can disable it with `--no-default-features`.
- `HybridSearchEngine::search` no longer takes a `chunk_index`
  parameter (breaking API change to the search engine trait).
- `openmemory-watch` Cargo.toml drops its own `default = ["fts5"]`
  feature and pulls `sqlite + fts5` directly from
  `openmemory-index` / `openmemory-graph`. The crate now compiles
  under `cargo test --workspace --no-default-features` (it
  previously failed because `SourceKind`, `SourceRecord`, and the
  `metadata` field on `OpenEngine` are sqlite-gated). Higher-level
  crates still gate the watcher behind their own optional feature.
- Concurrent-recall integration test uses a proportional noise guard
  (5% of serial estimate) instead of an absolute 20ms guard. The
  absolute guard flaked on fast machines with few cores where the
  serial estimate was small relative to scheduling noise.
- Hybrid search now treats missing embeddings as true keyword-only
  operation instead of inserting or searching empty vectors. This
  keeps keyword-only stores from assigning arbitrary vector ranks,
  and lets a profile created before model download accept real
  vectors later without a dimension mismatch.

### Fixed

- `openmemory_mcp::http::handle_mcp` no longer constructs the 204
  notification response via `Response::builder().unwrap()`; it
  returns `StatusCode::NO_CONTENT.into_response()` directly. The
  `application/json` content-type header is set via the infallible
  `HeaderValue::from_static`. No more panics on the request path.
- Entity normalization no longer auto-merges emoji-only,
  punctuation-only, or whitespace-only names solely because their
  alphanumeric-stripped forms are both empty. The normalization
  contract now has golden-table and property-test coverage for
  scoring ranges, threshold bucketing, symmetry, and Unicode safety.

## [0.2.0] - 2026-05-05

Phase 8 + Phase 9: multi-agent memory and a filesystem watcher.

### Added

- **Multi-agent memory.** `MemoryStore` now opens a pool of
  read-only `rusqlite::Connection`s alongside the writer mutex, so
  concurrent recall calls execute in parallel instead of
  serialising on a single mutex. WAL mode plus
  `OPEN_READ_ONLY | OPEN_NO_MUTEX` keeps the pool from blocking
  the writer (or vice versa). Pool size defaults to
  `Config::num_jobs()` (CPU count). The shared-writer fallback
  for `MemoryStore::open_in_memory` keeps the API uniform across
  on-disk and in-memory stores.
- **`MemoryStatus::reader_pool_size`** — surfaced through the
  `status` snapshot so callers can verify multi-reader concurrency.
- **`openmemory-watch` crate** — filesystem watcher with
  incremental re-indexing. `notify-debouncer-full` powers the
  event loop; `ignore` powers the initial-tree walk and a per-tree
  `.openmemory-ignore` custom-ignore filename. BLAKE3 deduplication
  against the existing `MetadataStore` makes a re-run over an
  unchanged tree free.
- **`openmemory watch <PATH>` CLI subcommand** with
  `--debounce-ms`, `--exts`, `--max-size`, `--no-initial-scan`
  flags. Behind a default-on `watch` build feature.
- **`[watch]` config section** (`debounce_ms`, `extensions`,
  `max_size`).
- **Concurrent recall integration test** that asserts no torn reads
  plus parallel time strictly less than the fully-serial bound. The
  numeric speedup floor that originally shipped here was relaxed
  in the production-hardening pass after it proved flaky on shared
  CI runners.
- **Watcher integration test** covering create / modify / delete /
  ignore-respect / dedup-on-restart, plus a latency smoke test that
  prints p50/p99 numbers for create / modify / delete.

### Changed

- Workspace version bumped to **0.2.0** to reflect the new public
  API surface (`MemoryStatus::reader_pool_size`, the
  `openmemory-watch` crate, the `[watch]` config section). The MCP
  tool surface is unchanged at v0.1; no MCP tool was added,
  renamed, or removed.
- `MemoryStore::open` now spins up `Config::num_jobs()` read-only
  Connections in addition to the writer connection.
- New workspace dependencies: `notify` 8, `notify-debouncer-full`
  0.7, `ignore` 0.4, `walkdir` 2. All four pin to versions whose
  `rust-version` metadata is ≤ 1.85, so MSRV stays at 1.85.

### Notes

- Runtime path filtering for the watcher honours always-ignore
  directories (`.git`, `target`, `node_modules`, …) and a small set
  of always-ignore globs (`*.lock*`). Per-tree `.openmemory-ignore`
  rules are honoured by the initial scan but not re-evaluated on
  every event — that's a v0.3 follow-up.
- The watcher currently has no graceful-shutdown signal hook on
  the CLI side; SIGINT / SIGTERM kills the process cleanly because
  every write goes through a SQLite transaction in WAL mode.

## [0.1.0] - 2026-05-05

The first end-to-end release: a persistent knowledge-graph memory
store + hybrid text index, exposed as eleven MCP tools, integrated
into OpenClaw with one command.

### Added

- **Crate `openmemory-core`** — `Clock` trait + `SystemClock` /
  `FixedClock`; `OmError` / `OmResult` thiserror enum; SQLite
  `Migrator` schema-versioning helper; `Config` loader / saver with
  `~/.openmemory/config.toml` and `OPENMEMORY_HOME` env override;
  exponential-backoff `with_retry` helper; `Embedder` trait +
  `FakeEmbedder` test double behind a `testing` feature.
- **Crate `openmemory-index`** — vector + keyword + hybrid search
  engine: `FlatVectorIndex` (brute-force cosine), optional
  `HnswIndex` (usearch, behind `hnsw`), `Fts5Store` (default) /
  `Bm25Store` (`--no-default-features`) keyword backends,
  `MetadataStore`, `HybridSearchEngine` with RRF fusion,
  `CachedSearchEngine` (50-entry LRU, 60s TTL), `open_engine`
  factory, criterion benches.
- **Crate `openmemory-embed`** — ONNX runner (CPU only), model
  registry with `nomic-embed-text-v1.5` (default, 768-dim) +
  `snowflake-arctic-embed-l-v2.0` (alternate, 1024-dim), SQLite
  embedding cache keyed by BLAKE3.
- **Crate `openmemory-graph`** — `MemoryStore` with bi-temporal
  `Entity` / `Observation` / `Relation` types, atomic `remember`
  write, hybrid `recall` with Ebbinghaus decay + spreading
  activation, `forget` (soft) / `forget_entity` (hard cascade) /
  `prune` (sweep tombstones + orphans), `consolidate` (dedup +
  decay-prune, idempotent). 89 unit tests + 14 integration tests.
- **Crate `openmemory-mcp`** — minimal hand-rolled JSON-RPC 2.0
  MCP server (no `rmcp` dependency: every published version of
  rmcp 0.13+ requires rustc 1.88+). Eleven `openmemory_*` tools
  registered through a single `Tool` trait; stdio transport always
  available; Streamable HTTP transport behind `mcp-http`.
- **Crate `openmemory-cli`** — `openmemory` binary with `init`,
  `status`, `mcp`, `consolidate`, `integrate openclaw`, plus
  scriptable `remember` / `recall` / `list-entities` /
  `forget-entity` and shell `completions`.
- **End-to-end MCP test** that spawns the real binary and exercises
  every tool over stdio JSON-RPC.

### Notes

- **rmcp dependency.** openmemory does not depend on the upstream
  `rmcp` Rust SDK in v0.1. Every published rmcp release uses
  `if-let` chain syntax that requires Rust 1.88+; we pin MSRV to
  1.85 and ship a hand-rolled JSON-RPC 2.0 server in
  `openmemory-mcp`. The Tool / ToolRouter shape mirrors rmcp
  closely; swapping upstream in once MSRV catches up is a
  mechanical change.
- **Default features.** `fts5`, `embeddings`, `completions`. Build
  with `--no-default-features` for a keyword-only, no-ONNX,
  no-completion-script binary.
- **OpenClaw config.** Always `~/.openclaw/openclaw.json` (or
  `$OPENCLAW_CONFIG_PATH`). The legacy `~/.openclaw/mcp.json`
  filename is intentionally not probed.

[Unreleased]: https://github.com/raymondj99/openmemory/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/raymondj99/openmemory/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/raymondj99/openmemory/releases/tag/v0.1.0
