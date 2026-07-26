# System Architecture

## Direction

Use a small desktop shell around the existing Rust engine. The product
should add lifecycle, UI, diagnostics, capture, and backup without
moving engine ownership into the app.

Recommended first stack:

- Tauri v2 desktop shell.
- Rust daemon with local authenticated admin API.
- TypeScript frontend.
- Existing MCP server as the agent surface.
- Existing CLI as the scriptable user/admin surface.

Electron remains a fallback only if a Phase 0 prototype in
`openmemory-desktop` exposes a real Tauri blocker around packaging,
app-store distribution, WebView limits, or frontend ecosystem needs.

## Process Model

```text
OpenMemory Desktop.app
+-- Tauri shell
|   +-- first-run setup
|   +-- dashboard
|   +-- memory explorer
|   +-- search
|   +-- review inbox
|   +-- integrations
|   +-- settings
+-- openmemory daemon
|   +-- local admin API on loopback
|   +-- event stream
|   +-- MCP lifecycle manager
|   +-- model manager
|   +-- capture pipeline
|   +-- backup/restore jobs
+-- existing engine crates
    +-- openmemory-core
    +-- openmemory-index
    +-- openmemory-embed
    +-- openmemory-graph
    +-- openmemory-engine
    +-- openmemory-mcp
    +-- openmemory-watch
```

The desktop app talks to the daemon. Agents keep talking to MCP.
The daemon calls existing engine APIs and CLI-compatible setup logic.

## Proposed Platform Additions

Add platform crates around the engine in this repository:

```text
crates/
+-- openmemory-admin/        # shared admin API request/response types
+-- openmemory-daemon/       # lifecycle, local API, events, jobs
+-- openmemory-capture/      # source adapters and candidate extraction
+-- openmemory-backup/       # export, restore, encryption primitives
```

Rules:

- `openmemory-admin` contains serializable types and error codes only.
- `openmemory-daemon` owns local HTTP, job orchestration, and eventing.
- `openmemory-capture` does not write approved memories directly; it
  proposes candidates.
- `openmemory-backup` is usable from both daemon and CLI.
- Desktop frontend code never imports storage or engine internals.

The sibling product repository owns the desktop app:

```text
openmemory-desktop/
+-- apps/desktop/            # Tauri app
|   +-- src-tauri/
|   +-- src/
+-- plan/                    # product-owned implementation specs
+-- docs/                    # support, release, and packaging docs
```

## CLI Additions

Keep CLI commands scriptable and JSON-friendly:

```bash
openmemory daemon start
openmemory daemon stop
openmemory daemon status --json
openmemory doctor --json
openmemory desktop open
openmemory backup create
openmemory backup restore <file>
```

`openmemory setup` should remain the friendly orchestration command.
Specific `integrate` commands remain available for partial installs.

## Data Ownership

The existing engine owns graph, index, embedding cache, and profile
storage. Product tables should be additive and focused on UX state.

Likely product tables:

| Table | Purpose |
|-------|---------|
| `workspaces` | Local path, display name, default profile, git remote hash, last seen client. |
| `capture_sessions` | Client, workspace, transcript URI, start/end, mode, ingestion status. |
| `memory_candidates` | Proposed fact, entity, tier, confidence, reason, source span, decision state. |
| `integration_status` | Client, config path, detected version, last verification result. |
| `product_settings` | Desktop preferences, telemetry state, update channel, model download policy. |

Avoid storing secrets in SQLite. Tokens, sync keys, and license keys
belong in the OS keychain or an encrypted local secret store.

## Migration Strategy

- Use forward-only migrations.
- Refuse schema-too-new stores with typed diagnostics.
- Keep product schema versioning separate from engine schema versioning
  unless a table must become a first-class engine concept.
- New tables are additive until multiple releases prove the model.
- Every migration that can affect user data must have backup preflight
  and restore coverage.

## Background Work

Long-running operations should be jobs:

- Model download.
- Initial client verification.
- Filesystem indexing.
- Candidate extraction.
- Consolidation.
- Backup create.
- Backup restore.

Jobs emit progress events and persist enough state for crash recovery.
Recall and search hot paths should not wait behind product jobs.

## Consistency With Existing Codebase

Follow the current repository posture:

- Rust 1.85 MSRV remains pinned.
- Workspace lints apply to new crates.
- JSON output stays stable for scripted CLI use.
- MCP tool names and field names remain stable.
- SQLite remains the local storage layer.
- Config and data directories remain explicit and inspectable.
- No new hosted dependency becomes required for local operation.
- Product-repo features consume platform contracts; they do not create
  hidden forks of engine, MCP, or storage behavior.
