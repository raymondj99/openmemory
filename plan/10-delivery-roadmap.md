# Delivery Roadmap

## Phase 0: Product Decisions And Discovery

Target duration: 1-2 weeks.

Goal: make irreversible choices small and explicit before writing app
code.

Deliverables:

- Product name decision.
- macOS-first platform decision.
- Tauri v2 spike with Electron fallback criteria.
- Repository split confirmed: platform work in `open-memory`, desktop
  product work in `openmemory-desktop`.
- OSS/free/Pro boundary draft.
- 8-12 interviews with AI coding-agent power users.
- Competitor notes covering hosted memory APIs and local MCP memory
  tools.
- Low-fidelity UI map for first-run, dashboard, memory explorer, inbox,
  integrations, and settings.
- Top 10 risks ranked by likelihood and impact.

Exit criteria:

- Positioning statement is one page.
- MVP scope is frozen.
- Tech spike has a clear shell decision.
- Pricing hypothesis exists but does not drive V1 scope.

## Phase 1: Product Foundations

Target duration: 2-4 weeks.

Goal: make the engine controllable by a desktop app without forking core
behavior.

Deliverables:

- `openmemory daemon`.
- Local authenticated admin API.
- Event stream for jobs and health.
- `openmemory doctor --json`.
- Client detection library factored from setup/integration commands.
- Integration state model.
- Profile/workspace mapping.
- Backup preflight and export/import wrapper.
- Fixture suite for common broken setups.

Acceptance criteria:

- Desktop can query health, entities, search, integrations, and profile
  status through daemon APIs.
- CLI and MCP tests pass unchanged.
- Admin API is loopback-only and token-protected.
- Errors use stable typed codes.
- `doctor --json` identifies missing binary, missing model, stale
  client config, unreadable config, unavailable MCP server, schema too
  new, and invalid token.

## Phase 2: Desktop MVP

Target duration: 4-6 weeks.

Goal: ship a usable local app that makes existing openmemory valuable
without terminal fluency.

Core screens:

- First-run setup.
- Dashboard.
- Memory explorer.
- Search.
- Recall explanation.
- Edit/delete.
- Integrations.
- Backups.
- Settings.
- Tray/menu bar.

Acceptance criteria:

- A non-terminal user can install the app, connect one MCP client,
  download or skip a model, save a memory, recall it from an agent,
  inspect it in the UI, edit it, and delete it.
- Common broken setups produce actionable UI states.
- The app remains useful with no account and no network.
- Existing CLI-only installs are preserved.
- Startup to dashboard health is under 2 seconds on a warm machine.
- Normal local-store search renders under 150 ms after daemon response.

## Phase 3: Assisted Capture Beta

Target duration: 4-6 weeks.

Goal: make memory capture reviewable and useful without polluting the
store.

Deliverables:

- Capture session model.
- Source adapters for stable local sources.
- `memory_candidates` table.
- Rule-based candidate extraction.
- Duplicate and suppression checks.
- Review inbox.
- Bulk approve/reject/edit.
- Capture pause by workspace/client.
- Optional LLM extraction behind explicit configuration.

Acceptance criteria:

- No candidate persists without approval by default.
- Every approved candidate records provenance.
- Rejections reduce future repeated noise.
- Extraction works offline by default.
- Capture failures do not corrupt existing memory.

## Phase 4: Private Beta Hardening

Target duration: 4-6 weeks.

Goal: make the product safe enough for daily use by 25-100 external
users.

Deliverables:

- Signed and notarized macOS build.
- In-app beta update channel.
- Backup and restore UI with dry-run validation.
- Store integrity checks and repair guidance.
- Crash reporting and telemetry behind explicit opt-in.
- Privacy report.
- UI end-to-end tests for core flows.
- Performance regression suite.
- In-app feedback flow.

Acceptance criteria:

- 25 beta users complete first-run setup with less than 15 percent
  support intervention.
- No known data-loss bugs.
- Backup restore round-trips a realistic profile in CI.
- App update preserves data and MCP config.
- Users can disable telemetry and cloud features.
- Logs and crash reports redact memory contents and secrets by default.

## Phase 5: V1 Pro Launch

Target duration: 6-8 weeks after private beta.

Goal: launch a paid local-first product without weakening the OSS core.

Deliverables:

- Public website with direct product demo.
- Signed download page.
- License activation.
- Clear open-core boundary.
- Terms, privacy policy, and data-processing explanation.
- Support docs for common MCP clients.
- Migration guide from CLI-only installs.

Launch metrics:

- Install to connected-client activation.
- Connected-client to first successful recall.
- Weekly active local stores.
- Review inbox approval rate.
- Search usefulness feedback.
- Pro trial conversion.
- Refund/support reasons.

## Phase 6: Team And Enterprise

Start only after individual Pro retention is proven.

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

## Milestones

| Milestone | Scope | Release Type |
|-----------|-------|--------------|
| M0 | Product decisions, UI map, tech spike | Internal |
| M1 | Daemon, admin API, doctor JSON | OSS pre-release |
| M2 | Product repo reads health/search/entities through daemon | Internal alpha |
| M3 | Product repo first-run setup and integration repair | Private alpha |
| M4 | Product repo memory explorer edit/delete + model manager | Private alpha |
| M5 | Product repo review inbox with rule-based capture | Private beta |
| M6 | Signed macOS product app, backup/restore, telemetry opt-in | Public beta |
| M7 | Product repo Pro licensing, updates, encrypted backup | V1 launch |
| M8 | Team sync feasibility and prototype | Post-V1 |

## Definitions Of Done

### Desktop MVP Done

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

### Private Beta Done

- Signed/notarized build.
- Backup and restore verified in CI and UI.
- Review inbox available.
- No silent capture by default.
- Crash reports and telemetry opt-in only.
- Update path tested.
- Top 10 support issues documented.

### V1 Done

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
| Project/global leakage | Repo facts can contaminate personal memory. | Workspace mapping and clear profile boundaries. |
| MCP setup fragility | Broken config kills activation. | `doctor`, verification, repair flows, known precedence checks. |
| Trust gap | Local memory users are privacy-sensitive. | No account required, network report, opt-in telemetry, open engine. |
| Desktop scope creep | App can sprawl into generic notes/search. | Keep V1 centered on agent memory control. |
| Hidden data loss | Local stores are user-owned and hard to recover. | Backups, dry-run restore, schema guards, integrity checks. |
| Overbuilding team features | Enterprise features distract before retention. | Team roadmap starts only after Pro usage is proven. |
| Framework mismatch | Wrong shell can slow app work. | Tauri spike in Phase 0 with Electron fallback criteria. |

## Metrics

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

## Open Decisions

- Whether the daemon ships as part of the existing `openmemory` binary
  or as a separate sidecar.
- Whether workspace/profile mapping starts as product metadata or a
  first-class engine concept.
- Whether capture initially parses transcripts or relies only on MCP
  tool calls and explicit user actions.
- Whether encrypted sync is built in-house or through a dumb hosted blob
  relay.
- Whether graph visualization belongs in V1.
