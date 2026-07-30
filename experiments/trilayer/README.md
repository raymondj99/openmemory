# T15 — Tri-layer retrieval on three real codebases

Stress test for the design principles in
`plan/17-trilayer-memory-principles.md`: three real repositories are
ingested as **one project in one shared knowledge graph** (single
store, single profile; codex `depends_on` clap and anyhow, per-file
`references` edges cross the repo boundary, `part_of` chains connect
file to crate to project), and file-level retrieval is measured
through three routes (graph-only, index-only, and the
tri-layer pipeline: graph concept localization scoping an index
search). Prototype code under the `experiments/` authority model:
proves behavior, never copied into production.

## Corpora

Three public repositories, shallow-cloned and pinned. `codex` really
depends on both `clap` and `anyhow`, so the cross-repo edges in the
graph are real, not staged.

| Repo | Revision | Role |
|---|---|---|
| `openai/codex` | `406dc9239492aff6d295cca5eebe2a548548d42f` | large consumer codebase (uses clap + anyhow) |
| `clap-rs/clap` | `f8ac8c5f1b3554657b557ba2d21279930434b401` | mid-size library workspace (clap_builder, clap_derive, clap_lex, ...) |
| `dtolnay/anyhow` | `1dbe1862aae650423e3361fbd20b7d17c5109cc3` | small focused library |

Natural homonym set: all three ship `README.md`, `src/lib.rs`,
`Cargo.toml`, error modules, derive/macro machinery, and completion
generation (clap_complete vs codex's own completion command).

## Layer mapping (current primitives standing in for the design)

| Design layer | Stand-in |
|---|---|
| L1 knowledge graph | `openmemory_remember` entities: `project` (codex, clap, anyhow), `concept` crates, `concept` files named `<repo>/<relpath>`; relations `part_of`, `depends_on`, `references` |
| L2 concept glosses / details | one fielded observation per file entity: extracted doc comments + pub item signatures (a heuristic gloss), `concepts=[repo, crate]`, `source_files=[relpath]` |
| L3 text index | `openmemory_index_text` of file content chunks under `omem://<repo>/<relpath>#<i>` |

Identity note: crate entities whose package name equals the project
name (clap, anyhow) are subsumed by the project entity rather than
created as a second entity, because the store's UNIQUE
`(name, entity_type)` schema makes cross-type duplicates unreachable
by name (T11). This is a workaround the principles doc says should be
unnecessary once names are assertions.

## Inclusion rules (declared, deterministic)

- anyhow: `src/**/*.rs`, `README.md`.
- clap: `{clap,clap_builder,clap_derive,clap_lex,clap_complete}/src/**/*.rs`
  (crates present at the pinned rev), `examples/**/*.rs`, root `*.md`.
  `tests/` excluded.
- codex: crates `{cli, core, exec, tui, protocol, config, apply-patch,
  arg0, codex-mcp, login}` under `codex-rs/`, `src/**/*.rs` only,
  **capped at 50 files per crate by descending file size** (cap
  declared here per the no-silent-caps rule); root `README.md` and
  `docs/*.md` capped at 15 by size.
- Chunking: line-boundary chunks of ~1800 chars, **capped at 6 chunks
  per file** (declared). File-level judgments are unaffected by the
  chunk cap for ranking, but content past ~11 KB per file is not
  recallable; semantic queries were authored against content inside
  the cap.

## Query set (authored, not mined)

Authored by reading the repositories, per the measurement-discipline
principle: queries must not restate their answers except in the
`direct-lexical` category, whose job is to measure exact-identifier
lookup. File-granularity judgments; multi-target queries carry every
acceptable file.

| Category | Tests |
|---|---|
| `direct-lexical` | exact identifier → defining file |
| `semantic` | behavior described without identifiers → implementing file |
| `homonym` | ambiguous artifact ("the error type", "the README") with repo intent stated → right repo's file |
| `relational` | cross-repo/multi-hop ("what does codex parse CLI args with") |
| `abstention` | topics in none of the three repos (no relevant file exists) |

## Arms

| Arm | Route |
|---|---|
| `index-{keyword,vector,hybrid}` | `openmemory_search` limit 30 → dedupe chunk URIs to files → top 10 |
| `graph-{keyword,vector,hybrid}` | `openmemory_recall` limit 30 → file entities → top 10 |
| `trilayer-{...}` | `openmemory_recall` (concept localization over glosses) → deepest directory-subtree scope agreed by the top entities (a concept neighborhood in the one shared graph, never a hard repo partition) → `openmemory_search` scoped by that `uri_prefix` interleaved with unscoped search (rank interleave, never score mixing) → top 10 files |

Metrics: R@5, R@10, MRR at file level; per-call wall-clock p50/p95
(warm; model-load cost reported separately); ingest throughput and
store size as byproducts. Abstention is reported as score-shape stats
per mode (top score, top margin) on answerable vs abstention queries;
the system today has no abstention mechanism, so no gate.

## What this does and does not establish

Does: whether graph-scoped index search beats flat search on a shared
multi-project store, at file granularity, on authored queries, with
real cross-repo structure; plus the honest cost of each layer.

Does not: validate supersession/correction semantics (no time-varying
facts here), the typed traversal planner (scoping only), or model
semantic accuracy. Judgments are single-author and not adjudicated;
treat absolute levels as provisional and paired deltas between arms as
the result.

## Reproducing

```sh
cd experiments/trilayer
python3 scripts/ingest.py            # builds .home store from repos/
python3 scripts/run_eval.py          # runs all arms over queries/
```

Store lives in `.home/` (gitignored), fully isolated from
`~/.openmemory`. Models are symlinked from the user cache.
