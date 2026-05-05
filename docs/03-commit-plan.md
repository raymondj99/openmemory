# Commit-by-commit port plan

The port is broken into **45 commits across 8 phases (Phase 0 through
Phase 7)**. Every commit is self-contained: the workspace builds and
tests pass at every commit. Phases end on a tag (`v0.0.1-core`,
`v0.0.2-index`, `v0.0.3-embed`, `v0.0.4-graph`, `v0.0.5-mcp`,
`v0.0.6-cli`) so a bad phase can be reverted as a unit. The final
commit cuts `v0.1.0`.

Per-phase commit counts (and where they land in the numbering):

| Phase | Crate / theme | Commits | Range | Tag |
|------:|---|---:|---|---|
| 0 | Repository bootstrap | 5 | 1–5 | — |
| 1 | `open-memory-core` | 6 | 6–11 | `v0.0.1-core` |
| 2 | `open-memory-index` | 9 | 12–20 | `v0.0.2-index` |
| 3 | `open-memory-embed` | 4 | 21–24 | `v0.0.3-embed` |
| 4 | `open-memory-graph` | 8 | 25–32 | `v0.0.4-graph` |
| 5 | `open-memory-mcp` | 5 | 33–37 | `v0.0.5-mcp` |
| 6 | `open-memory-cli` | 4 | 38–41 | `v0.0.6-cli` |
| 7 | Quality + release | 4 | 42–45 | `v0.1.0` |
|   | **Total** | **45** | | |

This file is the working checklist for the porter (human or agent).
Treat the format as a contract: title, scope, files added/modified,
verification gate. If a commit grows past its scope, split it. If
the totals above ever disagree with the per-commit headings below,
the headings win and this table is the bug.

## Conventions

- Conventional Commits prefixes: `chore:`, `feat:`, `fix:`, `refactor:`,
  `docs:`, `test:`, `ci:`, `build:`, `perf:`. **Scope is the crate**:
  e.g. `feat(graph): port Entity/Observation types`.
- Each commit has a `Co-Authored-By: Claude Opus 4.7 (1M context)`
  trailer.
- Verification is `cargo test --workspace --all-features` unless noted.
  CI replays the same matrix described in `05-quality-gates.md`.
- "From sift:" notes the **path under `~/sift/`** that the file is
  derived from. **Derived ≠ copied.** Every file is reviewed for:
  rename (`sift_*` → `om_*`, `Sift*` → `Om*`), trim (no Cortex hooks,
  no LLM, no parsers), and re-document (rustdoc rewritten for the new
  scope).

---

## Phase 0 — Repository bootstrap

### Commit 1 — `chore: initial commit (license, gitignore, readme)`

**Scope.** Make the repo bootable: license files, gitignore, a
top-level README announcing the project's intent. Nothing compiles
yet.

**Files.**
- `LICENSE-MIT` — verbatim MIT (from sift's LICENSE).
- `LICENSE-APACHE` — Apache 2.0 (Apache standard text).
- `README.md` — short one-pager. What it is, who it's for, status
  banner ("v0.1.0 in progress"), link to `docs/00-overview.md`.
- `.gitignore` — `target/`, `.DS_Store`, `*.swp`, `*.sqlite`,
  `*.sqlite-wal`, `*.sqlite-shm`, `.idea/`, `.vscode/`, `tmp/`.
- `.editorconfig` — copied from sift.

**Verify.** `git log --oneline | wc -l` = 1.

### Commit 2 — `docs: planning documents (overview, architecture, integration, mapping)`

**Scope.** Land the planning docs so future commits reference them.
This is the commit that lands `00-overview.md` through
`05-quality-gates.md` (this file included).

**Files.**
- `docs/00-overview.md`
- `docs/01-architecture.md`
- `docs/02-openclaw-integration.md`
- `docs/03-commit-plan.md`
- `docs/04-source-mapping.md`
- `docs/05-quality-gates.md`

**Verify.** `ls docs/` shows six files; markdown lints clean.

### Commit 3 — `build: workspace skeleton with empty crates`

**Scope.** The Cargo workspace and six empty crates. `cargo check`
succeeds. No code yet — every `lib.rs` is a doc-comment + an empty
module list.

**Files.**
- `Cargo.toml` — workspace, members, shared dependency versions
  (modeled on sift's, trimmed of unused deps), shared `[lints]`,
  release profile. `rusqlite` uses host SQLite by default; the
  `bundled-sqlite` feature is opt-in because it compiles SQLite C
  code.
- `Cargo.lock` — generated.
- `rust-toolchain.toml` — channel = "1.82.0".
- `rustfmt.toml` — copied from sift (`max_width = 100`, …).
- `clippy.toml` — workspace clippy config (banned methods list).
- `crates/open-memory-core/{Cargo.toml, src/lib.rs}` — stub.
- `crates/open-memory-index/{Cargo.toml, src/lib.rs}` — stub.
- `crates/open-memory-embed/{Cargo.toml, src/lib.rs}` — stub (feature
  off by default, so this builds without ONNX).
- `crates/open-memory-graph/{Cargo.toml, src/lib.rs}` — stub.
- `crates/open-memory-mcp/{Cargo.toml, src/lib.rs}` — stub.
- `crates/open-memory-cli/{Cargo.toml, src/main.rs}` — `fn main() {}`
  with a `#[bin] name = "open-memory"`.

**Verify.** `cargo check --workspace` is green. `cargo build --release`
produces a `target/release/open-memory` (zero-byte runtime, but the
linker succeeds).

### Commit 4 — `ci: build, test, fmt, clippy on push and PR`

**Scope.** GitHub Actions workflow. Three jobs: build+test (matrix on
ubuntu-latest, macos-latest), fmt (single job), clippy (single job
with `-D warnings`). Pinned action versions.

**Files.**
- `.github/workflows/ci.yml`
- `.github/dependabot.yml` (cargo + actions weekly)

**Verify.** Push triggers CI. All three jobs green.

### Commit 5 — `chore: editorconfig, denylist, deny.toml, audit workflow`

**Scope.** Static-analysis baseline. cargo-deny config, advisory audit
workflow. No new code paths.

**Files.**
- `deny.toml` — modeled on sift's. Allow MIT/Apache/MIT-0/BSD-3-Clause/
  ISC/Zlib/Unicode-3.0; deny GPL/AGPL.
- `.github/workflows/audit.yml` — `cargo deny check`, weekly schedule
  + on every PR.
- `CODEOWNERS` (optional, single-maintainer for now).

**Verify.** `cargo deny check` clean locally. Audit job green.

---

## Phase 1 — `open-memory-core`

### Commit 6 — `feat(core): clock trait + SystemClock + FixedClock`

**Scope.** Time abstraction. Identical contract to sift's `Clock`
trait: `now_secs() -> i64`. Two impls: real and test-double.

**Files.**
- `crates/open-memory-core/src/clock.rs` — derived from
  `sift/crates/sift-core/src/clock.rs` (150 LOC, ports verbatim with
  rename).
- `crates/open-memory-core/src/lib.rs` — re-export.

**Verify.** `cargo test -p open-memory-core` (≥3 unit tests).

### Commit 7 — `feat(core): error types (OmError, OmResult)`

**Scope.** Top-level error enum. Variants for SQLite, I/O, schema
migration, invalid input. `thiserror`-derived. **Rename**
`SiftError` → `OmError`.

**Files.**
- `crates/open-memory-core/src/error.rs` — derived from
  `sift/crates/sift-core/src/error.rs` (216 LOC). Trim:
  pipeline-stage variants (`Source`, `Parse`, `Chunk`) — none of these
  layers exist in open-memory.

**Verify.** `cargo test -p open-memory-core`.

### Commit 8 — `feat(core): SQLite Migrator helper`

**Scope.** The schema-versioning helper used by every store crate.
Reads/writes a `_meta` table, applies forward migrations idempotently,
refuses future-version databases.

**Files.**
- `crates/open-memory-core/src/migrations.rs` — derived from
  `sift/crates/sift-core/src/migrations.rs` (256 LOC). Ports verbatim
  with rename.

**Verify.** Existing migration tests from sift, ported.

### Commit 9 — `feat(core): config loader + default paths`

**Scope.** `Config` struct, TOML load/save, default paths
(`~/.open-memory/config.toml`, `OPEN_MEMORY_HOME` env override).

**Files.**
- `crates/open-memory-core/src/config.rs` — heavily trimmed from
  `sift/crates/sift-core/src/config.rs` (1247 LOC → ≤300 LOC). Drop:
  `[parsers]`, `[chunker]`, `[server]`, `[watch]`, `[memory]` sub-
  sections that referenced features we cut. Keep: `[default]` (paths,
  jobs), `[search]` (hybrid alpha, max results), `[memory]` (decay,
  consolidate interval), `[index]` (chunk size, max chars).
- `crates/open-memory-core/src/util.rs` — atomic_write, format_bytes
  (from sift).

**Verify.** Round-trip test: write default config, reload, equal.

### Commit 10 — `feat(core): retry helper`

**Scope.** Exponential-backoff retry helper used by the MCP HTTP
transport and (later) any flaky I/O. Pure copy from sift, no API
changes.

**Files.**
- `crates/open-memory-core/src/retry.rs` — derived from
  `sift/crates/sift-core/src/retry.rs` (233 LOC).

**Verify.** Sift's existing tests port unchanged.

### Commit 11 — `feat(core): Embedder trait + testing module`

**Scope.** The single workspace-owned `Embedder` trait plus test
doubles behind a `testing` feature. Downstream crates depend on this
trait; `open-memory-embed` implements and re-exports it later rather
than defining a second incompatible trait.

**Files.**
- `crates/open-memory-core/src/embed.rs` — adapted from
  `sift/crates/sift-embed/src/traits.rs`; defines `Embedder`,
  `EmbedError`, and `EmbedResult`.
- `crates/open-memory-core/src/testing.rs` — derived from
  `sift/crates/sift-core/src/testing.rs` (49 LOC), plus
  `FakeEmbedder` implementing the core `Embedder` trait.

**Verify.** `cargo test -p open-memory-core --features testing`.

### Tag `v0.0.1-core` — Phase 1 complete

Six commits, ~1500 LOC. Every other crate can now depend on a stable
`open-memory-core`.

---

## Phase 2 — `open-memory-index`

### Commit 12 — `feat(index): traits (VectorIndex, FullTextStore, VectorStore)`

**Scope.** Trait definitions only. No impls. Lets every backend file
land independently in subsequent commits.

**Files.**
- `crates/open-memory-index/src/traits.rs` — from
  `sift/crates/sift-store/src/traits.rs` (72 LOC).
- `crates/open-memory-index/src/error.rs` — from
  `sift/crates/sift-store/src/error.rs` (102 LOC).

**Verify.** `cargo check -p open-memory-index`.

### Commit 13 — `feat(index): FlatVectorIndex (cosine similarity)`

**Scope.** Brute-force vector store. Insert / search / delete /
export. Matches sift's behavior exactly — this code is well-tested and
benchmarks fine for graph-sized data (<10⁶ vectors).

**Files.**
- `crates/open-memory-index/src/flat.rs` — from
  `sift/crates/sift-store/src/flat.rs` (952 LOC).

**Verify.** Port sift's flat-vector tests verbatim.

### Commit 14 — `feat(index): MetadataStore (SQLite)`

**Scope.** The `sources` table — URI, content hash, size, type,
chunk_count, status, timestamps. WAL mode, busy timeout, WAL
checkpoint on shutdown. Migration runner via core's `Migrator`.

**Files.**
- `crates/open-memory-index/src/metadata.rs` — from
  `sift/crates/sift-store/src/metadata.rs` (1024 LOC). Drop the
  parser/source-type fields the file-scanning pipeline added; add a
  `kind: 'graph_obs' | 'index_text'` discriminator since open-memory
  uses one metadata table for both surfaces.

**Verify.** Round-trip + concurrent-reader test.

### Commit 15 — `feat(index): runtime FTS5 probe + BM25 fallback`

**Scope.** SQLite FTS5 backend plus a runtime probe that attempts to
create a temporary FTS5 virtual table against host SQLite. When the
probe succeeds, `open_engine` wires `Fts5Store`; when it fails, it
wires the pure-Rust `Bm25Store`. Default builds do not enable bundled
SQLite and must not compile SQLite C code.

**Files.**
- `crates/open-memory-index/src/fts5.rs` — from
  `sift/crates/sift-store/src/fts5.rs` (435 LOC).
- `crates/open-memory-index/src/bm25.rs` — pure-Rust fallback (498
  LOC). Ports verbatim.
- `crates/open-memory-index/src/fulltext.rs` — new runtime selector
  exposing `FullTextBackend::{Fts5, Bm25}` and `probe_fts5`.

**Verify.** Tests cover both forced paths: FTS5-present selects
`Fts5Store`; FTS5-missing selects `Bm25Store`; `status` reports
`fulltext_backend` accurately. Default build succeeds without the
`bundled-sqlite` feature. Optional `--features bundled-sqlite` test
confirms deterministic FTS5 when a C compiler is available.

### Commit 16 — `feat(index): HybridSearchEngine (RRF fusion)`

**Scope.** Reciprocal Rank Fusion of vector + keyword results.
Configurable RRF k (default 60) and `alpha` weight. **The** core
search routine that both graph recall and index search call.

**Files.**
- `crates/open-memory-index/src/hybrid.rs` — from
  `sift/crates/sift-store/src/hybrid.rs` (435 LOC).

**Verify.** Sift's RRF tests port unchanged.

### Commit 17 — `feat(index): LRU CachedSearchEngine wrapper`

**Scope.** 50-entry LRU with 60s TTL wrapping the hybrid engine.
Cache key is `(query, mode, filters_hash)`. Pure decorator.

**Files.**
- `crates/open-memory-index/src/cache.rs` — from
  `sift/crates/sift-store/src/cache.rs` (295 LOC).

**Verify.** Cache hit/miss + TTL eviction tests.

### Commit 18 — `feat(index): open_engine factory + lib.rs surface`

**Scope.** The single-call `open_engine(config, data_dir)` factory
that opens metadata + vector + fulltext + cache and wires them
together. Plus the `lib.rs` re-exports.

**Files.**
- `crates/open-memory-index/src/engine.rs` — from
  `sift/crates/sift-store/src/engine.rs` (49 LOC).
- `crates/open-memory-index/src/lib.rs` — final shape.

**Verify.** End-to-end: open engine, insert, search, recover after
reopen.

### Commit 19 — `feat(index): HnswIndex (optional, behind hnsw feature)`

**Scope.** usearch-backed HNSW. Off by default. Behind the `hnsw`
feature flag because `usearch` adds C++ build deps.

**Files.**
- `crates/open-memory-index/src/hnsw.rs` — from
  `sift/crates/sift-store/src/hnsw.rs` (812 LOC).

**Verify.** `cargo test -p open-memory-index --features hnsw`.

### Commit 20 — `bench(index): vector + hybrid search criterion benches`

**Scope.** Two criterion benches. Keep them tiny — these are smoke
tests for "did we regress 10×?", not full perf gates.

**Files.**
- `crates/open-memory-index/benches/vector_search.rs` — from
  `sift/crates/sift-store/benches/vector_search.rs`.
- `crates/open-memory-index/benches/hybrid_search.rs` (new, ~50 LOC).

**Verify.** `cargo bench -p open-memory-index` runs both and prints
results.

### Tag `v0.0.2-index` — Phase 2 complete

Nine commits, ~3500 LOC. Search engine usable as a library.

---

## Phase 3 — `open-memory-embed`

### Commit 21 — `feat(embed): core Embedder re-export + StubEmbedder`

**Scope.** Re-export the core `Embedder` trait, add adapters that map
ONNX/bootstrap/cache failures into the core `EmbedError`, and provide
deterministic stubs for tests. This crate does not define its own
`Embedder` trait or public embed error type.

**Files.**
- `crates/open-memory-embed/src/lib.rs` — re-exports
  `open_memory_core::embed::{Embedder, EmbedError, EmbedResult}`.
- `crates/open-memory-embed/src/testing.rs` — derived from sift
  (110 LOC).
- `crates/open-memory-embed/src/error.rs` — adapted from sift (149
  LOC); conversion helpers for the core `EmbedError`, not a second
  public error contract.

**Verify.** `cargo test -p open-memory-embed`.

### Commit 22 — `feat(embed): ONNX runner (CPU)`

**Scope.** Wrap `ort` for inference: load model, run forward pass,
mean-pool tokens, L2-normalize. CPU only (no `cuda`/`coreml` features
in v0.1).

**Files.**
- `crates/open-memory-embed/src/onnx.rs` — from
  `sift/crates/sift-embed/src/onnx.rs` (508 LOC).

**Verify.** Smoke test against a tiny model fixture (committed under
`tests/fixtures/`, ≤2 MB).

### Commit 23 — `feat(embed): model registry (nomic-embed-text-v1.5 + arctic)`

**Scope.** Two models in the registry, not 30. Download URLs + SHA-256
checksums + tokenizer config. Drop sift's vision-model entries.

**Files.**
- `crates/open-memory-embed/src/models.rs` — heavily trimmed from
  `sift/crates/sift-embed/src/models.rs` (1468 → ~400 LOC).
- `crates/open-memory-embed/src/bootstrap.rs` — first-run model
  download (95 LOC, ports almost verbatim).

**Verify.** Offline test: registry lookup. Online test (gated by env
flag): download + verify checksum.

### Commit 24 — `feat(embed): SQLite embedding cache (content-hash keyed)`

**Scope.** Cache table keyed by BLAKE3(content). Avoids re-embedding
identical text. Behind the `sqlite` feature (default on).

**Files.**
- `crates/open-memory-embed/src/cache.rs` — from sift (336 LOC).
- `crates/open-memory-embed/src/json_cache.rs` — JSON fallback for
  `--no-default-features` (196 LOC). Smaller than sift's, but
  identical semantics.

**Verify.** Cache hit ratio test.

### Tag `v0.0.3-embed` — Phase 3 complete

Four commits, ~1500 LOC. Optional layer; the rest of the workspace
runs without it.

---

## Phase 4 — `open-memory-graph`

This is the **heart of the project**. Eight commits because the graph
store deserves the granularity.

### Commit 25 — `feat(graph): types (Entity, Observation, Relation, EntityType)`

**Scope.** Pure data types + serde. No DB, no logic. UUIDv7 minted
with `uuid::Uuid::now_v7()`.

**Files.**
- `crates/open-memory-graph/src/types.rs` — from
  `sift/crates/sift-memory/src/types.rs` (570 LOC). Drop the `Episode`
  type — there are no hooks.

**Verify.** Serde round-trip tests.

### Commit 26 — `feat(graph): SQLite schema + migrations`

**Scope.** All four migration steps from sift v1→v4 ported as a single
"v1" migration here. We do not carry sift's migration history; new
project, new fresh schema. The `Migrator` machinery is reused as-is.

**Files.**
- `crates/open-memory-graph/src/schema.rs` — derived from
  `sift/crates/sift-memory/src/schema.rs` (690 LOC). Drop the
  `episodes` table entirely (no Episode type). Drop tier-promotion
  tracking columns that were Cortex-specific.

**Verify.** Init on empty DB; migration smoke test.

### Commit 27 — `feat(graph): MemoryStore::open + read paths`

**Scope.** Open the store (SQLite + index engine + optional embedder),
read-only methods: `list_entities`, `get_entity`, `status`. No writes
yet.

**Files.**
- `crates/open-memory-graph/src/store.rs` — extracted from
  `sift/crates/sift-memory/src/lib.rs` (3903 LOC, this commit takes
  the open + read sections, ~600 LOC). Splits sift's mega-file into
  `store.rs`, `consolidate.rs`, and the existing `types.rs` /
  `schema.rs`.

**Verify.** Open empty store; list returns empty; status returns zeros.

### Commit 28 — `feat(graph): MemoryStore::remember (write path)`

**Scope.** Atomic write: ensure entity, append observation(s), append
relation(s), keep search index in sync via the
`apply_search_sync_ops_with_recovery` transactional helper.

**Files.**
- `crates/open-memory-graph/src/store.rs` (extend) — adds `remember`,
  `apply_search_sync_ops_with_recovery`, the `RwLock` rebuild guard.
  Sourced from `sift/crates/sift-memory/src/lib.rs` (~700 LOC of the
  original).

**Verify.** Round-trip: remember; recall by entity name; same record
back.

### Commit 29 — `feat(graph): MemoryStore::recall (hybrid search + decay)`

**Scope.** The recall engine. Hybrid search delegated to
`open-memory-index`; result re-scored with Ebbinghaus decay
(`exp(-decay_rate * age_days)`), correction-tag boost, optional
spreading-activation through relations.

**Files.**
- `crates/open-memory-graph/src/store.rs` (extend) — `recall`,
  `RecallOptions`, `RecallResult`, decay scoring. ~600 LOC ported.

**Verify.** Decay test (one observation 30d old, one fresh; fresh wins
on equal text). Spreading-activation test.

### Commit 30 — `feat(graph): forget + forget_entity + prune`

**Scope.** The destructive paths. `forget` is a soft-delete (mark
observation tombstoned, keep in DB for audit). `forget_entity` is a
hard-delete with cascade. `prune` collects orphan tombstones older
than the configured TTL.

**Files.**
- `crates/open-memory-graph/src/store.rs` (extend) — ~400 LOC ported.

**Verify.** Soft vs hard delete behavior; prune respects TTL.

### Commit 31 — `feat(graph): consolidate (dedup + decay/prune)`

**Scope.** Two-phase consolidation, replacing sift's five-phase
Cortex pipeline. Phase 1: dedup (text similarity ≥ 0.95 within an
entity, optionally cosine if embeddings on). Phase 2: decay/prune
(score every observation, prune those below floor).

**Files.**
- `crates/open-memory-graph/src/consolidate.rs` — derived from
  `sift/crates/sift-memory/src/consolidation.rs` (1214 LOC →
  ~400 LOC). Drop episode-processing, tier-promotion, and skill-
  extraction phases.

**Verify.** Idempotence test (consolidate twice = consolidate once).

### Commit 32 — `test(graph): integration suite (in-memory SQLite)`

**Scope.** Integration tests at the public-API layer. ≥10 tests
covering remember/recall/forget cycles, schema migration, decay
correctness, dedup correctness.

**Files.**
- `crates/open-memory-graph/tests/integration.rs` — new, ~500 LOC.

**Verify.** Suite green; runs in <2s.

### Tag `v0.0.4-graph` — Phase 4 complete

Eight commits, ~5500 LOC. The library is feature-complete from a
caller's perspective.

---

## Phase 5 — `open-memory-mcp`

### Commit 33 — `feat(mcp): Tool trait + ToolRouter scaffolding`

**Scope.** The `Tool` trait, `ToolGroup` enum, registry. No tools yet.
Server bootstrap stub: a server with zero tools that answers
`initialize` and `tools/list` (returning empty).

**Files.**
- `crates/open-memory-mcp/src/lib.rs` — server bootstrap (from
  `sift/crates/sift-mcp/src/lib.rs`, trimmed of search-engine-
  specific wiring).
- `crates/open-memory-mcp/src/tools/mod.rs` — Tool trait + registry
  (from `sift/crates/sift-mcp/src/tools/mod.rs` 249 LOC, ports
  verbatim).

**Verify.** `cargo run -p open-memory-cli -- mcp` and an MCP client
sees the empty tool list.

### Commit 34 — `feat(mcp): memory tools (remember, recall, list, get, forget, forget_entity, status)`

**Scope.** Seven memory tools. Each is a unit struct + `Tool` impl +
input struct + handler. Mirrors sift's `tools/memory.rs` structure but
**renamed**: `sift_remember` → `open_memory_remember`, etc.

**Files.**
- `crates/open-memory-mcp/src/tools/memory.rs` — derived from
  `sift/crates/sift-mcp/src/tools/memory.rs` (759 LOC). Drop:
  `sift_consolidate` MCP tool? — keep, expose as
  `open_memory_consolidate`. Drop: `sift_forget` was correctly
  preserved; `sift_prune` collapses into `open_memory_consolidate`'s
  decay-prune phase, no separate tool.

**Verify.** `cargo test -p open-memory-mcp` (snapshot of tool list).
Black-box MCP round-trip via `tower::ServiceExt`.

### Commit 35 — `feat(mcp): index tools (index_text, search, delete)`

**Scope.** Three index tools. Mirrors sift's `tools/indexing.rs` and
`sift_search` from `tools/search.rs`, but renamed and trimmed —
no skill discovery, no list_sources.

**Files.**
- `crates/open-memory-mcp/src/tools/index.rs` — combines
  `sift/crates/sift-mcp/src/tools/indexing.rs` (220 LOC) and the
  search-engine bits of `tools/search.rs`. ~350 LOC total.

**Verify.** Round-trip via MCP: index a doc, search, delete.

### Commit 36 — `feat(mcp): consolidate tool + ServerHandler instructions`

**Scope.** The `open_memory_consolidate` tool (write, idempotent) +
the rendered `get_info` instructions block that lists all tools by
group.

**Files.**
- `crates/open-memory-mcp/src/tools/memory.rs` (extend) — add
  consolidate tool.
- `crates/open-memory-mcp/src/tools/mod.rs` (extend) — wire into
  `server_instructions()`.

**Verify.** `tools/list` returns 11 tools. `get_info` text matches
golden file.

### Commit 37 — `feat(mcp): Streamable HTTP transport (optional)`

**Scope.** The HTTP transport behind the `mcp-http` feature. Routes
under `/mcp`. Identical to sift's `http.rs`. Off by default.

**Files.**
- `crates/open-memory-mcp/src/http.rs` — derived from
  `sift/crates/sift-mcp/src/http.rs` (256 LOC).

**Verify.** Build with `--features mcp-http`. axum integration test
passes (sift's port).

### Tag `v0.0.5-mcp` — Phase 5 complete

Five commits, ~1500 LOC. MCP server functional.

---

## Phase 6 — `open-memory-cli`

### Commit 38 — `feat(cli): clap surface + status, init`

**Scope.** Bootstrap the binary. `clap` derive, top-level subcommands.
Implement `init` (create dirs + write default config) and `status`
(open everything read-only, print summary).

**Files.**
- `crates/open-memory-cli/src/main.rs` — clap struct.
- `crates/open-memory-cli/src/commands/{mod, init, status}.rs` —
  derived from `sift/crates/sift-cli/src/commands/{init.rs,
  status.rs}` (200 + 94 LOC), trimmed.

**Verify.** `open-memory init` then `open-memory status` prints zero
counts and the schema versions.

### Commit 39 — `feat(cli): mcp + consolidate subcommands`

**Scope.** `open-memory mcp` (start the MCP server, default stdio,
`--http` switches to HTTP). `open-memory consolidate` (run the
consolidation pipeline once and print the report).

**Files.**
- `crates/open-memory-cli/src/commands/mcp.rs` — derived from
  `sift/crates/sift-cli/src/commands/mcp.rs`.
- `crates/open-memory-cli/src/commands/consolidate.rs` — derived from
  the relevant slice of `sift/crates/sift-cli/src/commands/memory.rs`
  (535 LOC, this slice ~80 LOC).

**Verify.** `open-memory mcp` answers an `initialize` request from a
canned client.

### Commit 40 — `feat(cli): integrate openclaw subcommand`

**Scope.** **The defining v0.1 feature.** Resolve OpenClaw config
path (`$OPENCLAW_CONFIG_PATH` or `~/.openclaw/openclaw.json`); when
the `openclaw` binary is on `PATH`, delegate to
`openclaw mcp set open-memory '<json>'` and surface its exit code +
stderr verbatim; otherwise parse `openclaw.json` as JSON5, ensure
`mcp.servers` exists, set the `open-memory` entry, write atomically
(temp + rename). Round-trip the file after write and assert the
entry parses back byte-equivalent. Idempotent. Print exactly what
changed.

Conformance with `02-openclaw-integration.md` is mandatory: only
`mcp.servers.<name>` is written; no `~/.openclaw/mcp.json` and no
top-level `mcpServers` are produced.

**Files.**
- `crates/open-memory-cli/src/commands/integrate.rs` — new,
  ~300 LOC. No analogue in sift (sift's `integrate.rs` does Claude
  Code / Codex, not OpenClaw).
- `crates/open-memory-cli/tests/integrate.rs` — table-driven test
  matrix:
  1. No existing `openclaw.json` (file is created with the entry).
  2. `openclaw.json` exists, `mcp` block absent (block is created).
  3. `openclaw.json` exists, `mcp.servers` populated with sibling
     entries (siblings preserved; only `open-memory` mutated).
  4. Existing `open-memory` entry with stale fields (entry updated
     to match the intended shape; diff printed).
  5. Existing `open-memory` entry already correct (no-op; exit 0;
     stdout reports "no change").
  6. `openclaw` binary on `PATH` (assert delegation: spy `openclaw`
     stub records `mcp set open-memory <json>` was invoked).
  7. JSON5 input with comments + trailing commas (round-trip
     preserves both).
  8. Corrupt `openclaw.json` (clear error pointing at byte offset;
     no destructive write).

**Verify.** All 8 test cases produce expected output and no diff
beyond intended. The integrator's emitted JSON validates against
`mcp.servers.<name>` per OpenClaw's documented config-reference
shape (stdio: `command`/`args`/`env` only; remote:
`url`/`transport`/optional `headers` only).

### Commit 41 — `feat(cli): remember/recall/forget-entity/list-entities/completions`

**Scope.** The remaining ergonomics: scriptable command-line write
and read, plus shell completions.

**Files.**
- `crates/open-memory-cli/src/commands/remember.rs` — new, ~80 LOC.
- `crates/open-memory-cli/src/commands/recall.rs` — new, ~80 LOC.
- `crates/open-memory-cli/src/commands/forget_entity.rs` — new, ~30
  LOC.
- `crates/open-memory-cli/src/commands/list_entities.rs` — new, ~50
  LOC.
- `crates/open-memory-cli/src/commands/completions.rs` — clap_complete
  one-liner, behind the `completions` feature.

**Verify.** End-to-end: remember from CLI, recall from CLI, exit code 0.

### Tag `v0.0.6-cli` — Phase 6 complete

Four commits, ~600 LOC. Binary feature-complete.

---

## Phase 7 — Quality

### Commit 42 — `test: end-to-end MCP test against the real binary`

**Scope.** Spawn `open-memory mcp` as a subprocess, talk to it via
stdio, exercise every tool. The closest thing to "an OpenClaw agent
calling our server."

**Files.**
- `tests/e2e_mcp.rs` (workspace-root) — ~300 LOC.

**Verify.** Test runs in <10s. Green on macOS + Linux.

### Commit 43 — `chore: hardening pass (audit, deny, msrv)`

**Scope.** Apply the production-hardening punch list from
`05-quality-gates.md`. No feature additions.

**Files.**
- `Cargo.toml` — pin patch versions where it matters; trim default
  features on transitive deps.
- `clippy.toml` — final disallowed-methods list (e.g. `std::thread::sleep`
  outside test paths).
- `.github/workflows/audit.yml` — add `cargo audit` step.
- Any code-level fixes the audit surfaces.

**Verify.** `cargo deny check`, `cargo audit` clean. `cargo clippy --all-features --all-targets -- -D warnings` clean.

### Commit 44 — `docs: README + CHANGELOG + per-crate rustdoc landing pages`

**Scope.** Public-facing documentation.

**Files.**
- `README.md` — full rewrite. Quick install, quick start, OpenClaw
  setup, link to docs/.
- `CHANGELOG.md` — `[Unreleased]` and `[0.1.0]` sections.
- `crates/*/src/lib.rs` — each gets a complete crate-level rustdoc
  block (`#![doc = …]`).

**Verify.** `cargo doc --workspace --no-deps --all-features` clean,
no missing-docs warnings on public items.

### Commit 45 — `ci: release workflow (tagged builds, cargo publish dry-run)`

**Scope.** GitHub Actions release workflow. Builds platform tarballs,
runs `cargo publish --dry-run` on every crate.

**Files.**
- `.github/workflows/release.yml`

**Verify.** Manual workflow_dispatch run produces three tarballs
(macos-aarch64, macos-x86_64, linux-x86_64).

### Tag `v0.1.0` — Release

Workspace done. Cut a tag, run the release workflow, publish a
GitHub Release with binaries.

```
git tag -a v0.1.0 -m "open-memory v0.1.0 — port from sift v0.1.7"
git push origin v0.1.0
```

---

## Out-of-band: post-v0.1 work

Things explicitly **not** in the commit plan above, queued for v0.2:

- `cargo install open-memory --features llm` — re-add LLM-driven
  observation extraction (sift's `llm.rs`) as opt-in.
- `homebrew-tap`, `install.sh`, `Cross.toml` for cross-arch builds.
- A `migrate-from-sift` CLI subcommand that reads `~/.sift/indexes/<name>/`
  and copies entities/observations into open-memory's schema.
- HNSW + SIMD distance functions enabled by default if benchmarks
  show they pay off below 10⁶ vectors.
- Postgres backend behind a `postgres` feature flag (community ask).
