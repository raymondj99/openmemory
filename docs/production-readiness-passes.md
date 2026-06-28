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
