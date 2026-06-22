# openmemory documentation

`openmemory` is a Rust workspace that gives AI agents persistent
memory and hybrid (vector + keyword) text search behind a single
static binary and a Model Context Protocol (MCP) server. It ships
eleven `openmemory_*` MCP tools, an opt-in filesystem watcher,
and a one-command integration into [OpenClaw](https://openclaw.ai):

The project is built around three opinionated choices:

1. **SQLite is the storage layer.** Entities, observations,
   relations, full-text indexes, and embedding caches all live in
   SQLite databases under `~/.openmemory/`. There are no external
   services and no daemons beyond the MCP server itself.
2. **The MCP tool surface is the public contract.** Eleven tools
   under the `openmemory_*` prefix cover the entire knowledge-graph
   and free-text-index API. The Rust crate API is unstable;
   downstream consumers should pin patch versions.
3. **One binary, one config.** `cargo install openmemory` produces
   a static binary. `openmemory init` creates the data directory.
   `openmemory integrate openclaw` writes a working entry into
   OpenClaw's config. Nothing else is required.

Current release: **v0.3.0** (on the `feat/memory-retrieval-enhancements`
branch). Lands hybrid-search correctness fixes (callers now embed text
on the way into and out of the index, recall overfetches when filters
are active), the new `openmemory-eval` retrieval-quality harness
(R@K, MRR, NDCG@K with `longmem-s` and `coding-mem` adapters), the
`memory_tier` filter end-to-end (`openmemory_remember`,
`openmemory_recall`, `openmemory_get_entity`), and forward-only
schema v2 with fielded observation indexing (`title`, `summary`,
`importance`, `source_kind`, plus `observation_concepts` and
`observation_source_files` side tables, weighted via FTS5 repetition
under the new `[search.field_weights]` config). The recall hot path
was reshaped around the v2 schema and runs 2.6x–3.8x faster on the
keyword and hybrid benches versus pre-v0.3 `main`. v0.2.1 added
entity normalization on the `remember` write path, explicit
embedding-model management (`openmemory model list / download /
use`), SHA-256 integrity verification for ONNX models, and optional
bearer-token auth on the Streamable HTTP transport. v0.2.0 introduced
multi-agent memory (a pool of read-only WAL connections so concurrent
recalls run in parallel) and the `openmemory watch DIR` filesystem
watcher. The MCP tool surface is unchanged at v0.1 in name set;
`openmemory_remember` and `openmemory_recall` accept new optional
fields, and every recall result carries `memory_tier`.

## Document map

Read in this order if you want a cold start. Each topic stands on
its own; skip ahead if you only need one slice.

| Document | Summary |
|----------|---------|
| [overview.md](overview.md) | What `openmemory` is, the problem it solves, the goals and non-goals that bound the project, and the high-level deliverables. Start here. |
| [architecture.md](architecture.md) | Workspace layout, crate boundaries, the dependency graph, the writer-mutex + reader-pool + rebuild-barrier threading model, and the design philosophy that explains why several plausible abstractions are deliberately absent. |
| [context-engine.md](context-engine.md) | The concurrency bus between agents and the stores: write-behind sharding, crash-durable journals, domain partitioning with TAO-style mirrored edges, the facade recall cache, domain-count migration, and the measured numbers behind every design decision. |
| [crates.md](crates.md) | Per-crate reference: purpose, feature flags, public API surface, key types, and source-file map for each of the nine workspace crates (seven shipping crates plus `openmemory-bench` and v0.3's `openmemory-eval`). |
| [mcp.md](mcp.md) | MCP server contract. Wire-level JSON-RPC 2.0 framing, the eleven `openmemory_*` tools, their input schemas and annotations, error shapes, and the stdio + Streamable HTTP transports including bearer-token auth. |
| [openclaw.md](openclaw.md) | The OpenClaw integration contract. Config-file resolution rules, the JSON entry written by `openmemory integrate openclaw`, the stdio vs. HTTP entry shapes, multi-profile coexistence, and the verification path. |
| [search.md](search.md) | How recall actually works. Hybrid (vector + FTS5) search via Reciprocal Rank Fusion, the Ebbinghaus decay scoring with retrieval and correction boosts, optional spreading activation through relations, and the embedding model registry (Nomic, Snowflake Arctic). |
| [storage.md](storage.md) | On-disk layout under `~/.openmemory/`, the SQLite schemas for memory / index / embedding-cache databases, the schema-version migration helper, WAL configuration, and the vector file format. |
| [configuration.md](configuration.md) | The `config.toml` schema by section (`[default]`, `[search]`, `[memory]`, `[index]`, `[watch]`), every environment variable the binary reads, profile semantics, and the per-crate feature-flag matrix. |
| [cli.md](cli.md) | Reference for every `openmemory` subcommand and its flags: `init`, `status`, `mcp`, `consolidate`, `integrate openclaw`, `remember`, `recall`, `list-entities`, `forget-entity`, `completions`, `watch`. |
| [watcher.md](watcher.md) | The `openmemory-watch` crate. What it indexes, the initial-tree walk, the debounced event loop, BLAKE3 dedup, the precedence of `.gitignore` / `.ignore` / `.openmemory-ignore`, and the `file://` URI shape. |
| [development.md](development.md) | The local development loop, MSRV pin, lints and clippy config, the CI matrix, testing discipline, the hosted-Codespace walkthrough for HTTP-transport validation, and security-review checklist. |
| [roadmap.md](roadmap.md) | What shipped in v0.1.0, v0.2.0, and v0.2.1, what is in flight on `[Unreleased]`, and the post-v0.2 backlog (Homebrew, LLM features, Postgres backend, etc.). |

## Quick links

- **Read the source.** Start at
  [`crates/openmemory-cli/src/cli.rs`](../crates/openmemory-cli/src/cli.rs)
  for the CLI surface,
  [`crates/openmemory-mcp/src/tools/mod.rs`](../crates/openmemory-mcp/src/tools/mod.rs)
  for the MCP tool registry, and
  [`crates/openmemory-graph/src/store.rs`](../crates/openmemory-graph/src/store.rs)
  for the knowledge-graph store.
- **Try it.** `cargo install --path crates/openmemory-cli` then
  `openmemory init && openmemory integrate openclaw`.
- **Contribute.** See [`CONTRIBUTING.md`](../CONTRIBUTING.md) for
  the local development loop and the hosted-test walkthrough.
- **Track changes.** [`CHANGELOG.md`](../CHANGELOG.md) records every
  release.

## Stability promise

| Surface | Stability |
|---------|-----------|
| MCP tool names (`openmemory_*`) | Stable across minor versions. Renames require a major bump. |
| MCP tool input field names | Stable across minor versions. |
| SQLite schema versions | Forward-only. A v1 database opened by a newer binary always works after migration. |
| OpenClaw config keys | Tracks OpenClaw's spec. We follow upstream changes there. |
| `~/.openmemory/data/<profile>/` layout | **Not** stable. Treat the data directory as opaque. |
| Public Rust crate APIs | **Not** stable. Pin patch versions. |
| Log line wording | **Not** stable. `OPENMEMORY_LOG=json` is. |

The project is pre-1.0. Minor-version bumps (`0.1 → 0.2`) signal
breaking changes to any stable surface. v1.0 ships when the MCP
tool surface, SQLite schemas, and CLI flag set have lived through
at least one major OpenClaw release without churn.
