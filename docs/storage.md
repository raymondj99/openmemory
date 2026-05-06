# Storage layout

Everything `openmemory` persists lives under one root directory.
The default is `~/.openmemory/`; override with `$OPENMEMORY_HOME`
or the global `--home <PATH>` flag. Within that root, the data
directory holds one subdirectory per "profile" (the default profile
name is `default`, mirroring OpenClaw's `--profile <name>` concept).

> The on-disk layout under `~/.openmemory/data/<profile>/` is
> **not** part of the public-stability contract. Treat the data
> directory as opaque from outside the binary. The shape below is
> documentation for contributors and operators, not a contract.

## Directory layout

```
~/.openmemory/
├── config.toml                    # user-level config (TOML; see configuration.md)
└── data/                          # one directory per profile
    └── default/                   # profile name (default = "default")
        ├── memory.sqlite          # entities, observations, relations + WAL
        ├── memory.sqlite-wal      # SQLite WAL sidecar
        ├── memory.sqlite-shm      # SQLite shared memory sidecar
        ├── metadata.sqlite        # URI + source tracking, BLAKE3 hashes
        ├── fulltext.sqlite        # FTS5 keyword index
        │                          # (or bm25.json when --no-default-features)
        ├── vectors.bin            # FlatVectorIndex dump
        │                          # (or HNSW state when --features hnsw)
        └── embeddings/            # only when --features embeddings
            ├── models/            # downloaded ONNX + tokenizer files
            │   ├── nomic-embed-text-v1.5/
            │   │   ├── model.onnx
            │   │   └── tokenizer.json
            │   └── ...
            └── cache.sqlite       # BLAKE3-keyed embedding cache
```

`memory.sqlite`, `metadata.sqlite`, `fulltext.sqlite`, and
`embeddings/cache.sqlite` are independent SQLite databases. They
share no foreign keys at the SQL level; the `MemoryStore` and
`MetadataStore` keep them in lockstep through transactional writes.

## SQLite configuration

Every database is opened with the same pragmas:

| Pragma | Value | Why |
|--------|-------|-----|
| `journal_mode` | `WAL` | Lets the read-only connection pool read while the writer holds its mutex. |
| `synchronous` | `NORMAL` | Safe with WAL; one fsync per commit. |
| `busy_timeout` | `5000` ms | Bounds writer contention. |
| `foreign_keys` | `ON` | The graph schema relies on cascade deletes. |

The reader pool opens connections with `OPEN_READ_ONLY |
OPEN_NO_MUTEX` so they never serialise on internal SQLite locking
that's only relevant to the single writer. See
[architecture.md](architecture.md#threading-model):

## Schema versions

Every database carries its current schema version in a `*_meta`
table. The `openmemory_core::migrations::Migrator` reads the
version on open, applies forward migrations idempotently, and
**refuses to open** a database whose version is higher than the
binary supports. This prevents an older binary from corrupting a
newer database after a downgrade.

| Database | Version table | Current version | Owned by |
|----------|---------------|-----------------|----------|
| `memory.sqlite` | `memory_meta` | 2 (from `MEMORY_SCHEMA_VERSION` in [`crates/openmemory-graph/src/schema.rs`](../crates/openmemory-graph/src/schema.rs)) | `openmemory-graph` |
| `metadata.sqlite` | `index_meta` | 1 | `openmemory-index` |
| `fulltext.sqlite` | (FTS5 virtual table; no version row) | n/a | `openmemory-index` |
| `embeddings/cache.sqlite` | `embed_meta` | 1 | `openmemory-embed` |

Schema upgrades are forward-only. v1 always migrates to v2; the
reverse never works. If you need to roll back, restore from a
backup taken before the upgrade.

## `memory.sqlite` schema

The core tables, owned by
[`crates/openmemory-graph/src/schema.rs`](../crates/openmemory-graph/src/schema.rs):

### `entities`

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PRIMARY KEY | UUIDv7 string. Sortable by creation time. |
| `name` | TEXT | Case-preserving but case-insensitive lookups. |
| `entity_type` | TEXT | One of `person`, `project`, `concept`, `tool`, `preference`, `fact`, `event`, `location`, `organization`. |
| `created_at` | INTEGER | Unix seconds. |
| `updated_at` | INTEGER | Unix seconds; bumped on observation insert and on metadata change. |
| `confidence` | REAL | 0.0–1.0. Default 1.0. |
| `source` | TEXT | Origin tag for audit (e.g. `"cli"`, `"file_watcher"`). |

Indexes: `(name, entity_type)` (unique), `entity_type`.

### `observations`

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PRIMARY KEY | UUIDv7. |
| `entity_id` | TEXT | FK to `entities.id`. ON DELETE CASCADE. |
| `content` | TEXT | The observation text. |
| `observed_at` | INTEGER | Unix seconds. When the observation entered the store. |
| `valid_from` | INTEGER NULL | Unix seconds; null = always valid since `observed_at`. |
| `valid_until` | INTEGER NULL | Unix seconds; null = never expires. |
| `tombstoned` | INTEGER | 0 / 1. Soft-delete flag set by `forget`. |
| `access_count` | INTEGER | Incremented after each successful recall. Drives the retrieval boost. |
| `confidence` | REAL | 0.0–1.0. |
| `source` | TEXT | Origin tag. |
| `memory_tier` | TEXT | One of `episodic`, `semantic`, `procedural`. |

Indexes: `entity_id`, `observed_at`, `tombstoned`.

### `relations`

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PRIMARY KEY | UUIDv7. |
| `source_entity_id` | TEXT | FK to `entities.id`. ON DELETE CASCADE. |
| `target_entity_id` | TEXT | FK to `entities.id`. ON DELETE CASCADE. |
| `relation_type` | TEXT | Free-form (e.g. `"maintains"`, `"prefers"`). |
| `created_at` | INTEGER | Unix seconds. |
| `confidence` | REAL | 0.0–1.0. |

Indexes: `source_entity_id`, `target_entity_id`, `relation_type`.

### `consolidation_metadata`

A single-row table tracking the last consolidation timestamp,
duplicates merged, observations pruned, and entities removed.
Read by `MemoryStore::status` for the `last_consolidation` field.

### `memory_meta`

The schema-version row. Read on every open.

## `metadata.sqlite` schema

Owned by
[`crates/openmemory-index/src/metadata.rs`](../crates/openmemory-index/src/metadata.rs):

The `sources` table tracks every URI the index knows about,
regardless of which store created it. The `kind` discriminator
distinguishes graph observations from `index_text` rows from file
watcher entries.

| Column | Type | Notes |
|--------|------|-------|
| `uri` | TEXT PRIMARY KEY | The caller-supplied URI (e.g. `note://standup`, `file:///path/to/file.md`, or a graph-internal URI). |
| `kind` | TEXT | `observation`, `text`, `file_watcher`, ... |
| `content_hash` | BLOB | BLAKE3 hash of the canonical content; the watcher uses this to skip unchanged files. |
| `size` | INTEGER | Byte length of the canonical content. |
| `chunk_count` | INTEGER | How many chunks were derived from this URI. |
| `status` | TEXT | `live`, `tombstoned`. |
| `created_at` | INTEGER | Unix seconds. |
| `updated_at` | INTEGER | Unix seconds. |

The `index_meta` table holds the schema version and a couple of
maintenance counters.

## `fulltext.sqlite` (FTS5)

When the `fts5` feature is on (the default), keyword search lives
in a SQLite FTS5 virtual table. Schema:

```sql
CREATE VIRTUAL TABLE fts5_entries USING fts5(
    uri,
    text,
    chunk_index UNINDEXED
);
```

Search uses BM25 ranking via the `bm25(fts5_entries)` aggregate
function. There is no separate version row; FTS5's internal
representation is stable for the use case.

When the `fts5` feature is off (`--no-default-features`), the
fallback is `Bm25Store`: a pure-Rust BM25 index serialised to
`bm25.json` next to the would-be `fulltext.sqlite`.

## `vectors.bin` (FlatVectorIndex)

The default vector backend writes a binary dump of `(uri:String,
vector:Vec<f32>)` pairs to `vectors.bin`. Layout (little-endian):

```
[ u32 record_count ]
foreach record:
  [ u32 uri_len ][ uri_len bytes utf-8 ]
  [ u32 dim ][ dim * 4 bytes f32 vector ]
```

`open_engine` loads this on startup; `flush(engine)` rewrites it.
There is no incremental on-disk format; the file is rewritten in
full on each flush. This is fine at graph-sized cardinality (well
under 10⁶ vectors). `cargo install --features hnsw` swaps in the
`HnswIndex` backend, which uses usearch's native serialisation
format instead.

## `embeddings/` directory

Only present when the binary was built with `--features embeddings`
(graph crate) and at least one model has been requested.

### `models/<model-name>/`

Downloaded ONNX file plus tokenizer config. Resolved once at
startup; not re-resolved per request. The path is computed from
`Model::name`. SHA-256 verification runs on every load; a mismatch
surfaces as `EmbedError::ChecksumMismatch` and refuses to start.

### `cache.sqlite`

Keyed by `BLAKE3(content)` (hex string), with the embedding stored
as a BLOB of `f32` little-endian. Same `embed_meta` table for
schema versioning.

```sql
CREATE TABLE cache (
    content_hash BLOB PRIMARY KEY,
    embedding    BLOB NOT NULL
);
```

The hit ratio is high in practice because typical agent traffic
re-embeds the same observation text many times during recall and
consolidation.

When the `sqlite` feature is off, the cache degrades to
`json_cache::EmbeddingCache`: an in-memory `HashMap<String,
Vec<f32>>` serialised to a JSON file.

## Atomic writes

Configuration writes go through `core::util::atomic_write`: write
to a temp file in the same directory, fsync, then rename over the
target. This means a crash mid-write can never leave a partial
config file. SQLite writes use SQLite's own WAL; configuration
writes outside SQLite (the `config.toml`, the
`bm25.json` snapshot, `vectors.bin`) all use atomic-write.

## Multi-profile coexistence

`openmemory --profile alt status` opens
`~/.openmemory/data/alt/`. Two profiles share `config.toml` but
have entirely independent SQLite databases. The OpenClaw
integrator (`openmemory integrate openclaw --profile alt`)
registers the entry under `mcp.servers.openmemory-alt`, so two
profiles can coexist in one OpenClaw config without colliding. See
[openclaw.md](openclaw.md#config-resolution).
