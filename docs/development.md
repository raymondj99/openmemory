# Development

This document is the authoritative source for how to build, test,
lint, and verify changes to `openmemory`. The repo is small enough
that the development loop fits on one screen.

For a contribution-focused walkthrough (commit hygiene, hosted-test
walkthrough, security reporting), see
[`CONTRIBUTING.md`](../CONTRIBUTING.md):

## Local development loop

```bash
cargo fmt --all
cargo build --workspace --locked
cargo test --workspace --all-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

CI mirrors all of the above, plus `--no-default-features` variants
of `test` and `clippy`, plus a default-features `cargo doc` gate.
Get the full set green locally before pushing;
`--no-default-features` in particular has caught feature-gated
import bugs that the default-features matrix misses.

Daemon/admin changes also run the feature-specific production gate:

```bash
./scripts/daemon_quality_monitor.sh
```

Set `OPENMEMORY_DAEMON_MONITOR_BENCH=1` to include the local
`daemon_admin_api` Criterion run. The monitor saves that run under a
per-run `daemon-monitor-*` baseline so stale local Criterion baselines
do not turn the production gate into unrelated comparison noise. CI
runs the same monitor without the local benchmark; the CodSpeed
benchmark workflow runs the daemon admin API group with the rest of
`openmemory-bench`.

## CI matrix

The `.github/workflows/ci.yml` workflow runs on every push and
pull request. It is hard-required green on every row before
merging to `main`.

| Job | OS | Toolchain | Features | Command |
|-----|-----|-----------|----------|---------|
| `build-test` (ubuntu) | ubuntu-latest | 1.85.0 | default | `cargo build --locked` then `cargo test --locked` |
| `build-test` (macos) | macos-latest | 1.85.0 | default | `cargo build --locked` then `cargo test --locked` |
| `test-no-default-features` | ubuntu-latest | 1.85.0 | `--no-default-features` | `cargo test --locked --no-default-features` |
| `fmt` | ubuntu-latest | 1.85.0 | (n/a) | `cargo fmt --all -- --check` |
| `clippy-default` | ubuntu-latest | 1.85.0 | default | `cargo clippy --locked --all-targets -- -D warnings` |
| `clippy-no-default` | ubuntu-latest | 1.85.0 | `--no-default-features` | `cargo clippy --locked --no-default-features --all-targets -- -D warnings` |
| `doc-default` | ubuntu-latest | 1.85.0 | default | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` |
| `doc-all-features` | ubuntu-latest | 1.85.0 | `--all-features` | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` |
| `daemon-production-gate` | ubuntu-latest | 1.85.0 | daemon gate | `./scripts/daemon_quality_monitor.sh` |

The `audit` workflow (`.github/workflows/audit.yml`) runs
`cargo-deny check` weekly (Monday 06:00 UTC) and on every push.
It checks licenses, advisories, banned crates, and source
allowlists per `deny.toml`.

The `release` workflow (`.github/workflows/release.yml`) builds
release tarballs for `aarch64-apple-darwin`,
`x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu` on tag
push (`v*.*.*`) or manual dispatch.

## MSRV

**Pinned to Rust 1.85.0.** `rust-toolchain.toml` pins the channel;
CI runs against the same version. Bumping MSRV requires a
CHANGELOG note and a minor-version bump.

The MSRV pin is what forces the workspace to ship a hand-rolled
JSON-RPC server in `openmemory-mcp::protocol` instead of using
the upstream `rmcp` SDK. Every published `rmcp` release uses
`if-let` chain syntax that requires Rust 1.88+. We also pin
`ort` / `ort-sys` to `2.0.0-rc.9` so the embedding stack stays
under the 1.85 bar. If a new dependency needs 1.88+, find an older
version that does not.

## Lints

`Cargo.toml` workspace `[lints]` section:

```toml
[workspace.lints.rust]
unsafe_code = "warn"
unknown_lints = "allow"          # MSRV-friendly forward-compat

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
# Pragmatic allow-list (set at workspace level so every crate inherits):
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
wildcard_imports = "allow"
cast_possible_truncation = "allow"
cast_precision_loss = "allow"
cast_sign_loss = "allow"
cast_possible_wrap = "allow"
too_many_lines = "allow"
similar_names = "allow"
unnecessary_wraps = "allow"
match_same_arms = "allow"
items_after_statements = "allow"
needless_pass_by_value = "allow"
trivially_copy_pass_by_ref = "allow"
doc_markdown = "allow"
unreadable_literal = "allow"
return_self_not_must_use = "allow"
if_not_else = "allow"
uninlined_format_args = "allow"
manual_let_else = "allow"
redundant_closure_for_method_calls = "allow"
case_sensitive_file_extension_comparisons = "allow"
needless_raw_string_hashes = "allow"
option_if_let_else = "allow"
str_split_at_newline = "allow"
ref_option = "allow"
disallowed_methods = "warn"      # actual list in clippy.toml
```

`clippy.toml` keeps the workspace `disallowed-methods` list:

- `std::thread::sleep`: outside test code. Async code uses
  `tokio::time::sleep`; tests can opt in with `#[allow]`.

CI runs clippy with `-D warnings`, so warnings fail the build.

## Testing discipline

| Crate | Unit tests | Integration tests |
|-------|-----------|-------------------|
| `openmemory-core` | inline (`#[cfg(test)] mod tests` in each source file) | none |
| `openmemory-index` | inline | criterion benches in `benches/` |
| `openmemory-embed` | inline | `tests/onnx_smoke.rs` |
| `openmemory-graph` | inline | `tests/integration.rs` |
| `openmemory-mcp` | inline | (covered by the CLI's `tests/mcp_e2e.rs`) |
| `openmemory-cli` | inline | `tests/mcp_e2e.rs` |
| `openmemory-watch` | inline | `tests/integration.rs` |

Tests must:

- Run in well under one second each (a slow test needs a
  justification comment).
- Be deterministic. No `tokio::time::sleep` against real wall
  clock; use `tokio::time::pause`. The watcher tests synchronise
  on the `BatchSummary` notifier channel rather than sleeping.
- Not assume environment state. Use `tempfile::TempDir` for any
  I/O. The CLI tests own a `HOME_LOCK` mutex that serialises
  per-test `OPENMEMORY_HOME` mutations because env vars are
  process-global.

The full integration suite runs in well under 60 seconds. If it
ever exceeds 90 seconds, investigate before adding new tests.

### Property tests

`openmemory-index` has proptest cases for vector and FTS5
round-trips. The proptest seeds are committed; failures reproduce
deterministically.

### Fuzz targets

No fuzz targets currently ship. The schema migration runner and the
MCP request decoder are the natural targets when a fuzz harness lands.

## Performance gates

`cargo bench -p openmemory-index` runs the criterion benches in
`benches/vector_search.rs` and `benches/hybrid_search.rs`.
`cargo bench -p openmemory-bench` runs the workspace-level
benchmarks, including `daemon_admin_api` for desktop-facing health,
entity-list, search, and backup-preflight paths. The canonical
reference hardware is Apple M-series with 8 GB RAM. We do **not**
gate CI on absolute numbers; we do compare regressions of 50% or
more against the previous release.

## Production-hardening pass

The following items were verified ahead of v0.1.0 and are part of
the standing definition-of-done:

- **No zero-vector silent fallback.** When the embedder fails to
  produce a vector, the write fails loudly. No insertion of a
  zero-vector that would corrupt cosine ranking.
- **Metadata sync is transactional.** `index_text`,
  `forget_entity`, and `delete` update the metadata table in the
  same transaction as the search-index write. No "ghost" rows
  after a crash.
- **Schema-version check on open.** Covered by `Migrator::current
  <= binary's SCHEMA_VERSION` check.
- **Database lock contention bounded.** WAL mode plus 5000 ms
  busy timeout. We never hold a write lock across a network call.
- **MCP request size bounded.** 1 MB cap on incoming JSON-RPC
  requests; oversized requests get `INVALID_PARAMS` with a clear
  message.
- **Stable embeddings model file resolution.** The model path is
  resolved once at startup; never re-resolved per request.
- **No panic on the request path.** Every `unwrap()` / `expect()`
  in `tools/*.rs` was removed or justified with a test that
  verifies the invariant. The release-hardening pass (see
  `[Unreleased]` in `CHANGELOG.md`) replaced a stray
  `Response::builder().unwrap()` in `http::handle_mcp` for the
  same reason.
- **No data exfiltration in default logs.** Observation content
  is never logged at INFO; only at DEBUG. Logging defaults hide
  values, show counts.
- **No surprise outbound network calls.** The only outbound HTTP
  in the default build is the explicit `openmemory model download`
  command, gated by `embeddings`. MCP startup and tool calls never
  download models.

## Security review checklist

- **No `unsafe` in workspace code.** `unsafe_code = "warn"` makes
  any new use surface in CI; expect zero in the current release.
- **`cargo deny check` clean.** License allowlist is MIT,
  Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0,
  CC0-1.0, BSL-1.0, and CDLA-Permissive-2.0. Reviewed duplicate
  transitive crates are documented in `deny.toml`; new duplicates
  still warn.
- **Dependency footprint reviewed.** Every workspace dependency
  has a one-line comment in the workspace `Cargo.toml`.
- **Native dependencies are explicit and reviewed.** The default CLI
  enables `usearch` for adaptive large-corpus HNSW search; reduced builds
  can disable it with `--no-default-features`. `ort` loads ONNX Runtime as
  a dynamic library rather than building it from source.
- **Secrets handling.** The only secret-bearing env var read by the
  runtime is `OPENMEMORY_HTTP_TOKEN` for MCP HTTP auth; daemon admin
  tokens live in owner-only runtime files. Bearer-token comparisons
  are constant-time over the byte payload, and token `Debug` impls
  never log the secret.
- **Model integrity verification.** `OnnxEmbedder::load_for_model`
  verifies SHA-256 against the registered hash before handing the
  file to ONNX Runtime. Mismatches surface as
  `EmbedError::ChecksumMismatch` and refuse to start.

## Hosted-test walkthrough (HTTP transport against claude.ai)

When you need to validate the Streamable HTTP transport against a
real MCP client without installing anything on your laptop, the
`.devcontainer/devcontainer.json` plus a free GitHub Codespace
plus a claude.ai custom connector is the cheapest path. The full
step-by-step is in [`CONTRIBUTING.md`](../CONTRIBUTING.md). Quick
recap:

1. Open a Codespace from the GitHub repo page.
2. `cargo build --release --features mcp-http -p openmemory-cli`.
3. `export OPENMEMORY_HTTP_TOKEN=$(openssl rand -hex 32)` and
   `./target/release/openmemory mcp --http 0.0.0.0:7800`.
4. Make port 7800 public in the VS Code Ports panel.
5. Register `https://<codespace>-7800.app.github.dev/mcp` as a
   custom MCP server in claude.ai with the bearer header.
6. From a claude.ai conversation, call `openmemory_remember` and
   `openmemory_recall` end-to-end.

Codespaces public ports are reachable from anywhere with the URL,
so do **not** skip the `OPENMEMORY_HTTP_TOKEN` step.

## Versioning

- All workspace members share `version` via
  `workspace.package.version`.
- Pre-1.0: minor-bump (`0.1 → 0.2`) for any breaking change to the
  MCP tool surface, the SQLite schema, or the public Rust API.
- Patch-bump (`0.2.0 → 0.2.1`) for bug fixes and additive
  non-breaking changes.

## Release process

The release flow is:

1. Update `CHANGELOG.md`'s `[Unreleased]` section to be the new
   version's section. Date it. Add a fresh `[Unreleased]` block
   above.
2. `cargo release minor --workspace --execute` (or `patch` /
   `major`). This bumps every crate's version in lockstep, creates
   the release commit and tag, and pushes the tag.
3. The `release.yml` workflow fires on the pushed tag, builds the
   three-platform tarballs, and creates a GitHub Release with the
   tarballs and SHA-256 checksums attached.
4. Publishing to crates.io is **not** automated. After the GitHub
   Release is sanity-checked, a maintainer runs `cargo publish` per
   crate in dependency order (core, index, embed, graph, watch,
   mcp, cli).

## Reporting issues

For correctness or feature reports, open a GitHub issue with the
exact command and feature flags, expected vs. actual behaviour,
and the output of `openmemory status --json`. For
security-sensitive reports (auth bypass, integrity-check escape,
anything that smells like a vulnerability), email the maintainer
directly rather than filing a public issue. See
[`CONTRIBUTING.md`](../CONTRIBUTING.md) for full reporting
guidance.
