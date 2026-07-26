# Admin API

## Purpose

The admin API is a local control API for the desktop app and CLI.
It is not a public cloud-style REST API and should not be treated as
an integration surface for third parties until the product has shipped
and the contract has proven stable.

MCP remains the agent-facing protocol. The admin API handles health,
setup, browsing, search, edit/delete, capture review, backup, and local
product operations.

## Transport And Auth

- Bind to loopback only.
- Reject non-loopback listeners by construction.
- Require a per-install bearer token, even on loopback.
- Store the token with `0600` permissions or in the OS keychain.
- Support token rotation from CLI and UI.
- Never log tokens.

## Contract Rules

- Request and response bodies are typed in `openmemory-admin`.
- Error responses use stable machine-readable codes.
- Pagination is mandatory for collection endpoints.
- Long-running work returns a job ID and emits events.
- Destructive operations are idempotent where practical.
- The API never exposes raw SQLite paths as an editing mechanism.

## Error Shape

Use one consistent envelope:

```json
{
  "error": {
    "code": "model_missing",
    "message": "The embedding model is not installed.",
    "hint": "Download the default model or continue in keyword-only mode.",
    "retryable": false,
    "details": {
      "model": "nomic-embed-text-v1.5"
    }
  }
}
```

The UI can render `message` and `hint`. Tests should assert `code`.

Initial code families:

- `auth_required`
- `auth_invalid`
- `schema_too_new`
- `store_unreadable`
- `model_missing`
- `client_not_found`
- `client_config_unreadable`
- `client_config_stale`
- `integration_verify_failed`
- `backup_preflight_failed`
- `restore_preflight_failed`
- `job_not_found`
- `conflict`

## Endpoint Groups

### Health

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/health` | Daemon, store, model, MCP, watcher, jobs, and integration summary. |
| `GET /admin/events` | Server-sent event stream for logs, jobs, setup progress, and health changes. |
| `GET /admin/logs` | Paginated redacted local logs. |

### Profiles And Workspaces

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/profiles` | List profiles and summary status. |
| `POST /admin/profiles` | Create a profile. |
| `GET /admin/workspaces` | List workspace mappings. |
| `POST /admin/workspaces` | Create or update workspace mapping. |
| `DELETE /admin/workspaces/{id}` | Remove product mapping without deleting memory. |

### Memory Browser

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/entities` | Paginated entity browser. |
| `GET /admin/entities/{id}` | Entity, observations, relations, provenance, and scope. |
| `PATCH /admin/observations/{id}` | Edit content, tier, source, validity, confidence, importance. |
| `DELETE /admin/observations/{id}` | Soft-delete one observation. |
| `DELETE /admin/entities/{id}` | Forget an entity with cascade preview. |
| `POST /admin/consolidate` | Start consolidation job. |

### Search And Recall

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/search` | Hybrid search over memory and indexed text. |
| `POST /admin/recall/explain` | Recall with score breakdown and provenance. |
| `POST /admin/index` | Index free text under a URI. |
| `DELETE /admin/index` | Delete indexed text by URI. |

Search filters:

- Query.
- Profile.
- Workspace.
- Entity type.
- Memory tier.
- Source kind.
- URI prefix.
- Time range.
- Minimum score.

### Integrations

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/integrations` | Detected clients and install status. |
| `GET /admin/integrations/{client}` | Client-specific config details and verification history. |
| `POST /admin/integrations/{client}/preview` | Return proposed config changes without writing. |
| `POST /admin/integrations/{client}/install` | Idempotently install or repair config. |
| `POST /admin/integrations/{client}/verify` | Spawn or simulate verification. |
| `POST /admin/integrations/{client}/remove` | Remove openmemory config entry where safe. |

Supported initial clients:

- Codex.
- Claude Code.
- Claude Desktop.
- OpenClaw.

### Capture Inbox

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/inbox` | Candidate memories awaiting review. |
| `GET /admin/inbox/{id}` | Candidate with source span and explanation. |
| `POST /admin/inbox/{id}/approve` | Persist candidate as memory. |
| `POST /admin/inbox/{id}/reject` | Reject candidate with reason. |
| `PATCH /admin/inbox/{id}` | Edit proposed text, entity, tier, confidence, or source metadata. |
| `POST /admin/inbox/bulk` | Bulk approve/reject/edit supported fields. |
| `POST /admin/suppressions` | Add local suppression rule. |

### Backup And Restore

| Endpoint | Purpose |
|----------|---------|
| `POST /admin/backup/preflight` | Validate store, destination, size, encryption, and permissions. |
| `POST /admin/backup/create` | Start backup job. |
| `POST /admin/restore/preflight` | Validate backup artifact and restore plan. |
| `POST /admin/restore` | Start restore job after explicit confirmation. |

## Events

Events are server-sent events with stable `type` values:

- `health.changed`
- `job.started`
- `job.progress`
- `job.finished`
- `job.failed`
- `integration.changed`
- `capture.candidate_created`
- `backup.progress`
- `restore.progress`
- `log`

Event payloads must be redacted before serialization. The frontend
should not need to know which fields are sensitive.

## Testing Requirements

- Fixture tests for every error code.
- Contract tests for endpoint request/response JSON.
- Loopback/auth tests.
- Pagination tests with stable ordering.
- Job/event tests with reconnect behavior.
- Integration preview tests that assert no file writes.

