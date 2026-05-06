# Overview

`openmemory` is persistent agent memory plus hybrid (vector and
keyword) text search, packaged as a single Rust binary and an MCP
server. The first-class consumer is OpenClaw: a clean install of
`openmemory` should drop into `~/.openclaw/openclaw.json` and "just
work" for any agent running under OpenClaw, with no shell scripts,
no environment plumbing, and no vendor-specific assumptions.

## What problem this solves

Agents need a place to put facts. Without one, every conversation
starts cold; with one, the agent remembers user preferences,
project decisions, prior corrections, and references back to
earlier work.

A useful memory layer needs three properties at once:

1. **Structured memory.** Named entities with bounded observations
   and explicit relations. Otherwise recall reduces to
   string-matching against an unstructured blob.
2. **Free-text indexing.** Notes, transcripts, and scratchpads do
   not fit a graph, but they still need to be searchable on the
   same surface as the graph.
3. **A boring deployment story.** A single static binary with
   SQLite under the hood. No external services, no daemons beyond
   the MCP server, no network calls in the default install.

`openmemory` covers all three. The graph-side API (`remember`,
`recall`, `forget`, etc.) handles entities and observations; the
index-side API (`index_text`, `search`, `delete`) handles arbitrary
text under caller-supplied URIs; both ride the same hybrid search
engine internally.

## Headline features

- **Knowledge-graph memory.** Entities, observations with
  bi-temporal validity (`observed_at`, `valid_from`, `valid_until`),
  and directed relations. Hybrid recall scored with Ebbinghaus
  decay, optional spreading activation through relations to fill in
  related context. See [search.md](search.md).
- **Free-text URI index.** `index_text("note://...", body)` then
  search with the same hybrid engine. Mix structured graph memories
  with ad-hoc notes under one search surface.
- **MCP server.** Eleven `openmemory_*` tools served over stdio
  (always) and Streamable HTTP (behind the `mcp-http` feature). See
  [mcp.md](mcp.md).
- **OpenClaw integration.** `openmemory integrate openclaw` writes
  the config entry idempotently, JSON5-aware, and gets out of your
  way. See [openclaw.md](openclaw.md).
- **Filesystem watcher.** `openmemory watch DIR` walks a tree once
  on startup (BLAKE3-deduped against the metadata store), then tails
  `notify-debouncer-full` events to re-index only what changed.
  Behind a default-on `watch` feature. See [watcher.md](watcher.md).
- **Multi-agent concurrency.** A pool of read-only WAL connections
  alongside the writer mutex lets concurrent recall calls execute
  in parallel. See the threading model in
  [architecture.md](architecture.md).
- **Single static binary.** Default profile around 8 MB; full
  profile (with `embeddings` and `mcp-http`) under 18 MB.

## Goals

1. **Persistent memory for OpenClaw agents.** Entities,
   observations, relations, hybrid recall with temporal validity
   and decay scoring.
2. **Drop-in indexing backend.** Any agent can `index_text(uri,
   content)` then `search(query)` over its own corpus. No file
   scanning required at the API level.
3. **Out-of-the-box OpenClaw integration.** `openmemory integrate
   openclaw` writes a working entry into OpenClaw's JSON5 config.
   The first run self-bootstraps SQLite plus, optionally, the
   embedding model.
4. **Production-ready Rust.** Workspace, feature flags, MSRV
   pinned at 1.85.0, `clippy::pedantic`, deterministic schema
   migrations, snapshot tests, criterion benches, cargo-deny, and
   dependabot.
5. **Single static binary.** `cargo install openmemory` or download
   a tarball. Default profile under 8 MB; full profile under 18 MB.
6. **Boring storage.** SQLite (with WAL and FTS5) for everything.
   No external services.

## Non-goals

The following are **explicitly out of scope**. Some may return
behind feature flags later; none blocked v0.2.

- **File scanning and file-format parsers.** No PDF, no DOCX, no
  PPTX, no email, no archive extraction. Callers feed text in via
  the `index_text` API or the MCP tool. The watcher reads plain
  text from a curated extension list.
- **AST-aware code chunking.** Tree-sitter is not in scope. Callers
  chunk upstream if they need semantic boundaries.
- **HTTP REST API server.** MCP is the agent surface. Streamable
  HTTP exists only as an MCP transport.
- **Vendor-specific hook integrations.** `openmemory` does not
  parse Claude Code, Codex, or any other agent-runner hook
  payloads. Agents call the MCP tools directly.
- **Vendor-specific virtual-filesystem memory adapters** (e.g.
  Anthropic's `memory_20250818` shape). The MCP tool surface is the
  contract.
- **LLM-powered observation extraction.** v0.2 does not call out to
  any LLM provider. May return as an optional `llm` feature in a
  later release.
- **Vision and audio embeddings.** Text only.
- **Eval harnesses, fuzzers, performance corpora.** Criterion
  micro-benchmarks for hot paths *do* ship; broader perf and
  accuracy scaffolding does not.
- **Cross-architecture release pipelines beyond what is
  shipped.** v0.2 builds tarballs for `aarch64-apple-darwin`,
  `x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu`. Homebrew
  tap and `install.sh` are tracked for a future release.

## Status

| Version | Date | Headline |
|---------|------|----------|
| `[Unreleased]` | post-v0.2.0 | Production hardening: bearer-token auth on the HTTP transport, SHA-256 model integrity verification, and three new CI gates (`--no-default-features` test/clippy, default-features doc). |
| `v0.2.0` | 2026-05-05 | Multi-agent memory (read-only WAL connection pool) plus the `openmemory-watch` crate and `openmemory watch DIR` CLI subcommand. MCP tool surface unchanged at v0.1. |
| `v0.1.0` | 2026-05-05 | Initial release. Seven crates (core, index, embed, graph, mcp, cli) plus eleven MCP tools, one-command OpenClaw integration, Streamable HTTP behind `mcp-http`. |

See [`CHANGELOG.md`](../CHANGELOG.md) for the full per-release
detail.

## Workspace deliverables

| Crate | Purpose |
|-------|---------|
| `openmemory-core` | Clock trait, config loader, error types, SQLite migration helper, retry helper. |
| `openmemory-index` | Hybrid (vector + FTS5) search engine with RRF fusion, LRU cache, and a metadata store. |
| `openmemory-embed` | ONNX Runtime text embeddings with model registry, BLAKE3 cache, and SHA-256 integrity verification. Optional. |
| `openmemory-graph` | The knowledge-graph store: entities, observations, relations, and recall with decay scoring plus spreading activation. |
| `openmemory-mcp` | MCP server with the eleven `openmemory_*` tools, stdio transport always, optional Streamable HTTP behind `mcp-http`. |
| `openmemory-cli` | The `openmemory` binary. Tiny `clap` surface; dispatches to the other crates. |
| `openmemory-watch` | Filesystem watcher: initial-tree walk plus `notify-debouncer-full` event loop, BLAKE3-deduped against the metadata store. |

Per-crate detail lives in [crates.md](crates.md). The workspace
layout, crate dependency graph, and threading model live in
[architecture.md](architecture.md).
