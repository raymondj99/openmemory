# Context and Team Spaces

## Catalog service

`SpaceService` is the only component that creates, opens, updates, closes, or
deletes catalog entries. A creation is a recoverable state machine:

```text
catalog creating -> directory + manifest fsynced -> store opened/verified
                 -> catalog active
```

On failure, retain enough state to retry/clean safely. Never return an active
space before manifest/store verification. Deletion is likewise `active ->
closed -> deleting -> removed`; close runtime leases and take a verified backup
or explicit no-backup confirmation before destructive removal.

Required methods:

```rust
pub trait SpaceService {
    fn list(&self, actor: &Actor, filter: SpaceFilter) -> Result<Page<SpaceSummary>>;
    fn create(&self, actor: &Actor, request: CreateSpace) -> Result<SpaceSummary>;
    fn get(&self, actor: &Actor, id: &SpaceId) -> Result<SpaceDetail>;
    fn rename_display(&self, actor: &Actor, id: &SpaceId, name: &str) -> Result<SpaceSummary>;
    fn close(&self, actor: &Actor, id: &SpaceId) -> Result<()>;
    fn delete(&self, actor: &Actor, request: DeleteSpace) -> Result<AdminJob>;
}
```

The existing profile personal-global space cannot be deleted while the profile
exists. It may be backed up/reset only through explicit profile lifecycle.

## Stable project and workspace identity

Workspace paths are activation metadata:

1. Canonicalize the path, reject missing/non-directory/symlink escape.
2. If already mapped, use its stable `ProjectId` even if VCS metadata changed.
3. For a new mapping, optionally suggest a project from a normalized VCS remote
   fingerprint, but require human confirmation before joining an existing
   project. Same repo remote is strong workspace context, not cross-space entity
   identity.
4. A moved workspace adds/updates mapping while retaining `ProjectId`.
5. Multiple worktrees/workspaces may map to one project; one workspace maps to
   only one project per profile.

No recall request canonicalizes/scans every workspace. Resolution is one
indexed product database lookup made when minting/refreshing a context.

## Context resolver

Inputs:

```rust
pub struct ResolveContextRequest {
    pub principal: PrincipalId,
    pub actor_kind: ActorKind,
    pub profile: String,
    pub workspace: Option<PathBuf>,
    pub active_team: Option<TeamId>,
    pub read_mode: ReadMode,
    pub write_selection: WriteSelection,
}
```

Algorithm:

1. Authenticate actor; canonicalize/lookup workspace if supplied.
2. Load personal spaces by owner/context unique keys.
3. If a team is requested, load current team generation and unexpired grants;
   reject unknown/revoked/insufficient team context.
4. Construct the ordered 1–4 read set for `Contextual`, `ProjectOnly`,
   `GlobalOnly`, or explicit authorized overlay. Do not fill missing spaces by
   querying unrelated teams/projects.
5. Resolve one default write target and require sufficient role.
6. Record authorization generation (team plus membership/catalog generation).
7. Mint a cryptographically random opaque capability, persist only its BLAKE3
   hash and bounded concrete context, return plaintext once.

Capabilities expire after 15 minutes by default, are bound to the daemon bearer
credential/principal/actor kind, and are revalidated on use. Membership change,
team archive, space close, profile switch, token rotation, or material target
promotion invalidates affected capabilities immediately.

## Space registry

`SpaceRegistry` is a bounded cache of long-lived runtimes:

```rust
pub struct SpaceRuntime {
    pub id: SpaceId,
    pub manifest: SpaceManifest,
    pub domains: Arc<DomainStore>,
    pub engine: Option<Arc<ContextEngine>>,
    pub readiness: SpaceReadiness,
    // private lease/open/last-used state
}
```

Rules:

- Maximum eight open non-legacy spaces by default; hard max 64.
- An RAII lease pins a runtime during requests/jobs. Eviction only closes an
  unleased, idle runtime after flushing/checkpointing derived state.
- The personal-global runtime is pinned while the daemon runs for compatibility
  and fast single-space use.
- One opening task per `SpaceId`; concurrent requests wait for that result
  rather than opening duplicate pools/indexes.
- Open verifies catalog row, manifest binding, domain manifest, graph schema,
  identical bound space ID in every graph domain, index generation/outbox
  repair, and recovery intents before `Ready`.
- `close_for_promotion` blocks new leases, waits for current leases, pauses
  admissions/quiesces engine, flushes, and drops all handles so rename works.
- Cache keys include manifest/catalog generation. Promotion or membership
  changes cannot reuse a stale runtime/context.
- Registry metadata may contain 10,000 spaces while handles/connections remain
  bounded. Idle CPU is zero; no polling eviction loop faster than maintenance
  cadence.

## Layered recall

Request:

```rust
pub struct LayeredRecallRequest {
    pub query: String,
    pub top_k: usize,
    pub filters: RecallFilters,
    pub candidate_multiplier: usize, // internal bounded default
}

pub struct ScopedRecallResult {
    pub space: SpaceRef,
    pub read_priority: u8,
    pub local: RecallResult,
    pub layer_prior: f32,
    pub adjusted_score: f32,
    pub duplicate_origins: Vec<RecallOrigin>,
}
```

Execution:

- One space: direct `DomainStore::recall`, wrap provenance. No allocation-heavy
  scheduler or catalog query.
- Two cheap keyword spaces: sequential is permitted if measurements show lower
  latency.
- Vector or 3–4 space reads: use a fixed bounded executor/adaptive scoped tasks.
  `DomainStore` already fans out domains, so cap total work and avoid one thread
  per `(space, domain)` at both levels.
- Ask each space for `min(hard_cap, top_k * candidate_multiplier)` candidates.
  Default multiplier 2, hard component cap 256.
- A component failure returns a typed partial/degraded response only if product
  policy explicitly allows it and names the missing space. Default semantic
  context is fail-closed so the agent does not mistake incomplete team/project
  memory for a complete answer.

Fusion:

1. Preserve each component's graph-local score.
2. Apply a small versioned layer prior separately; never rewrite raw score.
   Start with all priors `1.0` unless retrieval evaluation justifies a change.
3. Presentation-deduplicate only exact semantic revision hashes. Retain every
   origin; do not deduplicate merely equal text or labels.
4. Sort by adjusted score descending, read priority ascending, observation
   validity/recency tie-break, `SpaceId`, and logical ID. Completion order never
   affects output.
5. Truncate to `top_k` and increment access counts only for physical winning
   hits according to one documented rule.

Cache key:

```text
ordered(space ID + semantic/index generation vectors)
+ authorization generation
+ query + every filter + top_k
+ ranking/fusion policy version
```

Do not cache a result across membership revocation, target promotion, or index
repair generation. Cache size/TTL remain bounded per context. Catalog display
name changes need not invalidate semantic results but response decoration must
read current safe metadata.

## Write routing and review policy

All writes call `ContextService::authorize_write(context, selection)` and get a
`SpaceWriteHandle` with role/current authorization generation.

| Owner / actor | Default behavior |
|---|---|
| Personal / human | Apply immediately unless explicitly proposed. |
| Personal / agent | Apply immediately for ordinary remember; destructive/manual lifecycle requires proposal or existing MCP destructive contract. |
| Team / contributor human | Propose. |
| Team / agent | Propose; cannot approve. |
| Team / reviewer human | May propose and approve another actor's proposal. |
| Team / maintainer human | May apply configured low-risk writes, manage membership, confirm merge/destruction. |

Default policy forbids self-approval for team changesets even when one principal
has both roles. A Maintainer may use an explicit emergency/direct path only if
configured, with reason and an audit event. Tests pin this behavior.

## Seamless agent context

### Stdio MCP

- `openmemory mcp` defaults workspace to process current directory.
- It canonicalizes once and asks a running daemon to resolve/mint an agent
  context. If no daemon is running, it uses the legacy fixed personal-global
  adapter, preserving existing setup.
- `--workspace`, `--team`, `--project-only`, `--global-only`, and
  `--no-project-context` let integrations choose explicitly.
- The selected context appears in MCP `initialize` instructions and through a
  read-only `openmemory_context` tool, so the agent can explain where it reads
  and writes.

### Daemon/HTTP MCP

- Extend the local stdio-to-daemon proxy to send an opaque
  `X-OpenMemory-Context` capability with each request.
- Direct HTTP MCP clients may acquire a capability through authenticated local
  admin context resolution; absence retains personal-global legacy behavior.
- HTTP extracts/authenticates capability before tool dispatch and passes a
  resolved context object, never raw space IDs, to the backend.
- Bearer token and context capability serve different purposes: bearer proves
  local client access; capability binds a bounded semantic context. Both are
  required when a capability is used.

### Tool behavior

- Existing remember/recall/entity/status tools work unchanged with omitted new
  fields.
- Recall automatically uses the resolved read set and returns provenance.
- Remember defaults to context write target; optional `target` is
  `default|personal|team`, not a raw ID.
- Team writes return a proposal receipt with `durable=true` for the proposal
  record but `applied=false`; tool descriptions make this explicit.
- An agent cannot call approval, identity decision, merge confirmation, team
  membership, or hard-destruction tools.

## Watchers and ingest

- A watcher/ingest invocation resolves one write space at startup and retains
  one handle/capability. It never changes target because current directory or
  active team metadata changes mid-run.
- Reports and metadata include `SpaceId` and semantic source, but filesystem
  watcher paths are not authorization evidence.
- Team watcher ingestion follows team proposal policy or an explicit trusted
  service-account policy; do not silently bypass review for throughput.
- Bulk non-atomic ingestion returns child changeset/ticket accounting and can
  be resumed idempotently.

## Isolation tests that must pass

- Same entity name and identical observation text in Project A/B: contextual
  A recall returns only A origins; B only B; overlay returns both origins.
- Same `cerpheus` label in personal-project and team-project: recall may show
  both, but neither graph gains an edge or shared logical ID.
- 1, 1,000, and 10,000 closed catalog spaces produce statistically equivalent
  single-context latency and identical results.
- Revoking membership invalidates a cached context before the next read/write.
- Moving a workspace preserves project/space identity; an unregistered sibling
  directory does not inherit it automatically.
- Agent team write creates proposal; agent approval attempt fails; authorized
  human approval makes it visible exactly once.
- Registry eviction/reopen preserves semantic/index generations and no handles
  exceed configured bounds under concurrent churn.
