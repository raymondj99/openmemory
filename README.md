# openmemory

[![CI](https://github.com/raymondj99/openmemory/actions/workflows/ci.yml/badge.svg)](https://github.com/raymondj99/openmemory/actions/workflows/ci.yml)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://codspeed.io/raymondj99/openmemory)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE-APACHE)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange)](https://www.rust-lang.org)

Persistent memory for AI agents. A single Rust binary that gives any
[MCP](https://modelcontextprotocol.io)-compatible agent a knowledge graph,
hybrid (vector + keyword) search, and a free-text index. Works with
Claude Code, Claude Desktop, [OpenClaw](https://openclaw.ai), and any
MCP client.

## Install

Install the binary:

```bash
curl -fsSL https://raw.githubusercontent.com/raymondj99/openmemory/main/scripts/install.sh | bash
```

The installer does not modify Claude Code, Claude Desktop, Codex CLI,
or OpenClaw configuration. Run setup when you're ready to detect and
register installed MCP clients:

```bash
openmemory setup
```

The next session in your agent has all `openmemory_*` tools available.
Try it:

```
> remember that I prefer Rust over Python
> what do you remember about my language preferences?
```

**Already have the binary?** Run the same setup flow with:

```bash
openmemory setup
```

`setup` is idempotent. Re-run it any time to register newly-installed
clients or pick up upgrades.

**From source** (requires Rust 1.85+):

```bash
cargo install --locked --git https://github.com/raymondj99/openmemory.git openmemory-cli
openmemory setup
```

## Explicit, per-client commands

`openmemory setup` orchestrates these for you, but each is still
available for scripted or partial installs:

```bash
openmemory integrate claude-code      # Claude Code
openmemory integrate claude-desktop   # Claude Desktop
openmemory integrate codex            # Codex CLI
openmemory integrate openclaw         # OpenClaw
```

For semantic vector recall, run `openmemory model download` once to
cache the default local ONNX model. Without that cache, recall still
works in keyword-only mode. (Or pass `--with-model` to
`openmemory setup`.)

Drive the graph from the CLI:

```bash
openmemory remember Raymond \
  --entity-type person \
  --observation 'prefers Rust' \
  --observation 'maintains openmemory'

openmemory recall 'Rust' --limit 3 --json | jq .
openmemory list-entities
openmemory status
```

Manual configuration (when you want full control over the JSON/TOML
yourself) lives in [docs/integrations.md](docs/integrations.md).

## Features

- **Knowledge graph.** Entities, observations with temporal validity,
  and relations. Recall is scored with Ebbinghaus decay and spreading
  activation through relations.
- **Free-text index.** Store and search arbitrary text under URIs
  (`note://standup`, `file:///path/to/doc.md`) on the same hybrid engine
  as the graph.
- **Hybrid search.** Vector (ONNX, local CPU) + keyword (FTS5/BM25)
  fused via Reciprocal Rank Fusion.
- **MCP server.** Eleven `openmemory_*` tools over stdio (default) or
  [Streamable HTTP](docs/mcp.md#streamable-http-behind-mcp-http) with
  bearer-token auth.
- **Filesystem watcher.** `openmemory watch ~/notes` incrementally
  indexes changed files, BLAKE3-deduped.
- **Multi-agent concurrency.** Read-only WAL connection pool so
  parallel recall calls scale on multi-agent deployments.
- **Single binary.** ~8 MB default, ~18 MB with all features. SQLite
  under the hood; no external services.

## MCP tools

All tools are prefixed `openmemory_` and work over stdio or HTTP.

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
openmemory-core           (clock, config, error, migrations)
├── openmemory-index      (vector + FTS5 hybrid search engine)
├── openmemory-embed      (ONNX embeddings, optional)
│
└── openmemory-graph      (knowledge graph: entities, relations, recall)
    ├── openmemory-mcp    (MCP server + tool router)
    ├── openmemory-watch  (filesystem watcher)
    └── openmemory-cli    (the `openmemory` binary)
```

## Configuration

`openmemory init` creates `~/.openmemory/config.toml` with sensible
defaults. Most users never edit it.

| Knob | Where |
|------|-------|
| Search tuning (alpha, RRF k, max results) | `[search]` in `config.toml` |
| Decay rate, dedup threshold, prune floor | `[memory]` in `config.toml` |
| Data root override | `$OPENMEMORY_HOME` or `--home` |
| Memory profiles | `--profile <name>` |
| Bearer-token auth (HTTP transport) | `$OPENMEMORY_HTTP_TOKEN` |

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
| `mcp-http` | on | Streamable HTTP transport |

## Documentation

| Document | Summary |
|----------|---------|
| [Overview](docs/overview.md) | Goals, non-goals, project pitch |
| [Architecture](docs/architecture.md) | Workspace layout, threading model, design philosophy |
| [MCP reference](docs/mcp.md) | Tool schemas, transports, error codes |
| [Integrations](docs/integrations.md) | All supported MCP clients and config details |
| [OpenClaw integration](docs/openclaw.md) | OpenClaw-specific contract and config shape |
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

Licensed under [Apache 2.0](LICENSE-APACHE).
