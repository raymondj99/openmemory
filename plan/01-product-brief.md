# Product Brief

## Positioning

OpenMemory should not lead as another hosted memory API. The strongest
positioning is narrower and more defensible:

> Private memory for your AI tools. Local by default. Inspectable,
> correctable, and portable.

The first wedge is AI coding memory. Developers already use multiple
agents and MCP clients, and those agents repeatedly forget repository
conventions, release steps, local corrections, and personal preferences.
The current engine already provides the hard local substrate: MCP tools,
profiles, hybrid search, graph recall, filesystem indexing, local ONNX
embeddings, and setup flows.

The desktop product should make that substrate understandable,
repairable, and trustworthy for users who do not want to operate it
from a terminal every day.

## Target Users

### Primary: AI Coding Power Users

Users who run Codex, Claude Code, Claude Desktop, OpenClaw, Cursor,
Windsurf, or similar tools every day. They want one memory that follows
them across tools without sending private project context to a memory
SaaS.

Jobs to be done:

- Remember project conventions and prior decisions.
- Avoid repeating corrected mistakes.
- Keep one memory across several AI clients.
- Inspect and delete sensitive or stale memories.
- Search past project context without opening old transcripts.
- Repair broken MCP setup without reading client-specific config docs.

### Secondary: Small Engineering Teams

Teams that want shared project memory but cannot adopt opaque hosted
memory infrastructure for private code.

Jobs to be done:

- Share stable project decisions across team members.
- Separate personal memory from repo/team memory.
- Audit what agents remember about a project.
- Back up and restore memory safely.

Team features are post-V1. The single-user product must work first.

### Later: Privacy-Sensitive Professionals

Lawyers, researchers, consultants, and other knowledge workers can use
the same local-first memory pattern, but vertical document-management
products should wait until the coding-agent wedge is proven.

## V1 Product Shape

V1 is a desktop control plane around the existing local engine.

It provides:

- First-run setup for supported MCP clients.
- Local daemon lifecycle management.
- Health, logs, and `doctor` diagnostics.
- Memory browsing, search, edit, delete, and consolidation controls.
- Recall explanations that show why memory was retrieved.
- Review inbox for candidate memories.
- Backup and restore.
- Settings for profiles, models, tokens, privacy, and updates.

It does not provide:

- Hosted memory as the default.
- Mobile apps.
- Enterprise policy controls.
- General e-discovery or document management.
- Silent transcript ingestion.
- A proprietary replacement for MCP.

## Open-Core Boundary

The engine remains open source and useful without the desktop app.

Free surfaces:

- CLI.
- MCP server.
- Local graph/index/embedding engine.
- Setup and integration commands.
- Basic desktop dashboard.
- Memory explorer.
- Manual edit/delete.
- `doctor`.

Paid Pro surfaces can add convenience and workflow depth:

- Review inbox.
- Advanced recall explanations.
- Automatic transcript capture.
- Encrypted local backups.
- Optional encrypted cloud backup/sync.
- Advanced filters and saved searches.
- Priority updates.

The paid layer must not hold local memory hostage. Users who stop
paying keep their local store, CLI, MCP access, and manual management.

## Success Criteria

Activation:

- User installs the app.
- Daemon starts.
- At least one MCP client connects.
- First memory is saved.
- First successful recall happens from an agent.

Trust:

- User can see where a memory came from.
- User can edit or delete it.
- User can confirm no account or cloud dependency is required.
- User can repair common MCP setup failures from the UI.

Retention:

- User returns weekly to inspect, search, approve, or repair memory.
- Review inbox approval rate stays high enough to prove extraction is
  useful rather than noisy.

