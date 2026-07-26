# Desktop UX

## UX Direction

The app is an operational control plane. It should feel quiet, direct,
and utilitarian. Avoid marketing-page layouts, decorative dashboards,
and clever visualizations before the core workflows are excellent.

Primary user goals:

- Get one AI client connected.
- Know whether memory is healthy.
- Find, inspect, edit, and delete memory.
- Understand why a memory was recalled.
- Approve useful candidate memories.
- Repair setup without terminal work.
- Back up local data.

## Information Architecture

Primary navigation:

- Dashboard.
- Memory.
- Search.
- Inbox.
- Integrations.
- Backups.
- Settings.

Secondary surfaces:

- First-run setup.
- Recall explanation drawer.
- Entity detail drawer.
- Integration repair modal.
- Job progress tray.
- Menu bar/tray menu.

## First-Run Setup

Goal: connect one client and create a working local memory loop.

Steps:

1. Confirm data directory and profile.
2. Detect supported clients.
3. Preview config changes.
4. Choose model behavior: keyword-only for now or download local model.
5. Verify MCP tools.
6. Save a test memory.
7. Show the dashboard.

Requirements:

- No account step.
- No forced network call.
- Config writes require preview.
- Each failure has a typed diagnosis and a repair action.
- The flow can be rerun from Integrations.

## Dashboard

Purpose: answer "is memory working?"

Show:

- Daemon status.
- Active profile.
- Connected clients.
- Model status.
- Store health.
- Indexed file count.
- Recent memory activity.
- Pending inbox count.
- Running jobs.

Do not show vanity metrics that do not help operation or trust.

## Memory Explorer

Purpose: browse and correct persisted memory.

List view:

- Entity name.
- Entity type.
- Observation count.
- Last observed.
- Profile/workspace.
- Health flags when relevant.

Entity detail:

- Observations.
- Relations.
- Source/provenance.
- Tier.
- Confidence.
- Validity timestamps.
- Access count and retrieval metadata where useful.

Actions:

- Edit observation.
- Promote/demote tier.
- Soft-delete observation.
- Forget entity with cascade preview.
- Run consolidation.
- Open related source when available.

## Search

Purpose: quickly find memory and indexed text.

Filters:

- Profile.
- Workspace.
- Entity type.
- Memory tier.
- Source kind.
- Time range.
- URI prefix.
- Minimum score.

Result rows should show why the result matched: keyword hit, vector hit,
entity match, source, and score summary. Keep advanced score details in
the explanation drawer.

Performance target: render normal local-store results in under 150 ms
after daemon response on a warm machine.

## Recall Explanation

Purpose: make agent behavior inspectable.

Show:

- Final score.
- Raw retrieval score.
- Decay factor.
- Retrieval boost.
- Correction boost.
- Importance.
- Relation path when spreading activation contributed.
- Source and timestamp.
- Scope/profile/workspace.

Use plain language. The user should understand whether the result came
from recent access, explicit importance, graph relation, or keyword and
vector relevance.

## Review Inbox

Purpose: turn capture into a controlled workflow.

Candidate row:

- Proposed memory.
- Proposed entity.
- Category.
- Confidence.
- Source client/workspace.
- Reason proposed.
- Duplicate warning when applicable.

Actions:

- Approve.
- Reject.
- Edit.
- Change entity.
- Change tier.
- Suppress similar candidates.
- Bulk approve/reject.

No candidate is persisted by default without approval.

## Integrations

Purpose: install, verify, and repair MCP client configs.

Supported initial clients:

- Codex.
- Claude Code.
- Claude Desktop.
- OpenClaw.

Each client page shows:

- Detection status.
- Config path.
- Installed command.
- Current binary path.
- Last verification result.
- Proposed repair diff.

Actions:

- Install.
- Verify.
- Repair.
- Remove where safe.
- Open docs for manual setup.

## Backups

Purpose: protect local user-owned memory.

Flows:

- Create local backup.
- Restore from backup.
- Validate backup artifact.
- Show last backup status.

Restore requires preflight, clear impact summary, and explicit
confirmation. CI must cover round-trip restore on realistic fixtures
before the UI ships restore controls.

## Settings

Settings groups:

- Data root.
- Profiles and workspaces.
- Model selection and downloads.
- Local admin token rotation.
- Privacy and telemetry.
- Update channel.
- Logs.
- License.

Every network-capable feature must be visible here with current state.

## Tray/Menu Bar

Minimum actions:

- Open dashboard.
- Start/stop daemon.
- Show current health.
- Pause capture.
- Quick search.

The tray should not run expensive polling. It consumes daemon events and
cached health.

