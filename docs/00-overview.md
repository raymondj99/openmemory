# open-memory — overview

`open-memory` is a Rust workspace that gives AI agents persistent
memory and hybrid (vector + keyword) text search behind a single
binary and an MCP server.

The first-class consumer is **OpenClaw**: a clean install of
`open-memory` should drop into `~/.openclaw/mcp.json` and "just work"
for any agent running under OpenClaw, with no shell scripts, no
environment plumbing, and no vendor-specific assumptions.

---

## Goals

1. **Persistent memory for OpenClaw agents.** Entities, observations,
   relations, hybrid recall with temporal validity and decay scoring.
2. **Drop-in indexing backend.** Any agent can `index_text(uri, content)`
   then `search(query)` over its own corpus — no file scanning required.
3. **Out-of-the-box OpenClaw integration.** `open-memory integrate openclaw`
   writes a working entry into `~/.openclaw/mcp.json`. The first run
   self-bootstraps SQLite + (optional) embedding model.
4. **Production-ready Rust.** Workspace, feature flags, MSRV pinned,
   `clippy::pedantic`, deterministic schema migrations, snapshot
   tests, criterion benches, cargo-deny, dependabot.
5. **Single static binary.** `cargo install open-memory` or download a
   tarball. Default profile <8 MB; full profile <18 MB.
6. **Boring storage.** SQLite (with WAL + FTS5) for everything. No
   external services.

## Non-goals

The following are **explicitly out of scope** for v0.1. Each may be
added later behind a feature flag, but none blocks the initial
release:

- **File scanning and file-format parsers.** No PDF, no DOCX, no
  PPTX, no email, no archive extraction. Callers feed text in via the
  `index_text` API or an MCP tool.
- **AST-aware code chunking.** Tree-sitter is not in scope. Callers
  chunk upstream if they need semantic boundaries.
- **HTTP REST API server.** MCP is the agent surface.
- **Background daemon and filesystem watcher.** The MCP server is the
  only long-running process. No `notify`, no debounce loops, no Unix
  socket.
- **Vendor-specific hook integrations.** `open-memory` does not parse
  Claude Code, Codex, or other agent-runner hook payloads. Agents
  call the MCP tools directly.
- **Vendor-specific virtual-filesystem memory adapters** (e.g.
  Anthropic's `memory_20250818` shape). The MCP tool surface is the
  contract.
- **LLM-powered observation extraction.** v0.1 does not call out to
  any LLM provider. May return as an optional `llm` feature in v0.2.
- **Vision and audio embeddings.** Text only.
- **Eval harnesses, fuzzers, performance corpora.** Criterion micro-
  benchmarks for hot paths *do* ship; broader perf and accuracy
  scaffolding does not.
- **Homebrew tap, install scripts, cross-architecture release
  pipelines.** Comes after v0.1.0; the v0.1 release is `cargo install`
  plus a GitHub Actions release artifact.

## Deliverables (v0.1.0)

Six crates and one binary:

| Crate | Purpose |
|-------|---------|
| `open-memory-core` | Clock, Config, errors, schema migrations |
| `open-memory-index` | Vector + FTS5 + RRF hybrid search |
| `open-memory-embed` | ONNX Runtime embeddings (optional) |
| `open-memory-graph` | Entity/Observation/Relation knowledge graph |
| `open-memory-mcp` | MCP server (stdio + optional HTTP) |
| `open-memory-cli` | CLI binary entry point |

## How to read this plan

- [`01-architecture.md`](01-architecture.md) — crate boundaries, public
  API shape, dependency graph, key design decisions.
- [`02-openclaw-integration.md`](02-openclaw-integration.md) — exact
  MCP tool surface, config writing, storage paths, first-run
  bootstrap.
- [`03-roadmap.md`](03-roadmap.md) — the actual commit-by-commit
  build plan. Every commit titled, scoped, and verified. **Read this
  last.**
- [`04-quality-gates.md`](04-quality-gates.md) — CI matrix, MSRV,
  clippy/rustfmt config, release process, security review checklist.
