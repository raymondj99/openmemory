# Surfaces and Compatibility

## API versioning

Keep current `v1alpha1` routes and DTOs backward compatible while the feature
lands. Add optional capability/provenance fields with serde defaults. Once all
new endpoints and semantics stabilize, either:

- retain `v1alpha1` because additions are compatible; or
- introduce `v1alpha2` in one deliberate contract commit with both versions
  served during the release.

Do not opportunistically rename existing fields/tools. Record the final choice
in `docs/admin-api`/crate docs and contract goldens.

## Admin DTO groups

### Spaces/context

```rust
AdminSpaceSummary
AdminSpaceDetail
AdminSpaceOwner
AdminSpaceContext
AdminSpaceRole
AdminSpaceReadiness
AdminCreateSpaceRequest
AdminUpdateSpaceRequest
AdminDeleteSpaceRequest
AdminProjectSummary
AdminWorkspaceMapping
AdminResolveContextRequest
AdminMemoryContext
AdminContextCapabilityResponse
AdminMembership
AdminUpdateMembershipRequest
AdminCapabilitiesResponse
```

### Changesets/history

```rust
AdminChangeSetSummary
AdminChangeSetDetail
AdminChangeSetState
AdminChangeOperation
AdminSubmitChangeSetRequest
AdminChangeDecisionRequest
AdminMemoryDiff
AdminFieldChange
AdminObjectHistory
AdminEditMemoryRequest
AdminLifecycleRequest
AdminRevertRequest
```

### Identity/merge

```rust
AdminIdentityCandidateSummary
AdminIdentityCandidateDetail
AdminIdentityEvidence
AdminIdentityDecisionRequest
AdminIdentityDecisionEvent
AdminMergePreviewRequest
AdminMergePreview
AdminMergeConflict
AdminMergeResolutionRequest
AdminMergeConfirmRequest
AdminMergeJob
AdminMergeAccounting
AdminRecoveryReport
```

All list responses use bounded cursor pagination. Content-bearing detail is
separate from summaries/events. IDs are opaque strings on the wire.

Add `space_id`, safe `space_label`, owner/context, revision ID, and generation
to existing entity/observation/search DTOs as optional/defaulted fields during
compatibility. Legacy single-space responses may populate them after binding;
old clients ignore them.

## Admin routes

### Space, project, context, team

```text
GET    /admin/capabilities
GET    /admin/spaces
POST   /admin/spaces
GET    /admin/spaces/{id}
PATCH  /admin/spaces/{id}
POST   /admin/spaces/{id}/close
POST   /admin/spaces/{id}/delete

GET    /admin/projects
POST   /admin/projects
POST   /admin/workspaces/resolve
PUT    /admin/workspaces/{workspace_id}/project

GET    /admin/teams
POST   /admin/teams
GET    /admin/teams/{id}/members
PUT    /admin/teams/{id}/members/{principal_id}
DELETE /admin/teams/{id}/members/{principal_id}

GET    /admin/context
POST   /admin/context/resolve
POST   /admin/context/revoke
```

### Audit/manual editing

```text
POST   /admin/changesets
GET    /admin/changesets?space_id=&state=&cursor=
GET    /admin/changesets/{id}
POST   /admin/changesets/{id}/approve
POST   /admin/changesets/{id}/reject
POST   /admin/changesets/{id}/revert

GET    /admin/memories/{logical_id}/history?space_id=&cursor=
GET    /admin/memories/{logical_id}/diff?space_id=&from=&to=
POST   /admin/memories/{logical_id}/edit
POST   /admin/memories/{logical_id}/retire
POST   /admin/memories/{logical_id}/restore
POST   /admin/memories/{logical_id}/destroy/preview
POST   /admin/memories/{logical_id}/destroy
POST   /admin/spaces/{source}/cherry-pick/{logical_id}
```

### Identity/material merge

```text
POST   /admin/merges/preview
GET    /admin/merges/{job_id}
GET    /admin/merges/{job_id}/candidates?state=&cursor=
GET    /admin/identity/candidates/{id}
POST   /admin/identity/candidates/{id}/decide
POST   /admin/identity/decisions/{id}/revise
POST   /admin/merges/{job_id}/resolve
POST   /admin/merges/{job_id}/confirm
POST   /admin/merges/{job_id}/cancel
POST   /admin/merges/recover
```

Long operations return `202 Accepted` plus existing typed `AdminJob`/merge job
summary. Preview discovery may itself be a job for large spaces. GET is
idempotent; confirmation/cancel/resolution use idempotency keys and expected
job/packet/plan versions.

Auth/status mapping:

- 400 invalid bounded input/cross-domain request.
- 401 missing/invalid bearer or context capability.
- 403 authenticated but insufficient current role/actor kind.
- 404 inaccessible resource as not-found when revealing existence would leak.
- 409 stale revision/generation/decision/plan, idempotency conflict, active job.
- 422 valid shape but unresolved merge conflicts/evidence policy failure.
- 423 space closed/promotion/recovery lock.
- 503 index repair/store recovery required.

## CLI

Add coherent nouns rather than one flag jungle:

```text
openmemory space list [--json]
openmemory space create --owner personal|team:<id> --context global|project:<id> --name <name>
openmemory space show <id>
openmemory space close <id>
openmemory space delete <id> --yes [--no-backup]

openmemory project map <path> [--project <id>|--new <name>]
openmemory context show [--workspace <path>] [--team <id>] [--json]

openmemory changeset list [--space <id>] [--state proposed]
openmemory changeset show <id>
openmemory review approve <id> --reason <text>
openmemory review reject <id> --reason <text>

openmemory memory history <logical-id> --space <id>
openmemory memory diff <logical-id> --space <id> [--from <rev>] [--to <rev>]
openmemory memory edit <logical-id> --space <id> --expected-revision <rev> ...
openmemory memory retire|restore|revert ...
openmemory memory cherry-pick <logical-id> --from <space> --to <space>

openmemory merge preview --source <space> --target <space>
openmemory merge candidates <job>
openmemory merge decide <candidate> same|different|undetermined --reason <text>
openmemory merge resolve <job> ...
openmemory merge apply <job> --plan-hash <hash> --yes
openmemory merge status <job>
openmemory merge recover [--job <id>]
```

Rules:

- Human-readable output shows space/owner/context and provenance; `--json`
  emits typed admin DTO JSON only.
- Destructive commands require `--yes` in noninteractive/scriptable mode.
- Review/merge administration requires a running daemon. State this plainly;
  do not acquire product/graph write locks in a second CLI process.
- Existing `remember`, `recall`, `list-entities`, and `forget-entity` keep
  current syntax/output by default. Add optional context/target flags without
  changing existing required arguments.
- Shell completions and CLI parser/output process tests cover every new command.

## MCP

### Existing tools

Keep all existing names. Extend input/output compatibly:

- `openmemory_remember`: optional `target` (`default|personal|team`),
  `idempotency_key`, `reason`; response includes concrete space provenance and
  either applied receipt or proposed review receipt.
- `openmemory_recall`: optional `read_mode`
  (`contextual|project_only|global_only`); results include space/revision and
  score explanation.
- `openmemory_list_entities`, `openmemory_get_entity`, `openmemory_status`:
  operate on resolved context, preserve origins, and expose context summary.
- `openmemory_forget`/`forget_entity`: retain existing compatibility and risk
  annotations; new manual lifecycle tools are safer.
- Index text/search/delete: bind to one explicit write/read context and include
  space provenance. Never search every catalog index.

### New agent-safe tools

```text
openmemory_context          # read-only current read set/write target/readiness
openmemory_history          # read-only bounded history for one contextual object
openmemory_propose_change   # non-destructive proposal; never approval
```

Do not expose membership administration, approval/rejection, identity decision,
merge confirmation/promotion, or hard destruction as agent MCP tools in this
release. Humans use authenticated admin/CLI/Desktop surfaces.

Tool schemas remain generated from serde/schemars; descriptors and handlers
stay colocated; registry/instructions/golden output is updated from one source.
Annotations:

- context/history: read-only, idempotent, non-destructive;
- proposal: write, non-destructive, non-idempotent unless key repeated;
- remember team proposal response is still write/non-destructive.

### Request context

- Add `OpenMemoryMcpServer::handle_with_context` internally.
- Stdio server holds one fixed resolved context.
- HTTP extracts an authenticated opaque context capability and passes it to
  the backend; absence uses fixed personal-global compatibility.
- Tool argument target cannot broaden the capability. `team` without an
  authorized active team returns typed invalid/forbidden behavior.
- `initialize.instructions` names active project/team/read layers and explains
  team proposal policy without leaking paths/secrets.

## Daemon integration and Desktop consumers

The sibling Desktop app is not implemented in this repository. This repository
delivers the complete typed admin contract it needs:

- context/space selector and readiness;
- provenance on search/entity detail;
- review inbox and typed field diff;
- history/editor/lifecycle/revert;
- identity evidence/decision review;
- merge preview/conflict/accounting/job/recovery state.

Server-sent events extend existing job events with identifiers/state/counts,
not memory contents. Token rotation closes streams and revokes context
capabilities just as current tests expect for auth generation.

## Watch/ingest/backup/status compatibility

- `watch` and `ingest` add explicit `--target default|personal|team` and resolve
  one space once. Partition support checks move from profile-wide to selected
  space capabilities.
- Status gains catalog/readiness summaries while existing single-store counts
  remain in their current fields for legacy callers.
- Consolidation/prune require one explicit space or an explicitly non-atomic
  all-authorized-spaces job with per-space reports. Never imply one transaction.
- Domain migration targets one closed space root and updates its manifest
  domain count/catalog hash after verified promotion.
- Backup defaults to full profile/catalog. Space-only backup is explicit and
  cannot restore imported membership as authority.

## Documentation changes

Update in the same implementation:

- `README.md`: concepts and compatibility quick start.
- `docs/architecture.md`: space vs domain boundaries and context flow.
- `docs/crates.md`: correct crate count/source maps including merge crate.
- `docs/storage.md`: catalog/manifests/schemas/staging/authority.
- `docs/context-engine.md`: space handle and layered recall relationship.
- `docs/mcp.md`: context capability, provenance, new/extended tools.
- `docs/cli.md`: every new command and destructive confirmation.
- `docs/configuration.md`: spaces/audit/merge sections and limits.
- `docs/development.md`: new tests/benches/crash harness.
- `docs/roadmap.md` and `CHANGELOG.md`: capability rollout/migration notes.

Rustdoc must describe atomicity boundaries and identity limitations, not just
method signatures.

## Compatibility acceptance

- Existing MCP E2E request corpus passes byte-compatible required fields and
  tool names; optional new response fields do not break old deserialization.
- Existing CLI process-output tests pass unless a deliberately additive line is
  accepted and golden-updated with reason.
- Old profile opens and behaves personal-global before any project mapping.
- Omitted context never broadens to all spaces.
- Direct no-daemon stdio remains functional on personal-global.
- Daemon and stdio proxy resolve the same workspace/team context and return the
  same provenance ordering.
- Default/all/no-default features compile and test; embeddings remain optional.

