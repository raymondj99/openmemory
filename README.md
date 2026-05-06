# open-memory

[![CI](https://github.com/raymondj99/open-memory/actions/workflows/ci.yml/badge.svg)](https://github.com/raymondj99/open-memory/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange)](https://www.rust-lang.org)

Persistent memory for AI agents. A single Rust binary that gives any
[MCP](https://modelcontextprotocol.io)-compatible agent a knowledge graph,
hybrid (vector + keyword) search, and a free-text index. Designed for
[OpenClaw](https://openclaw.ai); works with any MCP client.

## Install

```bash
# From source (requires Rust 1.85+)
cargo install --path crates/open-memory-cli

# Or build from a checkout
git clone https://github.com/raymondj99/open-memory.git
cd open-memory
cargo build --release
```

## Quick start

```bash
# 1. Initialize the data directory
open-memory init

# 2. Register with OpenClaw (one command, done)
open-memory integrate openclaw

# 3. Start using memory from any OpenClaw agent:
#    "remember that I prefer Rust over Python"
#    "what do you remember about my language preferences?"
```

Or drive the graph from the CLI:

```bash
open-memory remember Raymond \
  --entity-type person \
  --observation 'prefers Rust' \
  --observation 'maintains open-memory'

open-memory recall 'Rust' --limit 3 --json | jq .
open-memory list-entities
open-memory status
```

## Features

- **Knowledge graph.** Entities, observations with temporal validity,
  and relations. Recall is scored with Ebbinghaus decay and spreading
  activation through relations.
- **Free-text index.** Store and search arbitrary text under URIs
  (`note://standup`, `file:///path/to/doc.md`) on the same hybrid engine
  as the graph.
- **Hybrid search.** Vector (ONNX, local CPU) + keyword (FTS5/BM25)
  fused via Reciprocal Rank Fusion.
- **MCP server.** Eleven `open_memory_*` tools over stdio (default) or
  [Streamable HTTP](docs/mcp.md#streamable-http-behind-mcp-http) with
  bearer-token auth.
- **Filesystem watcher.** `open-memory watch ~/notes` incrementally
  indexes changed files, BLAKE3-deduped.
- **Multi-agent concurrency.** Read-only WAL connection pool so
  parallel recall calls scale on multi-agent deployments.
- **Single binary.** ~8 MB default, ~18 MB with all features. SQLite
  under the hood; no external services.

## MCP tools

All tools are prefixed `open_memory_` and work over stdio or HTTP.

| Tool | Purpose |
|------|---------|
| `remember` | Store entities, observations, and relations |
| `recall` | Semantic search over stored memory |
| `list_entities` | Browse entities by type |
| `get_entity` | Full record for one entity |
| `forget` | Soft-delete one observation |
| `forget_entity` | Hard-delete an entity and its data |
| `status` | Store statistics and health |
| `index_text` | Store free text under a URI |
| `search` | Hybrid search over indexed text |
| `delete` | Remove text by URI or prefix |
| `consolidate` | Deduplicate + decay-prune observations |

Full schemas and transport details in [`docs/mcp.md`](docs/mcp.md).

## Architecture

Seven workspace crates with strict layering:

```
open-memory-core           (clock, config, error, migrations)
├── open-memory-index      (vector + FTS5 hybrid search engine)
├── open-memory-embed      (ONNX embeddings, optional)
│
└── open-memory-graph      (knowledge graph: entities, relations, recall)
    ├── open-memory-mcp    (MCP server + tool router)
    ├── open-memory-watch  (filesystem watcher)
    └── open-memory-cli    (the `open-memory` binary)
```

## Configuration

`open-memory init` creates `~/.open-memory/config.toml` with sensible
defaults. Most users never edit it.

| Knob | Where |
|------|-------|
| Search tuning (alpha, RRF k, max results) | `[search]` in `config.toml` |
| Decay rate, dedup threshold, prune floor | `[memory]` in `config.toml` |
| Data root override | `$OPEN_MEMORY_HOME` or `--home` |
| Memory profiles | `--profile <name>` |
| Bearer-token auth (HTTP transport) | `$OPEN_MEMORY_HTTP_TOKEN` |

Full reference in [`docs/configuration.md`](docs/configuration.md).

## Build from source

```bash
cargo build --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

MSRV is **1.85.0** (pinned via `rust-toolchain.toml`).

### Feature flags

| Feature | Default | Effect |
|---------|---------|--------|
| `fts5` | on | SQLite FTS5 keyword backend |
| `embeddings` | on | ONNX Runtime local embeddings |
| `completions` | on | Shell completion generation |
| `watch` | on | Filesystem watcher |
| `hnsw` | off | usearch HNSW vector index |
| `mcp-http` | off | Streamable HTTP transport |

## Documentation

| Document | Summary |
|----------|---------|
| [Overview](docs/overview.md) | Goals, non-goals, project pitch |
| [Architecture](docs/architecture.md) | Workspace layout, threading model, design philosophy |
| [MCP reference](docs/mcp.md) | Tool schemas, transports, error codes |
| [OpenClaw integration](docs/openclaw.md) | Integration contract and config shape |
| [Search](docs/search.md) | Hybrid search, RRF, Ebbinghaus decay scoring |
| [Storage](docs/storage.md) | On-disk layout, SQLite schemas |
| [Configuration](docs/configuration.md) | Config file, env vars, profiles, feature flags |
| [CLI reference](docs/cli.md) | All subcommands and flags |
| [Watcher](docs/watcher.md) | Filesystem watcher internals |
| [Development](docs/development.md) | Build, test, lint, CI |
| [Roadmap](docs/roadmap.md) | Release history and backlog |

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the development workflow,
commit guidelines, and the hosted-Codespace walkthrough for testing the
HTTP transport.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE)
at your option.
