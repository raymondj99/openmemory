# Source mapping — sift to open-memory

This file is the line-item ledger of what gets ported, renamed,
trimmed, or dropped. It is the document a porter consults when they
hit `cp` paralysis: "is this file in the cut, or out?"

The mapping is from `~/sift/crates/<sift-crate>/src/<file>` to
`~/open-memory/crates/<om-crate>/src/<file>`. **Drop** means "this
file does not exist in open-memory at all."

## Identifier renames (mechanical)

These find/replace passes apply to **every** ported file:

| sift identifier | open-memory identifier |
|---|---|
| `sift_*` (tool names, CLI subcommands) | `open_memory_*` |
| `Sift*` (type names) | `Om*` |
| `SIFT_*` (constants, env vars) | `OPEN_MEMORY_*` |
| `sift::` (crate paths in tests/docs) | `open_memory_*::` |
| `sift-core` (crate name) | `open-memory-core` |
| `sift-store` | `open-memory-index` (note: rename, not just prefix) |
| `sift-memory` | `open-memory-graph` (note: rename) |
| `sift-embed` | `open-memory-embed` |
| `sift-mcp` | `open-memory-mcp` |
| `sift-cli` | `open-memory-cli` |
| `~/.sift/` | `~/.open-memory/` |
| `SIFT_INDEX`, `SIFT_HOME`, `SIFT_LOG` | `OPEN_MEMORY_PROFILE`, `OPEN_MEMORY_HOME`, `OPEN_MEMORY_LOG` |

Two crate renames (`sift-store` → `open-memory-index`, `sift-memory` →
`open-memory-graph`) reflect that these crates do conceptually
different things in open-memory than they did in sift, where the
naming was sift-internal.

## sift-core → open-memory-core

| sift file | open-memory destination | disposition |
|---|---|---|
| `clock.rs` | `clock.rs` | **Port verbatim** (modulo rename) |
| `migrations.rs` | `migrations.rs` | **Port verbatim** |
| `sift-embed/src/traits.rs` | `embed.rs` | **Adapt** — `Embedder`, `EmbedError`, and `EmbedResult` live in core as the single workspace-owned trait |
| `error.rs` | `error.rs` | **Port + trim** — drop `Source`, `Parse`, `Chunk`, `Embed` pipeline-stage variants |
| `config.rs` | `config.rs` | **Heavy trim** — 1247 → ~300 LOC. Drop `[parsers]`, `[chunker]`, `[server]`, `[watch]`, `[fs]` sections; drop file-walking knobs |
| `retry.rs` | `retry.rs` | **Port verbatim** |
| `util.rs` | `util.rs` | **Port verbatim** (atomic_write, format_bytes only) |
| `testing.rs` | `testing.rs` | **Port + trim** — keep FixedClock, FakeEmbedder; drop pipeline-stage doubles |
| `pipeline.rs` | — | **Drop** — pipeline data types only used by file-scanning |
| `types.rs` | — | **Drop** — `SourceItem`, `ParsedDocument`, `Chunk`, etc. all bound to file pipeline |

## sift-store → open-memory-index

| sift file | open-memory destination | disposition |
|---|---|---|
| `traits.rs` | `traits.rs` | **Port verbatim** |
| `error.rs` | `error.rs` | **Port verbatim** |
| `flat.rs` | `flat.rs` | **Port verbatim** |
| `hnsw.rs` | `hnsw.rs` | **Port verbatim** (behind `hnsw` feature) |
| `fts5.rs` | `fts5.rs` | **Port + adapt** — host SQLite FTS5 backend, runtime-probed at startup |
| `bm25.rs` | `bm25.rs` | **Port verbatim** — pure-Rust fallback when host SQLite lacks FTS5 |
| — | `fulltext.rs` | **New** — selects `Fts5Store` or `Bm25Store` at runtime and reports the active backend |
| `metadata.rs` | `metadata.rs` | **Port + extend** — same SQLite schema, plus a `kind` column to distinguish graph observations from index_text rows |
| `hybrid.rs` | `hybrid.rs` | **Port verbatim** |
| `cache.rs` | `cache.rs` | **Port verbatim** |
| `engine.rs` | `engine.rs` | **Port + trim** — drop file-pipeline factory, keep open_engine |
| `tantivy_store.rs` | — | **Drop** — FTS5 plus BM25 fallback covers v0.1 without Tantivy's heavier dependency surface |
| `json_metadata.rs` | — | **Drop** — SQLite metadata is required everywhere |
| `test_utils.rs` | `test_utils.rs` | **Port verbatim** |
| `testing.rs` | `testing.rs` | **Port verbatim** |
| `benches/vector_search.rs` | `benches/vector_search.rs` | **Port verbatim** |

## sift-embed → open-memory-embed

| sift file | open-memory destination | disposition |
|---|---|---|
| `traits.rs` | — | **Moved to core** — adapted into `open-memory-core/src/embed.rs`; this crate re-exports the core trait |
| `onnx.rs` | `onnx.rs` | **Port verbatim** (CPU-only; cuda/coreml features dropped) |
| `models.rs` | `models.rs` | **Heavy trim** — 1468 → ~400 LOC. Keep `nomic-embed-text-v1.5` and `snowflake-arctic-embed-l-v2.0`; drop bge-m3, embeddinggemma, jina-v5, all vision models |
| `bootstrap.rs` | `bootstrap.rs` | **Port verbatim** (download + verify checksum) |
| `cache.rs` | `cache.rs` | **Port verbatim** |
| `json_cache.rs` | `json_cache.rs` | **Port verbatim** |
| `error.rs` | `error.rs` | **Adapt** — map ONNX/bootstrap/model/cache failures into the core `EmbedError`; no second public embed error type |
| `testing.rs` | `testing.rs` | **Port verbatim** (StubEmbedder) |
| `vision.rs` | — | **Drop** — text embeddings only |

## sift-memory → open-memory-graph

This is the most surgical mapping. The graph crate is the heart of
the project; the cuts here matter most.

| sift file | open-memory destination | disposition |
|---|---|---|
| `types.rs` | `types.rs` | **Port + trim** — drop `Episode`, `EpisodeState`, `Skill`, and adapter-only rewrite planning types. Keep Entity, Observation, Relation, EntityType, MemoryTier, RecallResult, RecallFilters, MemoryStats, and trimmed consolidation config/report types |
| `schema.rs` | `schema.rs` | **Port + trim** — drop `episodes` table, drop tier-promotion bookkeeping columns |
| `lib.rs` | **split** into `store.rs` + `consolidate.rs` | **Port + trim** — 3903 LOC re-organized. Drop `PageMutationPlan` and related Anthropic-adapter rewrite planning types, plus episode-processing helpers. Keep MemoryStore, recall, remember, forget, forget_entity, prune, get_entity, list_entities, status, the `RwLock` rebuild guard, `apply_search_sync_ops_with_recovery` |
| `consolidation.rs` | `consolidate.rs` | **Heavy trim** — 1214 → ~400 LOC. Drop episode_processing phase, promotion phase, skill_extraction phase. Keep dedup, decay/prune. Add `ConsolidateConfig`, `ConsolidateReport` |
| `episodes.rs` | — | **Drop** — Cortex hot path, hook-specific |
| `rules.rs` | — | **Drop** — Claude-Code-hook-payload parsing |
| `llm.rs` | — | **Drop in v0.1**. Re-add as optional `llm` feature in v0.2 |
| `testing.rs` | `testing.rs` | **Port verbatim** |

## sift-mcp → open-memory-mcp

| sift file | open-memory destination | disposition |
|---|---|---|
| `lib.rs` | `lib.rs` | **Port + trim** — 1033 LOC → ~600 LOC. Drop file-scan-engine wiring, walkdir scanning, content-type heuristics; keep server bootstrap, cache, transport selection |
| `tools/mod.rs` | `tools/mod.rs` | **Port verbatim** — Tool trait + registry pattern is exemplary |
| `tools/search.rs` | merge into `tools/index.rs` | **Heavy trim** — 617 LOC. Drop `sift_search_skills`, `sift_list_sources`. Keep `sift_search` → `open_memory_search` and `sift_status` (split into per-domain status; the global one becomes `open_memory_status`) |
| `tools/indexing.rs` | merge into `tools/index.rs` | **Port + rename** — 220 LOC, becomes ~350 LOC after merging with the search slice |
| `tools/memory.rs` | `tools/memory.rs` | **Port + rename** — 759 LOC. Drop `sift_consolidate`'s episode-processing path, drop `sift_prune` (collapse into consolidate). Renames as documented in `02-openclaw-integration.md` |
| `http.rs` | `http.rs` | **Port verbatim** (behind `mcp-http` feature) |

## sift-cli → open-memory-cli

| sift file | open-memory destination | disposition |
|---|---|---|
| `main.rs` | `main.rs` | **Heavy trim** — 938 LOC → ~150 LOC. Drop most subcommand wiring; keep clap struct, global flags |
| `commands/init.rs` | `commands/init.rs` | **Port + trim** — drop file-scanning init steps |
| `commands/status.rs` | `commands/status.rs` | **Port + extend** — add memory + index counts |
| `commands/mcp.rs` | `commands/mcp.rs` | **Port verbatim** |
| `commands/memory.rs` | `commands/{remember.rs, recall.rs, forget_entity.rs, list_entities.rs, consolidate.rs}` | **Split + heavy trim** — 535 LOC down to ~250 LOC across five files. Drop `init-hooks`, `ingest`, `generate-rules` (all Claude-Code-specific) |
| `commands/integrate.rs` | `commands/integrate.rs` | **Rewrite** — sift's version handles Claude/Codex; open-memory's handles OpenClaw |
| `commands/scan.rs` | — | **Drop** — no file scanning |
| `commands/search.rs` | — | **Drop** — search is exposed via MCP and via `open-memory recall` (graph) / no CLI search-text-corpus subcommand in v0.1 |
| `commands/remove.rs` | — | **Drop** — folded into `forget_entity` and the `open_memory_delete` MCP tool |
| `commands/list.rs` | — | **Drop** — no file source listing |
| `commands/config.rs` | — | **Drop** — config edits via TOML or env in v0.1 |
| `commands/export.rs` | — | **Drop** — JSONL export not in v0.1 |
| `commands/daemon.rs` | — | **Drop** — no daemon |
| `commands/watch.rs` | — | **Drop** — no watcher |
| `commands/serve.rs` | — | **Drop** — no HTTP REST server |
| `commands/models.rs` | — | **Drop** — model registry is internal in v0.1 |
| `commands/bench.rs` | — | **Drop** — benchmarks via `cargo bench` |
| `commands/memory_tool.rs` | — | **Drop** — Anthropic memory-tool adapter |
| `commands/mod.rs` | `commands/mod.rs` | **Rewrite** — re-export the kept handlers |
| `commands/util.rs` | `commands/util.rs` | **Port + trim** |
| `pipeline.rs` | — | **Drop** — file-pipeline orchestration |
| `daemon_client.rs` | — | **Drop** |
| `output.rs` | `output.rs` | **Port + trim** — keep human + JSON output helpers |
| `color_stub.rs` | `color_stub.rs` | **Port verbatim** |

## Other sift assets

| sift path | open-memory disposition |
|---|---|
| `crates/sift-parsers/` | **Drop entire crate** |
| `crates/sift-sources/` | **Drop entire crate** |
| `crates/sift-chunker/` | **Drop entire crate** |
| `crates/sift-server/` | **Drop entire crate** |
| `evals/` | **Drop** — Python eval harness, post-v0.1 |
| `fuzz/` | **Drop** — fuzzers post-v0.1 |
| `perf/` | **Drop** — corpus generator post-v0.1 |
| `pkg/` | **Drop** — release packaging boilerplate, post-v0.1 |
| `install.sh` | **Drop in v0.1** — re-add post-v0.1 |
| `Cross.toml` | **Drop in v0.1** |
| `PLAN.md`, `PLAN-retrieval-lab.md`, `RESEARCH.md`, `SUMMARY.md`, `EMBEDDINGS.md`, `PRODUCTION-HARDENING.md` | **Drop** — sift's working docs; open-memory has its own under `docs/` |
| `AGENTS.md` | **Drop** — Cortex-generated, no longer applicable |
| `CHANGELOG.md` | **Replace** — fresh, starts at `[0.1.0]` |
| `README.md` | **Replace** — fresh, written for open-memory |
| `LICENSE` | **Replace** — split into `LICENSE-MIT` + `LICENSE-APACHE` |
| `clippy.toml`, `rustfmt.toml`, `deny.toml`, `rust-toolchain.toml`, `.editorconfig`, `.gitignore` | **Port verbatim** (modulo MSRV bump to 1.82) |

## What "Port verbatim" actually means

A "verbatim port" still requires:

1. The mechanical rename pass above.
2. Re-running `cargo fmt` after rename.
3. Updating doc-links (`[\`sift_core::Clock\`]` → `[\`open_memory_core::Clock\`]`).
4. Updating crate-level `//!` doc comments to match the new scope.
5. Re-checking that every test in the file still compiles and passes
   under the new module path.

It does **not** mean "git mv". The destination repo has no shared
history with sift; every file lands as a new addition.

## Audit list (do not skip)

Before each commit, the porter confirms the file under port:

- [ ] Has no remaining `sift` / `Sift` / `SIFT` substrings (other than
      in comments referencing the upstream project).
- [ ] Compiles in the open-memory workspace.
- [ ] Its tests pass under the open-memory feature matrix.
- [ ] Its rustdoc references resolve.
- [ ] Has been re-read for "is this file in scope?" — Anthropic memory-
      tool fragments, hook fragments, parser fragments are easy to
      drag along by accident.
