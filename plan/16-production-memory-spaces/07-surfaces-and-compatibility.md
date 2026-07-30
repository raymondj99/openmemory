# Surfaces and Compatibility

## Wire contract

Keep current `v1alpha1` routes and DTOs compatible. New endpoints are additive;
new fields on existing values are optional/defaulted. Preserve existing MCP
names, required arguments, JSON-RPC behavior, HTTP authentication, CLI default
output, and daemon-less personal-global stdio.

Admin DTOs are grouped in `spaces`, `changesets`, `identity`, and `merges` and
re-exported from `openmemory_admin::*`. They use:

- opaque string IDs;
- typed snake_case enums and stable error codes;
- bounded cursor pagination;
- summary/detail separation for content;
- optional space/revision/generation/provenance on legacy responses;
- serde equality/debug goldens and unknown-field policy appropriate to request
  versioning.

Do not rename existing fields opportunistically. A later API version is one
deliberate contract change with simultaneous compatibility support.

## Capability readiness

`GET /admin/capabilities` is the only feature-discovery authority. A capability
is true only when all of its dependencies are ready:

```text
spaces
audit_observations
manual_edit
team_spaces
identity_review
material_merge
```

Readiness combines schema version, legacy binding, backfill cursor, index/mirror
repair, registry/root health, backup compatibility, recovery intents, execution
runtime health, and platform/filesystem gates. A route may exist while its
capability is false, but it returns a stable unavailable/repair response and is
not advertised to UI/MCP.

## Admin routes

Required route families:

```text
GET    /admin/spaces
POST   /admin/spaces
GET    /admin/spaces/{id}
PATCH  /admin/spaces/{id}
POST   /admin/spaces/{id}/close
POST   /admin/spaces/{id}/delete
GET    /admin/projects
POST   /admin/projects
POST   /admin/workspaces/resolve
PUT    /admin/workspaces/{id}/project
GET    /admin/teams
POST   /admin/teams
GET    /admin/teams/{id}/members
PUT    /admin/teams/{id}/members/{principal}
DELETE /admin/teams/{id}/members/{principal}
GET    /admin/context
POST   /admin/context/resolve
POST   /admin/context/revoke

GET    /admin/changesets
POST   /admin/changesets
GET    /admin/changesets/{id}
POST   /admin/changesets/{id}/approve
POST   /admin/changesets/{id}/reject
POST   /admin/changesets/{id}/revert
GET    /admin/memories/{id}/history
GET    /admin/memories/{id}/diff
POST   /admin/memories/{id}/edit
POST   /admin/memories/{id}/retire
POST   /admin/memories/{id}/restore
POST   /admin/memories/{id}/destroy/preview
POST   /admin/memories/{id}/destroy
POST   /admin/spaces/{source}/cherry-pick/{logical_id}

POST   /admin/merges/preview
GET    /admin/merges/{job}
GET    /admin/merges/{job}/candidates
GET    /admin/identity/candidates/{id}
POST   /admin/identity/candidates/{id}/decide
POST   /admin/identity/decisions/{id}/revise
POST   /admin/merges/{job}/resolve
POST   /admin/merges/{job}/confirm
POST   /admin/merges/{job}/cancel
POST   /admin/merges/recover
```

Long work returns `202` and a typed persisted job. GET is idempotent;
mutation/review/confirmation accepts an idempotency key plus expected
state/packet/plan version.
Memory history/diff/mutation routes require an authorized `space_id` in query
or body; logical IDs are not globally unique and are never used to infer scope.

Status mapping:

- `400`: invalid bounded input or cross-domain interactive request;
- `401`: missing/invalid bearer or context capability;
- `403`: authenticated but current role/actor forbids action;
- `404`: absent or inaccessible resource where existence would leak;
- `409`: stale version/generation/receipt/plan, idempotency conflict, active job;
- `422`: unresolved semantic conflict/evidence failure;
- `423`: closed, promotion-locked, or recovery-locked space;
- `429`: execution coordinator/task/byte admission saturated;
- `503`: index repair, corruption recovery, stuck execution, or platform gate.

Every handler authenticates, deserializes a bounded DTO, invokes one service,
and maps its typed result.

## CLI

Add focused nouns:

```text
openmemory space list|create|show|close|delete
openmemory project map
openmemory context show
openmemory changeset list|show
openmemory review approve|reject
openmemory memory history|diff|edit|retire|restore|revert|cherry-pick
openmemory merge preview|candidates|decide|resolve|apply|status|recover
openmemory doctor
```

- Human output names owner/context/provenance; `--json` is an admin DTO.
- Destructive noninteractive commands require `--yes` and the appropriate
  confirmation hash.
- Review/merge/catalog administration requires a daemon. If absent, print one
  precise repair instruction; never open a second writer.
- Existing remember/recall/list/forget syntax and default output remain.
- Daemon-less legacy access holds the same lifetime shared space lock as the
  daemon, so exclusive snapshot/migration/promotion returns `space_busy` rather
  than racing an old open handle.
- Context flags are additive and cannot broaden authorization.
- Parser, completions, process output, JSON, exit codes, confirmations, and
  daemon-absent behavior have tests.

## MCP

Extend existing tools compatibly:

- remember: optional `target=default|personal|team`, idempotency key, reason;
  response distinguishes durable proposal from applied mutation;
- recall: optional contextual/project/global mode; response includes
  space/revision/raw and adjusted score provenance;
- entity/list/status/index tools use the resolved bounded context and never
  scan all catalog roots;
- legacy forget tools retain names/inputs/output and normal-recall removal
  semantics, but execute audited reversible retirement; hard destruction is not
  an MCP operation.

Add only agent-safe tools:

```text
openmemory_context
openmemory_history
openmemory_propose_change
```

Do not expose review decisions, identity decisions, membership, promotion, or
hard destruction. Descriptors/handlers/schemas stay colocated and generated
from one source. Tool annotations and initialize instructions explicitly state
read layers, target, proposal behavior, readiness, and destructive/idempotency
properties without leaking paths or tokens.

HTTP requires bearer plus authenticated context capability when context is
provided. Omission is legacy personal-global; raw requested IDs cannot broaden
it. Stdio holds one fixed resolved context. Keep `handle` as the compatibility
wrapper around internal `handle_with_context`.

## Operations and diagnostics

Status/doctor expose bounded, redacted diagnostics:

- catalog/manifest/root/schema/readiness by authorized space;
- semantic/index/mirror generations and pending outbox counts;
- execution workers active/peak, coordinators, queued tasks, reserved bytes,
  queue/execute latency, saturation, cancellation, timeout, panic, stuck and
  shutdown state;
- backfill/repair cursors and last safe error;
- snapshot/merge intent phase, verified artifact presence, backup retention,
  filesystem/platform capability, and required recovery action.

No endpoint/log/SSE/debug value exposes capability tokens, semantic content,
absolute paths, model secrets, or unbounded errors by default. Job SSE carries
IDs, states, counts, and trace IDs only.

Maintenance requires one explicit space or an explicitly non-atomic
all-authorized-spaces job with independent reports. Domain migration targets
one closed space, uses verified promotion, then updates manifest/catalog.
Backup defaults to full profile/catalog; space-only import cannot import
authority.

## Desktop and documentation

The sibling Desktop app is out of repository scope. Deliver the complete admin
contract for context selection/readiness, provenance, review/diff/history/edit,
identity evidence, merge preview/accounting, jobs, and recovery.

Update in the same slices:

- `README.md`;
- architecture, crates, storage, context-engine, MCP, CLI, configuration,
  development, and roadmap docs;
- admin API docs, rustdoc atomicity/identity limitations;
- changelog and backup/recovery operator runbook.

Documentation examples are contract-tested where practical.

## Compatibility acceptance

- Existing MCP corpus lists/calls all old tools with unchanged names and
  required fields.
- Existing CLI process goldens remain unless an explicitly additive line is
  reviewed.
- An old profile opens personal-global before project mapping and after binding.
- Omitted context never means all spaces.
- Direct no-daemon stdio remains personal-global.
- Daemon and proxy resolve identical context/provenance ordering.
- Backup/restore/status/watch/ingest/domain migration retain legacy behavior
  when only personal-global exists.
- Default, all-feature, and no-default builds pass; embeddings stay optional.
- No capability advertises before its production readiness and compatibility
  suites pass.
