# open-memory

> **Status:** v0.1.0 in progress. Planning docs only — no code yet.
> See [`docs/00-overview.md`](docs/00-overview.md).

`open-memory` is a Rust workspace that gives AI agents persistent
memory and hybrid (vector + keyword) text search behind a single
binary and an MCP server. It is a focused re-fork of the memory and
indexing subsystems of [sift](https://github.com/raymondj99/sift),
stripped of the file-scanning pipeline, the Claude Code hook plumbing,
the daemon, the HTTP REST surface, and other bloat that does not
belong in a memory backend.

The first-class consumer is [**OpenClaw**](https://openclaw.ai). A
clean install is one command:

```bash
open-memory integrate openclaw
```

After that, every `open_memory_*` MCP tool is available to any agent
running under OpenClaw.

## Why

Existing memory MCPs either (a) ship as Python services with non-trivial
ops cost, or (b) project memory through a file-shaped abstraction
designed for one specific vendor. `open-memory` is a single static
Rust binary, SQLite under the hood, no network at rest, and an MCP
tool surface designed for the open agent ecosystem rather than any
specific frontier model.

## What you get

- **Knowledge graph memory.** Entities, observations (with temporal
  validity), relations. Hybrid recall scored with Ebbinghaus decay.
- **Free-text URI index.** `index_text("note://…", body)` then search
  with the same hybrid engine.
- **MCP server.** Stdio always; Streamable HTTP behind a feature flag.
- **OpenClaw integration.** `open-memory integrate openclaw` writes
  the config entry and gets out of your way.
- **Single binary.** ~8 MB default, ~18 MB with everything.

## Documentation

The planning docs live under [`docs/`](docs/):

- [`00-overview.md`](docs/00-overview.md) — what this is and is not
- [`01-architecture.md`](docs/01-architecture.md) — crate layout,
  dependency graph, design decisions
- [`02-openclaw-integration.md`](docs/02-openclaw-integration.md) —
  the contract with OpenClaw: tool surface, config writing, paths
- [`03-commit-plan.md`](docs/03-commit-plan.md) — the
  commit-by-commit checklist for the v0.1.0 port
- [`04-source-mapping.md`](docs/04-source-mapping.md) — file-by-file
  ledger of what is ported, renamed, trimmed, or dropped from sift
- [`05-quality-gates.md`](docs/05-quality-gates.md) — CI matrix,
  hardening checklist, release process

## License

Dual-licensed under [MIT](LICENSE-MIT) or
[Apache 2.0](LICENSE-APACHE) at your option.

`open-memory` is derived in part from [`sift`](https://github.com/raymondj99/sift),
which is dual-licensed under the same terms.
