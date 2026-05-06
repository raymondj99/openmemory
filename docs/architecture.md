# Architecture

This document describes how the workspace is laid out, how the
crates depend on each other, the threading model that supports
concurrent reads alongside a single writer, and the design
philosophy that explains several decisions that look like
abstractions waiting to happen but are not.

## Workspace layout

```
openmemory/
├── Cargo.toml                  # workspace + shared deps + lints
├── Cargo.lock
├── rust-toolchain.toml         # MSRV pin (1.85.0)
├── rustfmt.toml                # max_width = 100, edition = 2021
├── clippy.toml                 # disallowed-methods list
├── deny.toml                   # cargo-deny licenses + advisories
├── README.md
├── CHANGELOG.md
├── CONTRIBUTING.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── docs/                       # you are reading this directory
│   ├── index.md
│   ├── overview.md
│   ├── architecture.md
│   ├── crates.md
│   ├── mcp.md
│   ├── openclaw.md
│   ├── search.md
│   ├── storage.md
│   ├── configuration.md
│   ├── cli.md
│   ├── watcher.md
│   ├── development.md
│   └── roadmap.md
├── .github/
│   └── workflows/
│       ├── ci.yml              # build/test/clippy/fmt/doc matrix
│       ├── audit.yml           # cargo-deny weekly + on push
│       └── release.yml         # tagged release tarballs
└── crates/
    ├── openmemory-core/       # clock, config, error, migrations, retry
    ├── openmemory-index/      # hybrid search engine: vector + FTS5 + RRF
    ├── openmemory-embed/      # ONNX embeddings (optional)
    ├── openmemory-graph/      # entity/observation/relation knowledge graph
    ├── openmemory-mcp/        # MCP server + tool router
    ├── openmemory-cli/        # binary `openmemory`
    └── openmemory-watch/      # filesystem watcher with incremental re-indexing
```

## Crate dependency graph

```
                   openmemory-core
                  ╱       │       ╲
            ┌────┘        │        └──────────┐
            ▼             ▼                    ▼
     openmemory-index    openmemory-embed (optional)
            │                 │
            └────────┐   ┌────┘
                     ▼   ▼
               openmemory-graph
                ╱       │       ╲
               ╱        │        ╲
              ▼         ▼         ▼
   openmemory-watch  openmemory-mcp
              ╲          │
               ╲         ▼
                ╲   openmemory-cli
                 ╲      ╱
                  ╲    ╱
                   ▼  ▼
              (watch is also a direct dep of cli for the
               `watch` subcommand)
```

Strict layering: there are no upward edges. `openmemory-core`
depends on no internal crate; `openmemory-cli` depends on every
other crate transitively.

The `openmemory-mcp` crate intentionally does **not** depend on
the upstream `rmcp` Rust SDK. Every published rmcp release uses
`if-let` chain syntax that requires Rust 1.88+; the workspace pins
MSRV to 1.85 and ships a hand-rolled JSON-RPC 2.0 server in
`openmemory-mcp::protocol`. The `Tool` and `ToolRouter` shapes
mirror rmcp closely; swapping upstream in once MSRV catches up is
a mechanical change.

## Crate responsibilities at a glance

| Crate | Owns | Depends on |
|-------|------|------------|
| `openmemory-core` | Clock, Config, OmError/OmResult, schema-migration helper, retry helper, test doubles. | (no internal crates) |
| `openmemory-index` | Vector + FTS5 backends, RRF hybrid engine, LRU cache, metadata store, `open_engine` factory. | `openmemory-core` |
| `openmemory-embed` | ONNX Runtime wrapper, two-model registry, BLAKE3 embedding cache, SHA-256 integrity verification. | `openmemory-core` |
| `openmemory-graph` | `MemoryStore`, entity/observation/relation types, atomic remember, hybrid recall with decay, forget/forget_entity/prune, consolidate (dedup + decay-prune). | `openmemory-core`, `openmemory-index`, `openmemory-embed` (optional) |
| `openmemory-mcp` | JSON-RPC 2.0 server, eleven `openmemory_*` tools, stdio transport, optional Streamable HTTP transport with bearer-token auth. | `openmemory-core`, `openmemory-index`, `openmemory-graph` |
| `openmemory-cli` | The `openmemory` binary with the eleven subcommands. | every crate above |
| `openmemory-watch` | Filesystem watcher: initial scan, debounced event loop, BLAKE3 dedup, ignore-file precedence. | `openmemory-core`, `openmemory-index`, `openmemory-graph` |

Per-crate API detail (every public type, trait, and feature flag)
lives in [crates.md](crates.md):

## Threading model

`openmemory` runs an MCP server, supports concurrent agents, and
serves a filesystem watcher that pushes new content into the same
store. Concurrency is intentional but bounded.

- **MCP server (stdio).** Single-threaded async (Tokio
  current-thread). Sufficient for the throughput one agent
  generates over stdio.
- **MCP server (HTTP).** axum on Tokio multi-thread. Each request
  goes through the same `OpenMemoryMcpServer::handle` path.
- **`MemoryStore` writer.** A single
  `Arc<Mutex<rusqlite::Connection>>` owns the writer connection.
  All `remember`, `forget`, `forget_entity`, `consolidate`, and
  `prune` paths take this mutex. SQLite is serial at the writer
  level anyway.
- **`MemoryStore` reader pool.** A `ReadPool` holds
  `Config::num_jobs()` (CPU count) read-only connections opened
  with `OPEN_READ_ONLY | OPEN_NO_MUTEX`. WAL mode lets these read
  through while the writer holds its mutex. `recall`,
  `list_entities`, `get_entity*`, and `status` route through the
  pool. This is what makes concurrent recalls scale on multi-agent
  deployments.
- **Vector index rebuild barrier.** A `RwLock<()>` guards the
  vector index. Writers grab the write lock during a vector
  rebuild; recall takes the read lock. This prevents recall from
  observing a half-rebuilt vector index during a bulk import. The
  barrier guards the *vector* index only; SQLite is already
  protected by WAL.
- **`MemoryStore::open_in_memory`.** The reader pool degrades to a
  shared-writer fallback so the API stays uniform across on-disk
  and ephemeral test instances.

The MCP layer bridges sync and async by wrapping store calls in
`tokio::task::spawn_blocking` where appropriate. There is no
async-all-the-way-down API; SQLite is sync, and adding async
plumbing on top would only buy ceremony, not throughput.

## Schema versioning

Every SQLite database the workspace owns carries a version row in
a `*_meta` table (`memory_meta`, `index_meta`, `embed_meta`). On
open, the `openmemory_core::migrations::Migrator` runs forward
migrations idempotently and **refuses** to open a database whose
version is higher than the binary supports. This prevents an older
binary from corrupting a newer database after a downgrade.

Schema versions are forward-only; downgrades are not supported.
Migrations live alongside the owning crate (e.g. graph migrations
in `openmemory-graph::schema`, index migrations alongside
`openmemory-index::metadata`). See [storage.md](storage.md) for
the per-database schema reference.

## Public-API stability

| Surface | Stability |
|---------|-----------|
| MCP tool names (`openmemory_*`) | Stable across minor versions. Renames require a major bump. |
| MCP tool input field names | Stable across minor versions. |
| SQLite schema versions | Forward-only. v1 always migrates to v2; the reverse never works. |
| OpenClaw config keys | Tracks OpenClaw's spec; we follow upstream changes there. |
| `~/.openmemory/data/<profile>/` directory layout | **Not** stable. Treat the data directory as opaque. |
| Public Rust crate APIs (any `pub` symbol) | **Not** stable. Pin patch versions. |
| Log line wording | **Not** stable. `OPENMEMORY_LOG=json` is. |

The project is pre-1.0. Minor bumps (`0.1 → 0.2`) signal breaking
changes to any stable surface. v1.0 ships when the MCP tool
surface, SQLite schemas, and CLI flag set have lived through at
least one major OpenClaw release without churn.

## Build profiles

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

Default features (what `cargo install openmemory` gives you):

```toml
default = ["fts5", "embeddings", "completions", "watch"]
```

Toggleable features (per crate detail in
[configuration.md](configuration.md)):

| Feature | Default | Effect |
|---------|---------|--------|
| `fts5` | on | SQLite FTS5 keyword backend with BM25 ranking. |
| `embeddings` | on (CLI), opt-in (graph) | ONNX Runtime + Nomic Embed v1.5. |
| `completions` | on | clap shell completion generation. |
| `watch` | on (CLI) | The `openmemory watch` subcommand and the watcher crate. |
| `hnsw` | off | usearch-backed approximate vector index. Adds a C++ build dep. |
| `mcp-http` | off | Streamable HTTP transport for the MCP server. |
| `simd` | off | Reserved. |

## Design philosophy

A small set of opinions explains most decisions in the codebase.

**SQLite is the storage layer.** Not "a storage layer." There is
no `MemoryBackend` trait abstracting SQLite from a hypothetical
Postgres or Redis backend. If somebody wants Postgres later, they
fork or write a feature-flagged alternative; we will not maintain
the abstraction speculatively.

**Embeddings have a trait, but only for testability.** The
`Embedder` trait in `openmemory-core::testing` exists so the
graph crate can substitute a deterministic stub during tests. It
is not a "pluggable provider" abstraction. The shipped
implementation is `OnnxEmbedder` in `openmemory-embed`; that is
the only real embedder the binary ever loads.

**The MCP tool surface is the contract.** The Rust crate API is
not. Library consumers should pin patch versions; the only stable
external surface is MCP plus the OpenClaw integration JSON shape
plus the `openmemory` CLI flag set.

**No async memory API.** SQLite is sync. The MCP layer bridges to
async via `spawn_blocking`. A fully-async memory API would just
add ceremony without throughput; recall calls already finish in
sub-millisecond wall time on the canonical hardware.

**Boring dependencies.** `tokio`, `rusqlite`, `serde`, `clap`,
`tracing`, `axum`. Two opinionated picks: `usearch` for HNSW (only
when the `hnsw` feature is enabled) and `ort` for ONNX (only when
`embeddings` is enabled). Both gate behind features so the default
build has no C/C++ toolchain dependency.

**No data exfiltration in default logs.** Observation content is
never logged at INFO; only at DEBUG. Logging defaults hide values
and show counts.

**No surprise outbound network calls.** The only outbound HTTP in
the default build is the on-demand model download, which is gated
by the `embeddings` feature and only fires on a first-run state.
The bearer-token comparison is constant-time over the byte
payload, and the `BearerToken` type's `Debug` impl never logs the
secret.

## What is intentionally not abstracted

- **No "memory backend" trait.** SQLite is the backend.
- **No "embedding provider" trait abstraction over remote vs.
  local.** ONNX is the implementation; the `Embedder` trait exists
  for tests only.
- **No "transport" trait.** stdio and Streamable HTTP each call
  `OpenMemoryMcpServer::handle` directly. The two transports do
  not share an abstract Tower-style middleware tower; the HTTP
  transport adds bearer-token auth in `is_authorized` and the
  stdio transport does not.
- **No "tool plugin" loading.** All eleven MCP tools are
  registered in one place,
  [`crates/openmemory-mcp/src/tools/mod.rs`](../crates/openmemory-mcp/src/tools/mod.rs),
  via `build_router()`. Adding a new tool is a one-line change to
  that registry; loading external tools at runtime is not a goal.
- **No async memory API.** See above.

If you find yourself wanting one of these abstractions, ask whether
the second consumer actually exists. So far, the answer has been
"no" each time, and we have kept the code shorter for it.
