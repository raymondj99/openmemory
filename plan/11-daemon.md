# OpenMemory Daemon

## Purpose

The daemon is the local platform process behind OpenMemory Desktop. It
turns the existing engine, MCP setup logic, diagnostics, and future
product jobs into one controllable local service without making the
desktop app own storage internals.

MCP remains the agent-facing protocol. The daemon exposes a local admin
API for the desktop app and CLI.

## Non-Goals

- Public hosted API.
- Replacement for MCP.
- Required dependency for existing CLI or stdio MCP usage.
- Direct desktop access to SQLite.
- Silent transcript capture.
- Background network service.

## Process Model

Run one daemon per local user. It can manage multiple profiles under the
same OpenMemory home.

Initial lifecycle:

- The desktop app starts `openmemory daemon start --foreground`.
- The CLI can also start it for development and diagnostics.
- The daemon binds loopback only.
- The daemon writes runtime metadata under `<home>/run/`.
- The desktop app supervises the foreground child in the first alpha.

Later lifecycle:

- Add `openmemory daemon start` detached mode.
- Add launchd/system service integration only after the API is stable.
- Add graceful update/restart once signed desktop packaging exists.

## Runtime Files

Recommended files:

```text
<home>/
+-- run/
|   +-- daemon.json          # pid, port, started_at, version, home, profile
|   +-- admin-token          # bearer token, 0600, never logged
+-- product/
    +-- product.sqlite       # daemon/product metadata, not engine memory
```

`daemon.json` is not a trust boundary. It is a discovery file. The
admin token is the trust boundary and must be stored separately.

## Transport And Auth

Initial transport:

- TCP loopback.
- Default bind address `127.0.0.1:0`.
- Bearer token on every admin endpoint.
- No permissive CORS.
- No request-body logging.

Loopback must be enforced in code before binding. Binding `0.0.0.0`,
`::`, or any non-loopback address is a hard startup error.

Token v1:

- Generate once per OpenMemory home.
- Store in `<home>/run/admin-token` with owner-only permissions where
  the platform supports it.
- Rotate through CLI and UI once the settings surface exists.
- Never expose the token through health, logs, events, or panic output.

Token v2:

- Move to OS keychain where available.
- Keep file fallback for headless/dev installs.

## Crate Boundaries

Add platform crates in this repository:

```text
crates/openmemory-admin/    # typed admin API contracts
crates/openmemory-daemon/   # local server, auth, jobs, events, diagnostics
```

Rules:

- `openmemory-admin` has serializable DTOs, error codes, and versioned
  API shapes only.
- `openmemory-daemon` owns axum routes, local auth, runtime files, job
  orchestration, event fan-out, and profile handles.
- `openmemory-cli` can call daemon library code for foreground start and
  can call the admin API for status later.
- Product desktop code depends on released contracts and binaries, not
  storage internals.

## Admin API V1 Slice

Build endpoints in this order:

| Endpoint | First Purpose |
|----------|---------------|
| `GET /admin/health` | Prove daemon, auth, loopback, and typed JSON. |
| `GET /admin/events` | Stream health and job changes. |
| `GET /admin/profiles` | List known profiles and current active profile. |
| `GET /admin/entities` | Paginated memory browser rows. |
| `GET /admin/entities/{id}` | Entity detail with observations and relations. |
| `GET /admin/search` | Hybrid memory/index search for desktop search. |
| `POST /admin/recall/explain` | Recall with score/provenance breakdown. |
| `POST /admin/consolidate` | Start consolidation job. |
| `GET /admin/jobs/{id}` | Read job state. |
| `GET /admin/integrations` | Client detection/status summary. |
| `POST /admin/integrations/{client}/preview` | Return proposed config diff only. |
| `POST /admin/integrations/{client}/install` | Idempotent install/repair. |
| `POST /admin/integrations/{client}/verify` | Verification job. |

Every collection endpoint is paginated from the first implementation.
Every long-running operation returns a job ID and reports progress
through events.

## Health Model

Health should be a compact operational summary, not a full diagnostics
dump.

Initial fields:

- Daemon version.
- API version.
- OpenMemory home.
- Active/default profile.
- Store state.
- Model state.
- MCP state.
- Watcher state.
- Job runner state.
- Integration summary.

Detailed failure analysis belongs in `doctor --json`, with health
linking to the same typed error codes.

## Job Model

Long-running work becomes daemon jobs:

- Model download.
- Integration verification.
- Consolidation.
- File indexing.
- Candidate extraction.
- Backup create.
- Restore.

Job requirements:

- Stable ID.
- Status: queued, running, succeeded, failed, canceled.
- Typed failure code.
- Redacted message and hint.
- Progress fraction where meaningful.
- Event emission on start/progress/finish/failure.
- Enough persisted state for crash recovery before V1 backup/restore.

## Profile And Store Ownership

The daemon should own long-lived handles for active profiles:

- `DomainStore` for read/search paths.
- `ContextEngine` when enabled for write-behind memory writes.
- Config snapshot and data directory metadata.
- Health cache.

Direct CLI commands keep working without a running daemon. During the
transition, CLI commands may still open stores directly. Once daemon
contracts are stable, desktop-driven flows should go through the admin
API.

## MCP Relationship

M1 should preserve existing stdio MCP integrations.

The daemon should:

- Detect MCP client configs.
- Preview config changes.
- Install or repair entries.
- Verify that `openmemory mcp` starts and exposes tools.
- Report stale binary paths and broken configs.

The daemon should not initially proxy all MCP traffic. Later it can
optionally supervise an HTTP MCP listener for clients that support
streamable HTTP.

## Product Metadata

Keep product metadata separate from engine memory.

Likely daemon-owned tables:

| Table | Purpose |
|-------|---------|
| `daemon_jobs` | Job state, progress, failures, timestamps. |
| `integration_status` | Detected clients, config paths, last verification. |
| `workspaces` | Local path, display name, profile mapping. |
| `product_settings` | Local desktop/daemon preferences. |

Do not store secrets in product SQLite. Use keychain or strict-permission
files.

## First Milestone

M1 daemon is deliberately small:

- `openmemory-admin` crate with error and health DTOs.
- `openmemory-daemon` crate with loopback-only axum server.
- Bearer-token auth for `/admin/health`.
- `openmemory daemon start --foreground`.
- Contract tests for health JSON and auth failures.
- Loopback rejection test.

Exit criteria:

- `cargo test --workspace --all-features` passes.
- Missing/invalid token returns stable `auth_required` or
  `auth_invalid` error code.
- Non-loopback bind address is rejected before listening.
- Existing CLI and MCP tests pass unchanged.

## Follow-On Milestones

M2:

- Runtime discovery file.
- Token generation and rotation.
- `openmemory daemon status --json`.
- Basic redacted log ring.
- Health backed by actual profile/store/model checks.

M3:

- Profile registry.
- Paginated entities endpoint.
- Search endpoint.
- Consolidation job and event stream.

M4:

- Integration detection extracted from setup commands.
- Preview/install/verify endpoints for Codex and Claude Code.
- `doctor --json` shares daemon diagnostics types.

M5:

- Backup preflight.
- Backup create job.
- Restore preflight shape.

Capture/review belongs after the daemon has reliable jobs, events, and
product metadata storage.
