# open-memory

Persistent agent memory and hybrid (vector + keyword) text search,
shipped as a single Rust binary and an MCP server. Designed to drop
into [OpenClaw](https://openclaw.ai) with one command:

```bash
open-memory integrate openclaw
```

> **Status:** v0.2.0 adds multi-agent memory (a pool of read-only
> WAL connections so concurrent recalls run in parallel) and an
> incremental file-watcher (`open-memory watch DIR`) backed by
> `notify-debouncer-full`. v0.1.0 shipped the eleven `open_memory_*`
> MCP tools, the CLI surface, and the OpenClaw integrator; the MCP
> tool surface is unchanged at v0.1. CI is green on every commit.

## What you get

- **Knowledge graph memory.** Entities, observations with temporal
  validity, relations. Hybrid recall scored with Ebbinghaus decay,
  spreading activation through relations to fill in related context.
- **Free-text URI index.** `index_text("note://…", body)` then search
  with the same hybrid engine — mix structured graph memories with
  ad-hoc notes under one search surface.
- **MCP server.** Stdio always (the OpenClaw default); Streamable
  HTTP behind the `mcp-http` feature.
- **OpenClaw integration.** `open-memory integrate openclaw` writes
  the config entry idempotently and gets out of your way.
- **Single static binary.** ~8 MB default; ~18 MB with everything.

## Quick start

```bash
cargo install --path crates/open-memory-cli  # from a checkout
open-memory init
open-memory integrate openclaw

# then, from any OpenClaw agent:
#   "remember that I prefer Rust over Python"
#   "what do you remember about my language preferences?"
```

You can also drive the graph from the shell:

```bash
open-memory remember Raymond \
  --entity-type person \
  --observation 'prefers Rust' \
  --observation 'maintains open-memory'

open-memory recall 'Rust' --limit 3 --json | jq .
open-memory list-entities
open-memory consolidate
open-memory status
```

Or watch a directory and incrementally re-index changed files:

```bash
open-memory watch ~/notes --exts md,txt
# walks the tree once, then tails create/modify/delete events;
# BLAKE3-deduped against the metadata store, so a re-run over an
# unchanged tree is free.
```

## MCP tools

Eleven tools, all `open_memory_*`:

| Tool | Group | Type |
|------|-------|------|
| `open_memory_remember` | memory | write |
| `open_memory_recall` | memory | read |
| `open_memory_list_entities` | memory | read |
| `open_memory_get_entity` | memory | read |
| `open_memory_forget` | memory | destructive |
| `open_memory_forget_entity` | memory | destructive |
| `open_memory_status` | memory | read |
| `open_memory_index_text` | index | write |
| `open_memory_search` | index | read |
| `open_memory_delete` | index | destructive |
| `open_memory_consolidate` | maintenance | write |

See [`docs/mcp.md`](docs/mcp.md) for the full schema and tool
reference, and [`docs/openclaw.md`](docs/openclaw.md) for the
OpenClaw integration contract.

## HTTP transport

The default transport is stdio (matching what OpenClaw runs locally).
To serve the same MCP router over HTTPS for remote clients
(e.g. a [claude.ai connector] or a hosted OpenClaw instance), build
with the `mcp-http` feature and pass `--http`:

```bash
cargo build --release --features mcp-http
open-memory mcp --http 0.0.0.0:7800
```

The endpoint is `POST /mcp` (Streamable HTTP, JSON-RPC 2.0 envelope)
plus `GET /healthz` for load-balancer probes.

### Bearer-token auth

For anything bound to a non-loopback address, set
`OPEN_MEMORY_HTTP_TOKEN` before launching the server. Each `/mcp`
request must then carry a matching `Authorization: Bearer <token>`
header; missing or wrong tokens get a 401 with `WWW-Authenticate:
Bearer` and a JSON-RPC `-32600` error envelope. `/healthz` is never
auth-gated. With the env var unset (or empty), the server logs a
warning and serves unauthenticated — fine for `127.0.0.1`, never run
that on a public address.

```bash
export OPEN_MEMORY_HTTP_TOKEN="$(openssl rand -hex 32)"
open-memory mcp --http 0.0.0.0:7800
```

The token is compared in constant time against the `Authorization`
header value; the `BearerToken` type's `Debug` impl never logs the
secret.

[claude.ai connector]: https://docs.anthropic.com/en/docs/agents-and-tools/mcp

## Build / test

```bash
cargo build --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

MSRV is **1.85.0** (pinned via `rust-toolchain.toml`).

## Documentation

Start at [`docs/index.md`](docs/index.md) for the table of contents
and project summary. Notable entry points:

- [`docs/overview.md`](docs/overview.md): project pitch, goals, non-goals.
- [`docs/architecture.md`](docs/architecture.md): workspace layout,
  crate dependency graph, threading model.
- [`docs/crates.md`](docs/crates.md): per-crate API reference.
- [`docs/mcp.md`](docs/mcp.md): MCP server contract and the eleven
  `open_memory_*` tools.
- [`docs/openclaw.md`](docs/openclaw.md): OpenClaw integration
  contract.
- [`docs/search.md`](docs/search.md): hybrid search, RRF, Ebbinghaus
  decay scoring.
- [`docs/storage.md`](docs/storage.md): on-disk layout, SQLite
  schemas, migrations.
- [`docs/configuration.md`](docs/configuration.md): config file,
  env vars, profiles, feature flags.
- [`docs/cli.md`](docs/cli.md): CLI subcommand reference.
- [`docs/watcher.md`](docs/watcher.md): filesystem watcher.
- [`docs/development.md`](docs/development.md): build, test, lint,
  CI gates.
- [`docs/roadmap.md`](docs/roadmap.md): release history and backlog.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE)
at your option.
