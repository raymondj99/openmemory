# Configuration

Three layers of configuration converge on a running `openmemory`
process: the TOML config file, environment variables, and CLI
flags. CLI flags win over env vars, env vars win over config-file
values, config-file values win over compiled defaults.

## Config file

The default location is `~/.openmemory/config.toml`. Override
with `$OPENMEMORY_HOME` (which moves the entire data root) or
the global `--home <PATH>` flag. `openmemory init` creates the
file with all sections present and defaults populated.

The schema is owned by
[`crates/openmemory-core/src/config.rs`](../crates/openmemory-core/src/config.rs):

```toml
[default]
jobs = 0   # 0 = use Config::num_jobs() (CPU count)

[search]
hybrid_alpha = 0.7
max_results  = 10
rrf_k        = 60

[memory]
decay_rate              = 0.01    # per day
consolidation_interval  = 1800    # seconds (= 30 minutes)
dedup_threshold         = 0.95
prune_floor             = 0.05

[index]
chunk_size = 512
max_chars  = 100_000

[watch]
debounce_ms = 200
extensions  = ["md", "markdown", "txt", "rs", "py"]
max_size    = 10_485_760   # 10 MiB

[normalization]
enabled              = true
auto_merge_threshold = 0.95
flag_threshold       = 0.85
max_candidates       = 100

[engine]
enabled                = false
domains                = 1
shards                 = 32
flush_interval_ms      = 20
shard_capacity         = 4096
flush_threads          = 2
durable_ack            = true
normalize              = true
journal                = true
checkpoint_interval_ms = 1000
```

### `[default]` section

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `jobs` | usize | `0` | Read-only connection-pool size. `0` resolves to `num_cpus`. Cap this if you run on a many-core box but want bounded memory. |

### `[search]` section

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `hybrid_alpha` | f32 | `0.7` | Weight for vector vs. keyword in RRF fusion. `1.0` = vector only; `0.0` = keyword only; `0.7` favours vector. |
| `max_results` | usize | `10` | Default `top_k` cap when callers do not pass one explicitly. |
| `rrf_k` | u32 | `60` | RRF dampening constant. Larger flattens the curve; smaller sharpens it. |

### `[search.field_weights]` section (v0.3)

Per-field BM25 weights applied at index time by the FTS5 keyword
backend. The writer concatenates the v0.3 fielded inputs into the
single FTS5 `text` column, repeating high-weight fields per the
weights below so a match on `title` ranks above a match on `text`.
Weights must be finite and non-negative. Existing indexed rows keep the
weights used when they were written; rebuild or re-index to apply new
weights to old content.

| Key | Type | Default |
|-----|------|---------|
| `title` | f32 | `5.0` |
| `text` | f32 | `1.0` |
| `summary` | f32 | `2.0` |
| `concepts` | f32 | `2.0` |
| `source_files` | f32 | `2.0` |
| `source_kind` | f32 | `0.5` |
| `entity_type` | f32 | `0.5` |
| `entity_name` | f32 | `4.0` |

### `[memory]` section

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `decay_rate` | f64 | `0.01` per day | Lambda in `exp(-lambda * days)`. Higher = faster forgetting. |
| `consolidation_interval` | u64 (secs) | `1800` | Minimum spacing between automatic consolidate runs. No in-process scheduler currently ships; this remains reserved for a future scheduler. |
| `dedup_threshold` | f32 | `0.95` | Jaccard text-similarity threshold for the consolidation dedup pass. |
| `prune_floor` | f32 | `0.05` | Score floor for the consolidation decay-prune pass. |

### `[index]` section

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `chunk_size` | usize | `512` | Soft target chars per chunk for `index_text`. |
| `max_chars` | usize | `100_000` | Hard cap on a single `index_text` payload. |

### `[watch]` section

Used only by the `openmemory-watch` crate.

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `debounce_ms` | u64 | `200` | Debounce window for filesystem events. |
| `extensions` | Vec<String> | The crate's curated list (see [watcher.md](watcher.md#default-extensions)) | Per-tree extension allowlist. |
| `max_size` | u64 (bytes) | `10_485_760` | Per-file size cap. Files larger than this are skipped with `SkipReason::TooLarge`. |

CLI flags (`--debounce-ms`, `--exts`, `--max-size`,
`--no-initial-scan`) override the config-file values for one
process only.

### `[normalization]` section

Entity-name normalization on the `remember` write path. When an
incoming name does not exactly match an existing entity, the
normalizer scores it against recent entities of the same type
using string similarity scoring. Scores above `auto_merge_threshold` silently
redirect to the existing entity; scores in the flag zone create
a new entity with a `SAME_AS` relation; scores below
`flag_threshold` create a new entity with no relation.

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `enabled` | bool | `true` | Toggle normalization on or off. When off, `remember` uses exact-match only (pre-normalization behavior). |
| `auto_merge_threshold` | f64 | `0.95` | Minimum similarity to silently merge into an existing entity. |
| `flag_threshold` | f64 | `0.85` | Minimum similarity to create a `SAME_AS` relation. Must be strictly less than `auto_merge_threshold`. |
| `max_candidates` | usize | `100` | Maximum entities of the same type to compare against, ordered by `updated_at DESC`. |

### `[engine]` section

The write-behind context engine: a sharded ingestion lane in front of
the store for high write concurrency (many agents writing at once).
Submissions hash by entity name onto N shards; background flushers
drain whole shards every epoch and commit each drain as one batched
SQLite transaction. With the journal on, acknowledged writes survive a
crash and replay exactly-once on the next start. Used by the MCP
`openmemory_remember` tool when `enabled`, and always by
`openmemory ingest`.

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `enabled` | bool | `false` | Route MCP `remember` calls through the engine. The tool then returns an ingestion receipt instead of ids. |
| `domains` | usize | `1` | Storage domains: independent SQLite store families with parallel writers, entities hash-routed by name. `1` keeps the classic single-store layout. Pinned per profile (in `domains.toml`) on first partitioned open; change it later with `openmemory migrate-domains --domains N --yes` (offline staging + verified swap). Must divide `shards`. |
| `shards` | usize | `32` | Shard count. Contention divides by this. Must be a multiple of `domains`. |
| `flush_interval_ms` | u64 | `20` | Epoch length: how often idle shards are drained and committed. |
| `shard_capacity` | usize | `4096` | Per-shard queue bound; submission blocks (backpressure) when a shard is full. |
| `flush_threads` | usize | `2` | Background flusher threads. Raise toward `domains` when partitioned so domain commits actually run in parallel. |
| `durable_ack` | bool | `true` | MCP remember waits for the SQLite commit (read-your-writes; costs up to one epoch). Per-call `durable: false` opts out. |
| `normalize` | bool | `true` | Run fuzzy entity-name normalization on drained batches. |
| `journal` | bool | `true` | Keep per-shard crash-recovery journals under `<data_dir>/engine-journal/`. Journals are fsynced before each commit and reclaimed only after a WAL checkpoint makes the commit power-loss durable. |
| `checkpoint_interval_ms` | u64 | `1000` | Maintenance cadence: WAL checkpoints, deferred search-index persistence, and journal reclamation. The engine disables SQLite's in-commit auto-checkpoint and runs it here instead. |

Domain partitioning notes: writes route by entity-name hash, so each
domain owns a disjoint slice of the graph and commits on its own SQLite
writer. Relations whose endpoints live in different domains are
double-written (a mirror edge plus marked stub entities) so both sides
traverse locally; listings and status subtract the bookkeeping rows.
Recall fans out to every domain, merges by score, and memoises merged
results in a facade cache invalidated on every write (measured: ~1 us
repeated-query hits vs ~300 us fanned out, and p99 under epoch-paced
write load drops from ~1.4 ms to ~0.7 ms). Two trade-offs to know
about: fuzzy entity-name normalization cannot merge near-duplicate
names across domains (exact-name matches still route consistently),
and cached recalls freeze decay scores and skip access-count bumps for
up to 60 seconds. Partition when sustained ingest saturates the single
writer (tens of thousands of observations per second); profiles are
the tenant boundary and partition independently; re-home with
`openmemory migrate-domains`. `openmemory watch` does not support
partitioned profiles yet.

## Environment variables

| Variable | Read by | Effect |
|----------|---------|--------|
| `OPENMEMORY_HOME` | every subcommand | Override the data root. Defaults to `~/.openmemory`. The OpenClaw integrator writes this into the MCP entry's `env` block so re-locating the config does not silently break the integration. |
| `OPENMEMORY_PROFILE` | every subcommand (informational) | The OpenClaw integrator writes this so a multi-profile OpenClaw user can pin which memory store is attached to which OpenClaw profile. The CLI's `--profile` flag is what actually selects the data subdirectory at runtime. |
| `OPENMEMORY_HTTP_TOKEN` | `mcp --http` (with `mcp-http` feature) | Bearer token for the Streamable HTTP transport. When set, `/mcp` requires `Authorization: Bearer <token>`. When unset, the server logs a warning and serves unauthenticated. See [mcp.md](mcp.md#bearer-token-authentication). |
| `OPENCLAW_CONFIG_PATH` | `integrate openclaw` | Override the OpenClaw config path. Defaults to `~/.openclaw/openclaw.json`. |

Other variables `clap` reads for global flags:

| Variable | Equivalent CLI flag |
|----------|--------------------|
| `OPENMEMORY_HOME` | `--home <PATH>` |

`openmemory` deliberately does **not** read any `*_KEY`,
`*_TOKEN`, or `*_SECRET` variables besides `OPENMEMORY_HTTP_TOKEN`.
That keeps the secret surface auditable. (The `llm` feature, if it
ships in a future release, will add `ANTHROPIC_API_KEY` /
`OPENAI_API_KEY` / `OLLAMA_HOST` reads behind the same feature
gate.)

## Profiles

A profile is a named subdirectory under
`~/.openmemory/data/<profile>/`. The default profile name is
`default`. Two profiles share the user-level `config.toml` but
have entirely independent SQLite databases, vector files, and
embedding caches.

Switch profiles with `--profile <NAME>`:

```bash
openmemory --profile work status
openmemory --profile personal recall "rust"
```

The OpenClaw integrator handles multi-profile setups by suffixing
the entry name (`mcp.servers.openmemory-work`,
`mcp.servers.openmemory-personal`) so two profiles can coexist
in one OpenClaw config without collision. See
[openclaw.md](openclaw.md):

## Feature flags

Feature flags are opt-in compilation switches. The default install
(`cargo install openmemory`) enables `fts5`, `embeddings`,
`completions`, `watch`, and `mcp-http`.

### Workspace-level features (relevant to `cargo install openmemory`)

| Feature | Default | Effect |
|---------|---------|--------|
| `fts5` | on | SQLite FTS5 keyword backend (BM25 ranking). When off, falls back to the pure-Rust `Bm25Store`. |
| `embeddings` | on | Compiles `openmemory-embed` and links ONNX Runtime via `ort` (load-dynamic). When off, recall is keyword-only. |
| `hnsw` | off | Compiles `HnswIndex` (usearch). Adds a C++ build-time dep. Useful at >10⁵ vectors. |
| `mcp-http` | on | Streamable HTTP transport for the MCP server. Adds `axum`, `tower-http`. |
| `completions` | on | The `openmemory completions <SHELL>` subcommand. Adds `clap_complete`. |
| `watch` | on | The `openmemory watch <PATH>` subcommand. Compiles `openmemory-watch`. |

### Crate-level features (for library consumers)

Each crate has its own feature set; see
[crates.md](crates.md) for the per-crate Cargo.toml summary. Most
features in higher-level crates re-export the same names from
lower crates so a single `--features embeddings` passed to the CLI
flows down to `openmemory-graph` and `openmemory-embed`.

A common reduced build for a keyword-only deployment:

```bash
cargo install --path crates/openmemory-cli \
    --no-default-features \
    --features fts5,completions,watch
```

(That keeps FTS5, completions, and the watcher; drops embeddings
and HNSW so the binary has no ONNX or C++ build dep.)

## Putting it together

| Knob | Where it lives | Wins over |
|------|----------------|-----------|
| Compile-time (feature flags) | Cargo features | nothing |
| `config.toml` defaults | `~/.openmemory/config.toml` | compiled defaults |
| Environment variables | `$OPENMEMORY_*`, `$OPENCLAW_*` | config.toml |
| CLI flags | `--home`, `--profile`, per-subcommand flags | env vars |

The "defaults are good" principle holds: most operators run
`openmemory init && openmemory integrate openclaw` and never
edit `config.toml`. The settings are documented because they are
real escape hatches when default behaviour does not fit the
workload.
