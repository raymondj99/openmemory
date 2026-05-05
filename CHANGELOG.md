# Changelog

All notable changes to `open-memory` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
(pre-1.0: minor bumps for breaking changes to the MCP tool surface,
SQLite schema, or public Rust API; patch bumps for fixes).

## [Unreleased]

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
- **`open-memory-watch` crate** — filesystem watcher with
  incremental re-indexing. `notify-debouncer-full` powers the
  event loop; `ignore` powers the initial-tree walk and a per-tree
  `.open-memory-ignore` custom-ignore filename. BLAKE3 deduplication
  against the existing `MetadataStore` makes a re-run over an
  unchanged tree free.
- **`open-memory watch <PATH>` CLI subcommand** with
  `--debounce-ms`, `--exts`, `--max-size`, `--no-initial-scan`
  flags. Behind a default-on `watch` build feature.
- **`[watch]` config section** (`debounce_ms`, `extensions`,
  `max_size`).
- **Concurrent recall integration test** that asserts both correctness
  (no torn reads) and ≥2× speedup over the fully-serial bound.
- **Watcher integration test** covering create / modify / delete /
  ignore-respect / dedup-on-restart, plus a latency smoke test that
  prints p50/p99 numbers for create / modify / delete.

### Changed

- Workspace version bumped to **0.2.0** to reflect the new public
  API surface (`MemoryStatus::reader_pool_size`, the
  `open-memory-watch` crate, the `[watch]` config section). The MCP
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
  of always-ignore globs (`*.lock*`). Per-tree `.open-memory-ignore`
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

- **Crate `open-memory-core`** — `Clock` trait + `SystemClock` /
  `FixedClock`; `OmError` / `OmResult` thiserror enum; SQLite
  `Migrator` schema-versioning helper; `Config` loader / saver with
  `~/.open-memory/config.toml` and `OPEN_MEMORY_HOME` env override;
  exponential-backoff `with_retry` helper; `Embedder` trait +
  `FakeEmbedder` test double behind a `testing` feature.
- **Crate `open-memory-index`** — vector + keyword + hybrid search
  engine: `FlatVectorIndex` (brute-force cosine), optional
  `HnswIndex` (usearch, behind `hnsw`), `Fts5Store` (default) /
  `Bm25Store` (`--no-default-features`) keyword backends,
  `MetadataStore`, `HybridSearchEngine` with RRF fusion,
  `CachedSearchEngine` (50-entry LRU, 60s TTL), `open_engine`
  factory, criterion benches.
- **Crate `open-memory-embed`** — ONNX runner (CPU only), model
  registry with `nomic-embed-text-v1.5` (default, 768-dim) +
  `snowflake-arctic-embed-l-v2.0` (alternate, 1024-dim), SQLite
  embedding cache keyed by BLAKE3.
- **Crate `open-memory-graph`** — `MemoryStore` with bi-temporal
  `Entity` / `Observation` / `Relation` types, atomic `remember`
  write, hybrid `recall` with Ebbinghaus decay + spreading
  activation, `forget` (soft) / `forget_entity` (hard cascade) /
  `prune` (sweep tombstones + orphans), `consolidate` (dedup +
  decay-prune, idempotent). 89 unit tests + 14 integration tests.
- **Crate `open-memory-mcp`** — minimal hand-rolled JSON-RPC 2.0
  MCP server (no `rmcp` dependency: every published version of
  rmcp 0.13+ requires rustc 1.88+). Eleven `open_memory_*` tools
  registered through a single `Tool` trait; stdio transport always
  available; Streamable HTTP transport behind `mcp-http`.
- **Crate `open-memory-cli`** — `open-memory` binary with `init`,
  `status`, `mcp`, `consolidate`, `integrate openclaw`, plus
  scriptable `remember` / `recall` / `list-entities` /
  `forget-entity` and shell `completions`.
- **End-to-end MCP test** that spawns the real binary and exercises
  every tool over stdio JSON-RPC.

### Notes

- **rmcp dependency.** open-memory does not depend on the upstream
  `rmcp` Rust SDK in v0.1. Every published rmcp release uses
  `if-let` chain syntax that requires Rust 1.88+; we pin MSRV to
  1.85 and ship a hand-rolled JSON-RPC 2.0 server in
  `open-memory-mcp`. The Tool / ToolRouter shape mirrors rmcp
  closely; swapping upstream in once MSRV catches up is a
  mechanical change.
- **Default features.** `fts5`, `embeddings`, `completions`. Build
  with `--no-default-features` for a keyword-only, no-ONNX,
  no-completion-script binary.
- **OpenClaw config.** Always `~/.openclaw/openclaw.json` (or
  `$OPENCLAW_CONFIG_PATH`). The legacy `~/.openclaw/mcp.json`
  filename is intentionally not probed.

[Unreleased]: https://github.com/raymondj99/open-memory/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/raymondj99/open-memory/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/raymondj99/open-memory/releases/tag/v0.1.0
