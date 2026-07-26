# Quality And Performance

## Quality Bar

Product work inherits the engine's engineering posture: explicit
contracts, deterministic tests, local-first behavior, and boring
failure modes.

Standing checks:

```bash
cargo fmt --all
cargo build --workspace --locked
cargo test --workspace --all-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

Frontend checks belong in the `openmemory-desktop` repository when the
desktop app lands:

```bash
npm run lint
npm run typecheck
npm run test
npm run test:e2e
```

Use the actual package manager chosen by the product scaffold. The
commands above name the expected gates, not a dependency decision.

## Request Path Rules

No panic or `unwrap()` on:

- Daemon requests.
- Admin API handlers.
- MCP request handling.
- Setup/integration writes.
- Capture ingestion.
- Candidate approval.
- Backup.
- Restore.
- Migration.

Errors must become typed diagnostics that the UI can render.

## Test Matrix

Rust:

- Unit tests for product data types.
- Admin API contract tests.
- Loopback/auth tests.
- Integration preview/write tests.
- Backup/restore round-trip tests.
- Capture extraction fixture tests.
- Store migration tests.
- CLI JSON output tests.

Desktop:

- First-run setup.
- Client detection.
- Integration repair.
- Memory search.
- Memory edit/delete.
- Recall explanation.
- Inbox approve/reject/edit.
- Backup create.
- Restore preflight.
- Settings privacy toggles.

These UI tests run in the product repo, but they should pin a released
or path-pinned platform build and exercise the real daemon/admin API.

Fixtures:

- Missing binary.
- Missing model.
- Corrupt store.
- Schema too new.
- Stale client config.
- Unreadable client config.
- Invalid admin token.
- Profile mismatch.
- Large local store.
- No network.

## Performance Budgets

Initial budgets:

| Path | Budget |
|------|--------|
| Warm startup to dashboard health | Under 2 seconds. |
| Normal local-store search render | Under 150 ms after daemon response. |
| Entity browser page fetch | Under 100 ms for normal stores. |
| Idle CPU | Effectively zero. |
| Idle memory | Stable, no unbounded growth from event listeners or polling. |
| Capture ingestion | Queued, does not block recall/search hot paths. |
| Integration verification | Progress visible within 300 ms. |

Budgets should be measured on macOS first, then extended to Windows and
Linux when those platforms enter scope.

## Performance Design

- Paginate from the API boundary.
- Avoid frontend polling when daemon events are available.
- Cache health summaries for startup, then refresh incrementally.
- Keep capture and backup in background jobs.
- Put heavy graph/search work in existing engine paths.
- Avoid loading full entities, observations, or transcripts into the UI
  when a page of rows is enough.
- Add large-store fixtures before adding rich visualizations.

## Observability

Local observability:

- Redacted logs.
- Health endpoint.
- Job status.
- Event stream.
- `doctor --json`.
- Debug bundle export with explicit user action.

Production observability with opt-in telemetry:

- Error code counts.
- Startup timing.
- Search timing.
- Setup funnel events.
- Backup/restore success.
- Daemon crash rate.

Do not send memory content, transcript content, search queries, or raw
file paths.

## Release-Blocking Failures

Block release for:

- Known data-loss bug.
- Restore round-trip failure.
- Auth bypass on admin API.
- Non-opt-in memory/network leakage.
- Broken existing CLI or MCP behavior.
- Panic on common request paths.
- App update that loses config or data.
- Unsigned/not-notarized public macOS beta build.
