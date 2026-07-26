# Security And Privacy

## Security Posture

OpenMemory Desktop handles private code context, project decisions, and
personal preferences. The product must be conservative by default.

Security goals:

- Local operation without account or cloud dependency.
- No surprise network traffic.
- No secrets in SQLite, logs, crash reports, localStorage, or bundles.
- Clear recovery path before data rewrites.
- Inspectable memory with clear deletion controls.

## Threat Model

In scope:

- Local malicious web content trying to call the admin API.
- Another local process trying to read tokens or memory.
- Broken MCP configs that point clients at stale binaries.
- Accidental upload of memory content through telemetry or crash logs.
- Backup artifacts copied to untrusted storage.
- Schema or restore bugs that lose local data.

Out of scope for V1:

- Full endpoint protection against a compromised user account.
- Enterprise device management.
- Multi-tenant hosted memory infrastructure.

## Network Policy

No network call should happen unless it belongs to a visible feature:

- Model download.
- Update check.
- License activation.
- Optional telemetry.
- Optional crash reports.
- Optional encrypted backup/sync.

Settings must show current state for each network-capable feature.
The app should remain useful with all network features disabled after
local assets are installed.

## Admin API Protection

- Bind to loopback only.
- Require a local bearer token.
- Store token outside SQLite.
- Rotate token from UI and CLI.
- Reject missing/invalid auth with stable error codes.
- Do not enable permissive CORS.
- Do not log request bodies by default.

## Secret Storage

Secrets include:

- Admin token.
- Backup encryption key.
- Cloud sync key.
- License token.
- Crash-report token.

Storage priority:

1. OS keychain.
2. Encrypted local secret store with strict file permissions.
3. Plain config only for non-secret preferences.

No secret belongs in SQLite product tables, frontend localStorage, logs,
or crash reports.

## File Permissions

Validate permissions for:

- Data root.
- Config files.
- Admin token file.
- Backup destination.
- Restore source.
- Client config paths before writes.

Permission failures should surface as repairable diagnostics. The app
should not silently chmod broad paths unless the user approves the fix.

## Backups

Backup requirements:

- Preflight validates source store, destination path, free space,
  permissions, encryption setting, and schema compatibility.
- Restore has dry-run validation and impact summary.
- Restore never overwrites current data without explicit confirmation.
- Backup artifacts can be encrypted locally.
- CI includes backup/restore round-trip fixtures.

## Logs And Crash Reports

Logs:

- Redact memory content by default where possible.
- Redact tokens, paths when privacy setting requires it, and provider
  credentials.
- Keep raw debug logs opt-in and local.

Crash reports:

- Opt-in only.
- No memory content.
- No secrets.
- Show what is collected before enabling.

## Telemetry

Telemetry must be opt-in and product-quality, not exhaust data.

Allowed with opt-in:

- Activation funnel events.
- Feature usage counters.
- Error codes.
- Performance timings.
- App and OS versions.

Disallowed:

- Memory content.
- Transcript content.
- Search queries.
- Raw file paths by default.
- Client config contents.

## Data Deletion

Users need clear controls for:

- Soft-delete observation.
- Forget entity.
- Delete indexed URI.
- Delete candidate.
- Clear suppressions.
- Remove workspace mapping without deleting memory.
- Delete profile/store where supported.

Deletion screens should preview impact and distinguish product metadata
from engine memory.

