# Quality gates

Production-ready means consistent green CI across a defined matrix,
explicit MSRV, hardened dependencies, public-API rustdoc with no
warnings, and a release process that does not depend on a human's
muscle memory. This file defines those gates so a porter can tell at
a glance whether a commit is "good enough" before pushing.

## CI matrix

Run on every push and PR. Hard requirement: green on **all** rows
before merging to `main`.

| Job | OS | Toolchain | Features | Command |
|-----|-----|-----------|----------|---------|
| `build-default` | ubuntu-latest | stable | default | `cargo build --workspace` |
| `build-default` | macos-latest | stable | default | `cargo build --workspace` |
| `test-default` | ubuntu-latest | stable | default | `cargo test --workspace` |
| `test-default` | macos-latest | stable | default | `cargo test --workspace` |
| `test-all` | ubuntu-latest | stable | `--all-features` | `cargo test --workspace --all-features` |
| `test-no-default` | ubuntu-latest | stable | `--no-default-features` | `cargo test --workspace --no-default-features` |
| `test-msrv` | ubuntu-latest | 1.82.0 | default | `cargo test --workspace` |
| `fmt` | ubuntu-latest | stable | — | `cargo fmt --all -- --check` |
| `clippy` | ubuntu-latest | stable | `--all-features` | `cargo clippy --workspace --all-features --all-targets -- -D warnings` |
| `doc` | ubuntu-latest | stable | `--all-features` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` |
| `audit` | ubuntu-latest | stable | — | `cargo deny check` (advisories, bans, sources, licenses) |

Total: 11 CI jobs. Run-time budget: <8 min wall-clock with caching
(sccache + actions/cache on `~/.cargo` and `target/`).

## MSRV

**Pinned to Rust 1.82.0.**

`Duration::from_mins`/`from_hours` (stabilized 1.82) and `OnceLock`
unblock cleaner code in several places without forcing a churn on
older toolchains. We do not chase nightly features.

`rust-toolchain.toml` pins the channel; the `test-msrv` CI row catches
inadvertent MSRV bumps. Bumping MSRV requires a CHANGELOG note and a
minor version bump.

## Lints

`Cargo.toml` workspace `[lints]` section:

```toml
[workspace.lints.rust]
unsafe_code = "warn"
unknown_lints = "allow"          # MSRV-friendly forward-compat
missing_docs = "warn"            # public items must be documented

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
# Allowed exceptions (must be justified per-allow):
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
wildcard_imports = "allow"
cast_possible_truncation = "allow"
cast_precision_loss = "allow"
cast_sign_loss = "allow"
too_many_lines = "allow"
similar_names = "allow"
needless_pass_by_value = "allow"
trivially_copy_pass_by_ref = "allow"
doc_markdown = "allow"
unreadable_literal = "allow"
return_self_not_must_use = "allow"
if_not_else = "allow"
uninlined_format_args = "allow"
manual_let_else = "allow"
needless_raw_string_hashes = "allow"
disallowed_methods = "warn"      # actual list in clippy.toml
```

`clippy.toml` (workspace root) keeps a `disallowed-methods` list:

- `std::thread::sleep` — outside test code
- `std::env::var` — except in `open-memory-core::config`
- `std::fs::read_to_string` / `write` — outside test code
  (use `core::util::atomic_write`)

These are *warnings* in regular code and *deny-by-default* on CI.

## Test discipline

Every public function in every crate has at least one test. Coverage
targets are not enforced via tooling (codecov bloat) but each crate
ships a unit-test count budget:

| Crate | Min unit tests | Min integration tests |
|---|---|---|
| `open-memory-core` | 20 | 0 |
| `open-memory-index` | 30 | 5 |
| `open-memory-embed` | 10 | 2 |
| `open-memory-graph` | 30 | 10 |
| `open-memory-mcp` | 15 | 5 |
| `open-memory-cli` | 5 | 5 |

Test files live under `<crate>/tests/` (integration) or alongside
sources with `#[cfg(test)] mod tests {}` (unit).

Tests must:

- Run in <30 ms each (any test slower needs a justification comment).
- Be deterministic. No `tokio::time::sleep` waiting on real wall
  clock; use `tokio::time::pause`.
- Not assume environment state. Use `tempfile::TempDir` for any I/O.
- Snapshot tests use `insta` (committed `.snap` files) — they may
  exceed the 30 ms budget.

The full integration suite runs in <60 s. If it ever exceeds 90 s, we
investigate before adding new tests.

## Property tests + fuzzers

`open-memory-index/tests/proptests.rs` carries proptest cases for:

- Vector index round-trip: insert N vectors, query each, recover
  exact id at top-1 (within precision tolerance).
- Fulltext round-trip: force `Bm25Store`, insert a document, search
  for any contained word, document is in results. When host SQLite has
  FTS5, also run the same round-trip through `Fts5Store`.

Fuzzers ship in v0.2 (`cargo fuzz` targets for the schema migration
runner and the MCP request decoder). Not in v0.1.

## Performance gates

`cargo bench -p open-memory-index` runs criterion micro-benchmarks for
the hot paths. We commit the criterion output for the canonical
hardware target (Apple M-series, 8 GB RAM) but **do not gate** on
absolute numbers in CI — only on regression deltas of 50%+ vs the
last release.

## Hardening checklist (post-port, pre-v0.1.0)

Adopted from `~/sift/PRODUCTION-HARDENING.md` and adjusted for the
narrower surface:

- [ ] **No zero-vector silent fallback.** When the embedder fails to
  produce a vector, the write fails loudly — no insertion of a
  zero-vector that would corrupt cosine ranking.
- [ ] **Metadata sync is transactional.** `index_text` /
  `forget_entity` / `delete` updates the metadata table in the same
  transaction as the search-index write. No "ghost" rows after crash.
- [ ] **Schema-version check on open.** Already covered by core's
  `Migrator::current` ≤ binary's `SCHEMA_VERSION` check.
- [ ] **Database lock contention is bounded.** WAL mode + busy timeout
  5000 ms. We never hold a write lock across a network call.
- [ ] **MCP request size is bounded.** 1 MB cap on incoming JSON-RPC
  requests; oversized requests get `INVALID_PARAMS` with a clear
  message.
- [ ] **Stable embeddings model file resolution.** Resolve the model
  path once at startup; do not re-resolve per request (avoids the
  Homebrew-versioned-path bug from sift v0.1.7).
- [ ] **No panic on the request path.** Every `unwrap()` /
  `expect()` in `tools/*.rs` is removed or justified with a
  test that verifies the invariant.
- [ ] **No data exfiltration in default logs.** Observation content
  is **not** logged at INFO level; only at DEBUG. Logging defaults
  hide values, show counts.
- [ ] **No surprise outbound network calls.** The only outbound HTTP
  in v0.1 is the on-demand model download, gated by the `embeddings`
  feature and only triggered by an explicit first-run state.
- [ ] **FTS5 fallback is deterministic.** Startup probes host SQLite
  for FTS5 by creating a temporary virtual table. Probe success uses
  `Fts5Store`; probe failure uses pure-Rust `Bm25Store`; `status`
  reports `fulltext_backend` as `"fts5"` or `"bm25"`.

## Security review checklist

- [ ] **No `unsafe` in workspace code.** `unsafe_code = "warn"` makes
  any new use surface in CI; the porter must add a `#[allow]` with a
  justifying comment if absolutely required (we expect zero in v0.1).
- [ ] **`cargo deny check` clean.** License allowlist is MIT,
  Apache-2.0, MIT-0, BSD-3-Clause, BSD-2-Clause, ISC, Zlib, Unicode-3.0.
- [ ] **`cargo audit` clean.** Run weekly via `audit.yml`.
- [ ] **Dependency footprint reviewed.** Every `[dependencies]` line
  has a one-line comment explaining why we need it (matches sift's
  Cargo.toml comment style).
- [ ] **No C/C++ compilation in the default build.** Default
  `cargo install open-memory` uses host SQLite through `rusqlite`
  without the bundled SQLite feature, keeps `usearch` behind
  `--features hnsw`, keeps SQLite C compilation behind
  `--features bundled-sqlite`, and keeps the embedding stack behind
  `--features embeddings`.
- [ ] **Secrets handling.** No env var named `*_KEY`, `*_TOKEN`,
  `*_SECRET` is read by the binary in v0.1. (Re-evaluated when the
  `llm` feature lands in v0.2.)

## Release process

Release is `cargo release`-driven:

```bash
cargo release minor --workspace --execute
# bumps every crate's version in lockstep (workspace.package.version),
# updates CHANGELOG, creates the release commit + tag,
# pushes the tag, kicks off the release workflow.
```

The release workflow (`.github/workflows/release.yml`):

1. Builds release binaries for `aarch64-apple-darwin`,
   `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`.
2. Generates SHA-256 checksums.
3. Runs `cargo publish --dry-run` on every crate.
4. Creates a GitHub Release with the binaries + checksums attached.
5. **Does NOT** publish to crates.io automatically. Publishing is a
   manual `cargo publish` per crate, in dependency order, after the
   release is sanity-checked.

## Versioning

- All workspace members share `version` via `workspace.package.version`.
- Pre-1.0: minor-bump (`0.1.0 → 0.2.0`) for any breaking change to the
  MCP tool surface, the SQLite schema, or the public Rust API.
- Patch-bump (`0.1.0 → 0.1.1`) for bug fixes and additive non-breaking
  changes.

## OpenClaw integration acceptance tests

These run as part of `tests/e2e_openclaw.rs` and gate v0.1.0. They
exercise the published contract from `02-openclaw-integration.md`
end-to-end so a regression in the integrator surfaces in CI rather
than at install time.

- [ ] **Config path resolution.** With `OPENCLAW_CONFIG_PATH` set,
      the integrator writes only to that path. Without it, only
      `~/.openclaw/openclaw.json` is touched. **Assert:** the
      integrator never creates `~/.openclaw/mcp.json` and never emits
      a top-level `mcpServers` key (those are not part of OpenClaw's
      surface).
- [ ] **Schema conformance.** The emitted entry validates against a
      JSON Schema fixture (`tests/fixtures/openclaw-mcp-server.schema.json`)
      derived from OpenClaw's
      [configuration reference](https://docs.openclaw.ai/gateway/configuration-reference):
      stdio entries carry only `command`, `args`, optional `env`;
      remote entries carry `url`, `transport` ∈ {`streamable-http`,
      `sse`}, optional `headers`. No extra keys.
- [ ] **Delegation parity.** When a stub `openclaw` binary is on
      `PATH`, the integrator invokes it with
      `openclaw mcp set open-memory <json>` and the stub records the
      JSON arg. Round-trip: `openclaw mcp show open-memory` (also
      stubbed) returns byte-equivalent JSON.
- [ ] **JSON5 round-trip.** A pre-existing `openclaw.json` with
      block comments, line comments, and trailing commas survives
      the write — only the `mcp.servers["open-memory"]` slot is
      mutated; siblings, comments, and trailing commas are
      preserved.
- [ ] **Idempotence.** Running `open-memory integrate openclaw`
      twice in a row leaves the file byte-equivalent on the second
      run; stdout reports "no change" and exit code is 0.
- [ ] **Profile isolation.** `open-memory --profile work integrate
      openclaw` writes `mcp.servers["open-memory-work"]` without
      touching `mcp.servers["open-memory"]`.
- [ ] **Failure surfacing.** When the stub `openclaw mcp set`
      returns non-zero with a stderr message, the integrator exits
      non-zero and prints the stub's stderr verbatim — it does not
      silently fall through to the JSON5-edit path.
- [ ] **Verification command.** `open-memory status` reports
      `openclaw: ✔ entry present at mcp.servers.open-memory in
      ~/.openclaw/openclaw.json` after a successful integrate.

## "Definition of done" for v0.1.0

The release ships when **every** item below is checked:

- [ ] All 11 CI jobs green on the release commit.
- [ ] `cargo install --path crates/open-memory-cli` works from a
      fresh clone on macOS-aarch64 and linux-x86_64 without compiling
      C/C++ code, assuming the host SQLite library/headers are present
      for dynamic linking. Lean default build has no `libonnxruntime`
      dep and is ≤8 MB stripped on `x86_64-linux`. `--features
      bundled-sqlite` succeeds where a C compiler is available;
      `--features embeddings` succeeds; `--all-features` succeeds.
- [ ] `open-memory init && open-memory integrate openclaw` is a
      successful no-error invocation against an empty `~/.openclaw/`,
      and **every item in the OpenClaw integration acceptance tests
      above passes**.
- [ ] `open-memory mcp` answers an MCP `initialize` + `tools/list`
      successfully under stdio and (with `--features mcp-http`) under
      Streamable HTTP.
- [ ] All 11 MCP tools have a working integration test in
      `tests/e2e_mcp.rs` that asserts both the input schema (round-
      tripped through `tools/list`) and the output schema (validated
      against the contract in `02-openclaw-integration.md`).
- [ ] CHANGELOG.md `[0.1.0]` section is filled in with a one-paragraph
      "what is this" intro and a bulleted feature list.
- [ ] README.md leads with: a one-paragraph pitch, the OpenClaw
      install snippet (`cargo install open-memory && open-memory
      integrate openclaw`), and a link to `docs/00-overview.md`.
- [ ] Every public symbol in every crate has a doc comment; the
      `doc` CI job is green.
- [ ] Hardening checklist (above) is complete.
- [ ] Security review checklist (above) is complete.
