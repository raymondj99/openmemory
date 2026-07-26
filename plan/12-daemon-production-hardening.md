# Daemon Production Hardening

## Research Anchors

The daemon hardening work follows patterns from primary Rust/runtime
sources and production-grade local systems:

- Tokio's graceful shutdown topic: split shutdown into detection,
  notification, and waiting for work to finish. The daemon now has an
  authenticated shutdown route and server-side graceful shutdown.
  Source: <https://tokio.rs/tokio/topics/shutdown>
- axum's server and SSE APIs: expose server-sent events through typed
  streams and keep-alives, with deterministic event IDs. The daemon
  now persists job events and replays events after `Last-Event-ID`.
  Source: <https://docs.rs/axum/latest/axum/response/sse/>
- SQLite's WAL and backup guidance: keep local metadata durable in
  SQLite, use WAL/busy timeouts, checkpoint profile stores before
  copying, and validate restore artifacts before replacing data.
  Sources: <https://www.sqlite.org/wal.html>,
  <https://www.sqlite.org/backup.html>
- Rust API Guidelines on error handling and interoperability: expose
  typed, machine-readable errors and avoid panics on request paths.
  Source: <https://rust-lang.github.io/api-guidelines/>
- cargo-deny supply-chain checks: keep license, advisory, source, and
  duplicate-crate policy automated in CI and the local production gate.
  Source: <https://embarkstudios.github.io/cargo-deny/>

## Implemented Hardening

- Durable daemon product metadata lives in
  `<home>/product/product.sqlite`.
- The product metadata DB records a schema version and refuses to open
  newer schemas instead of silently downgrading future product data.
- Jobs persist across daemon/router restarts.
- Job events persist and can be replayed by SSE clients after
  `Last-Event-ID`.
- `GET /admin/health` reports durable job-registry health.
- `POST /admin/shutdown` is authenticated and drives graceful server
  shutdown.
- `openmemory daemon stop --json` calls the admin shutdown route and
  removes stale runtime discovery metadata.
- Restore is a real admin job through `POST /admin/restore`; it
  refuses to overwrite an existing profile unless `replace_existing`
  is explicit.
- Restore copies into staging, validates the restored store opens, and
  swaps atomically enough for local filesystem semantics.
- The daemon tests cover auth, runtime metadata, token rotation, logs,
  profiles, entity browse/detail, search, durable jobs/events,
  shutdown, integration preview/install/verify, backup create, restore
  preflight, restore conflict, and restore round-trip.
- `daemon_admin_api` benchmarks are part of `openmemory-bench`, so the
  existing CodSpeed workflow measures desktop-facing admin routes.
- `scripts/daemon_quality_monitor.sh` is the authoritative local
  daemon production gate and is also run in CI.

## Production Gate

Run:

```bash
./scripts/daemon_quality_monitor.sh
```

Optional local benchmark run:

```bash
OPENMEMORY_DAEMON_MONITOR_BENCH=1 ./scripts/daemon_quality_monitor.sh
```

The optional benchmark uses a per-run `daemon-monitor-*` Criterion
baseline to avoid stale developer-machine comparison output; trend
gating is owned by the CodSpeed benchmark workflow.

The monitor enforces:

- Clean whitespace diff.
- No unfinished daemon markers (`not wired yet`, `TODO`, `FIXME`) in
  admin/daemon/daemon-CLI code.
- No `unwrap()`, `expect()`, or `panic!()` on daemon non-test request
  paths.
- Focused daemon/admin all-features tests.
- Daemon no-default-features tests.
- CLI output tests for all-features and no-default-features.
- Full workspace all-features tests.
- Full workspace no-default-features tests.
- Workspace all-target clippy with `-D warnings`.
- Workspace no-default-features all-target clippy with `-D warnings`.
- Workspace all-features rustdoc with `-D warnings`.
- Locked workspace build.
- `cargo deny check`.

## Remaining Scope Boundaries

This branch hardens the daemon/admin platform in `open-memory`.
Product-repo UI flows, Tauri packaging, signed/notarized app builds,
telemetry opt-in UI, and desktop end-to-end tests remain owned by the
future `openmemory-desktop` repository.
