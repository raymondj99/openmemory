# Contract and Invariants

## Vocabulary

- **Profile**: the existing top-level CLI/config namespace under
  `data/<profile>`. A profile contains a catalog of semantic memory spaces.
- **Memory space**: one user-visible semantic silo backed by one complete
  `DomainStore` root.
- **Engine domain**: one internal hash partition inside a `DomainStore`. It is
  never an authorization or product scope.
- **Owner**: either one installation principal or one locally defined team.
- **Context**: global or one stable project identity. A filesystem workspace is
  a mapping to a project, not the project identity itself.
- **Read set**: the immutable ordered authorized spaces consulted by a request.
- **Write target**: the one authorized space a request may mutate.
- **Overlay**: a temporary read set combining spaces without changing them.
- **Cherry-pick**: a provenance-bearing copy of selected revisions into a
  destination changeset.
- **Material merge**: a directional source-to-target merge that creates and
  atomically promotes a verified new target root.
- **Changeset**: the durable intent, decision, and compact result metadata for a
  semantic mutation.
- **Logical object**: a stable entity, observation, or relation identity whose
  semantic content evolves through immutable revisions.
- **Origin contribution**: immutable source material retained when an
  independently created source object is coalesced into a target object.

## Required value types

Implement these in `openmemory-core::space` with private fields, canonical
parsing, `Display`, `FromStr`, serde string encoding, and path-safe validation.
UUID-backed IDs are UUIDv7. Principal/team external strings are bounded opaque
identifiers and never interpreted as paths.

```rust
pub struct SpaceId(/* UUIDv7 */);
pub struct ProjectId(/* UUIDv7 */);
pub struct PrincipalId(/* 1..=128 safe bytes */);
pub struct TeamId(/* 1..=128 safe bytes */);
pub struct ChangeSetId(/* UUIDv7 */);
pub struct RevisionId(/* UUIDv7 */);
pub struct MergeJobId(/* UUIDv7 */);

pub enum SpaceOwner {
    User(PrincipalId),
    Team(TeamId),
}

pub enum SpaceContext {
    Global,
    Project(ProjectId),
}

pub enum SpaceRole {
    Reader,
    Contributor,
    Reviewer,
    Maintainer,
}

pub enum ActorKind {
    Human,
    Agent,
    System,
}

pub struct SpaceRef {
    pub id: SpaceId,
    pub owner: SpaceOwner,
    pub context: SpaceContext,
}

pub struct SpaceGrant {
    pub space: SpaceRef,
    pub role: SpaceRole,
    pub authority_generation: u64,
}

pub struct ReadSet(/* 1..=4 unique SpaceGrant values */);

pub struct MemoryContext {
    pub principal: PrincipalId,
    pub actor_kind: ActorKind,
    pub profile: String,
    pub project: Option<ProjectId>,
    pub active_team: Option<TeamId>,
    pub read_set: ReadSet,
    pub default_write: SpaceId,
    pub authorization_generation: u64,
}
```

`ReadSet::new` rejects empty, duplicate, unauthorized, or more than four
entries. It retains explicit precedence order. `MemoryContext::validate`
requires `default_write` to appear in the grants with at least Contributor
unless the context is read-only.

## Space combinations and precedence

When present and authorized, contextual recall uses this exact order:

1. Personal + active project.
2. Active team + active project.
3. Personal + global.
4. Active team + global.

Missing layers are omitted; the ordering of remaining layers does not change.
An unregistered workspace resolves to personal-global and optional active-team-
global. The user can choose project-only, global-only, or an explicit temporary
overlay, but the server records that choice in the request context and response
explanation.

Default write selection:

| Session | Default write |
|---|---|
| Mapped project, no explicit target | Personal-project |
| Unmapped/no project | Personal-global |
| Explicit `team` target | Team-project when mapped, otherwise team-global |
| Explicit `personal` target | Personal-project when mapped, otherwise personal-global |
| Explicit concrete space in admin UI | That space after role/current-generation checks |

MCP tools do not accept arbitrary space IDs. They accept `default`, `personal`,
or `team`; the capability-bound context resolves the concrete target. Admin
routes may name a space because they authenticate a human and re-authorize it.

## Local team authority for this implementation

This release is local-first and fully implementable without a hosted identity
system:

- First startup creates a random installation `PrincipalId` in product
  metadata. It is the local human principal.
- A local human Maintainer creates a team and grants roles to known opaque
  principal IDs. These records are the local authority for this release.
- Membership changes increment a team authority generation. Every cached
  context and every approval records and rechecks that generation.
- Agent context capabilities are bound to the local principal but carry
  `ActorKind::Agent`. Agents can read according to grants and can apply personal
  writes; team writes become proposals. Agents can never approve/reject/revise
  identity decisions or confirm material promotion.
- Human admin/CLI calls carry `ActorKind::Human`. Reviewer or Maintainer role is
  required for team proposal decisions; Maintainer is required for membership,
  destructive space lifecycle, and material merge confirmation.
- No remote synchronization, invite transport, SSO, signed remote membership,
  or offline multi-device cache is claimed. Those require a future authority
  source and are explicit non-goals.

## Semantic write contract

Every new semantic mutation has:

- A concrete `SpaceId` and home engine domain.
- Actor principal/kind, reason, source, and authorization generation.
- A caller idempotency key plus canonical request hash, scoped to the resolved
  space and home engine domain.
- Expected logical row version and/or expected revision for existing objects.
- One changeset state transition.
- Compact typed events and immutable new revisions on success.
- A monotonically increasing home-domain semantic generation.
- Durable index/mirror outbox work where required.

An immediate personal write creates and applies one changeset inside one
transaction. A proposal creates only proposal/request rows. Approval rechecks
authorization, current revisions, lifecycle, request hash, and state before one
atomic apply. A retry with the same key and request hash returns the original
receipt in that routed domain; key reuse with different content in that routed
domain is a conflict. Surface-generated keys are UUID-backed and globally
unique so callers never intentionally share one across unrelated domains.

An interactive changeset may contain multiple operations only when every
canonical object routes to the same engine domain. Cross-domain requests fail
before proposal insertion. An explicitly non-atomic batch is represented as
independent child changesets with independent results.

## Identity contract

Two equal labels—including two concepts both named `cerpheus`—mean only
“candidate.” The concepts remain separate unless one of these is current:

1. Recorded lineage proves one was copied from the exact revision of the other.
2. Both revisions carry the same source-verified canonical value in a namespace
   configured as unique and compatible for their controlled kinds.
3. A human Reviewer/Maintainer approves a packet-bound decision after seeing
   both revisions, evidence, contradictions, and proposed merge effect.

Embeddings, aliases, descriptions, neighboring relations, repository owner,
user assertions, agent assertions, and name similarity are context. They may
rank a candidate but cannot automatically establish identity. Conflicting
trusted identifiers bypass the agent and require human resolution. No evidence
or agent response causes an error state: the safe result is `different` or
`undetermined`, and both source concepts remain represented.

## Merge contract

- Merge direction is explicit: source is read-only; target is replaced by a
  verified new target.
- Planning consumes immutable snapshots and current reviewed identity receipts.
- Every candidate has exactly one resolution bound to both current revisions
  and the evidence-policy generation.
- Every source entity has exactly one disposition: coalesce with one target or
  add under a deterministic source-qualified logical ID.
- One target cannot absorb two source entities in one plan. Normalize/review
  source duplicates first.
- Coalescing never overwrites the target projection with source text. It appends
  an origin contribution and merges claims/revisions according to policy.
- Every relation assertion retains source/evidence provenance and rewires both
  endpoints through the disposition map. Missing endpoints block the plan.
- When lineage provides a common base, semantic revisions use three-way
  classification: unchanged, source-only, target-only, same change, disjoint
  change, or conflict. Wall-clock last-write-wins is forbidden.
- Without a common base, the planner performs conservative union and surfaces
  collisions. It does not invent a base.
- The plan includes source/target snapshot hashes, target expected generation,
  resolution receipt hashes, complete object accounting, predicted result
  hash, and deterministic plan hash.
- Materialization does not mutate a live target. It builds a staged root,
  rebuilds derived state, verifies it, pauses target admission, rechecks the
  target hash/generation, and promotes by same-filesystem rename with an fsynced
  intent.

## Non-negotiable invariants

1. A graph method receives one already authorized `SpaceHandle`; graph code
   never guesses a scope or performs authorization.
2. Every entity, observation, relation, recall result, history row, diff,
   identity packet, and merge result carries space provenance at the API edge.
3. Catalog size does not affect recall work. Only the read set is consulted.
4. Relations and spreading activation never cross space roots.
5. Proposed, rejected, conflicted, or stale changes never alter canonical rows
   or search indexes.
6. Canonical mutation and its applied audit metadata are all-or-nothing in one
   SQLite transaction.
7. Cross-domain atomicity is never implied. Staging is used when an operation
   genuinely spans the whole space.
8. Stale row/revision, stale membership generation, stale snapshot, or stale
   plan invalidates the entire operation.
9. Access counts and ranking telemetry do not create semantic revisions and do
   not affect canonical snapshot hashes.
10. Display names, workspace paths, repo URLs, and caller `source` values are
    not identities or authorization evidence.
11. Search/index lag cannot silently return an old semantic revision as if it
    were current.
12. Derived state is rebuilt, never semantically merged.
13. Recovery is idempotent and yields verified old or verified new target.
14. Source snapshot hash is unchanged after preview, cancel, failure, apply,
    and recovery.
15. No model call appears on recall, ordinary remember, or approval critical
    paths. Identity model proposals are asynchronous and optional.

## Explicit non-goals

- Hosted synchronization, distributed consensus, remote team membership, SSO,
  enterprise RBAC, or multi-device conflict-free replication.
- Automatic cross-space merge by fuzzy name, embedding similarity, timestamps,
  or agent confidence.
- Arbitrary cross-space graph edges.
- Making a material merge an interactive hot-path operation.
- Replacing SQLite, `DomainStore`, FTS/vector backends, the context engine, or
  existing MCP protocol implementation.
- A generic version-control system for every byte of product metadata.
- Direct Desktop/CLI SQLite edits.
- Atomic interactive mutation across multiple engine domains.
- Windows promotion guarantees in the first release. Do not advertise Windows
  material merge until platform-specific rename/fsync tests exist.

## Stable failure categories

Add typed internal variants and corresponding admin codes:

```text
space_not_found
space_closed
space_manifest_mismatch
space_access_denied
team_context_required
read_set_too_large
context_expired
authority_generation_moved
changeset_not_found
changeset_not_pending
changeset_stale
changeset_cross_domain
idempotency_conflict
revision_not_found
object_retired
hard_delete_confirmation_required
index_repair_required
identity_candidate_stale
identity_evidence_invalid
identity_decision_moved
identity_revalidation_required
merge_conflicts_unresolved
merge_target_moved
merge_cross_filesystem
merge_disk_budget_exceeded
merge_recovery_required
```

Errors exposed through admin carry stable `snake_case` codes, a safe message,
optional repair hint, retryability, and bounded structured details. MCP maps
caller errors to invalid-params/tool errors and internal durability errors to a
trace-ID-bearing internal error without exposing content or local paths.
