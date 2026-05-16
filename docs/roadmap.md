# Roadmap

This document is the version-by-version rear view: what shipped,
what is in flight on the unreleased branch, and what is queued
for after the current release. The authoritative per-release
detail lives in [`CHANGELOG.md`](../CHANGELOG.md); this file
gives the bird's-eye view.

## Released

### v0.2.1 (2026-05-16). Production-hardening patch

A patch release on top of v0.2.0. The MCP tool surface is
unchanged at v0.1; `openmemory_remember` responses gain an
optional `normalized` field. Five themes:

- **Entity normalization on the `remember` write path.** Fuzzy
  matches incoming entity names against existing entities of the
  same type to prevent near-duplicate fragmentation
  (`"ProjectAlpha"` / `"Project Alpha"` / `"project alpha"`
  collapse to one entity). Three configurable thresholds in the
  new `[normalization]` config section: auto-merge (>= 0.95),
  flag with `SAME_AS` relation (0.85-0.95), or create new entity
  (< 0.85). Scoring is `JW.min(NLev)` over token-sorted and
  alnum-stripped forms; the slow-path floor prevents prefix-
  biased false positives like `"Topic1"` / `"Topic10"`. New
  public Rust type `NormalizeMatch`; new optional `normalized`
  field on `openmemory_remember` responses; enabled by default.
- **Explicit embedding-model management.**
  `openmemory model list` reports every registry model and its
  cache status; `openmemory model download [MODEL]` fetches the
  graph, tokenizer, and (when present) external-data files into
  `~/.openmemory/models/<model>/`; `openmemory model use <name>`
  writes `default.model` to `config.toml` so the next process
  picks it up. Downloads stream in 64 KiB chunks, enforce
  per-file size caps, verify `Content-Length`, retry transients,
  use a `.part` sibling for the final rename, and check SHA-256
  against the registry hash.
- **SHA-256 model integrity verification.**
  `OnnxEmbedder::load_for_model` hashes `model.onnx` and
  `tokenizer.json` before handing them to ONNX Runtime;
  mismatches surface as `EmbedError::ChecksumMismatch`. Both
  shipped models have recorded hashes; empty hashes still
  surface as `VerificationOutcome::Skipped` (warns, loads).
- **Bearer-token auth on the HTTP transport.**
  `OPENMEMORY_HTTP_TOKEN` reads on startup; when set, every
  `POST /mcp` request must carry `Authorization: Bearer <token>`
  or it gets a 401 with `WWW-Authenticate: Bearer` and a
  JSON-RPC `-32600` envelope. Constant-time comparison;
  redacting `Debug` impl. `/healthz` is never auth-gated. With
  the env var unset the server logs a warning and serves
  unauthenticated, preserving the local-dev workflow.
- **CI gates.** `cargo test --workspace --no-default-features`,
  `cargo clippy --workspace --no-default-features --all-targets
  -- -D warnings`, and `cargo doc --workspace --no-deps
  --all-features` now run on every push. The first two catch
  feature-gated import bugs; the third catches intra-doc links
  that resolve only when an optional module compiles.

Notable Rust-side changes (the Rust API is documented as
unstable; library consumers should pin patch versions):

- `HybridSearchEngine::search` no longer takes a `chunk_index`
  parameter.
- `mcp-http` becomes a default feature in `openmemory-cli`
  (opt out via `--no-default-features`).
- `openmemory-watch` drops its own `default = ["fts5"]` feature
  and pulls `sqlite + fts5` directly from
  `openmemory-index` / `openmemory-graph`.
- New public type `NormalizeMatch` and new `[normalization]`
  config section.

Test coverage added in this release: a normalization golden
table plus property tests in `openmemory-graph::normalize` lock
in the scoring contract; the `remember` test module pins the
`SAME_AS` row shape, candidate recency tie-break,
`max_candidates` truncation, cross-type isolation, and orphan-
entity reachability; a new concurrent-near-duplicate integration
test asserts that eight threads each writing a variant of the
same name coalesce into one entity.

### v0.2.0 (2026-05-05). Multi-agent memory + filesystem watcher

Two themes:

- **Multi-agent memory.** `MemoryStore` now opens a pool of
  read-only `rusqlite::Connection`s alongside the writer mutex, so
  concurrent recall calls execute in parallel instead of
  serialising on a single mutex. WAL mode plus
  `OPEN_READ_ONLY | OPEN_NO_MUTEX` keeps the pool from blocking
  the writer (or vice versa). Pool size defaults to
  `Config::num_jobs()` (CPU count). The shared-writer fallback for
  `MemoryStore::open_in_memory` keeps the API uniform across
  on-disk and ephemeral test instances.
- **Filesystem watcher.** New `openmemory-watch` crate with
  initial-tree walk plus `notify-debouncer-full` event loop,
  BLAKE3-deduped against the metadata store, ignore-file
  precedence (`.gitignore`, `.ignore`, `.openmemory-ignore`),
  and a curated default extension list. CLI exposed as
  `openmemory watch <PATH>`.

Behind the scenes:

- New workspace dependencies: `notify` 8, `notify-debouncer-full`
  0.7, `ignore` 0.4, `walkdir` 2. All four pin to versions whose
  `rust-version` metadata is at or below 1.85, so MSRV stays
  pinned.
- New public surface: `MemoryStatus::reader_pool_size`, the
  `openmemory-watch` crate, the `[watch]` section in
  `config.toml`, the `openmemory watch` CLI subcommand. Workspace
  version bumped to `0.2.0` to reflect the breaking surface
  changes.
- The MCP tool surface is **unchanged** at v0.1; no MCP tool was
  added, renamed, or removed.

Test coverage: a concurrent-recall integration test asserts
parallel time strictly less than the fully-serial bound (the
numeric speedup ratio originally shipped was relaxed in the
production-hardening pass after it proved flaky on shared CI
runners). Watcher integration test covers create / modify /
delete / ignore-respect / dedup-on-restart, plus a latency smoke
test.

### v0.1.0 (2026-05-05). Initial release

The first end-to-end release: a persistent knowledge-graph memory
store plus hybrid text index, exposed as eleven MCP tools,
integrated into OpenClaw with one command.

Crates landed:

- `openmemory-core`. `Clock` trait + `SystemClock` /
  `FixedClock`; `OmError` / `OmResult` thiserror enum; SQLite
  `Migrator`; `Config` loader/saver with
  `~/.openmemory/config.toml` and `OPENMEMORY_HOME` override;
  exponential-backoff retry helper; `Embedder` trait stub.
- `openmemory-index`. `FlatVectorIndex` (brute-force cosine);
  optional `HnswIndex` (usearch, behind `hnsw`); `Fts5Store`
  (default) and `Bm25Store` (`--no-default-features`); `MetadataStore`;
  `HybridSearchEngine` with RRF fusion; `CachedSearchEngine`;
  `open_engine` factory; criterion benches.
- `openmemory-embed`. ONNX runner (CPU only); model registry
  with `nomic-embed-text-v1.5` (default, 768-dim) and
  `snowflake-arctic-embed-l-v2.0` (alternate, 1024-dim); SQLite
  embedding cache keyed by BLAKE3.
- `openmemory-graph`. `MemoryStore` with bi-temporal entity /
  observation / relation types; atomic `remember`; hybrid
  `recall` with Ebbinghaus decay + spreading activation;
  `forget` (soft) / `forget_entity` (hard cascade) / `prune`
  (sweep tombstones + orphans); idempotent `consolidate` (dedup +
  decay-prune).
- `openmemory-mcp`: minimal hand-rolled JSON-RPC 2.0 MCP
  server (no `rmcp` dependency: every published rmcp release
  requires rustc 1.88+); eleven `openmemory_*` tools registered
  through a single `Tool` trait; stdio always; Streamable HTTP
  behind `mcp-http`.
- `openmemory-cli`. `openmemory` binary with `init`, `status`,
  `mcp`, `consolidate`, `integrate openclaw`, plus scriptable
  `remember` / `recall` / `list-entities` / `forget-entity` and
  shell `completions`.

End-to-end MCP test in `tests/mcp_e2e.rs` spawns the real binary
and exercises every tool over stdio JSON-RPC.

## Unreleased

Nothing queued yet. See
[`CHANGELOG.md`](../CHANGELOG.md#unreleased) for the live list.

## Backlog (post-v0.2)

Items explicitly **not** in the current release, queued for
future minor versions:

- **LLM-powered observation extraction.** `--features llm` for an
  optional LLM-driven extraction path with three providers:
  `anthropic` (`ANTHROPIC_API_KEY`), `openai` (`OPENAI_API_KEY`,
  `OPENAI_BASE_URL`), `ollama` (`OLLAMA_HOST`). Read-time only;
  storage stays unchanged. Earliest target: v0.3.
- **Multi-chunk file ingestion in the watcher.** v0.2 writes one
  chunk per file (`chunk_index = 0`). Multi-chunk per-file
  ingestion lets long files surface fragments individually in
  recall. Earliest target: v0.3.
- **`.openmemory-ignore` re-evaluation on events.** v0.2 honours
  it on the initial scan only. The event loop should re-walk the
  ignore matcher when ignore files themselves change.
- **Homebrew tap and `install.sh`.** Cross-arch release pipelines
  beyond the existing three-target tarballs.
- **Postgres backend behind a `postgres` feature flag.** The
  community ask. Would mean writing a real `MemoryBackend` trait;
  a hard call until the second-consumer story is concrete.
- **HNSW + SIMD distance functions enabled by default.** If the
  benchmarks show they pay off below 10⁶ vectors, flip the
  defaults.
- **Cargo fuzz targets** for the schema migration runner and the
  MCP request decoder.

## How releases are cut

The release process is documented in
[development.md](development.md#release-process). Quick recap:

1. Promote `[Unreleased]` to a versioned section in
   `CHANGELOG.md`; add a fresh `[Unreleased]` block.
2. `cargo release <minor|patch> --workspace --execute`.
3. The `release.yml` workflow fires on the tag, builds the three
   tarballs (`aarch64-apple-darwin`, `x86_64-apple-darwin`,
   `x86_64-unknown-linux-gnu`), and attaches them with SHA-256
   checksums to a GitHub Release.
4. After the release is sanity-checked, a maintainer runs `cargo
   publish` per crate in dependency order. crates.io publishing
   is **not** automated.

## Stability commitments going forward

The MCP tool surface (`openmemory_*` tool names and field
names), the SQLite schema versions (forward-only migration), and
the OpenClaw config JSON keys are stable across minor versions.
Renames or removals require a major-version bump.

The Rust crate API (any `pub` symbol in any crate) is **not**
stable. Library consumers should pin patch versions. The on-disk
directory layout under `~/.openmemory/data/<profile>/` is **not**
stable; treat the data directory as opaque.

See [architecture.md](architecture.md#public-api-stability) for
the full stability matrix.
