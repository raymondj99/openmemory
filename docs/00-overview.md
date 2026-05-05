# open-memory — overview

`open-memory` is a Rust workspace that provides persistent agent memory and
hybrid (vector + keyword) text search behind a single binary and an MCP
server. It is a clean re-fork of the memory and indexing subsystems of
[sift](https://github.com/raymondj99/sift), stripped of the file-scanning
pipeline, the Claude Code / Codex hook plumbing, the daemon, the HTTP
REST surface, and other bloat that does not belong in a focused memory
backend.

The first-class consumer is **OpenClaw**: a clean install of `open-memory`
should drop an entry into the `mcp.servers` block of
`~/.openclaw/openclaw.json` and "just work" for any agent running under
OpenClaw, with no shell scripts, no environment plumbing, and no
Anthropic-specific assumptions.

---

## Goals

1. **Persistent memory for OpenClaw agents.** Entities, observations,
   relations, hybrid recall with temporal validity and decay scoring.
2. **Drop-in indexing backend.** Any agent can `index_text(uri, content)`
   then `search(query)` over its own corpus — no file scanning required,
   no parsers shipped.
3. **Out-of-the-box OpenClaw integration.** `open-memory integrate openclaw`
   adds or updates `mcp.servers["open-memory"]` in
   `~/.openclaw/openclaw.json` (delegating to `openclaw mcp set` when
   available, falling back to atomic JSON5 edit-in-place otherwise).
   The first run self-bootstraps SQLite; the embedding model is fetched
   on demand only when embeddings are compiled in.
4. **Production-ready Rust.** Workspace, feature flags, MSRV pinned,
   `clippy::pedantic`, deterministic schema migrations, pinned snapshot
   tests, criterion benches, cargo-deny, dependabot, signed releases.
5. **Lean default binary, optional native features.** Default
   `cargo install open-memory` avoids C/C++ compilation: it links to
   the host SQLite through `rusqlite` without the bundled SQLite
   feature. This requires the host SQLite library at build/link time
   but does not compile SQLite from C. At startup, it probes FTS5,
   uses `Fts5Store` when available, and falls back to the pure-Rust
   `Bm25Store` when host SQLite lacks FTS5. The lean build answers
   `recall` / `search` keyword-only, reports `vector_used: false`, and
   exposes `fulltext_backend` in `status`. Users who want
   deterministic bundled SQLite can opt into `--features
   bundled-sqlite`, accepting SQLite C compilation.
   Hybrid vector recall is opt-in via
   `cargo install open-memory --features embeddings`, which links
   against the [`ort`](https://crates.io/crates/ort) crate. `ort`
   loads the platform's ONNX Runtime as a **dynamic** library at
   first MCP call; the embeddings build weighs ~14–18 MB on disk and
   pulls a ~180 MB model at first run.
6. **Boring storage.** SQLite for durable storage, FTS5 when the host
   SQLite provides it, pure-Rust BM25 fallback otherwise. No external
   services.

## Non-goals

The following are **explicitly out of scope** for the v0.1 port. Any of
these can be added later behind a feature flag, but they will not block
the initial release:

- **File scanning / file-format parsers.** No PDF, no DOCX, no PPTX,
  no email, no archive extraction. Callers feed text in via the
  `index_text` API or an MCP tool. (`sift-parsers`, `sift-sources`
  dropped entirely.)
- **AST-aware code chunking.** Tree-sitter is dropped. Callers chunk
  upstream if they need to. (`sift-chunker` dropped.)
- **HTTP REST API server.** MCP is the agent surface. Anything else is
  yagni until somebody asks for it. (`sift-server` dropped.)
- **Background daemon + filesystem watcher.** No long-running process
  beyond the MCP server itself; no `notify`, no file-change debouncing,
  no Unix socket. (`daemon.rs`, `watch.rs`, `serve.rs` dropped.)
- **Claude Code / Codex hook integration.** The `Episode`, hook payload
  parser, and `init-hooks` machinery are Claude-specific and do not
  belong in an OpenClaw-first project. (`episodes.rs`, `rules.rs`,
  `integrate codex`, `init-hooks` dropped.)
- **Anthropic `memory_20250818` file-shape adapter.** The `memory-tool`
  subcommand and `PageMutationPlan` machinery are dropped. OpenClaw
  consumes MCP tools directly; there is no need to project memory onto
  a virtual filesystem.
- **LLM-powered observation extraction.** Dropped from the core port to
  keep dependencies tight. May return as an optional `llm` feature in a
  later release; not in v0.1.
- **Vision and audio embeddings.** Text embeddings only. (`vision.rs`
  dropped from the embed crate; the `audio` parser dropped with the
  rest of `sift-parsers`.)
- **Tantivy fulltext backend.** SQLite FTS5 is preferred when the host
  library supports it; pure-Rust BM25 is the fallback. Tantivy stays
  out of v0.1 to avoid a heavier dependency surface.
- **Eval harness, fuzzers, benchmark corpora.** The Python eval suite
  under `evals/`, the `fuzz/` crate, and the `perf/` corpus generator
  do not ship in v0.1. Criterion micro-benchmarks for hot paths *do*
  ship.
- **Homebrew tap, install.sh, Cross.toml, Codecov, downstream release
  pipeline polish.** Comes after v0.1.0 lands; the v0.1 release is
  `cargo install` + a GitHub Actions release artifact.

## Inputs

- Source codebase: `~/sift` (sift v0.1.7, ~35K LOC across 10 crates).
  See `04-source-mapping.md` for the file-by-file mapping.
- Target: `~/open-memory` — fresh git repo, currently empty.
- Consumer: OpenClaw (`~/.openclaw/`, MCP config at `mcp.servers.*`
  in `~/.openclaw/openclaw.json`, see `02-openclaw-integration.md`).

## Deliverables (v0.1.0)

Six crates (down from sift's ten), one binary:

| Crate | Purpose | Approx. LOC ported |
|-------|---------|--------------------|
| `open-memory-core` | Clock, Config, errors, schema migrations | ~1,500 |
| `open-memory-index` | Runtime-probed FTS5, BM25 fallback, vector + RRF search | ~3,500 |
| `open-memory-embed` | ONNX Runtime embeddings (optional) | ~1,500 |
| `open-memory-graph` | Entity/Observation/Relation knowledge graph | ~5,500 |
| `open-memory-mcp` | MCP server (stdio + optional HTTP) | ~1,500 |
| `open-memory-cli` | CLI binary entry point | ~600 |

Total: ~14,000 LOC of well-tested Rust, vs sift's ~35K. The reduction
comes from dropping parsers, sources, chunker, server, hook glue, and
the Anthropic memory-tool adapter.

## How to read this plan

- `01-architecture.md` — crate boundaries, public API shape, dependency
  graph, key design decisions and where they diverge from sift.
- `02-openclaw-integration.md` — exact MCP tool surface, config writing,
  storage paths, first-run bootstrap.
- `03-commit-plan.md` — the actual commit-by-commit checklist. Every
  commit titled, scoped, and verified. **Read this last.**
- `04-source-mapping.md` — file-by-file mapping from `sift/crates/*` to
  `open-memory/crates/*`. Says exactly what is ported, renamed,
  simplified, or dropped.
- `05-quality-gates.md` — CI matrix, MSRV, clippy/rustfmt config,
  release process, security review checklist.
