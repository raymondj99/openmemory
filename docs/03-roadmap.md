# Roadmap

The original path to v0.1.0 is the eight phases below; v0.2.0 adds
two more phases for multi-agent memory and a filesystem watcher.
Every commit leaves the workspace green: `cargo check`,
`cargo test`, `cargo clippy --all-features`, and `cargo fmt --check`
all pass at every revision.

Each phase ends on an annotated tag (`v0.0.1-core`,
`v0.0.2-index`, …) so a bad phase reverts cleanly. The final commit
in each minor cuts `v0.1.0` / `v0.2.0`.

This document is the working checklist. Treat the format as a
contract: title, scope, files added/modified, verification gate. If
a commit grows past its scope, split it.

## Conventions

- **Conventional Commits** prefixes: `chore:`, `feat:`, `fix:`,
  `refactor:`, `docs:`, `test:`, `ci:`, `build:`, `perf:`. Scope is
  the crate: `feat(graph): MemoryStore::recall (hybrid search + decay)`.
- Every commit ends with
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.
- Verification is `cargo test --workspace --all-features` unless the
  commit notes otherwise. CI replays the matrix in
  [`04-quality-gates.md`](04-quality-gates.md).
- Files listed in each commit are the **only** files that commit
  touches. If you find yourself modifying something not on the list,
  stop and split.

---

## Phase 0 — Bootstrap

### Commit 1 — `chore: bootstrap workspace`

**Scope.** Get the repo to a buildable workspace. A fresh clone runs
`cargo check` cleanly with empty crate stubs. No code yet.

**Files.**
- `LICENSE-MIT`, `LICENSE-APACHE`
- `README.md`
- `.gitignore`, `.editorconfig`
- `rust-toolchain.toml` (channel = `1.82.0`)
- `rustfmt.toml`
- `clippy.toml` (workspace `disallowed-methods` list)
- `Cargo.toml` (workspace, members, shared dep versions, lints,
  release profile)
- `crates/open-memory-{core,index,embed,graph,mcp}/Cargo.toml`
- `crates/open-memory-{core,index,embed,graph,mcp}/src/lib.rs`
  (one-line doc comment + empty module list each)
- `crates/open-memory-cli/Cargo.toml`
- `crates/open-memory-cli/src/main.rs` (`fn main() {}`)
- `.github/workflows/ci.yml` (build + test + clippy + fmt + doc)

**Verify.** `cargo check --workspace`, `cargo build --release`, CI
green on first push.

### Commit 2 — `docs: planning documents (overview, architecture, integration, roadmap)`

**Scope.** Land the planning docs.

**Files.** `docs/00-overview.md` through `docs/04-quality-gates.md`.

**Verify.** `ls docs/` shows five files; markdown lints clean.

### Commit 3 — `chore: cargo-deny config + audit workflow`

**Scope.** Static-analysis baseline.

**Files.**
- `deny.toml` — license allowlist (MIT, Apache-2.0, BSD-3-Clause,
  ISC, Zlib, Unicode-3.0), advisory tracking config, deny GPL/AGPL.
- `.github/workflows/audit.yml` — `cargo deny check` + weekly
  schedule.
- `.github/dependabot.yml` — cargo + actions weekly.

**Verify.** `cargo deny check` clean locally; audit job green.

---

## Phase 1 — `open-memory-core`

### Commit 4 — `feat(core): clock trait + SystemClock + FixedClock`

**Scope.** `Clock` trait, real impl (`SystemClock`), test impl
(`FixedClock` behind the `testing` feature). Pure trait + Unix-time
helpers.

**Verify.** ≥3 unit tests; `cargo test -p open-memory-core`.

### Commit 5 — `feat(core): error types (OmError, OmResult)`

**Scope.** Top-level `thiserror`-derived error enum: SQLite, I/O,
schema migration, invalid input. No file-pipeline variants.

**Verify.** `cargo test -p open-memory-core`.

### Commit 6 — `feat(core): SQLite Migrator helper`

**Scope.** The schema-versioning helper used by every store crate.
Reads/writes a `_meta` table, applies forward migrations
idempotently, refuses future-version databases.

**Verify.** Migrate empty DB → v1 → v2; downgrade refuses to open.

### Commit 7 — `feat(core): config loader + default paths`

**Scope.** `Config` struct, TOML load/save, default paths
(`~/.open-memory/config.toml`, `OPEN_MEMORY_HOME` env override).
Sections: `[default]` (paths, jobs), `[search]` (hybrid alpha, max
results), `[memory]` (decay, consolidate interval), `[index]`
(chunk size, max chars).

**Verify.** Round-trip test: write defaults, reload, equal.

### Commit 8 — `feat(core): retry helper`

**Scope.** Exponential-backoff retry helper used by the MCP HTTP
transport and any flaky I/O. `with_retry`, `RetryConfig`, jitter.

**Verify.** Deterministic retry tests using `FixedClock`.

### Commit 9 — `feat(core): testing module (FixedClock, FakeEmbedder)`

**Scope.** Test doubles behind a `testing` feature. Downstream
crates opt in. The `Embedder` trait stub here is the same one
re-exported by `open-memory-embed`.

**Verify.** `cargo test -p open-memory-core --features testing`.

### Tag `v0.0.1-core`

Phase 1 done. Six feature commits, ~1,500 LOC.

---

## Phase 2 — `open-memory-index`

### Commit 10 — `feat(index): traits + error types`

**Scope.** `VectorIndex`, `FullTextStore`, `VectorStore` traits.
`IndexError` enum. No impls yet.

### Commit 11 — `feat(index): FlatVectorIndex (cosine similarity)`

**Scope.** Brute-force vector store. Insert / search / delete /
export. Sufficient for graph-sized data (<10⁶ vectors).

**Verify.** Round-trip + N-vector retrieval correctness; proptest.

### Commit 12 — `feat(index): MetadataStore (SQLite)`

**Scope.** The `sources` table — URI, content hash, size, type,
chunk_count, status, timestamps. WAL mode, busy timeout, WAL
checkpoint on shutdown. Migrations via `core::Migrator`. A `kind`
discriminator distinguishes graph observations from `index_text`
rows.

**Verify.** Round-trip + concurrent-reader test.

### Commit 13 — `feat(index): Fts5Store + Bm25Store fallback`

**Scope.** SQLite FTS5 backend behind the `fts5` feature (default).
Pure-Rust `Bm25Store` fallback for `--no-default-features`.

**Verify.** Featureful test matrix: `--features fts5`,
`--no-default-features`.

### Commit 14 — `feat(index): HybridSearchEngine (RRF fusion)`

**Scope.** Reciprocal Rank Fusion of vector + keyword results.
Configurable RRF k (default 60) and `alpha` weight. The core search
routine that both graph recall and index search call.

**Verify.** RRF correctness against hand-computed expected values.

### Commit 15 — `feat(index): LRU CachedSearchEngine wrapper`

**Scope.** 50-entry LRU with 60 s TTL wrapping the hybrid engine.
Cache key is `(query, mode, filters_hash)`.

**Verify.** Cache hit/miss + TTL eviction tests.

### Commit 16 — `feat(index): open_engine factory + lib.rs surface`

**Scope.** `open_engine(config, data_dir)` opens metadata + vector +
fulltext + cache and wires them together. Public `lib.rs` re-exports.

**Verify.** End-to-end: open engine, insert, search, recover after
reopen.

### Commit 17 — `feat(index): HnswIndex (optional, behind hnsw feature)`

**Scope.** usearch-backed HNSW. Off by default — `usearch` adds C++
build deps.

**Verify.** `cargo test -p open-memory-index --features hnsw`.

### Commit 18 — `bench(index): vector + hybrid search criterion benches`

**Scope.** Two criterion benches as smoke tests for "did we regress
10×?".

**Verify.** `cargo bench -p open-memory-index` runs both.

### Tag `v0.0.2-index`

Phase 2 done. Nine feature commits, ~3,500 LOC.

---

## Phase 3 — `open-memory-embed`

### Commit 19 — `feat(embed): Embedder trait + StubEmbedder + error types`

**Scope.** Trait + deterministic stub for tests. Pure plumbing.

### Commit 20 — `feat(embed): ONNX runner (CPU)`

**Scope.** Wrap `ort` for inference: load model, run forward pass,
mean-pool tokens, L2-normalize. CPU only.

**Verify.** Smoke test against a tiny model fixture
(committed under `tests/fixtures/`, ≤2 MB).

### Commit 21 — `feat(embed): model registry (nomic + arctic)`

**Scope.** Two models: `nomic-embed-text-v1.5` (default, 768-dim)
and `snowflake-arctic-embed-l-v2.0` (alternate, 1024-dim). Download
URLs + SHA-256 checksums + tokenizer config.

**Verify.** Offline test: registry lookup. Online test (gated by env
flag): download + verify checksum.

### Commit 22 — `feat(embed): SQLite embedding cache`

**Scope.** Cache table keyed by BLAKE3(content). Avoids re-embedding
identical text. Behind the `sqlite` feature (default on); JSON
fallback for `--no-default-features`.

**Verify.** Cache hit ratio test.

### Tag `v0.0.3-embed`

Phase 3 done. Four feature commits, ~1,500 LOC.

---

## Phase 4 — `open-memory-graph`

The heart of the project. Eight commits because the graph store
deserves the granularity.

### Commit 23 — `feat(graph): types (Entity, Observation, Relation, EntityType)`

**Scope.** Pure data types + serde. UUIDv7 minted with
`uuid::Uuid::now_v7()`. Bi-temporal observation model
(`observed_at`, `valid_from`, `valid_until`).

**Verify.** Serde round-trip tests; `EntityType::parse` /
`as_str` round-trip.

### Commit 24 — `feat(graph): SQLite schema + migrations`

**Scope.** Tables: `entities`, `observations`, `relations`,
`memory_meta`. WAL mode, busy timeout, foreign keys. Indexes on
common query patterns. Single v1 migration via `core::Migrator`.

**Verify.** Init on empty DB; idempotent re-init smoke test.

### Commit 25 — `feat(graph): MemoryStore::open + read paths`

**Scope.** Open the store (SQLite + index engine + optional
embedder). Read-only methods: `list_entities`, `get_entity`,
`status`. No writes yet.

**Verify.** Open empty store; list returns empty; status returns
zeros.

### Commit 26 — `feat(graph): MemoryStore::remember (write path)`

**Scope.** Atomic write: ensure entity, append observations and
relations, keep search index in sync via a transactional
`apply_search_sync_ops_with_recovery` helper. Vector rebuild
guarded by `RwLock<()>`.

**Verify.** Round-trip: remember → recall by entity name → same
record back.

### Commit 27 — `feat(graph): MemoryStore::recall (hybrid search + decay)`

**Scope.** Hybrid search delegated to `open-memory-index`; result
re-scored with Ebbinghaus decay
(`exp(-decay_rate * age_days)`), correction-tag boost, optional
spreading-activation through relations.

**Verify.** Decay test (one observation 30 days old, one fresh; fresh
wins on equal text). Spreading-activation test.

### Commit 28 — `feat(graph): forget + forget_entity + prune`

**Scope.** `forget` is soft-delete (mark observation tombstoned).
`forget_entity` is hard-delete with cascade. `prune` collects
orphaned tombstones older than the configured TTL.

**Verify.** Soft vs hard delete behavior; prune respects TTL.

### Commit 29 — `feat(graph): consolidate (dedup + decay/prune)`

**Scope.** Two-phase consolidation. Phase 1: dedup (text similarity
≥ 0.95 within an entity, optionally cosine ≥ 0.92 if embeddings
on). Phase 2: decay/prune (score every observation, prune those
below floor).

**Verify.** Idempotence test (consolidate twice = consolidate once).

### Commit 30 — `test(graph): integration suite (in-memory SQLite)`

**Scope.** Integration tests at the public-API layer. ≥10 tests
covering remember/recall/forget cycles, schema migration, decay
correctness, dedup correctness.

**Verify.** Suite green; runs in <2 s.

### Tag `v0.0.4-graph`

Phase 4 done. Eight feature commits, ~5,500 LOC.

---

## Phase 5 — `open-memory-mcp`

### Commit 31 — `feat(mcp): Tool trait + ToolRouter scaffolding`

**Scope.** The `Tool` trait, `ToolGroup` enum, registry. No tools
yet. Server bootstrap stub: a server with zero tools that answers
`initialize` and `tools/list` (returning empty).

**Verify.** `cargo run -p open-memory-cli -- mcp` and an MCP client
sees the empty tool list.

### Commit 32 — `feat(mcp): memory tools (remember, recall, list, get, forget, forget_entity, status)`

**Scope.** Seven memory tools. Each is a unit struct + `Tool` impl
+ input struct + handler.

**Verify.** `cargo test -p open-memory-mcp` (snapshot of tool list).
Black-box MCP round-trip via `tower::ServiceExt`.

### Commit 33 — `feat(mcp): index tools (index_text, search, delete)`

**Scope.** Three index tools. Hybrid search with optional URI
prefix filter, score threshold, mode override.

**Verify.** Round-trip via MCP: index a doc, search, delete.

### Commit 34 — `feat(mcp): consolidate tool + ServerHandler instructions`

**Scope.** The `open_memory_consolidate` tool (write, idempotent) +
the rendered `get_info` instructions block listing all tools by
group.

**Verify.** `tools/list` returns 11 tools. `get_info` text matches
golden file.

### Commit 35 — `feat(mcp): Streamable HTTP transport (optional)`

**Scope.** HTTP transport behind the `mcp-http` feature. Routes
under `/mcp`. Off by default.

**Verify.** Build with `--features mcp-http`. axum integration test
passes.

### Tag `v0.0.5-mcp`

Phase 5 done. Five feature commits, ~1,500 LOC.

---

## Phase 6 — `open-memory-cli`

### Commit 36 — `feat(cli): clap surface + status, init`

**Scope.** Bootstrap the binary. `clap` derive, top-level
subcommands. Implement `init` (create dirs + write default config)
and `status` (open everything read-only, print summary).

**Verify.** `open-memory init` then `open-memory status` prints
zero counts and the schema versions.

### Commit 37 — `feat(cli): mcp + consolidate subcommands`

**Scope.** `open-memory mcp` (start the MCP server, default stdio,
`--http` switches to HTTP). `open-memory consolidate` (run the
consolidation pipeline once and print the report).

**Verify.** `open-memory mcp` answers an `initialize` request from a
canned client.

### Commit 38 — `feat(cli): integrate openclaw subcommand`

**Scope.** **The defining v0.1 feature.** Resolve OpenClaw config
path, parse JSON5, add or update the `open-memory` MCP server entry
idempotently, write back. Print exactly what changed.

**Verify.** Five-case test matrix (no existing config; `mcp.json`
exists; `openclaw.json` exists with `mcp.servers` block; existing
`open-memory` entry to update; entry corrupt). All pass.

### Commit 39 — `feat(cli): remember/recall/forget-entity/list-entities/completions`

**Scope.** Scriptable command-line write and read; shell completions.

**Verify.** End-to-end: remember from CLI, recall from CLI, exit 0.

### Tag `v0.0.6-cli`

Phase 6 done. Four feature commits, ~600 LOC.

---

## Phase 7 — Quality

### Commit 40 — `test: end-to-end MCP test against the real binary`

**Scope.** Spawn `open-memory mcp` as a subprocess, talk to it via
stdio, exercise every tool. Closest thing to "an OpenClaw agent
calling our server."

**Verify.** Test runs in <10 s. Green on macOS + Linux.

### Commit 41 — `chore: hardening pass (audit, deny, msrv)`

**Scope.** Apply the production-hardening punch list from
[`04-quality-gates.md`](04-quality-gates.md). No feature additions.

**Verify.** `cargo deny check`, `cargo audit` clean.
`cargo clippy --all-features --all-targets -- -D warnings` clean.

### Commit 42 — `docs: README + CHANGELOG + per-crate rustdoc landing pages`

**Scope.** Public-facing documentation.

**Files.**
- `README.md` — full rewrite. Quick install, quick start, OpenClaw
  setup, link to `docs/`.
- `CHANGELOG.md` — `[Unreleased]` and `[0.1.0]` sections.
- `crates/*/src/lib.rs` — each gets a complete crate-level rustdoc
  block (`#![doc = …]`).

**Verify.** `cargo doc --workspace --no-deps --all-features` clean,
no missing-docs warnings on public items.

### Commit 43 — `ci: release workflow (tagged builds, cargo publish dry-run)`

**Scope.** GitHub Actions release workflow. Builds platform
tarballs, runs `cargo publish --dry-run` on every crate.

**Verify.** Manual `workflow_dispatch` run produces three tarballs
(macos-aarch64, macos-x86_64, linux-x86_64).

### Tag `v0.1.0`

Workspace done. Cut a tag, run the release workflow, publish a
GitHub Release with binaries.

```
git tag -a v0.1.0 -m "open-memory v0.1.0"
git push origin v0.1.0
```

---

## Out-of-band: post-v0.1 work

Things explicitly **not** in the plan above, queued for v0.2:

- `cargo install open-memory --features llm` — opt-in LLM-driven
  observation extraction (Anthropic / OpenAI / Ollama).
- `homebrew-tap`, `install.sh`, `Cross.toml` for cross-arch builds.
- HNSW + SIMD distance functions enabled by default if benchmarks
  show they pay off below 10⁶ vectors.
- Postgres backend behind a `postgres` feature flag (community ask).

---

## Phase 8 — Multi-agent memory (v0.2.0)

Re-architect `MemoryStore` from a single `Mutex<Connection>` into
a writer mutex plus a pool of read-only WAL handles. Concurrent
recall calls (multiple agent processes hitting the same MCP server,
or a future "MCP server with embedded watcher" deployment) must not
block each other on read paths.

| Commit | Title |
|--------|-------|
| 44 | `feat(graph): readonly Connection pool helper` |
| 45 | `refactor(graph): route get_entity / list_entities through reader pool` |
| 46 | `refactor(graph): route recall through reader pool` |
| 47 | `refactor(graph): route status / observations / relations through pool` |
| 48 | `test(graph): concurrent recall invariant + speedup test` |
| 49 | `docs(graph): rustdoc concurrency model after read-pool refactor` |

**Invariants.** WAL mode + `synchronous=NORMAL` + `busy_timeout=5000`
already configured by Phase 4; kept untouched. Pool size defaults to
`Config::num_jobs()` (CPU count). The shared-writer fallback for the
in-memory store keeps `MemoryStore::open_in_memory` calling-equivalent
to `open` from a test author's perspective. The existing `RwLock<()>`
rebuild barrier stays as-is — it guards the *vector* index, not
SQLite.

**Verify.** `cargo test --workspace --all-features` green; the new
`integration_concurrent_recall_runs_in_parallel` test asserts ≥2×
speedup over the fully-serial bound.

---

## Phase 9 — File watcher (v0.2.0)

A new `open-memory-watch` crate plus an `open-memory watch <PATH>`
CLI subcommand. Walks the tree once on startup (BLAKE3-deduped),
then tails `notify-debouncer-full` events to re-index only what
changed.

| Commit | Title |
|--------|-------|
| 50 | `feat(watch): scaffold open-memory-watch crate` |
| 51 | `feat(watch): initial-tree indexer (BLAKE3 dedup + ignore matcher)` |
| 52 | `feat(watch): notify-debouncer-full event loop + Watcher::run` |
| 53 | `feat(cli): open-memory watch subcommand` |
| 54 | `test(watch): integration tests for create/modify/delete` |
| 55 | `docs: README + CHANGELOG entry + roadmap update` |

**Behaviour.**

- URI shape: `file://<canonical-absolute-path>`; one chunk per file
  (`chunk_index = 0`) for v0.2.
- BLAKE3 of file contents stored in the existing `MetadataStore`
  `sources` table. Re-run over an unchanged tree is free.
- `notify-debouncer-full` 0.7 (200ms default debounce, configurable).
- `ignore::WalkBuilder` for `.gitignore` / `.ignore` /
  `.open-memory-ignore` precedence; always-skip for `.git/`,
  `target/`, `node_modules/`, `.venv/`, `__pycache__/`, and
  `*.lock*` globs.
- The watcher takes an `Arc<MemoryStore>`, so a future
  `open-memory mcp --watch DIR` can share the MCP server's handle.

**Constraints.** MSRV stays 1.85 (notify-debouncer-full 0.7 ships
with that as its rust-version metadata; notify-debouncer-full 0.8
is in RC). No new global mutex. Tests synchronise on the
`BatchSummary` notifier channel — no `thread::sleep` in the test
body.

**Verify.** `cargo test -p open-memory-watch` green; the
integration suite covers create / modify / delete / dedup-on-restart
/ ignore-respect plus a latency smoke test that prints p50/p99 for
each operation.
