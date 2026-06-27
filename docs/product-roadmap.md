# Local Desktop Product Roadmap

This roadmap describes how to turn `openmemory` from a local memory
engine into a local desktop product. It is intentionally separate from
[`roadmap.md`](roadmap.md), which tracks crate and release history.

The product thesis:

> OpenMemory Desktop is the local-first memory control plane for AI
> coding agents. It gives users one inspectable, correctable, portable
> memory across Codex, Claude Code, Claude Desktop, OpenClaw, and other
> MCP clients.

The engine stays open source. The product layer adds lifecycle
management, UX, assisted capture, diagnostics, backup/sync, and paid
distribution.

## Positioning

OpenMemory should not lead as "another hosted memory API." That market
already has managed platforms with SDK-first narratives, hosted
infrastructure, and connector breadth. The strongest positioning for
this codebase is narrower:

> Private memory for your AI tools. Local by default. Inspectable,
> correctable, and portable.

The first wedge is AI coding memory:

- Developers already use several agents and MCP clients.
- Agents forget repo conventions, release steps, corrections, and user
  preferences across sessions.
- The current engine already handles local MCP, profiles, hybrid
  search, graph recall, filesystem watching, and setup flows.
- A desktop product can add trust and usability without replacing the
  Rust core.

## Target Users

### Primary: AI coding power users

Users who run Codex, Claude Code, Claude Desktop, OpenClaw, Cursor, or
similar tools every day. They want memory that follows them across
tools without sending private project context to a memory SaaS.

Jobs to be done:

- Remember project conventions and prior decisions.
- Avoid repeating corrected mistakes.
- Keep one memory across several AI clients.
- Inspect and delete sensitive or stale memories.
- Search past project context without opening old transcripts.

### Secondary: small engineering teams

Teams that want shared project memory but cannot adopt an opaque hosted
memory platform for private code.

Jobs to be done:

- Share stable project decisions across team members.
- Separate personal memory from repo/team memory.
- Audit what agents remember about a project.
- Back up and restore memory safely.

### Later: privacy-sensitive professionals

Lawyers, researchers, consultants, and other knowledge workers can use
the same local-first memory pattern, but vertical document-management
products should wait until the coding-agent wedge is working.

## Product Principles

1. **Local first is the product.** The product must work with no cloud
   account, no hosted dependency, and no network access after install,
   except explicit model downloads or opt-in sync.
2. **The engine remains separable.** CLI, MCP, and Rust crates keep
   working without the desktop app.
3. **Memory must be inspectable.** Every persisted fact needs
   provenance, timestamps, scope, and a clear delete/edit path.
4. **Automatic capture requires review.** Do not silently save every
   inferred fact. Product users need an inbox to approve, reject, edit,
   or downgrade candidates.
5. **Project scope is mandatory.** Global user preferences and
   repo-specific decisions must not pollute each other.
6. **Diagnostics are a feature.** MCP config failures are common enough
   that `doctor`, setup verification, and visible health should be
   first-class.
7. **Cloud is optional value, not the foundation.** Paid cloud features
   should be encrypted backup, sync, license management, and team
   sharing. The local app must stay useful without them.

## Non-Goals For V1

- Hosted memory API as the default product.
- General-purpose e-discovery, legal document review, or full document
  management.
- Mobile app.
- Enterprise RBAC, SSO, or compliance controls.
- Parsing every proprietary document format in the desktop app.
- Silent, unsupervised memory extraction.
- Replacing the MCP tool surface with a proprietary protocol.

## Product Architecture

Recommended desktop stack for the first product:

- **Tauri v2 desktop shell.** It matches the Rust core, uses the
  system WebView instead of bundling Chromium, supports tray/menu
  concepts, and has documented signing/distribution paths. Tauri's
  own docs position it as a framework for small, fast binaries using a
  Rust backend and web frontend.
- **Rust daemon and admin API.** The desktop app should talk to a
  local loopback admin API rather than opening SQLite directly.
- **Existing MCP server remains separate.** MCP is for agents. The
  admin API is for the desktop UI and product operations.
- **Frontend in TypeScript.** Use React, Svelte, or another familiar
  web UI layer; keep business logic in Rust where it touches local
  files, auth tokens, models, and memory stores.

Electron remains a fallback if mature app-store packaging, deep Node
ecosystem needs, or a larger frontend team outweigh bundle size and
Rust integration. Electron's official docs describe the tradeoff
clearly: it embeds Chromium and Node.js to provide a cross-platform
desktop runtime.

Initial process model:

```text
OpenMemory Desktop.app
+-- Tauri shell
|   +-- Dashboard
|   +-- Memory explorer
|   +-- Review inbox
|   +-- Integrations setup
|   +-- Settings
+-- openmemory daemon
|   +-- admin HTTP API on loopback
|   +-- MCP stdio/http lifecycle manager
|   +-- model manager
|   +-- capture pipeline
|   +-- log/event stream
+-- existing engine
    +-- openmemory-core
    +-- openmemory-index
    +-- openmemory-embed
    +-- openmemory-graph
    +-- openmemory-engine
    +-- openmemory-mcp
    +-- openmemory-watch
```

The admin API should be local-only and authenticated even on loopback.
Generate a per-install token, store it with `0600` permissions or the
OS keychain, and rotate it from the settings UI.

## Proposed Workspace Additions

Do not move the engine into the app. Add product crates around it.

```text
crates/
+-- openmemory-daemon/       # long-running lifecycle, admin API, events
+-- openmemory-admin/        # request/response types shared by daemon/UI
+-- openmemory-capture/      # transcript/session adapters and candidate extraction
+-- openmemory-backup/       # export, restore, encrypted backup primitives

apps/
+-- desktop/                 # Tauri app
    +-- src-tauri/
    +-- src/
```

The CLI can gain product-facing commands while keeping scriptability:

```bash
openmemory daemon start
openmemory daemon status --json
openmemory doctor --json
openmemory desktop open
openmemory backup create
openmemory backup restore <file>
```

## Admin API Sketch

The admin API should not be a public cloud-style REST API. It is a
local UI/control API with stable enough shapes for the desktop app and
CLI.

Minimum endpoints:

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/health` | Daemon, store, model, MCP, and watcher status. |
| `GET /admin/events` | Server-sent event stream for logs, jobs, setup progress. |
| `GET /admin/profiles` | List memory profiles and mapped workspaces. |
| `POST /admin/profiles` | Create profile or workspace mapping. |
| `GET /admin/entities` | Paginated entity browser. |
| `GET /admin/entities/{id}` | Entity, observations, relations, provenance. |
| `PATCH /admin/observations/{id}` | Edit content, tier, source, validity, importance. |
| `DELETE /admin/observations/{id}` | Soft-delete observation. |
| `GET /admin/search` | Hybrid search over memory and indexed text. |
| `POST /admin/recall/explain` | Recall with scoring/provenance explanation. |
| `GET /admin/integrations` | Detected clients and install status. |
| `POST /admin/integrations/{client}/install` | Idempotent setup for a client. |
| `POST /admin/integrations/{client}/verify` | Spawn or simulate verification. |
| `GET /admin/inbox` | Candidate memories awaiting review. |
| `POST /admin/inbox/{id}/approve` | Persist candidate as memory. |
| `POST /admin/inbox/{id}/reject` | Reject candidate with reason. |
| `POST /admin/backup/create` | Create local backup artifact. |
| `POST /admin/backup/restore` | Restore from backup with preflight checks. |

## Data Model Extensions

Prefer additive schema changes that improve provenance and product UX.

Likely additions:

- `workspaces`: local path, display name, default profile, git remote
  hash, last seen client.
- `capture_sessions`: client, workspace, transcript URI, start/end,
  capture mode, ingestion status.
- `memory_candidates`: extracted fact, proposed entity, proposed tier,
  confidence, reason, source span, decision state.
- `integration_status`: client, config path, detected version, last
  verification result.
- `product_settings`: desktop preferences, telemetry opt-in state,
  update channel, model download policy.

Avoid storing secrets in SQLite. Tokens, sync keys, and license keys
belong in the OS keychain or an encrypted local secret store.

## Quality Bar

Product work should inherit the same engineering posture as the Rust
engine: explicit contracts, local-first behavior, deterministic tests,
and boring failure modes. A desktop app is not permission to weaken the
core.

| Area | Requirement |
|------|-------------|
| Rust code | Keep the workspace MSRV pinned. New crates inherit workspace lints, run under `cargo fmt`, `cargo test`, `cargo clippy -- -D warnings`, rustdoc warnings, and `cargo deny check`. |
| Public surfaces | MCP tool names and field names stay stable. The admin API gets typed request/response structs and fixture tests before the UI depends on it. |
| Request paths | No panic/`unwrap()` on daemon, admin API, MCP, setup, backup, restore, or capture paths. Errors return typed diagnostics the UI can render. |
| Storage | Forward-only migrations only. Refuse schema-too-new stores. Desktop code never edits SQLite directly; it goes through engine/admin APIs. |
| Privacy | No surprise outbound network calls. Model downloads, telemetry, license checks, sync, and crash reports are explicit and visible in settings. |
| Secrets | No secrets in SQLite, logs, crash reports, localStorage, or frontend bundles. Use OS keychain or an encrypted local secret store. |
| Backups | Any feature that can rewrite or migrate user data needs a preflight check, dry-run validation where possible, and a tested restore path. |
| UI | UI tests cover first run, integration repair, memory edit/delete, inbox approval/rejection, backup, and restore. Text must be actionable, not raw internal errors. |
| Performance | Keep idle CPU effectively zero. Add regression fixtures for startup, search, dashboard load, large entity browsing, and capture ingestion. |
| Distribution | Signed/notarized macOS builds before public beta. Installer/update paths must preserve existing CLI/MCP installs and user data. |

The standing development loop for product crates remains:

```bash
cargo fmt --all
cargo build --workspace --locked
cargo test --workspace --all-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

## Roadmap Phases

### Phase 0: Product Decisions And Discovery

Target duration: 1-2 weeks.

Goal: make the irreversible product choices small and explicit before
writing app code.

Deliverables:

- Name and packaging decision: `OpenMemory Desktop` as the working
  product name.
- Platform decision: macOS first, then Windows, then Linux.
- App shell decision: Tauri v2 unless a prototype exposes a blocker.
- SKU decision: OSS engine plus paid desktop Pro features.
- Support boundary: desktop supports CLI/MCP installation but does not
  replace CLI/MCP.
- 8-12 user interviews with heavy AI coding-agent users.
- Competitor notes covering Mem0, Supermemory, Zep, Letta, and local
  MCP memory tools.
- Pricing hypothesis for Pro and Team.

Exit criteria:

- One-page positioning statement.
- MVP scope frozen.
- A clickable low-fidelity UI map for the first-run flow, memory
  explorer, review inbox, and integrations screen.
- Top 10 risks ranked by likelihood and impact.

### Phase 1: Product Foundations

Target duration: 2-4 weeks.

Goal: make the existing engine controllable by a desktop app without
forking core behavior.

Deliverables:

- `openmemory daemon` process with start, stop, status, and logs.
- Local authenticated admin API on loopback.
- Event stream for long-running jobs.
- `openmemory doctor` with machine-readable output.
- Client detection library factored out of setup commands.
- Integration state model for Codex, Claude Code, Claude Desktop, and
  OpenClaw.
- Profile/workspace mapping.
- Backup preflight and raw export/import wrapper.
- Test fixtures for broken MCP configs, missing model, corrupt store,
  stale token, and profile mismatch.

Acceptance criteria:

- Desktop UI can query health, entities, search, integrations, and
  profile status through the daemon.
- CLI and MCP tests still pass unchanged.
- New daemon/admin crates are covered by unit and integration tests.
- `doctor --json` identifies at least: missing binary, missing model,
  stale client config, unreadable config, unavailable MCP server,
  store schema too new, and invalid HTTP token.
- Admin API is not reachable from non-loopback interfaces.
- Admin API rejects requests without the local token.
- Admin API errors use stable, typed codes rather than free-form
  strings.

### Phase 2: Desktop MVP

Target duration: 4-6 weeks.

Goal: ship a usable local app that makes existing openmemory valuable
without requiring terminal fluency.

Core screens:

- **First-run setup.** Detect installed clients, select integrations,
  choose profile/workspace strategy, download model, verify MCP tools.
- **Dashboard.** Daemon status, active profile, indexed files,
  connected clients, model status, recent memory activity.
- **Memory explorer.** Browse entities, observations, relations,
  tiers, source, confidence, and timestamps.
- **Search.** Hybrid search with filters for profile, entity type,
  source, tier, time, and URI prefix.
- **Recall explanation.** Show why an item was recalled: raw score,
  decay, retrieval boost, correction boost, importance, relation path,
  and source.
- **Edit/delete.** Edit observation text and metadata, promote/demote
  tier, soft-delete, forget entity, and run consolidation.
- **Integrations.** Install, verify, repair, or remove MCP client
  configs.
- **Settings.** Data root, profiles, model, HTTP token, updates, logs,
  privacy controls.

Tray/menu bar:

- Open dashboard.
- Start/stop daemon.
- Current health.
- Pause captures.
- Quick search.

Acceptance criteria:

- A non-terminal user can install the app, connect one MCP client,
  download a model, save a memory, recall it from an agent, inspect it
  in the UI, edit it, and delete it.
- Common broken setups produce actionable UI states, not stack traces.
- The app remains useful with no account and no network.
- The app preserves existing CLI-only installs and never rewrites
  client config without previewing the change.
- Default idle CPU usage is effectively zero.
- Startup to dashboard health under two seconds on a warm machine.
- Search results render under 150 ms for a normal local store.

### Phase 3: Assisted Capture Beta

Target duration: 4-6 weeks.

Goal: turn memory from a manual tool into a reviewable workflow.

Deliverables:

- Capture adapters for transcript/session sources that are locally
  available and stable enough to parse.
- `memory_candidates` table and review inbox.
- Rule-based candidate extraction for explicit "remember" and
  correction language.
- Optional LLM extraction behind explicit configuration.
- Source spans linking a candidate back to the transcript line or file.
- Candidate dedup against existing memories before showing the inbox.
- Bulk approve, reject, edit, and "never ask for this pattern again."
- Capture pause by workspace/client.

Candidate categories:

- User preference.
- Project decision.
- Correction or mistake to avoid.
- Release/deployment procedure.
- Tooling gotcha.
- Architecture decision.
- Security/privacy instruction.

Acceptance criteria:

- No candidate is persisted without user approval unless the user
  explicitly enables auto-approve for a narrow category.
- Every approved candidate records provenance.
- Review inbox can explain why a candidate was proposed.
- Rejected candidates reduce future noise through local suppressions.
- Candidate extraction works offline in the default configuration.
- Capture failures are isolated to the session being processed and do
  not corrupt existing memory.

### Phase 4: Private Beta Hardening

Target duration: 4-6 weeks.

Goal: make the product safe enough for daily use by 25-100 external
users.

Deliverables:

- Signed and notarized macOS build.
- In-app update channel for beta builds.
- Backup and restore UI with dry-run validation.
- Store integrity checks and repair guidance.
- Crash reporting and telemetry behind explicit opt-in.
- Privacy report showing every network-capable feature and current
  setting.
- End-to-end UI tests for first run, integration repair, memory edit,
  inbox approval, backup, and restore.
- Performance regression suite for recall, dashboard load, and large
  entity browsing.
- Beta feedback flow inside the app.

Acceptance criteria:

- 25 beta users complete first-run setup with less than 15 percent
  support intervention.
- No known data-loss bugs.
- Backup restore round-trips a realistic profile in CI.
- App update preserves data and MCP config.
- Privacy-sensitive users can run with all telemetry and cloud
  features disabled.
- Crash reports and logs redact memory contents and secrets by
  default.

### Phase 5: V1 Pro Launch

Target duration: 6-8 weeks after private beta.

Goal: launch a paid local-first product without weakening the OSS core.

Free tier:

- CLI.
- MCP server.
- Local engine.
- Basic desktop dashboard.
- Memory explorer.
- Manual edit/delete.
- Client setup and doctor.

Pro tier:

- Review inbox.
- Advanced recall explanations.
- Automatic transcript capture.
- Encrypted local backups.
- Optional encrypted cloud backup/sync.
- Advanced filters and saved searches.
- Priority updates.

Launch deliverables:

- Public website with a direct product demo.
- Download page with signed installers.
- License activation that does not block offline local use after
  activation.
- Clear open-core boundary.
- Terms, privacy policy, data-processing explanation.
- Support docs for common MCP clients.
- Migration guide from CLI-only installs.

Launch metrics:

- Install to connected-client activation rate.
- Connected-client to first successful recall rate.
- Weekly active local stores.
- Candidate approval rate.
- Search success feedback.
- Pro trial conversion.
- Refund/support reasons.

### Phase 6: Team And Enterprise

Target duration: after individual Pro has retention.

Goal: add collaboration without making local users dependent on a
hosted platform.

Candidate features:

- Shared project memory.
- End-to-end encrypted team sync.
- Admin-managed memory policy.
- Per-repo/team namespaces.
- Audit log.
- SSO for team license management.
- Self-hosted sync relay.
- Import/export review workflow.
- Sensitive-memory classifiers.
- Legal/compliance deployment guide.

Do not start here. Team features need strong single-user memory UX
first.

## Workstream Backlog

### Desktop UX

- First-run wizard.
- Dashboard.
- Memory explorer.
- Search and filters.
- Entity graph view.
- Recall explanation panel.
- Candidate inbox.
- Integration repair flow.
- Backup/restore flow.
- Settings and privacy report.
- Tray/menu bar.

### Engine And Daemon

- Long-running daemon mode.
- Admin API.
- Event/log stream.
- Local auth token.
- Daemon crash recovery.
- Store integrity checks.
- Profile/workspace mapping.
- Job queue for model download, consolidation, backup, restore.
- Stable JSON status objects for UI.

### Capture

- Transcript source registry.
- Candidate extraction.
- Candidate dedup.
- Source span model.
- Review decisions.
- Suppression rules.
- Optional LLM provider config.
- Auto-approve policy with narrow scopes.

### Integrations

- Codex setup and verification.
- Claude Code setup and verification.
- Claude Desktop setup and verification.
- OpenClaw setup and verification.
- Cursor/Windsurf feasibility spike.
- Import existing MCP configs.
- Repair stale local-scoped config.
- Remove/uninstall integration safely.

### Security And Privacy

- Loopback-only admin API.
- Local token rotation.
- OS keychain secret storage.
- Network activity inventory.
- Telemetry opt-in.
- Backup encryption.
- Threat model for desktop app and daemon.
- File permission checks for data root, config, backups, and token.
- Redaction rules for logs and crash reports.

### Packaging And Distribution

- macOS `.dmg`.
- macOS code signing and notarization.
- Auto-update channel.
- Windows installer.
- Linux AppImage/deb/rpm later.
- Homebrew formula for CLI remains separate.
- Release checklist that verifies binary size and installer size.

### Quality And Evaluation

- Preserve existing cargo test/clippy/doc gates.
- Add daemon API contract tests.
- Add UI e2e tests.
- Add backup/restore round-trip tests.
- Add fixture-based `doctor` tests.
- Add large-store performance fixtures.
- Add candidate-extraction precision review set.
- Add app startup and idle resource benchmarks.

### Commercial

- Pricing page.
- License activation.
- Offline grace period.
- Trial management.
- Support docs.
- In-app feedback.
- Privacy policy.
- OSS/commercial feature matrix.

## Suggested Milestone Ordering

| Milestone | Scope | Release Type |
|-----------|-------|--------------|
| M0 | Product decisions, UI map, tech spike | Internal |
| M1 | Daemon, admin API, doctor JSON | OSS pre-release |
| M2 | Desktop reads health/search/entities | Internal alpha |
| M3 | First-run setup and integration repair | Private alpha |
| M4 | Memory explorer edit/delete + model manager | Private alpha |
| M5 | Review inbox with rule-based capture | Private beta |
| M6 | Signed macOS app, backup/restore, telemetry opt-in | Public beta |
| M7 | Pro licensing, updates, encrypted backup | V1 launch |
| M8 | Team sync feasibility and prototype | Post-V1 |

## Definitions Of Done

### Desktop MVP done

- Installs on macOS without terminal setup.
- Starts daemon reliably.
- Detects supported clients.
- Installs and verifies at least Codex and Claude Code integrations.
- Downloads or detects local embedding model.
- Shows memory health and profile status.
- Searches memory.
- Browses entities and observations.
- Edits and deletes observations.
- Runs `doctor` from UI and shows fixes.
- Leaves CLI/MCP behavior unchanged.

### Private beta done

- Signed/notarized build.
- Backup and restore verified in CI and UI.
- Review inbox available.
- No silent capture by default.
- Crash reports and telemetry opt-in only.
- Update path tested.
- Top 10 support issues documented.

### V1 done

- Public installer.
- Stable update channel.
- Clear OSS vs Pro boundary.
- Paid license path.
- Migration path from CLI.
- Support docs for each supported MCP client.
- Measured activation and retention dashboard.
- No unresolved P0/P1 data-loss, privacy, or setup bugs.

## Risks And Mitigations

| Risk | Why it matters | Mitigation |
|------|----------------|------------|
| Memory pollution | Bad memories make agents worse. | Review inbox, provenance, easy delete, conservative extraction. |
| Project/global leakage | Repo-specific facts can contaminate personal memory. | Workspace mapping and clear profile boundaries. |
| MCP setup fragility | Broken config kills activation. | `doctor`, verification, repair flows, known precedence checks. |
| Trust gap | Local memory users are privacy-sensitive. | No account required, network report, opt-in telemetry, open engine. |
| Desktop scope creep | App can sprawl into generic notes/search. | Keep v1 centered on agent memory control. |
| Hidden data loss | Local stores are user-owned and hard to recover. | Backups, dry-run restore, schema guards, integrity checks. |
| Overbuilding team features | Enterprise features distract before retention. | Team roadmap starts only after Pro usage is proven. |
| Framework mismatch | Wrong shell can slow app work. | Tauri spike in Phase 0 with Electron fallback criteria. |

## Product Metrics

Activation:

- Install completed.
- Daemon started.
- At least one client connected.
- First memory saved.
- First successful recall from an agent.

Engagement:

- Weekly active stores.
- Weekly memory searches.
- Memory edits/deletes.
- Review inbox approvals.
- Integration repairs completed.

Quality:

- Recall useful/not useful feedback.
- Candidate approval rate by category.
- Candidate rejection reasons.
- Duplicate memory rate after consolidation.
- Average recall latency.

Reliability:

- Daemon crash rate.
- App startup time.
- Backup/restore success rate.
- Failed integration setup rate.
- Support tickets per activated user.

Business:

- Trial start rate.
- Trial to paid conversion.
- Refund rate.
- Pro feature usage.
- Team waitlist conversion.

## Go-To-Market Sequence

1. Build in public around local-first AI memory.
2. Publish technical demos aimed at AI coding-agent users.
3. Launch CLI/desktop alpha to existing `openmemory` users.
4. Recruit private beta users from Claude Code, Codex, and MCP
   communities.
5. Ship comparison pages against hosted memory platforms without
   attacking them: hosted API vs local memory control plane.
6. Launch Pro only after review inbox and backup/restore are strong.
7. Explore team memory after individual retention is proven.

## Open Decisions

- Whether to ship the daemon as part of the existing `openmemory`
  binary or as a separate sidecar binary.
- Whether profile/workspace mapping should be implemented as a product
  table first or as a first-class engine concept.
- Whether auto-capture should initially parse client transcripts or
  rely only on MCP tool calls and explicit user actions.
- Whether encrypted sync should be built in-house or through a hosted
  relay protocol with dumb blob storage.
- Whether the desktop UI should expose graph visualization in v1 or
  defer it until after search/edit/inbox flows are excellent.

## References

- Tauri docs: <https://v2.tauri.app/start/>
- Electron docs: <https://www.electronjs.org/docs/latest/>
- Apple notarization docs: <https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution>
