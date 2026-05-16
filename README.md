# openmemory

[![CI](https://github.com/raymondj99/openmemory/actions/workflows/ci.yml/badge.svg)](https://github.com/raymondj99/openmemory/actions/workflows/ci.yml)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://codspeed.io/raymondj99/openmemory)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange)](https://www.rust-lang.org)

Persistent memory for AI agents. A single Rust binary that gives any
[MCP](https://modelcontextprotocol.io)-compatible agent a knowledge graph,
hybrid (vector + keyword) search, and a free-text index. Works with
Claude Code, Claude Desktop, [OpenClaw](https://openclaw.ai), and any
MCP client.

## Install

**Pre-built binary** (macOS, Linux):

```bash
curl -fsSL https://raw.githubusercontent.com/raymondj99/openmemory/main/scripts/install.sh | bash
```

**From source** (requires Rust 1.85+):

```bash
cargo install --locked --git https://github.com/raymondj99/openmemory.git openmemory
```

## Quick start

Pick your MCP client, then run one command to register `openmemory`:

```bash
openmemory init
openmemory integrate claude-code      # Claude Code
openmemory integrate claude-desktop   # Claude Desktop
openmemory integrate openclaw         # OpenClaw
```

That's it. The next session in your client has all `openmemory_*` tools
available. Try it:

For semantic vector recall, run `openmemory model download` once to
cache the default local ONNX model. Without that cache, recall still
works in keyword-only mode. Use `openmemory model list` to see
available models and cache status, and `openmemory model use <name>`
to switch the active model.

```
> remember that I prefer Rust over Python
> what do you remember about my language preferences?
```

Or drive the graph from the CLI:

```bash
openmemory remember Raymond \
  --entity-type person \
  --observation 'prefers Rust' \
  --observation 'maintains openmemory'

openmemory recall 'Rust' --limit 3 --json | jq .
openmemory list-entities
openmemory status
```

### Manual configuration

If you prefer to configure your client by hand, add this to its MCP
server config (the key path varies by client):

```json
{
  "openmemory": {
    "command": "openmemory",
    "args": ["mcp"]
  }
}
```

| Client | Config file | Key path |
|--------|-------------|----------|
| Claude Code | `~/.claude.json` | `mcpServers` |
| Claude Desktop | [platform-specific](docs/integrations.md#claude-desktop) | `mcpServers` |
| OpenClaw | `~/.openclaw/openclaw.json` | `mcp.servers` |

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

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE)
at your option.
