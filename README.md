# open-memory

Persistent agent memory and hybrid (vector + keyword) text search,
shipped as a single Rust binary and an MCP server. Designed to drop
into [OpenClaw](https://openclaw.ai) with one command:

```bash
open-memory integrate openclaw
```

> **Status:** v0.1.0 in progress.

## What you get

- **Knowledge graph memory.** Entities, observations with temporal
  validity, relations. Hybrid recall scored with Ebbinghaus decay.
- **Free-text URI index.** `index_text("note://…", body)` then search
  with the same hybrid engine.
- **MCP server.** Stdio always; Streamable HTTP behind a feature
  flag.
- **OpenClaw integration.** `open-memory integrate openclaw` writes
  the config entry idempotently and gets out of your way.
- **Single static binary.** ~8 MB default, ~18 MB with everything.

## Quick start

```bash
# once v0.1.0 ships
cargo install open-memory
open-memory init
open-memory integrate openclaw

# then, from any OpenClaw agent:
#   "remember that I prefer Rust over Python"
#   "what do you remember about my language preferences?"
```

## Documentation

See [`docs/00-overview.md`](docs/00-overview.md) for the project
pitch, [`docs/01-architecture.md`](docs/01-architecture.md) for the
crate layout, and [`docs/03-roadmap.md`](docs/03-roadmap.md) for the
build plan.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE)
at your option.
