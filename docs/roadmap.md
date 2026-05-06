# Roadmap

This document is the version-by-version rear view: what shipped,
what is in flight on the unreleased branch, and what is queued
for after the current release. The authoritative per-release
detail lives in [`CHANGELOG.md`](../CHANGELOG.md); this file
gives the bird's-eye view.

## Released

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
- **Filesystem watcher.** New `open-memory-watch` crate with
  initial-tree walk plus `notify-debouncer-full` event loop,
  BLAKE3-deduped against the metadata store, ignore-file
  precedence (`.gitignore`, `.ignore`, `.open-memory-ignore`),
  and a curated default extension list. CLI exposed as
  `open-memory watch <PATH>`.

Behind the scenes:

- New workspace dependencies: `notify` 8, `notify-debouncer-full`
  0.7, `ignore` 0.4, `walkdir` 2. All four pin to versions whose
  `rust-version` metadata is at or below 1.85, so MSRV stays
  pinned.
- New public surface: `MemoryStatus::reader_pool_size`, the
  `open-memory-watch` crate, the `[watch]` section in
  `config.toml`, the `open-memory watch` CLI subcommand. Workspace
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

- `open-memory-core`. `Clock` trait + `SystemClock` /
  `FixedClock`; `OmError` / `OmResult` thiserror enum; SQLite
  `Migrator`; `Config` loader/saver with
  `~/.open-memory/config.toml` and `OPEN_MEMORY_HOME` override;
  exponential-backoff retry helper; `Embedder` trait stub.
- `open-memory-index`. `FlatVectorIndex` (brute-force cosine);
  optional `HnswIndex` (usearch, behind `hnsw`); `Fts5Store`
  (default) and `Bm25Store` (`--no-default-features`); `MetadataStore`;
  `HybridSearchEngine` with RRF fusion; `CachedSearchEngine`;
  `open_engine` factory; criterion benches.
- `open-memory-embed`. ONNX runner (CPU only); model registry
  with `nomic-embed-text-v1.5` (default, 768-dim) and
  `snowflake-arctic-embed-l-v2.0` (alternate, 1024-dim); SQLite
  embedding cache keyed by BLAKE3.
- `open-memory-graph`. `MemoryStore` with bi-temporal entity /
  observation / relation types; atomic `remember`; hybrid
  `recall` with Ebbinghaus decay + spreading activation;
  `forget` (soft) / `forget_entity` (hard cascade) / `prune`
  (sweep tombstones + orphans); idempotent `consolidate` (dedup +
  decay-prune).
- `open-memory-mcp`: minimal hand-rolled JSON-RPC 2.0 MCP
  server (no `rmcp` dependency: every published rmcp release
  requires rustc 1.88+); eleven `open_memory_*` tools registered
  through a single `Tool` trait; stdio always; Streamable HTTP
  behind `mcp-http`.
- `open-memory-cli`. `open-memory` binary with `init`, `status`,
  `mcp`, `consolidate`, `integrate openclaw`, plus scriptable
  `remember` / `recall` / `list-entities` / `forget-entity` and
  shell `completions`.

End-to-end MCP test in `tests/mcp_e2e.rs` spawns the real binary
and exercises every tool over stdio JSON-RPC.

## Unreleased

`[Unreleased]` in
[`CHANGELOG.md`](../CHANGELOG.md#unreleased) is the production-
hardening pass on top of v0.2.0. The themes:

- **HTTP-transport bearer-token auth.** `OPEN_MEMORY_HTTP_TOKEN`
  reads on startup; when set, every `POST /mcp` request must carry
  `Authorization: Bearer <token>` or it gets a 401 with
  `WWW-Authenticate: Bearer` and a JSON-RPC `-32600` envelope.
  Constant-time comparison; redacting `Debug` impl. `/healthz` is
  never auth-gated. Documented in
  [mcp.md](mcp.md#bearer-token-authentication).
- **SHA-256 model integrity verification.**
  `OnnxEmbedder::load_for_model` hashes the on-disk `model.onnx`
  and `tokenizer.json` files before handing them to ONNX Runtime;
  mismatches surface as `EmbedError::ChecksumMismatch` with a
  "refusing to load" message. The new `integrity` module streams
  files in 64 KiB blocks and treats empty hashes as
  `VerificationOutcome::Skipped` (the v0.2.0 placeholder for the
  shipped registry models; populating real hashes tightens this
  to "always verified" with no further code changes).
- **CI gates.** `cargo test --workspace --no-default-features`,
  `cargo clippy --workspace --no-default-features --all-targets
  -- -D warnings`, and `cargo doc --workspace --no-deps
  --all-features` now run on every push. The first two would have
  caught a feature-gated import bug in `open-memory-watch`; the
  third catches intra-doc links that resolve only when an
  optional module compiles.
- **Bug fix.** `open_memory_mcp::http::handle_mcp` no longer
  constructs the 204 notification response via
  `Response::builder().unwrap()`; it returns
  `StatusCode::NO_CONTENT.into_response()` directly. The
  `application/json` content-type header is set via the
  infallible `HeaderValue::from_static`. No more panics on the
  request path.

The `open-memory-watch` Cargo.toml also dropped its own `default
= ["fts5"]` feature and pulls `sqlite + fts5` directly from
`open-memory-index` and `open-memory-graph`, so the crate now
compiles under `cargo test --workspace --no-default-features`.

## Backlog (post-v0.2)

Items explicitly **not** in the current release, queued for
future minor versions:

- **LLM-powered observation extraction.** `--features llm` for an
  optional LLM-driven extraction path with three providers:
  `anthropic` (`ANTHROPIC_API_KEY`), `openai` (`OPENAI_API_KEY`,
  `OPENAI_BASE_URL`), `ollama` (`OLLAMA_HOST`). Read-time only;
  storage stays unchanged. Earliest target: v0.3.
- **Real SHA-256 hashes in the model registry.** The current
  `Model` constants carry empty hashes (`VerificationOutcome::Skipped`
  on load). Populating real hashes is a registry-level change
  with no code change required to enforce.
- **Multi-chunk file ingestion in the watcher.** v0.2 writes one
  chunk per file (`chunk_index = 0`). Multi-chunk per-file
  ingestion lets long files surface fragments individually in
  recall. Earliest target: v0.3.
- **`.open-memory-ignore` re-evaluation on events.** v0.2 honours
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

The MCP tool surface (`open_memory_*` tool names and field
names), the SQLite schema versions (forward-only migration), and
the OpenClaw config JSON keys are stable across minor versions.
Renames or removals require a major-version bump.

The Rust crate API (any `pub` symbol in any crate) is **not**
stable. Library consumers should pin patch versions. The on-disk
directory layout under `~/.open-memory/data/<profile>/` is **not**
stable; treat the data directory as opaque.

See [architecture.md](architecture.md#public-api-stability) for
the full stability matrix.
