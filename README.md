# open-memory

Persistent agent memory and hybrid (vector + keyword) text search,
shipped as a single Rust binary and an MCP server. Designed to drop
into [OpenClaw](https://openclaw.ai) with one command:

```bash
open-memory integrate openclaw
```

> **Status:** v0.1.0 ships the eleven `open_memory_*` MCP tools, the
> CLI surface, and the OpenClaw integrator. CI is green on every
> commit.

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

See [`docs/02-openclaw-integration.md`](docs/02-openclaw-integration.md)
for the full schema.

## Build / test

```bash
cargo build --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

MSRV is **1.85.0** (pinned via `rust-toolchain.toml`).

## Documentation

See [`docs/00-overview.md`](docs/00-overview.md) for the project
pitch, [`docs/01-architecture.md`](docs/01-architecture.md) for the
crate layout, [`docs/02-openclaw-integration.md`](docs/02-openclaw-integration.md)
for the OpenClaw contract, [`docs/03-roadmap.md`](docs/03-roadmap.md)
for the build plan, and [`docs/04-quality-gates.md`](docs/04-quality-gates.md)
for the CI matrix.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE)
at your option.
