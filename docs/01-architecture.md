# open-memory — architecture

## Workspace layout

```
open-memory/
├── Cargo.toml                  # workspace + shared deps + lints
├── Cargo.lock
├── rust-toolchain.toml         # MSRV pin
├── rustfmt.toml
├── clippy.toml                 # workspace-wide disallowed-methods
├── deny.toml                   # cargo-deny config
├── .editorconfig
├── .gitignore
├── README.md
├── CHANGELOG.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── docs/
│   ├── 00-overview.md
│   ├── 01-architecture.md      # ← you are here
│   ├── 02-openclaw-integration.md
│   ├── 03-roadmap.md
│   └── 04-quality-gates.md
├── .github/
│   └── workflows/
│       ├── ci.yml              # build/test/clippy/fmt matrix
│       ├── audit.yml           # cargo-deny + advisory audit
│       └── release.yml         # tagged release artifacts
└── crates/
    ├── open-memory-core/       # foundation: clock, config, error, migrations
    ├── open-memory-index/      # search backend: vector + FTS5 + RRF
    ├── open-memory-embed/      # ONNX embeddings (optional feature)
    ├── open-memory-graph/      # knowledge graph: entities, observations, relations
    ├── open-memory-mcp/        # MCP server + tool router
    └── open-memory-cli/        # binary `open-memory`
```

## Dependency graph

```
                   open-memory-core
                       │       │
            ┌──────────┘       └─────────────┐
            ▼                                 ▼
     open-memory-index               open-memory-embed (optional)
            │                                 │
            └──────────┐         ┌────────────┘
                       ▼         ▼
                 open-memory-graph
                       │
                       ▼
                 open-memory-mcp
                       │
                       ▼
                 open-memory-cli
```

Strict layering. No upward edges. `open-memory-core` depends on no
internal crate; `open-memory-cli` depends on every other crate.

## Crate-by-crate

### `open-memory-core`

The thinnest possible foundation: trait abstractions (clock,
embedder), shared error type, config loader/saver, and the
`Migrator` schema-versioning helper used by every store.

```rust
pub mod clock;        // Clock, SystemClock, FixedClock
pub mod config;       // Config, load(), save(), default paths
pub mod error;        // OmError, OmResult
pub mod migrations;   // Migrator (SQLite version-table helper)
pub mod retry;        // with_retry, RetryConfig
pub mod util;         // atomic_write, format_bytes
#[cfg(feature = "testing")]
pub mod testing;      // FakeEmbedder, FixedClock re-exports
```

Nothing pipeline-shaped lives here — there is no parser stage, no
chunker stage, no source stage. The crate exists to give the
graph and index crates a shared clock, error, and schema-migration
vocabulary.

### `open-memory-index`

Hybrid search engine. Text in by URI, hybrid (vector + FTS5 BM25)
results out, ranked by Reciprocal Rank Fusion. Pluggable backends
behind feature flags but only one of each compiled at a time.

```rust
pub use traits::{VectorIndex, FullTextStore, VectorStore};
pub use flat::{FlatVectorIndex, ExportEntry};
pub use hnsw::HnswIndex;          // feature = "hnsw"
pub use fts5::Fts5Store;          // default
pub use bm25::Bm25Store;          // when fts5 is off
pub use metadata::MetadataStore;  // SQLite metadata
pub use hybrid::HybridSearchEngine;
pub use cache::CachedSearchEngine;
pub use engine::open_engine;      // single-call factory
pub use error::IndexError;

pub type DefaultVectorStore = …;     // alias picks flat or HNSW by feature
pub type DefaultFullTextStore = …;   // alias picks FTS5 or BM25 by feature
```

One fulltext backend (FTS5), one metadata backend (SQLite). Plurality
behind feature flags is not a goal in v0.1; we pick the backend that
works well and own it.

### `open-memory-embed`

Optional. Loads ONNX Runtime, runs Nomic Embed Text v1.5 (default) or
Snowflake Arctic Embed L v2.0, caches embeddings in SQLite by content
hash. When disabled, the entire embedding feature gates off and the
system runs keyword-only — `recall()` still works, just with no
vector contribution to RRF.

```rust
pub use onnx::OnnxEmbedder;
pub use models::{Model, ModelRegistry};
pub use cache::EmbeddingCache;
pub use traits::Embedder;
#[cfg(feature = "testing")]
pub use testing::StubEmbedder;
```

CPU-only in v0.1. CUDA / CoreML acceleration are post-v0.1
experiments behind the `ort` runtime — adding them later does not
break the library API.

### `open-memory-graph`

The knowledge graph. SQLite + the index crate's hybrid engine, kept
in lockstep by `MemoryStore`. Entities have stable names; observations
are temporal facts about entities (`valid_from`, `valid_until`);
relations are directed edges. Recall is hybrid search over
observation text, filtered by temporal validity, scored with
Ebbinghaus-style decay and access-frequency boosts.

```rust
pub use types::{Entity, EntityType, Observation, Relation, MemoryTier};
pub use store::{MemoryStore, RecallOptions, RecallResult};
pub use error::{MemoryError, MemoryResult};
pub use consolidate::{ConsolidateConfig, ConsolidateReport};
```

Consolidation runs in two phases:

- **dedup** — merge near-duplicate observations within an entity
  (text similarity ≥ 0.95, optionally cosine ≥ 0.92 if embeddings
  are enabled).
- **decay_prune** — apply decay to observation scores, prune
  tomb-stoned observations and orphaned entities.

### `open-memory-mcp`

The MCP server. Exposes graph + index operations as MCP tools over
stdio (always) and Streamable HTTP (optional, behind the `mcp-http`
feature).

Eleven tools, all `open_memory_*`. See
[`02-openclaw-integration.md`](02-openclaw-integration.md) for the
full schema. The `Tool` trait colocates the JSON-Schema descriptor
and the router handler, so the tool listing an agent sees in
`tools/list` cannot drift from what the dispatcher actually answers.

### `open-memory-cli`

The `open-memory` binary. Tiny `clap` surface, no business logic — it
dispatches to the other crates.

```
open-memory init                 create config + database files
open-memory status               summary of memory + index state
open-memory mcp                  start MCP server (stdio default; --http for HTTP)
open-memory consolidate          run dedup + decay/prune
open-memory integrate openclaw   write entry into ~/.openclaw/mcp.json
open-memory remember <args…>     command-line write (mainly for scripting)
open-memory recall <query>       command-line read
open-memory list-entities        list every entity
open-memory forget-entity <id>   destructive
open-memory completions <SHELL>  shell completions (optional feature)
```

The `integrate openclaw` subcommand is the v0.1-defining piece:
edit-in-place `~/.openclaw/mcp.json` (creating if missing), idempotent,
JSON5-aware (preserves comments and trailing commas where possible),
prints exactly what changed.

## Storage layout

Default storage root: `~/.open-memory/`. Override with
`OPEN_MEMORY_HOME` env var or `--home <PATH>`.

```
~/.open-memory/
├── config.toml                 # user-level config
└── data/                       # one directory per "profile" (default = "default")
    └── default/
        ├── memory.sqlite       # entities, observations, relations + WAL
        ├── index.sqlite        # FTS5 fulltext + metadata
        ├── vectors.bin         # flat vector dump (or HNSW with --features hnsw)
        └── embeddings/         # ONNX models + cache (when embed feature on)
            ├── models/
            └── cache.sqlite
```

The `default` profile name mirrors OpenClaw's `--profile <name>`
concept. Multiple OpenClaw profiles each get their own subdirectory
without bleeding memories across.

## Feature-flag matrix

Default features (what `cargo install open-memory` gives you):

```toml
default = ["fts5", "embeddings", "completions"]
```

Toggleable:

| Feature       | Default | Effect |
|---------------|---------|--------|
| `fts5`        | on      | SQLite FTS5 backend (BM25 keyword) |
| `embeddings`  | on      | ONNX Runtime + Nomic Embed v1.5 |
| `hnsw`        | off     | usearch-backed approximate vector index |
| `mcp-http`    | off     | Streamable HTTP MCP transport |
| `completions` | on      | clap shell completion generation |
| `simd`        | off     | reserved for v0.2 |

Build profiles:

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

## Threading model

- **MCP server** is single-threaded async (Tokio current-thread).
  Sufficient for the throughput an agent generates.
- **MemoryStore** wraps a `Mutex<rusqlite::Connection>` (SQLite is
  serial anyway) and exposes a sync API. Calls from MCP are wrapped
  in `tokio::task::spawn_blocking`.
- **Vector rebuild** happens under a `RwLock<()>`: writers grab the
  write lock, recall takes the read lock. This prevents recall from
  observing a half-rebuilt vector index during a bulk import.

## Schema versioning

Every SQLite database carries its version in a `*_meta` table
(`memory_meta`, `index_meta`, `embed_meta`). On open, the
`open-memory-core::Migrator` runs forward migrations idempotently and
**refuses** to open a database with a version higher than the binary
supports. This prevents an older binary from corrupting a newer
database after a downgrade.

## What is intentionally *not* abstracted

- No "memory backend" trait. SQLite is the backend. If somebody
  wants Postgres later, they fork or write a feature-flagged
  alternative.
- No "embedding provider" trait abstraction over remote vs. local.
  ONNX is the implementation; the `Embedder` trait exists for tests
  and stubs only, not pluggability.
- No async memory API. SQLite is sync. The MCP layer bridges to
  async via `spawn_blocking`. Async-all-the-way down would just add
  ceremony without throughput.

## Public-API stability

v0.1.x is **pre-stable**. Breaking changes are allowed; bumping the
minor version (0.1 → 0.2) is the signal. v1.0 ships when the MCP
tool surface, the SQLite schema, and the CLI flag set have lived
through at least one major OpenClaw release without churn.
