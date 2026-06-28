# Production Readiness Passes

This log records focused production-hardening passes. Each pass follows:
review a narrow domain, research a production Rust reference, apply a scoped
change, verify with tests and realistic load, then commit the result.

## 2026-06-27 - Model Cache Integrity And Download Concurrency

Domain reviewed: `openmemory-embed` model downloads, model-file readiness,
and ONNX load integrity. This path is cold compared with recall/remember, but
it gates semantic search setup and daemon health for agent integrations.

Production reference: Astral `uv` cache design and source. The relevant
principles are that cache mutation should be serialized across processes and
cache entries should be treated as recoverable data, not trusted merely because
files exist.

Applied changes:

- Added a per-model `.download.lock` using `fs4::FileExt` so concurrent
  `openmemory model download` processes serialize mutation of the same model
  cache directory.
- Changed download skip behavior from "non-empty file exists" to "file exists
  and passes the registry SHA-256 when one is recorded." Corrupt or empty files
  are replaced instead of causing every future download command to skip them.
- Removed freshly downloaded files if post-download verification fails, so a
  bad transfer cannot poison the next run.
- Synced the parent directory after the part-file rename on Unix, preserving
  the existing atomic write shape while making the rename more crash-durable
  where directory fsync is supported.
- Added `model.onnx_data` existence and SHA-256 verification to
  `OnnxEmbedder::load_for_model` for external-data ONNX models.
- Bumped the workspace `fs4` line to `1.1`, which supports the repo MSRV and
  avoids the older duplicate `rustix` dependency that `fs4 0.12` introduced.

Tests and load checks:

- Focused unit coverage for cached-file readiness, mismatched hashes, empty
  files, required external data, and external-data load rejection.
- `cargo test -p openmemory-embed --all-features`
- `cargo test -p openmemory-embed --no-default-features`
- `cargo clippy -p openmemory-embed --all-targets --all-features -- -D warnings`
- `cargo clippy -p openmemory-embed --all-targets --no-default-features -- -D warnings`
- `cargo run -p openmemory-cli -- --home <tempdir> model list`
- Release context-engine stress: `1000` agents x `10` ops, `4` readers,
  journal enabled; verified no lost writes, `0` engine errors.
- `cargo bench -p openmemory-bench --bench openmemory -- daemon_admin_api
  --sample-size 10` was rerun after one noisy first sample; the repeat showed
  no statistically significant change across the daemon admin API group.
- `scripts/daemon_quality_monitor.sh`
- `cargo deny check`

Performance note: no new criterion benchmark was added for this pass because
the changed work is intentionally off the hot recall/remember path. Hashing
happens when explicitly downloading or repairing model files; steady-state
daemon startup still verifies model files before ONNX Runtime parsing, as it
already did for `model.onnx` and `tokenizer.json`.

References:

- `uv` cache docs: https://docs.astral.sh/uv/concepts/cache/
- `uv-cache` source: https://github.com/astral-sh/uv/tree/main/crates/uv-cache
- `fs4` crate info: https://docs.rs/fs4/1.1.0/fs4/

## 2026-06-28 - Watcher Event Noise Filtering

Domain reviewed: `openmemory-watch` initial scans and runtime filesystem event
filtering. This path is not the core recall hot path, but it can become a
resource sink in real workspaces where editors, language servers, build tools,
and OS metadata files produce many events around every real note edit.

Production reference: `watchexec`, a production Rust file-watching CLI. Its
CLI filterer rejects disallowed filesystem event kinds before running glob and
program filters, and its default ignore set includes `.DS_Store`, Python
bytecode, editor autosave/lock/swap files, tool logs, and VCS directories. The
principle applied here is to keep the watcher pipeline cheap at the front:
debounce, then reject known-noise path names before any hashing, decoding,
SQLite write, or embedding work.

Applied changes:

- Expanded the watcher's always-ignore glob list beyond lock files to cover
  Python bytecode, Vim/Kate swap files, Emacs autosave/lock files, backup
  suffixes, `.DS_Store`, and `watchexec.*.log`.
- Kept the new patterns narrow: broad temporary suffixes such as `*.tmp` and
  `*.part` were intentionally left out because a user might explicitly choose
  those extensions for a watched workflow.
- Made runtime event filtering support `*` and `?` in the small in-crate glob
  matcher so event handling matches the shipped always-ignore patterns without
  adding another dependency or re-running full ignore-file resolution per event.
- Centralized extension matching for scans and events, using
  `eq_ignore_ascii_case` instead of allocating a lowercased extension string for
  every candidate path. The helper also tolerates custom extension entries with
  a leading dot.
- Added scan and runtime tests proving noisy files are skipped even when a
  permissive custom extension list would otherwise allow them.
- Added a synthetic noisy-event batch test with 600 scratch-file create events
  plus one real note; only the real note reaches the index.

Tests and load checks:

- `cargo fmt --all -- --check`
- `cargo test -p openmemory-watch`
- `cargo clippy -p openmemory-watch --all-targets --all-features -- -D warnings`
- `cargo test -p openmemory-watch watcher -- --nocapture`
  - `watcher_latency_smoke`: create p99 `111.853166ms`, modify p99
    `115.237209ms`, delete p99 `100.807959ms` with an `80ms` debounce.
- `cargo test -p openmemory-watch noisy_editor_batch_is_filtered_before_indexing -- --nocapture`
- Release context-engine stress: `1000` agents x `10` ops, `4` readers,
  journal enabled; verified no lost writes, `0` backpressure waits, `0` engine
  errors.
- `scripts/daemon_quality_monitor.sh`
- `cargo bench -p openmemory-bench --bench openmemory -- daemon_admin_api
  --sample-size 10`
  - The first run reported no change for entity paging and keyword search but a
    noisy backup-preflight regression. A full rerun reported no detected change
    for all three groups. A backup-preflight-only rerun with `--sample-size 20`
    reported an improvement versus the stored baseline.

Performance note: the main win is avoided work under noisy filesystem batches.
Every skipped scratch file avoids file reads, BLAKE3 hashing, UTF-8 validation,
metadata lookups, index writes, and embedding calls. Steady-state recall and
remember performance is unaffected; the daemon admin benchmark rerun detected
no regression across the changed branch.

References:

- `watchexec` filterer source: https://github.com/watchexec/watchexec/blob/main/crates/cli/src/filterer.rs
- `watchexec` repository: https://github.com/watchexec/watchexec
