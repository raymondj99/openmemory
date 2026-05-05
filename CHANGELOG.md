# Changelog

All notable changes to `open-memory` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
(pre-1.0: minor bumps for breaking changes to the MCP tool surface,
SQLite schema, or public Rust API; patch bumps for fixes).

## [Unreleased]

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

[Unreleased]: https://github.com/raymondj99/open-memory/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/raymondj99/open-memory/releases/tag/v0.1.0
