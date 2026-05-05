# open-memory — architecture

## Workspace layout

```
open-memory/
├── Cargo.toml                  # workspace + shared deps + lints
├── Cargo.lock
├── rust-toolchain.toml         # MSRV 1.82 (Duration::from_mins, OnceLock)
├── rustfmt.toml
├── clippy.toml                 # workspace clippy.deny
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
│   ├── 03-commit-plan.md
│   ├── 04-source-mapping.md
│   └── 05-quality-gates.md
├── .github/
│   └── workflows/
│       ├── ci.yml              # build/test/clippy/fmt matrix
│       ├── audit.yml           # cargo deny + advisory audit
│       └── release.yml         # tagged release artifacts
└── crates/
    ├── open-memory-core/       # foundation: clock, config, error, migrations
    ├── open-memory-index/      # search backend: runtime FTS5 + BM25 + RRF
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

**Purpose.** The thinnest possible foundation: trait abstractions
(clock, embedder), shared error type, config loader/saver, and the
`Migrator` schema-versioning helper used by every store. The
`Embedder` trait lives here so graph/index code never depends on the
optional ONNX crate.

**Public API.**
```rust
pub mod clock;        // Clock, SystemClock, FixedClock
pub mod config;       // Config, load(), save(), default paths
pub mod embed;        // Embedder, EmbedError, EmbedResult
pub mod error;        // OmError, OmResult
pub mod migrations;   // Migrator (SQLite version-table helper)
pub mod retry;        // with_retry, RetryConfig
pub mod util;         // atomic_write, format_bytes
#[cfg(feature = "testing")]
pub mod testing;      // FakeEmbedder, FixedClock re-exports
```

**Divergence from sift.** Drop `pipeline.rs` (the `SourceItem` /
`ParsedDocument` pipeline types — only used by file scanning). Drop
`types.rs` (most types lived only for the file-indexing pipeline). Keep
only what `open-memory-index` and `open-memory-graph` actually consume.

### `open-memory-index`

**Purpose.** Hybrid search engine. Text in by URI, keyword results
from host SQLite FTS5 when available or pure-Rust BM25 when not,
optional vector results when embeddings are enabled, and final ranking
by Reciprocal Rank Fusion. The default build avoids bundled SQLite and
therefore avoids C/C++ compilation.

**Public API.**
```rust
pub use traits::{VectorIndex, FullTextStore, VectorStore};
pub use flat::{FlatVectorIndex, ExportEntry};
pub use hnsw::HnswIndex;          // feature = "hnsw"
pub use fts5::{Fts5Store, probe_fts5}; // used when host SQLite supports FTS5
pub use bm25::Bm25Store;          // pure-Rust fallback
pub use metadata::MetadataStore;  // SQLite metadata
pub use hybrid::HybridSearchEngine;
pub use cache::CachedSearchEngine;
pub use engine::open_engine;      // single-call factory
pub use error::IndexError;

pub type DefaultVectorStore = …;     // alias picks flat or HNSW by feature
pub enum DefaultFullTextStore { Fts5(Fts5Store), Bm25(Bm25Store) }
```

**Divergence from sift.** Drop `tantivy_store.rs` (the `fulltext`
feature). Keep SQLite FTS5, but make it runtime-probed against the
host SQLite library. When the probe fails, fall back to `Bm25Store` so
the default source build does not compile bundled SQLite C code. Drop
the JSON metadata backend (`json_metadata.rs`) because SQLite metadata
is required everywhere. Drop `simd` feature flag; keep the regular
flat impl and add SIMD later if benchmarks demand it.

### `open-memory-embed`

**Purpose.** Optional and **off by default**. When enabled via
`--features embeddings`, loads ONNX Runtime via the `ort` crate
(which dynamically links the system's ONNX Runtime shared library at
first call), runs Nomic Embed Text v1.5 (default) or Snowflake Arctic
Embed L v2.0, and caches embeddings in SQLite by content hash. When
disabled, the crate compiles as a no-op shim: the core `Embedder`
trait still resolves but every call returns `EmbedError::Unavailable`,
and the hybrid engine sets `vector_used: false` on every response.
`recall()` and `search()` still work; they just answer keyword-only via
FTS5/BM25.

**Public API.**
```rust
pub use onnx::OnnxEmbedder;
pub use models::{Model, ModelRegistry};
pub use cache::EmbeddingCache;
pub use open_memory_core::embed::{Embedder, EmbedError, EmbedResult};
#[cfg(feature = "testing")]
pub use testing::StubEmbedder;
```

**Divergence from sift.** Drop `vision.rs` (image embeddings —
out-of-scope for memory). Drop the `models.rs` registry's `~30` models
down to **two**: `nomic-embed-text-v1.5` (default, 768 dim) and
`snowflake-arctic-embed-l-v2.0` (alternate, 1024 dim). Keep the
download/cache machinery. Drop CUDA / CoreML acceleration features in
v0.1 (CPU-only); the `ort` runtime can grow them back in v0.2 without
breaking anything.

### `open-memory-graph`

**Purpose.** The knowledge graph. SQLite + the index crate's hybrid
engine, kept in lockstep by `MemoryStore`. Entities have stable names;
observations are temporal facts about entities (`valid_from`,
`valid_until`); relations are directed edges. Recall is hybrid search
over observation text, filtered by temporal validity, scored with
Ebbinghaus-style decay and access-frequency boosts.

**Public API.**
```rust
pub use types::{Entity, EntityType, Observation, Relation, MemoryTier};
pub use store::{MemoryStore, RecallOptions, RecallResult};
pub use error::{MemoryError, MemoryResult};
pub use consolidate::{ConsolidateConfig, ConsolidateReport};
```

**Divergence from sift.** Major. Drop:

- `episodes.rs` — the Claude Code hook capture path. Episodes were a
  staging area for hook payloads; without hooks there is nothing to
  stage.
- `rules.rs` — rule-based JSON-payload extraction. 1.7K LOC of
  Claude-Code-hook–specific parsing. None of it transfers.
- `llm.rs` — LLM-driven observation extraction. Useful, but pulls in
  HTTP, retry, secret management. Cut from v0.1; reintroduce as an
  optional `llm` feature later if there is demand.
- The 5-phase Cortex consolidation pipeline drops to **2 phases**:
  - `dedup` — merge near-duplicate observations within an entity
    (text-similarity + cosine if embeddings are on).
  - `decay_prune` — apply decay to observation scores, prune tomb-
    stoned observations and orphaned entities.
  The `episode_processing`, `promotion` (between memory tiers), and
  `skill_extraction` phases either depended on hooks or duplicate work
  the agent already does upstream.
- `PageMutationPlan` and related rewrite-planning types — internals
  of the Anthropic memory-tool adapter. Dropped with the adapter.

What stays from `sift-memory`:

- All of `types.rs` (Entity, Observation, Relation, EntityType,
  MemoryTier).
- All of `schema.rs` (SQLite DDL + migration runner).
- The core read/write paths in `lib.rs` (`open`, `remember`,
  `recall`, `forget`, `forget_entity`, `prune`, `list_entities`,
  `get_entity`).
- Decay scoring, spreading-activation recall, the `RwLock` rebuild
  guard, the transactional `apply_search_sync_ops_with_recovery`
  helper.

### `open-memory-mcp`

**Purpose.** The MCP server. Exposes graph + index operations as MCP
tools over stdio (always) and Streamable HTTP (optional).

**Tool surface.** See `02-openclaw-integration.md` for the full schema.
Eleven tools, all under the `open_memory_*` prefix:

```
open_memory_remember           write   entity-graph
open_memory_recall             read    entity-graph
open_memory_list_entities      read    entity-graph
open_memory_get_entity         read    entity-graph
open_memory_forget             write   entity-graph
open_memory_forget_entity      write   entity-graph
open_memory_consolidate        write   entity-graph
open_memory_status             read    entity-graph + index
open_memory_index_text         write   index
open_memory_search             read    index
open_memory_delete             write   index
```

**Divergence from sift.** All tool names are reprefixed `open_memory_*`
(was `sift_*`). Skill discovery (`sift_search_skills`) and the
`sift_list_sources` browser are dropped — the index is no longer
backed by file URIs walked from disk, so the "list files" mental model
does not fit. The `Tool` trait + registry pattern is preserved
verbatim — it is the cleanest part of the sift-mcp design and what
keeps schema and dispatch from drifting.

### `open-memory-cli`

**Purpose.** The `open-memory` binary. Tiny clap surface, no business
logic — it dispatches to the other crates.

**Subcommands.**
```
open-memory init                 create config + database files
open-memory status               summary of memory + index state
open-memory mcp                  start MCP server (stdio default; --http for HTTP)
open-memory consolidate          run dedup + decay/prune
open-memory integrate openclaw   add/update mcp.servers["open-memory"] in ~/.openclaw/openclaw.json
open-memory remember <args…>     command-line write (mainly for scripting)
open-memory recall <query>       command-line read
open-memory list-entities        list every entity
open-memory forget-entity <id>   destructive
open-memory completions <SHELL>  shell completions (optional feature)
```

**Divergence from sift.** Drop everything below from `sift-cli`:
`scan`, `search` (search the index — actually we keep this, see above
under `recall`/`search`), `daemon`, `watch`, `serve`, `models`,
`bench`, `export`, `list` (sources), `config`, `memory_tool`,
`memory init-hooks`. The remaining CLI is one short clap struct.

The `integrate openclaw` subcommand is the v0.1-defining piece. It
prefers delegating to `openclaw mcp set open-memory '<json>'` when
the binary is on `PATH` so OpenClaw can normalize, validate, and
hot-apply the entry; otherwise it edits `~/.openclaw/openclaw.json`
in place (creating if missing), idempotent, JSON5-aware (preserves
comments and trailing commas where possible), and prints exactly
what changed. See `02-openclaw-integration.md` for the full
contract.

## Storage layout

Default storage root: `~/.open-memory/`. Override with
`OPEN_MEMORY_HOME` env var or `--home <PATH>`.

```
~/.open-memory/
├── config.toml                 # user-level config
└── data/                       # one directory per "profile" (default = "default")
    └── default/
        ├── memory.sqlite       # entities, observations, relations + WAL
        ├── index.sqlite        # metadata + FTS5 tables when host SQLite supports FTS5
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
default = ["completions"]
```

The default deliberately excludes anything that compiles C/C++ code.
It links to the host SQLite library through `rusqlite`, probes FTS5 at
startup, and falls back to the pure-Rust `Bm25Store` when FTS5 is not
available. Minimal Linux builders must provide the system SQLite
library/headers; that is a link prerequisite, not a bundled C
compilation path. Users who want deterministic bundled SQLite opt in
explicitly with `bundled-sqlite`; users who want hybrid vector recall
opt in explicitly with `embeddings`:

```bash
cargo install open-memory --features bundled-sqlite
# or
cargo install open-memory --features embeddings
# or
cargo install open-memory --all-features
```

Toggleable:

| Feature       | Default | Effect |
|---------------|---------|--------|
| `bundled-sqlite` | off  | Compiles SQLite C code through `libsqlite3-sys` for deterministic FTS5 |
| `embeddings`  | **off** | Pulls in `ort` (dynamically linked ONNX Runtime) + Nomic Embed v1.5; ~180 MB model fetched on first MCP call |
| `hnsw`        | off     | usearch-backed approximate vector index (C++ build dep) |
| `mcp-http`    | off     | Streamable HTTP MCP transport |
| `completions` | on      | clap shell completion generation |
| `simd`        | off     | reserved for v0.2 |

Because `embeddings` is off by default, the `open-memory-embed` crate
compiles as a no-op shim in lean builds: the `Embedder` trait is owned
by `open-memory-core`, the lean shim's `OnnxEmbedder::new()` returns
`EmbedError::Unavailable`, and the hybrid engine reports
`vector_used: false` on every response. No `#[cfg]` leaks past the
`embed` crate boundary; the API is identical across build modes.

Binary footprint (stripped, `x86_64-unknown-linux-gnu`, LTO on):

| Build | Approx. size | Build/linkage notes |
|-------|--------------|---------|
| `cargo install open-memory` (default) | 6–8 MB | no bundled SQLite, no C/C++ compilation by open-memory; links host SQLite |
| `cargo install open-memory --features bundled-sqlite` | 7–10 MB | compiles SQLite C code, deterministic FTS5 |
| `cargo install open-memory --features embeddings` | 14–18 MB binary + ~180 MB model on first run | may compile native deps, dynamic `libonnxruntime` |
| `cargo install open-memory --all-features` | 18–24 MB binary | native deps, dynamic `libonnxruntime`, `usearch` C++ |

Build profiles:

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

Identical to sift's; this is the right call.

## Threading model

- **MCP server** is single-threaded async (Tokio current-thread).
  Sufficient for the throughput an agent generates.
- **MemoryStore** wraps a `Mutex<rusqlite::Connection>` (SQLite is
  serial anyway) and exposes a sync API. Calls from MCP are wrapped in
  `tokio::task::spawn_blocking`.
- **Vector rebuild** happens under a `RwLock<()>`: writers grab the
  write lock, recall takes the read lock. Same as sift today —
  preserved verbatim because it solves a real corruption bug.

## Schema versioning

Every SQLite database carries its version in a `*_meta` table
(`memory_meta`, `index_meta`, `embed_meta`). On open, the
`open-memory-core::Migrator` runs forward migrations idempotently and
**refuses** to open a database with a version higher than the binary
supports. This prevents an older binary from corrupting a newer
database — a hardening item that landed in sift v0.1.7 and ports
unchanged.

## What is intentionally *not* abstracted

- No "memory backend" trait. SQLite is the backend. If somebody wants
  Postgres later, they fork.
- No "embedding provider" trait abstraction over remote vs. local.
  The single `Embedder` trait lives in `open-memory-core` so optional
  embeddings do not infect crate layering. ONNX is the only production
  implementation; the trait exists for the lean unavailable shim and
  tests, not provider pluggability.
- No async memory API. SQLite is sync. The MCP layer bridges to async
  via `spawn_blocking`. Async-all-the-way down would just add
  ceremony without throughput.

## Public-API stability

v0.1.x is **pre-stable**. Breaking changes are allowed; bumping the
minor version (0.1 → 0.2) is the signal. v1.0 ships when the MCP tool
surface, the SQLite schema, and the CLI flag set have lived through at
least one major OpenClaw release without churn.
