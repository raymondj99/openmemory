# Changelog

All notable changes to `openmemory` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
(pre-1.0: minor bumps for breaking changes to the MCP tool surface,
SQLite schema, or public Rust API; patch bumps for fixes).

## [Unreleased]

### Added

- **`openmemory setup` onboarding command.** Runs `init`, detects
  supported MCP clients, registers `openmemory` with each detected
  client, and verifies `openmemory mcp` boots by completing an
  `initialize` round trip. The installer now chains into this command
  after placing the binary on PATH.
- **Codex CLI integration.** `openmemory integrate codex` writes
  `~/.codex/config.toml` or `$CODEX_HOME/config.toml` under
  `[mcp_servers.<name>]` using `toml_edit`, preserving sibling tables,
  comments, and unrelated settings.

### Changed

- **MCP relation input aliases are now agent-friendly.**
  `openmemory_remember` relations now accept the canonical
  `to_entity` / `to_entity_type` shape that matches
  `openmemory_add_relation`, while preserving the existing
  `type` / `to` / `to_type` form. Entity tools also accept
  `entity_name` as an alias for `entity`.

### Fixed

- **macOS ONNX Runtime extraction.** v0.3.2 introduced bundled
  runtime installation via `openmemory model download`, but the tar
  extractor matched archive entries by literal string prefix. macOS
  ONNX Runtime tarballs prefix every entry with `./` while Linux
  ones do not, so on macOS no entries matched: nothing extracted,
  and then `ensure_dylib_symlink` wrote a broken
  `libonnxruntime.dylib` symlink, triggering the post-check
  `extraction completed but ...libonnxruntime.1.20.0.dylib is
  missing`. The matcher now strips a leading `./` before comparing,
  and `ensure_dylib_symlink` uses `symlink_metadata` so a broken
  symlink left by a prior failed run no longer poisons the retry
  with `File exists (os error 17)`. New tests build synthetic
  Linux- and macOS-shaped tarballs and assert both extract the
  versioned dylib and the unversioned symlink.

## [0.4.0] - 2026-05-17

### Added

- **`openmemory_add_relation` MCP tool.** Attach a relation
  (`supersedes`, `clarifies`, `depends_on`, …) between two existing
  entities resolved by `(name, type)`. Closes the post-hoc curator
  gap where the only way to add an edge was through
  `openmemory_remember`, which forced callers to write a dummy
  observation just to attach a relation. Both entities must already
  exist; missing entities surface as a typed `-32004` error rather
  than being silently created. New `MemoryStore::add_relation`
  method backs the tool.
- **`openmemory_promote_observation` MCP tool.** Move an observation
  between `memory_tier` values (`episodic` ↔ `semantic` ↔
  `procedural`) without rewriting its content. Use after a fact has
  survived consolidation, been accessed repeatedly, or earned
  promotion out of short-term storage. Returns `{ modified: bool,
  memory_tier: <new> }`; unknown or tombstoned observation ids
  return `{ modified: false }` rather than erroring. New
  `MemoryStore::set_observation_memory_tier` method backs the tool.

### Changed

- **`openmemory_remember` description now spells out its idempotency
  contract.** Observations are always **appended**, not deduplicated
  at write time. Calling twice with the same `entity` + identical
  `content` creates two parallel observation rows. Use
  `openmemory_consolidate` for periodic dedup, or
  `openmemory_recall` the proposed title and skip the write on a
  high-scoring hit. Relations follow the same append-only contract.

## [0.3.3] - 2026-05-17

### Fixed

- **`init_ort_dylib_path` now respects `--home`.** v0.3.2 installs the
  ONNX Runtime under `<home>/runtime/onnxruntime-<ver>/`, but the
  CLI's startup hook resolved `<home>` from `OPENMEMORY_HOME` /
  `~/.openmemory` only — not from the `--home` flag. Spawning
  `openmemory --home X mcp` from an integrating tool (e.g.
  `omdemos check --mode vector`) therefore missed the runtime install
  and panicked with `cannot open shared object file:
  libonnxruntime.so`. The startup hook now scans argv for
  `--home <path>` / `--home=<path>` and promotes the value into
  `OPENMEMORY_HOME` before the runtime path resolves. User-set
  `OPENMEMORY_HOME` and `ORT_DYLIB_PATH` still take precedence.

## [0.3.2] - 2026-05-17

### Added

- **`openmemory model download` now installs the platform-matched ONNX
  Runtime.** Previously the released binary was compiled with
  `ort = "load-dynamic"` but the tarball shipped no `libonnxruntime`,
  so every fresh install of vector or hybrid mode panicked with
  `cannot open shared object file: libonnxruntime.so`. The model
  downloader now fetches the Microsoft ONNX Runtime 1.20.0 release for
  the current target (linux x86_64/aarch64, macOS arm64/x86_64),
  verifies its SHA-256, extracts the `lib/` subtree into
  `<openmemory home>/runtime/onnxruntime-1.20.0/lib/`, and points the
  CLI at it at startup via `ORT_DYLIB_PATH`. Users who set
  `ORT_DYLIB_PATH` or `LD_LIBRARY_PATH` themselves take precedence;
  the new behavior never overwrites a user-supplied value.

### Fixed

- **Flaky `integration_concurrent_recall_runs_in_parallel` test.** The
  single-thread baseline already took the median of three runs, but
  the parallel measurement ran exactly once, so a single noisy CI
  invocation (cgroup throttling, runner contention) could fail the
  test even when reader parallelism was healthy. The parallel
  measurement now also runs three times and the assertion uses the
  minimum elapsed time, matching the single-thread noise filter; a
  real reader-serialisation regression still flunks every attempt.

## [0.3.1] - 2026-05-17

### Changed

- **Raised the embedding-model download cap from 500 MB to 1 GB.** The
  default `nomic-embed-text-v1.5` ONNX (547 MB) and the registered
  `snowflake-arctic-embed-l-v2.0` model both exceeded the previous
  cap, blocking `openmemory model download` on fresh installs. The new
  cap accommodates every model in the typed registry; `MAX_DATA_BYTES`
  for ancillary data files is unchanged at 3 GB.

## [0.3.0] - 2026-05-17

### Added

- **Fielded observation indexing.** `openmemory_remember` now accepts a
  detailed observation shape with optional `title`, `summary`,
  `importance`, `source_kind`, `concepts`, and `source_files` (the
  legacy bare-string shape still works). The FTS5 keyword backend
  weights matches across the new fields via the repetition-based
  `Config::search.field_weights` table (defaults bias `title` 5.0 and
  `entity_name` 4.0, with `summary` at 2.0). `summary` participates in
  keyword search, `importance` participates in recall scoring, and
  `openmemory_get_entity` returns every new field on each observation row.
- **Memory-store schema v2.** Forward-only migration adds four
  nullable columns to `observations` (`title`, `summary`,
  `importance`, `source_kind`) plus two new tables
  (`observation_concepts`, `observation_source_files`) with
  ON DELETE CASCADE. v1 stores upgrade in place on next open;
  observations missing the new fields surface as `null` /
  empty arrays.
- **`memory_tier` end-to-end.** The `memory_tier` filter on
  `openmemory_recall` now actually filters; previously the field was
  accepted but dropped on the floor. The new `memory_tier` field on
  `openmemory_remember` stamps every new observation with the
  caller-supplied tier (defaults to `episodic`). Every
  `openmemory_recall` result JSON now carries the source observation's
  tier as `memory_tier`. The column has existed in schema v1 since
  v0.2; this closes the read- and write-side gap.
- **Retrieval-quality evaluation harness.** New `openmemory-eval`
  crate ships a `Dataset` trait, a pure-function metrics module
  (R@K, MRR, NDCG@K), and an `EvalRunner` that ingests a corpus into
  a fresh `MemoryStore` and scores the configured queries. Two
  adapters (`longmem-s`, `coding-mem`) read a JSONL fixture tree.
  Drive it via the new `openmemory eval` CLI subcommand behind the
  optional `eval` build feature; `--report <path>` writes the JSON
  artifact, `--baseline <path>` prints per-metric deltas against a
  prior run. Hybrid and vector evals require a downloaded embedding
  model, and a non-gating `.github/workflows/eval.yml` downloads the
  default model before running the longmem-s adapter on every PR once
  fixtures land at `tests/fixtures/longmem-s`.

### Fixed

- **Hybrid search now actually runs on every documented path.** The
  `openmemory_search` and `openmemory_index_text` MCP tools and the
  `openmemory watch` ingestion loop previously bypassed the vector
  backend even with an embedding model loaded. They now pass the
  query vector into `HybridSearchEngine::search` and persist the
  document vector via `engine.insert`. The change is silent when no
  embedding model is loaded; hybrid mode continues to degrade to
  keyword-only without raising.
- **MCP `openmemory_search` no longer double-pumps `top_k * 3`.**
  The wrapper used to inflate `limit` before handing off to the
  hybrid engine, which already inflates internally. The duplicate
  inflation produced slightly more candidates than needed and
  inflated CPU on large indexes. Resolved by passing `limit`
  through unchanged.
- **MCP URI-prefix filters no longer hide lower-ranked matches.**
  `openmemory_search` now fetches enough candidates before applying
  `uri_prefix`, so a high-scoring result outside the requested URI
  subtree cannot consume the caller's entire `limit`.

## [0.2.1] - 2026-05-16

Production-hardening patch on top of v0.2.0. No MCP tool changes;
the Rust crate API is documented as unstable, so the additive
`NormalizeMatch` type and `[normalization]` config section, plus
the removal of the unused `chunk_index` parameter on
`HybridSearchEngine::search`, ship as patch-level.

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

[Unreleased]: https://github.com/raymondj99/openmemory/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/raymondj99/openmemory/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/raymondj99/openmemory/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/raymondj99/openmemory/releases/tag/v0.1.0
