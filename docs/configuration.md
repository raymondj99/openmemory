# Configuration

Three layers of configuration converge on a running `open-memory`
process: the TOML config file, environment variables, and CLI
flags. CLI flags win over env vars, env vars win over config-file
values, config-file values win over compiled defaults.

## Config file

The default location is `~/.open-memory/config.toml`. Override
with `$OPEN_MEMORY_HOME` (which moves the entire data root) or
the global `--home <PATH>` flag. `open-memory init` creates the
file with all sections present and defaults populated.

The schema is owned by
[`crates/open-memory-core/src/config.rs`](../crates/open-memory-core/src/config.rs):

```toml
[default]
jobs = 0   # 0 = use Config::num_jobs() (CPU count)

[search]
hybrid_alpha = 0.6
max_results  = 50
rrf_k        = 60

[memory]
decay_rate              = 0.05    # per day
consolidation_interval  = 86400   # seconds (= 1 day)
dedup_threshold         = 0.95
prune_floor             = 0.05

[index]
chunk_size = 1000
max_chars  = 1_000_000

[watch]
debounce_ms = 200
extensions  = ["md", "markdown", "txt", "rs", "py"]
max_size    = 10_485_760   # 10 MiB
```

### `[default]` section

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `jobs` | usize | `0` | Read-only connection-pool size. `0` resolves to `num_cpus`. Cap this if you run on a many-core box but want bounded memory. |

### `[search]` section

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `hybrid_alpha` | f32 | `0.6` | Weight for vector vs. keyword in RRF fusion. `1.0` = vector only; `0.0` = keyword only; `0.6` slightly favours vector. |
| `max_results` | usize | `50` | Default `top_k` cap when callers do not pass one explicitly. |
| `rrf_k` | u32 | `60` | RRF dampening constant. Larger flattens the curve; smaller sharpens it. |

### `[memory]` section

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `decay_rate` | f64 | `0.05` per day | Lambda in `exp(-lambda * days)`. Higher = faster forgetting. |
| `consolidation_interval` | u64 (secs) | `86400` | Minimum spacing between automatic consolidate runs (no in-process scheduler ships in v0.2; this is for future use). |
| `dedup_threshold` | f32 | `0.95` | Jaccard text-similarity threshold for the consolidation dedup pass. |
| `prune_floor` | f32 | `0.05` | Score floor for the consolidation decay-prune pass. |

### `[index]` section

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `chunk_size` | usize | `1000` | Soft target chars per chunk for `index_text`. |
| `max_chars` | usize | `1_000_000` | Hard cap on a single `index_text` payload. |

### `[watch]` section

Used only by the `open-memory-watch` crate.

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `debounce_ms` | u64 | `200` | Debounce window for filesystem events. |
| `extensions` | Vec<String> | The crate's curated list (see [watcher.md](watcher.md#default-extensions)) | Per-tree extension allowlist. |
| `max_size` | u64 (bytes) | `10_485_760` | Per-file size cap. Files larger than this are skipped with `SkipReason::TooLarge`. |

CLI flags (`--debounce-ms`, `--exts`, `--max-size`,
`--no-initial-scan`) override the config-file values for one
process only.

## Environment variables

| Variable | Read by | Effect |
|----------|---------|--------|
| `OPEN_MEMORY_HOME` | every subcommand | Override the data root. Defaults to `~/.open-memory`. The OpenClaw integrator writes this into the MCP entry's `env` block so re-locating the config does not silently break the integration. |
| `OPEN_MEMORY_PROFILE` | every subcommand (informational) | The OpenClaw integrator writes this so a multi-profile OpenClaw user can pin which memory store is attached to which OpenClaw profile. The CLI's `--profile` flag is what actually selects the data subdirectory at runtime. |
| `OPEN_MEMORY_LOG` | tracing initialisation | Set to `json` for one JSON object per log line (suitable for OpenClaw log capture). Anything else (or unset) gives the human-readable text output. |
| `OPEN_MEMORY_HTTP_TOKEN` | `mcp --http` (with `mcp-http` feature) | Bearer token for the Streamable HTTP transport. When set, `/mcp` requires `Authorization: Bearer <token>`. When unset, the server logs a warning and serves unauthenticated. See [mcp.md](mcp.md#bearer-token-authentication). |
| `OPENCLAW_CONFIG_PATH` | `integrate openclaw` | Override the OpenClaw config path. Defaults to `~/.openclaw/openclaw.json`. |

Other variables `clap` reads for global flags:

| Variable | Equivalent CLI flag |
|----------|--------------------|
| `OPEN_MEMORY_HOME` | `--home <PATH>` |

`open-memory` deliberately does **not** read any `*_KEY`,
`*_TOKEN`, or `*_SECRET` variables besides `OPEN_MEMORY_HTTP_TOKEN`.
That keeps the secret surface auditable. (The `llm` feature, if it
ships in a future release, will add `ANTHROPIC_API_KEY` /
`OPENAI_API_KEY` / `OLLAMA_HOST` reads behind the same feature
gate.)

## Profiles

A profile is a named subdirectory under
`~/.open-memory/data/<profile>/`. The default profile name is
`default`. Two profiles share the user-level `config.toml` but
have entirely independent SQLite databases, vector files, and
embedding caches.

Switch profiles with `--profile <NAME>`:

```bash
open-memory --profile work status
open-memory --profile personal recall "rust"
```

The OpenClaw integrator handles multi-profile setups by suffixing
the entry name (`mcp.servers.open-memory-work`,
`mcp.servers.open-memory-personal`) so two profiles can coexist
in one OpenClaw config without collision. See
[openclaw.md](openclaw.md):

## Feature flags

Feature flags are opt-in compilation switches. The default install
(`cargo install open-memory`) enables `fts5`, `embeddings`,
`completions`, and `watch`.

### Workspace-level features (relevant to `cargo install open-memory`)

| Feature | Default | Effect |
|---------|---------|--------|
| `fts5` | on | SQLite FTS5 keyword backend (BM25 ranking). When off, falls back to the pure-Rust `Bm25Store`. |
| `embeddings` | on | Compiles `open-memory-embed` and links ONNX Runtime via `ort` (load-dynamic). When off, recall is keyword-only. |
| `hnsw` | off | Compiles `HnswIndex` (usearch). Adds a C++ build-time dep. Useful at >10⁵ vectors. |
| `mcp-http` | off | Streamable HTTP transport for the MCP server. Adds `axum`, `tower-http`. |
| `completions` | on | The `open-memory completions <SHELL>` subcommand. Adds `clap_complete`. |
| `watch` | on | The `open-memory watch <PATH>` subcommand. Compiles `open-memory-watch`. |

### Crate-level features (for library consumers)

Each crate has its own feature set; see
[crates.md](crates.md) for the per-crate Cargo.toml summary. Most
features in higher-level crates re-export the same names from
lower crates so a single `--features embeddings` passed to the CLI
flows down to `open-memory-graph` and `open-memory-embed`.

A common reduced build for a keyword-only deployment:

```bash
cargo install --path crates/open-memory-cli \
    --no-default-features \
    --features fts5,completions,watch
```

(That keeps FTS5, completions, and the watcher; drops embeddings
and HNSW so the binary has no ONNX or C++ build dep.)

## Putting it together

| Knob | Where it lives | Wins over |
|------|----------------|-----------|
| Compile-time (feature flags) | Cargo features | nothing |
| `config.toml` defaults | `~/.open-memory/config.toml` | compiled defaults |
| Environment variables | `$OPEN_MEMORY_*`, `$OPENCLAW_*` | config.toml |
| CLI flags | `--home`, `--profile`, per-subcommand flags | env vars |

The "defaults are good" principle holds: most operators run
`open-memory init && open-memory integrate openclaw` and never
edit `config.toml`. The settings are documented because they are
real escape hatches when default behaviour does not fit the
workload.
