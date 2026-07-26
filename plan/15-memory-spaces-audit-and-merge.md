# Memory Spaces, Audit History, and Merge

## Status

Validated architecture and production implementation plan. Not implemented.

The executable evidence is in `experiments/memory-model`; commands, failed
options, measurements, caveats, and open questions are in `LOGBOOK.md` entries
E000–E017. This document is the production path derived from that evidence.

This plan generalizes `14-project-scoped-memory.md`. Its physical isolation and
bounded layered-recall decisions stand, but `GraphRef::Global | Project` is too
narrow for personal/team ownership and safe cross-space operations. Production
code should use the model below and expose project/global language as a product
view over it.

## Validated decisions

1. A semantic **memory space** owns one complete `DomainStore` family. Engine
   domains remain internal performance partitions.
2. Space identity is independent of workspace, project, user, and team IDs.
3. Owner (`User` or `Team`) and context (`Global` or `Project`) are orthogonal.
4. A request receives an immutable, authorized, ordered read set of one through
   four spaces. Recall never scans the catalog.
5. Overlay, cherry-pick, and material merge are different operations. Overlay
   is the default because it is read-only and reversible.
6. Every semantic write is a changeset. A proposal is invisible; approval
   checks optimistic versions and applies atomically with canonical rows.
7. Logical object IDs are stable. Semantic content is immutable and changes by
   supersession. Diff is revision lineage plus compact typed lifecycle events,
   not stored copies of full before/after JSON.
8. Merge has two separate layers. Cross-space identity maps independently
   created IDs through reviewed, revision-bound receipts. Semantic revision
   merge then uses lineage/common-base three-way classification when a base
   exists, or a conservative union with explicit collisions when it does not.
   Neither layer resolves with fuzzy names or wall-clock last-write-wins.
9. Material merge builds and verifies a complete staged target, then promotes
   it with a durable intent and same-filesystem rename. The source is read-only
   and the old target is retained temporarily.
10. Search indexes, access counts, relation mirrors, caches, and engine journals
    are derived state. They are rebuilt or repaired, never merged as truth.

## Vocabulary and type model

```rust
pub struct SpaceId(/* validated UUIDv7 */);
pub struct ProjectId(/* validated UUIDv7 */);
pub struct PrincipalId(/* validated opaque ID */);
pub struct TeamId(/* validated opaque ID */);

pub enum SpaceOwner {
    User(PrincipalId),
    Team(TeamId),
}

pub enum SpaceContext {
    Global,
    Project(ProjectId),
}

pub struct SpaceRef {
    pub id: SpaceId,
    pub owner: SpaceOwner,
    pub context: SpaceContext,
}

pub enum SpaceRole {
    Reader,
    Contributor,
    Reviewer,
    Maintainer,
}

pub struct AuthorizedSpace {
    pub space: SpaceRef,
    pub role: SpaceRole,
}

pub struct MemoryContext {
    pub principal: PrincipalId,
    pub project: Option<ProjectId>,
    pub active_team: Option<TeamId>,
    pub read_set: ReadSet,       // 1..=4 unique AuthorizedSpace values
    pub default_write: SpaceId,
    pub resolution_generation: u64,
}
```

Tuple fields remain private. IDs accept canonical opaque encodings only; paths,
display names, repository URLs, and caller-controlled `source` values are never
identities or authorization evidence.

Workspace is local activation metadata. Multiple workspaces may map to one
project, and a moved workspace retains the same project and spaces. A project
may exist without a local workspace.

## Default personal/team behavior

Only one team is active in an agent session. This keeps the read set bounded
and makes provenance explainable.

When all four spaces exist and the principal is authorized, default precedence
is:

1. User + active project.
2. Active team + active project.
3. User + global.
4. Active team + global.

Missing layers are omitted. An unmapped session defaults to user-global plus an
optional active team-global. Default writes go to user-project when mapped and
user-global otherwise. A team write is always explicit and requires at least
`Contributor`; deployments may require review before it becomes visible.

MCP cannot name an arbitrary team or project space. It selects from session
capabilities resolved by the daemon. Administrative APIs may name a space but
must authorize the principal before returning a handle.

## Non-negotiable invariants

- A graph operation receives one `SpaceHandle`; graph code never guesses scope.
- Every returned entity, observation, relation, recall result, diff, and event
  carries `SpaceId` provenance at the API boundary.
- One semantic object belongs to one space. Sharing creates a provenance-bearing
  revision in the destination; it does not create a cross-space foreign key.
- Relations never cross space roots. Overlay can visualize similarly named
  nodes, but cannot invent a cross-space edge.
- Proposed changes and conflicted merges do not update canonical rows or search
  indexes.
- Applied semantic rows and their changeset metadata share one SQLite
  transaction in their canonical home domain. Interactive changesets never
  span the independent SQLite files inside a partitioned space.
- Stale expected revision IDs or row versions fail the entire domain-local
  changeset.
- A retry with the same idempotency key and request hash returns the original
  result. Reusing a key for another request conflicts.
- Access telemetry and ranking feedback never create semantic revisions.
- No material merge promotes a target whose canonical hash changed after plan.
- Every discovered cross-space candidate has exactly one current resolution;
  missing, duplicate, extra, stale, or undetermined resolutions block planning.
- Every source entity has exactly one merge disposition. A reviewed `same`
  coalesces into one target; otherwise the source is added under a deterministic
  source-qualified ID. No target can receive two source identities until those
  source duplicates are normalized and reviewed first.
- Coalescence appends immutable origin contributions; it never replaces the
  target label, description, identifiers, or revision with a source projection.
- Every imported relation assertion retains origin space and source evidence;
  both endpoints are rewired through the entity disposition map before the
  assertion can enter the target.
- Recovery produces the old verified target or new verified target, never an
  unverified mixture.

## On-disk layout

Keep an existing profile root in place as the initial user-global space. New
spaces live under opaque IDs:

```text
data/<profile>/                       # existing user-global DomainStore
data/<profile>/space.toml             # binds root to SpaceId and metadata
data/<profile>/spaces/<space-id>/     # every new semantic space
data/<profile>/spaces/<space-id>/space.toml
data/<profile>/spaces/<space-id>/memory.sqlite or domains/
data/<profile>/spaces/<space-id>/lineage/
data/<profile>/.merge-intents/
product/product.sqlite                # catalog, mappings, membership cache, jobs
```

`space.toml` contains format version, `SpaceId`, owner, context, creation time,
and pinned domain count. Opening a catalog entry verifies the manifest; moving a
database under another catalog ID must fail closed.

Staging and target must share a filesystem. The job validates this before
copying. Staging/backup names include merge job IDs and are never constructed
from display names. Recovery artifacts count against an explicit disk quota.

## Product metadata schema

Add forward-only daemon migrations:

```sql
CREATE TABLE memory_spaces (
    id                    TEXT PRIMARY KEY,
    profile               TEXT NOT NULL,
    owner_kind            TEXT NOT NULL CHECK(owner_kind IN ('user','team')),
    owner_id              TEXT NOT NULL,
    context_kind          TEXT NOT NULL CHECK(context_kind IN ('global','project')),
    project_id            TEXT,
    display_name          TEXT NOT NULL,
    root_key              TEXT NOT NULL UNIQUE,
    state                 TEXT NOT NULL CHECK(state IN ('active','detached','deleting','error')),
    format_version        INTEGER NOT NULL,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    CHECK((context_kind = 'project') = (project_id IS NOT NULL)),
    UNIQUE(profile, owner_kind, owner_id, context_kind, project_id)
);

CREATE TABLE space_memberships (
    space_id              TEXT NOT NULL REFERENCES memory_spaces(id),
    principal_id          TEXT NOT NULL,
    role                  TEXT NOT NULL CHECK(role IN ('reader','contributor','reviewer','maintainer')),
    authority_generation  INTEGER NOT NULL,
    expires_at            INTEGER,
    PRIMARY KEY(space_id, principal_id)
);

CREATE TABLE workspace_projects (
    workspace_id          TEXT PRIMARY KEY,
    project_id            TEXT NOT NULL,
    profile               TEXT NOT NULL,
    canonical_path        TEXT NOT NULL,
    state                 TEXT NOT NULL,
    UNIQUE(profile, canonical_path)
);

CREATE TABLE space_lineages (
    id                    TEXT PRIMARY KEY,
    source_space_id       TEXT NOT NULL,
    target_space_id       TEXT NOT NULL,
    source_generation     INTEGER NOT NULL,
    target_generation     INTEGER NOT NULL,
    base_hash             TEXT NOT NULL,
    last_change_set_id    TEXT NOT NULL,
    manifest_path         TEXT NOT NULL,
    created_at            INTEGER NOT NULL
);
```

`space_memberships` is a local authorization cache, not an authority invented by
the memory engine. Team sync/auth work must define its signed source and expiry.
Until then, team spaces remain behind a capability flag and local maintainers
manage membership explicitly.

## Graph schema: compact semantic history

Increment `MEMORY_SCHEMA_VERSION` in phases. Do not rewrite every existing row
during daemon startup.

### Atomicity boundary in a partitioned space

A `DomainStore` with `K > 1` is a family of independent SQLite databases. There
is no space-wide SQLite transaction. Production APIs make that boundary
explicit:

- One interactive changeset has one canonical home domain.
- A same-domain changeset may contain several operations and is atomic.
- A request whose canonical objects route to different domains is rejected
  before proposal insertion. The caller may submit an explicitly non-atomic
  batch of child changesets and receives a result for each child.
- Canonical relation mutation and audit live in the source entity's home
  domain. Target-domain stubs/mirrors are derived idempotent outbox work.
- Space version is an ordered vector of durable domain generations. A
  consistent whole-space snapshot takes an admission barrier and records every
  component generation and the resulting canonical root hash.
- A truly atomic cross-domain semantic operation uses an invisible staged copy
  and whole-root promotion. Its audit is a merge/job manifest plus the complete
  set of domain-local child changesets inside the promoted root.

Do not add a home-directory “coordinator commit” and call it atomic. Without a
prepared-transaction protocol in every domain it can still expose partial
state after a crash; staging is the simpler verified primitive for rare
cross-domain administration.

### Changesets

```sql
CREATE TABLE change_sets (
    id                    TEXT PRIMARY KEY,
    space_id              TEXT NOT NULL,
    idempotency_key       TEXT NOT NULL UNIQUE,
    request_hash          BLOB NOT NULL,
    actor_principal       TEXT NOT NULL,
    actor_kind            TEXT NOT NULL,
    reason                TEXT NOT NULL,
    state                 TEXT NOT NULL CHECK(state IN
                              ('proposed','applied','rejected','conflicted','reverted')),
    base_generation       INTEGER NOT NULL,
    committed_generation  INTEGER,
    created_at            INTEGER NOT NULL,
    decided_at            INTEGER,
    decided_by            TEXT
);

CREATE TABLE change_requests (
    change_set_id         TEXT NOT NULL REFERENCES change_sets(id),
    ordinal               INTEGER NOT NULL,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    operation             TEXT NOT NULL,
    expected_revision_id  TEXT,
    expected_row_version  INTEGER,
    requested_payload     BLOB,
    PRIMARY KEY(change_set_id, ordinal)
) WITHOUT ROWID;

CREATE TABLE change_events (
    change_set_id         TEXT NOT NULL REFERENCES change_sets(id),
    ordinal               INTEGER NOT NULL,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    operation             TEXT NOT NULL,
    before_revision_id    TEXT,
    after_revision_id     TEXT,
    before_lifecycle      TEXT,
    after_lifecycle       TEXT,
    compact_payload       BLOB,
    PRIMARY KEY(change_set_id, ordinal)
) WITHOUT ROWID;
```

These tables exist in each physical domain database. A proposed add is routed
from its destination entity key; an edit is routed from the existing canonical
object. The proposal and its eventual canonical mutation therefore share one
SQLite file.

`change_requests` retains the proposed intent. Applied diffs resolve immutable
revision IDs and lifecycle transitions from `change_events`; they do not store
serialized copies of whole objects. Encode typed payloads with an explicit
version, bounded size, and deterministic field order. JSON is acceptable at the
API boundary, not as a redundant canonical history format.

### Observation revisions

Use the existing observation ID as its stable logical ID. Add current-projection
columns and an immutable revision table:

```sql
ALTER TABLE observations ADD COLUMN current_revision_id TEXT;
ALTER TABLE observations ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE observations ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';

CREATE TABLE observation_revisions (
    id                    TEXT PRIMARY KEY,
    observation_id        TEXT NOT NULL REFERENCES observations(id),
    parent_revision_id    TEXT,
    semantic_hash         BLOB NOT NULL,
    content               TEXT NOT NULL,
    observed_at           INTEGER NOT NULL,
    valid_from            INTEGER,
    valid_until           INTEGER,
    confidence            REAL NOT NULL,
    source                TEXT NOT NULL,
    memory_tier           TEXT NOT NULL,
    title                 TEXT,
    summary               TEXT,
    importance            REAL,
    source_kind           TEXT,
    created_by_change_set TEXT NOT NULL REFERENCES change_sets(id),
    created_at            INTEGER NOT NULL
);
```

Concepts and source files need revision-owned child tables. The existing
`observations` row remains the recall projection during compatibility rollout;
the same transaction inserts a revision, updates the projection/head, appends
events, increments generation, and enqueues index work.

Legacy rows may have `current_revision_id = NULL`. On their first semantic
mutation, create a baseline revision from the current projection and the new
revision in the same transaction. A resumable background job backfills the rest
with counts, checkpoints, and restart safety. Do not make startup O(corpus).

All prose/content changes supersede. A “fix typo” UI action preserves logical
identity but creates a new revision. Metadata that does not affect semantic
recall may use a compact typed patch event.

### Entity and relation readiness

Observation audit/edit can land first. Entity rename and relation modification
remain disabled until their identity work is complete.

- Give every canonical relation a stable logical relation ID.
- Mark mirror rows with the canonical relation ID and mirror role.
- Write mirror maintenance through a durable outbox; make it idempotent.
- Rebuild legacy mirrors from canonical relations before enabling relation
  merge/edit.
- Treat entity rename as an administrative re-home because entity-name hashing
  may change the owning domain. It uses staging/rebuild, not an `UPDATE name`.
- Entity deletion must not use the current irreversible cascade when history or
  revert is promised. Introduce lifecycle state and an explicit legal hard-delete
  path with a preview of history destruction.

## Durable index synchronization

Canonical SQLite and changeset history can be atomic; external vector/index
files cannot join that transaction. Add a durable outbox/generation:

```sql
CREATE TABLE index_outbox (
    generation            INTEGER NOT NULL,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    operation             TEXT NOT NULL,
    revision_id           TEXT,
    PRIMARY KEY(generation, object_kind, logical_id)
) WITHOUT ROWID;
```

Write flow:

1. Validate and normalize the draft outside the writer lock.
2. Compute embeddings before the lock when the desired revision is known.
3. Acquire the graph rebuild/write barrier and begin `IMMEDIATE` transaction.
4. Recheck authorization generation, expected revisions, lifecycle, and
   changeset state.
5. Insert immutable revisions/events, update current projections, increment the
   home-domain semantic generation, and append outbox rows.
6. Commit once.
7. While still excluding inconsistent recall, apply index operations and mark
   the generation indexed.
8. If index work fails, return a typed degraded result, keep outbox rows, and
   block or use a correct fallback for affected recall until repair succeeds.

On open, drain a small outbox or rebuild the index when generations diverge.
Crash tests must cover commit before index update, torn external index state,
idempotent drain, and recall during repair. Never return new canonical content
ranked by stale old index text without an explicit degraded explanation.

## Changeset state machine

```text
draft -> proposed -> applied
                  -> conflicted
                  -> rejected
applied -> reverted (by a new applied changeset)
```

- Immediate writes insert and apply one changeset in one transaction.
- Proposal stores expected revision/version and bounded desired payload only.
- Approval does all validation again; preview-time checks are advisory.
- Conflict does not partially apply. The user may create a new proposal against
  the new head.
- Reject records reviewer and reason without canonical mutation.
- Revert is a new forward changeset which selects earlier semantic revisions.
  History is never deleted to simulate undo.
- Multi-object changesets are all-or-nothing only when every canonical object
  routes to the same home domain. Reject a spanning request with
  `changeset_cross_domain` before inserting the proposal.
- “Approve all” is a batch of independently reported domain-local changesets,
  never a falsely atomic space-wide transaction.
- Cross-space “move” is copy/cherry-pick followed by an optional, separate
  source delete. The UI must display that these are two durable operations.

The daemon, not the graph crate, enforces reviewer policy. The graph transaction
still records principal, authorization generation, and decision so a forged
caller cannot bypass checks through another surface.

## Layered recall implementation

Add a shared orchestration layer in `openmemory-engine`:

```text
MemoryContextResolver -> authorized ReadSet -> SpaceRegistry leases
    -> bounded component recall -> deterministic LayeredRecallMerger
```

The daemon registry owns lazy open/eviction. A `SpaceRuntime` owns one
`DomainStore`, optional `ContextEngine`, health, generation, and lease count.
Default maximum open non-global spaces remains eight; registry metadata may be
much larger.

Do not create nested unbounded threads. `DomainStore` already fans out its
partitions. Implement a small bounded executor shared by read-set and domain
work, or an adaptive strategy:

- One space: call directly.
- Two cached/cheap keyword spaces: sequential is allowed.
- Expensive/vector or three/four-space reads: bounded parallel tasks.

Fusion rules:

1. Request enough candidates from each selected space; cap per-space work.
2. Preserve graph-local score and record any layer prior separately.
3. Deduplicate only exact normalized semantic keys for presentation and retain
   every origin. Do not turn recall dedup into identity merge.
4. Sort by adjusted score, read-set priority, temporal tie-break, `SpaceId`, and
   stable object ID.
5. Return at most `top_k` with component score, prior, winning origin, and all
   exact-duplicate origins in recall explanation.

Space version is the ordered vector of durable per-domain semantic/index
generations. Cache keys include ordered space IDs, those generation vectors,
authorization generation, query, filters, and ranking configuration. Access
count updates go back only to the winning physical observations and never to
duplicate origins unless they were actually retrieved under the product rule.

## Overlay, cherry-pick, and material merge

### Overlay

An overlay is a read set. It changes no graph, creates no lineage, and is the
safe default for “use team/global/another project context.” Relation spreading
stays inside components.

### Cherry-pick

Cherry-pick selects logical revisions from a source snapshot and proposes a
destination changeset. It records:

- Source space, logical ID, revision ID, semantic hash, and source generation.
- Destination logical ID and collision decision.
- Actor/reason and optional link to the source changeset.

An absent destination ID is an add. An exact semantic match is a no-op with
provenance. A differing same logical ID conflicts unless the user explicitly
chooses a new logical ID or supersession.

### Cross-space entity identity

Identity resolution runs before material merge classification when two
independently created logical IDs may represent the same concept. Exact names
and embedding similarity only discover candidates; they never establish
identity.

Resolution order:

1. Validate and bound entity evidence before indexing. Build candidates through
   indexed shared lineage, external-identifier assertions, normalized labels,
   bounded aliases, source-verified relation-target selectors, and bounded
   semantic retrieval. A relation selector may nominate a non-homonymous
   target only when its source snapshot is bound to the subject. Prefer a
   source-verified canonical target identifier when the source supplies one;
   otherwise require both the normalized target label and controlled target
   kind to match and retain bounded ambiguity rather than selecting one.
   Claimed,
   unbound, and wrong-kind assertions cannot expand scope. Exact lookup returns a
   deterministic page with a hard cap and `truncated` flag; omission is never a
   `different` decision. Source-verified identifiers and lineage enter before
   source-bound relation selectors, labels, and untrusted IDs so a homonym
   bucket cannot starve stronger signals. Relation-based discovery remains
   context and cannot prove identity or directly create an edge.
   Never scan the Cartesian product.
2. Distinguish `Claimed` identifiers from `SourceVerified` identifiers at the
   ingestion boundary. A verified assertion retains source snapshot/hash,
   retrieval time, extractor/verifier version, namespace resolver generation,
   raw value, and canonical value. Only two verified canonical values under the
   same configured unique-namespace contract can prove or contradict identity.
   User text, model output, missing provenance, unknown namespaces, and values
   canonicalized under incomparable resolver generations are context only.
3. Construct an immutable packet bound to both entity revisions and an explicit
   evidence-policy version. Evidence is typed as proof, contradiction, context,
   or a source-verified directional relation assertion. Aliases, labels,
   descriptions, graph neighborhoods, embeddings, shared URLs, and unknown
   ontology pairs are always context.
4. Use a versioned controlled ontology for kind compatibility. A pair is
   `compatible`, `incompatible`, or `unknown`; raw string inequality is not a
   contradiction. Historical views such as asteroid/dwarf-planet can be
   compatible. Unknown fails open to review context, never to identity proof.
5. Resolve consistent proof deterministically through personal/team policy.
   Conflicting verified identifiers without same-proof establish `different`.
   Any mixture of trusted same-proof and contradiction yields
   `ConflictingProofs`, bypasses the agent, and requires direct human conflict
   review. Trusted evidence is not silently ordered by precedence.
6. An asynchronous agent may return `same`, `different`, or `undetermined`,
   citing packet evidence and recording model and prompt provenance. Its
   proposal includes a deterministic hash of the complete packet, including
   policy version and evidence. It cannot write canonical rows, authorize
   itself, reverse a deterministic decision, resolve conflicting proofs, or
   override a hard contradiction. All strings, arrays, and evidence references
   are size-bounded and structurally validated.
7. Default team policy reviews every non-lineage `same` decision and every
   agent recommendation. Agent failure or abstention keeps both concepts
   separate and does not block a merge.
8. Persist packet-bound proposals, deferred abstentions, and immutable decision
   events. An identical packet suppresses repeat prompts. Entity revision,
   source evidence, namespace resolver, or policy changes produce a different
   packet hash and reopen review. Reversal supersedes history rather than
   erasing it.
9. Keep identity and semantic relations separate. A `different` recommendation
   may include `named_after` or another relation suggestion only when it cites
   source-verified directional evidence bound to the exact relation type,
   subject, and object. Generic overlap and agent/user claims cannot authorize
   an edge. Relation suggestions become separate reviewable graph changesets;
   grouping by type/source is presentation, not atomic approval.
10. Once identity is approved, issue a receipt bound to the exact target/source
    entity revisions and evidence packet hash. The material planner maps the
    source address to the target canonical entity and appends the source entity
    as an immutable origin contribution. It never overwrites the target
    projection. Merge claims and relations separately; entity equivalence never
    resolves a contradictory claim by itself. Many-to-one coalescence fails
    closed until duplicate source identities are normalized in their own space.

The packet and durable records need at least these conceptual fields:

```rust
pub enum AssertionTrust {
    Claimed,
    SourceVerified {
        source_snapshot: SourceSnapshotId,
        verifier_version: VerifierVersion,
        resolver_generation: ResolverGeneration,
    },
}

pub struct IdentityPacket {
    pub pair: CanonicalEntityPair,
    pub first_revision: RevisionId,
    pub second_revision: RevisionId,
    pub policy_version: IdentityPolicyVersion,
    pub evidence: BoundedVec<IdentityEvidence>,
    pub binding_hash: PacketHash,
}
```

The identity ledger is control-plane state. Canonical entity revisions and
provenance remain in their home graph domain. The agent adapter stays outside
`openmemory-graph`; packet-hash, deterministic validation, authorization, and
current policy/resolver generation are checked again at approval.

### Semantic merge plan contract

The planner consumes only immutable canonical snapshots, the complete bounded
candidate set, and current resolution receipts. Fixture expectations, model
prompts, labels, and UI choices are not planner inputs.

```rust
pub struct IdentityResolution {
    pair: DirectionalCandidatePair, // target, source
    decision: SameOrDifferent,
    target_revision: RevisionId,
    source_revision: RevisionId,
    evidence_packet_hash: PacketHash,
    authority: PolicyRuleOrHumanReview,
}

pub enum EntityDisposition {
    Merge {
        source: ScopedEntityId,
        target: CanonicalEntityId,
        resolution_hash: ResolutionHash,
    },
    Add {
        source: ScopedEntityId,
        target: SourceQualifiedEntityId,
    },
}

pub struct SemanticMergePlan {
    target_hash: CanonicalRootHash,
    source_hash: CanonicalRootHash,
    resolution_hashes: Vec<ResolutionHash>,
    entity_dispositions: Vec<EntityDisposition>,
    relation_actions: Vec<ProvenanceBearingRelationAction>,
    predicted_result_hash: CanonicalRootHash,
    plan_hash: PlanHash,
}
```

Planning is deterministic and ordered:

1. Verify that source and target are different spaces.
2. Require one resolution for every candidate and no resolution outside the
   candidate set.
3. Recheck both entity revisions and packet binding. A `same` result requires
   human review under team policy even when an identifier proves identity.
4. Reject multiple `same` targets for one source and multiple source identities
   for one target.
5. Account for every source entity exactly once as `Merge` or `Add`. Added IDs
   are source-qualified; any generated-ID collision blocks the plan.
6. Build one endpoint map, then rewrite every source relation through it.
   Relation identity and provenance are separate: an existing relation may gain
   a new source assertion without becoming a duplicate edge.
7. Simulate materialization, compare the exact target-plus-source contribution
   set, prove every target assertion survived, prove every source assertion is
   present after rewiring, and hash the predicted result.
8. Bind the complete plan, result hash, source/target hashes, and every
   resolution hash into one deterministic plan hash.

Materialization rechecks the plan hash and both snapshot hashes before doing
work, applies to a new snapshot/staging root, repeats complete accounting, and
requires the observed root hash to equal the prediction. Failure returns no
partially mutated in-memory graph; production persistence uses the staged-root
promotion protocol below.

The target-facing entity view is a projection over origin contributions. An
approved coalescence stores both source records rather than blending prose or
discarding disagreement. Projection/consolidation is a later audited semantic
operation, not part of identity merge.

### Material merge

Material merge is a daemon job with preview and confirmation. It requires a
lineage/common-base manifest for automatic three-way classification. An
unrelated-space merge may safely union distinct IDs, but every differing same
ID is a conflict; it does not invent a base.

Authoritative comparison tuple:

```text
(object_kind, logical_id, semantic_hash, lifecycle)
```

Three-way classification per logical ID:

| Source vs base | Target vs base | Result |
|----------------|----------------|--------|
| unchanged | any | keep target |
| changed | unchanged | take source |
| same semantic result | same result | keep target + provenance |
| changed | changed differently | conflict |
| deleted | edited | conflict |
| edited | deleted | conflict |

Relations compare canonical endpoint logical IDs, type, direction, validity,
and semantic attributes. Derived mirrors do not participate.

Materialization protocol:

1. Resolve authorization: source `Reader`, target `Maintainer` (or configured
   merge role).
2. Take consistent source/base/target canonical snapshots and generations.
3. Plan in stable logical-ID order; persist plan hash and bounded conflict
   records. Preview is read-only.
4. Confirm plan through a short-lived token bound to hashes and target.
5. Build a same-filesystem staging root from a transactional target snapshot.
6. Apply deterministic domain-local child changesets to staging; import
   immutable revisions and provenance, never source physical row IDs blindly.
   A staged merge manifest binds all children to one plan/job/result hash.
7. Rebuild FTS/vector indexes, partition routing, relation mirrors, caches, and
   lineage manifest from canonical staged state.
8. Verify schema, foreign keys, canonical counts, revision/event linkage,
   semantic root hash, index counts/generation, and a sampled recall fixture.
9. Pause target admission, quiesce/flush/close its runtime, and recheck target
   hash. If it moved, reopen it and abort without promotion.
10. Fsync intent and parent, rename target to unique backup, rename staging to
    target, fsync parent, open/verify the promoted root, update catalog, and
    clear intent.
11. Keep backup until the configured retention/health window expires. Cleanup
    is a separate recoverable job.

Recovery runs before any affected space opens. State combinations and actions
are explicit and exhaustively tested; ambiguous combinations fail closed and
surface backup/staging paths to `doctor`, never guess.

## Repository change map

| Crate/area | Production responsibility |
|------------|---------------------------|
| `openmemory-core` | Validated space/project/principal/team IDs, owner/context/role, read set, logical revision references, changeset states. |
| `openmemory-graph` | Schema migrations, immutable revisions, compact changeset transaction, lifecycle/revert, index outbox, canonical export/import, semantic hashing. |
| `openmemory-engine` | Space runtime facade, bounded layered recall/fusion, targeted write helpers, admission pause, canonical merge planner primitives. |
| `openmemory-daemon` | Product catalog, context/authorization resolver, bounded registry, review policy, merge jobs, recovery, backup coordination. |
| `openmemory-admin` | Space/read-set provenance, changeset/diff/review DTOs, merge preview/conflict/job DTOs, stable error codes. |
| `openmemory-mcp` | Optional authorized target/read mode, context status, provenance-bearing results; no arbitrary space access. |
| CLI | `space`, `changeset`, `review`, `history`, `revert`, `copy`, `merge preview/apply/recover`, backup and doctor commands. |
| Desktop | Active project/team context, provenance, review inbox, diff/history/editor, merge conflict UI, recovery status. |

Keep SQL and state machines in graph/daemon modules, not HTTP handlers. CLI,
MCP, and admin routes invoke the same typed services.

## API surface

Administrative endpoints, with pagination and capability discovery:

```text
GET    /admin/spaces
POST   /admin/spaces
GET    /admin/spaces/{id}
PATCH  /admin/spaces/{id}
GET    /admin/context
POST   /admin/context/preview

POST   /admin/changesets
GET    /admin/changesets?space=&state=&cursor=
GET    /admin/changesets/{id}
POST   /admin/changesets/{id}/approve
POST   /admin/changesets/{id}/reject
POST   /admin/changesets/{id}/revert

GET    /admin/memories/{logical_id}/history?space=
POST   /admin/memories/{logical_id}/edit
POST   /admin/memories/{logical_id}/delete
POST   /admin/memories/{logical_id}/restore
POST   /admin/spaces/{source}/copy/{logical_id}

GET    /admin/identity/candidates?merge_job=&state=&cursor=
GET    /admin/identity/candidates/{id}
POST   /admin/identity/candidates/{id}/decide
POST   /admin/identity/decisions/{id}/revise

POST   /admin/merges/preview
GET    /admin/merges/{job_id}
POST   /admin/merges/{job_id}/confirm
POST   /admin/merges/{job_id}/resolve
POST   /admin/merges/recover
```

Diff responses contain typed field changes resolved from revisions, lifecycle
changes, provenance, expected/current head, and conflict status. They do not
expose raw internal SQL rows.

Stable new errors include:

```text
space_not_found, space_closed, space_manifest_mismatch,
space_access_denied, team_context_required, read_set_too_large,
changeset_not_found, changeset_not_pending, changeset_stale,
changeset_cross_domain,
revision_not_found, hard_delete_confirmation_required,
identity_candidate_stale, identity_evidence_invalid,
identity_decision_moved, identity_revalidation_required,
merge_base_missing, merge_conflicts_unresolved, merge_target_moved,
merge_cross_filesystem, merge_recovery_required, index_repair_required
```

## Backup, restore, retention, and deletion

- Full-profile backup captures catalog generation, all space manifests, graph
  roots, lineage manifests, and pending changesets under one coordinated
  barrier/snapshot protocol.
- Space-only export includes owner/context metadata but restore always requires
  an explicit new/existing destination decision and authorization.
- Restore never overwrites an open root. It stages, verifies, and promotes using
  the same recovery primitive as merge.
- Semantic history retention is separate from current 14-day recall tombstone
  pruning. Applied history is retained by default; rejected proposal payloads
  may have a configurable shorter retention.
- Legal hard-delete enumerates affected current rows, immutable revisions,
  provenance, backups, and lineage. It requires explicit confirmation and
  leaves a content-free destruction receipt unless policy forbids even that.
- Access telemetry can be compacted independently without changing semantic
  root hashes.

## Implementation sequence

Each phase lands independently behind capability flags and has its own schema,
recovery, compatibility, and performance evidence. Do not combine this into one
large migration or pull request.

### Phase 0 — Contracts and permanent fixtures

1. Add validated IDs, owner/context/role/read-set types to `openmemory-core`.
2. Freeze fixture profiles at schema v1/v2, single/four domains, relation
   mirrors, tombstones, and large stores.
3. Add semantic canonical hashing over export types, excluding derived state.
4. Move POC invariants into production test names; keep the POC as design
   evidence until production gates supersede it.

Exit: no runtime behavior change; old fixtures open unchanged; hashes are
deterministic across export order and reopen.

### Phase 1 — Spaces and bounded read sets

1. Replace plan-14 `GraphRef` plumbing with `SpaceId`-based catalog entries.
2. Bind the existing profile root as user-global without moving data.
3. Implement manifest verification, workspace-to-project mapping, authorized
   context resolution, and bounded lazy registry.
4. Land layered recall with provenance and adaptive/bounded scheduling.
5. Make backup, domain migration, status, MCP fallback, and daemon paths
   explicitly space-targeted.

Exit: Project A/B and user/team fixtures have exact isolation; old clients with
no mappings remain global-only; 1,000+ catalog entries do not alter hot recall.

### Phase 2 — Compact observation changesets

1. Add changeset/request/event schema, space generation, observation revisions,
   and lazy legacy baseline creation.
2. Route every new observation add through an immediate changeset.
3. Add durable index outbox and reopen repair before enabling edit.
4. Add proposal/apply/reject, optimistic approval, idempotency, history, and
   semantic supersession.
5. Add revert, delete, restore, and retention policy.

Exit: crash tests prove canonical/history atomicity and index repair; p95 write
overhead meets the fixed-fixture budget; no existing recall result changes
except intentionally revised objects.

### Phase 3 — Manual audit and review product surface

1. Add admin/CLI/MCP typed APIs and stable errors.
2. Build Desktop review inbox, field diff, history, edit/supersede, delete,
   restore, reject, and revert flows.
3. Add reviewer policy and authorization-generation recheck.
4. Add redacted local events and audit export.

Exit: users can explain and modify every new semantic observation without raw
SQLite access; stale UI tabs cannot overwrite newer memory.

### Phase 4 — Team spaces

1. Define the actual team authority, signed membership source, offline expiry,
   and revocation behavior before enabling remote/shared use.
2. Add active-team context and role enforcement to every surface.
3. Require review for team writes by configurable policy.
4. Add membership-change races, revoked-offline client, confused-deputy, and
   backup ownership tests.

Exit: no principal can construct or retain a readable/writable team handle
outside current authority; audit records real principal identity.

### Phase 5 — Copy and merge readiness

1. Land observation cherry-pick and lineage manifests.
2. Land bounded, proof-prioritized identity candidates; trusted identifier
   assertions; versioned ontology compatibility; packet-hashed deterministic
   evidence; durable decisions/deferred abstentions; conflicting-proof review;
   team policy; reversal; and policy/evidence revalidation. Ship the human
   review path before enabling any model adapter.
3. Add source snapshot/extractor metadata and namespace canonicalizers with
   redirect/merge fixtures. A resolver-generation change must shadow-recompute
   packets and queue revalidation without silently rewriting decision history.
4. Add the asynchronous agent proposal interface behind an offline-eval gate;
   persist model/prompt/packet provenance, bound every field, and make failure
   keep concepts separate.
5. Add canonical relation IDs, source-verified directional relation proposals,
   separate relation changesets, mirror outbox/rebuild, and relation revision
   history.
6. Add entity lifecycle and administrative rename/re-home.
7. Extend canonical export/import and hashing to all semantic kinds.
8. Land the generic semantic planner contract: complete candidate coverage,
   revision-bound resolution receipts, one-to-one coalescence, source-qualified
   additions, immutable origin contributions, endpoint rewiring, relation
   provenance, source accounting, result prediction, and plan hashing.

Exit: a full graph can round-trip through canonical export/import with identical
semantic hash, revision lineage, and rebuilt derived state. The permanent
Codex/Homebrew, Axum/Actix, and Mathlib/Lean corpora all pass through the same
planner with no fixture-directed merge path.

### Phase 6 — Material merge and recovery

1. Port the validated generic planner and contribution model, then compose it
   with lineage-aware three-way revision conflict classes. Do not rebuild the
   planner in the daemon or UI.
2. Reuse/refactor existing domain-migration staging, sentinel, verification,
   and swap primitives rather than create a second filesystem protocol.
3. Add preview/confirm/resolve jobs, admission pause, registry close/reopen, disk
   preflight, backup retention, and `doctor` recovery.
4. Add subprocess aborts at every fsync/rename/catalog transition on macOS and
   Linux; add platform-specific promotion tests before Windows support.

Exit: exhaustive state tests and fixtures prove old-or-new recovery; source
hash never changes; stale target and unresolved conflicts cannot promote.

### Phase 7 — Rollout and cleanup

1. Run shadow changeset recording with audit UI hidden; compare projection and
   revision hashes.
2. Enable spaces, then review/edit, then team, then material merge as separate
   capabilities.
3. Keep rollback at the feature-routing level; never downgrade a migrated
   database with an older binary.
4. Remove POC only after equivalent production tests and benchmarks exist, or
   retain it permanently as a compact recovery reference.

## Test matrix

### Deterministic and property tests

- ID/path validation, owner/context uniqueness, read-set cap and order.
- No query or relation traversal outside the authorized read set.
- Score/provenance fusion stable under component completion order.
- Changeset transition table, idempotency, multi-op rollback, revert lineage.
- Same-domain multi-op commit and pre-write rejection of cross-domain
  changesets; independently reported non-atomic batch behavior.
- Random operation sequences agree with an in-memory revision model.
- Merge classification deterministic; conflict classification symmetric where
  semantics permit; independent changes commute.
- Identity pair keys and complete analysis symmetric; shared labels/aliases and
  claimed IDs never prove identity; controlled kind-policy tables exhaustive;
  verified-ID/lineage proof cannot be starved by homonym buckets; truncation is
  explicit and deterministic under insertion-order changes.
- Agent outputs packet-hash-bound, bounded, and evidence-cited; hard
  contradictions and deterministic decisions cannot be agent-overridden;
  conflicting trusted proofs bypass the agent; relation suggestions require
  source-verified directional evidence.
- Persisted decisions/deferred abstentions suppress only an identical packet;
  entity revision, source, canonicalizer, ontology, or policy changes reopen
  review; reversal retains the complete event chain.
- Candidate coverage is exact: missing, extra, duplicate, stale, and
  undetermined resolutions fail closed; policy-only `same` is impossible.
- Every source entity is accounted once; generated imported IDs cannot
  overwrite target IDs; many-to-one coalescence requires prior normalization.
- Endpoint rewiring leaves no dangling relation; target and source assertions
  retain provenance; contribution union is invariant under input ordering.
- Plan and result hashes detect tampering and moved inputs. Materialization is
  deterministic and leaves both input snapshots byte-identical.
- Semantic hash invariant under row/export ordering and derived-state rebuild.

### Concurrency and crash

- Two daemon/process reviewers race one proposal and competing proposals.
- Writes race proposal approval, revert, delete, and membership revocation.
- Recall races canonical commit, index outbox drain, registry eviction, backup,
  and target admission pause.
- Abort after every changeset/outbox commit boundary.
- Two reviewers race one identity proposal; abort after identity-decision
  insert, proposal-state update, and commit; reopen is exactly old or new.
- Policy/source resolver generation changes race proposal review; stale packet
  hashes cannot apply or suppress the replacement packet.
- Abort after staging build, verification, intent fsync, target backup rename,
  promotion, promoted reopen, and catalog update.
- Recovery is idempotent across repeated process deaths.

### Integration and compatibility

- User/team global/project combinations, identical names/content, one/four
  engine domains, keyword/vector/hybrid recall, relations, consolidation,
  pruning, export/import, and backup/restore.
- Every schema fixture from two released minor versions opens and migrates.
- Old MCP requests remain valid and cannot acquire broader scope.
- Daemon and direct fallback resolve identical contexts or fail typed.
- Full/rejected/pending changeset retention and legal hard-delete fixtures.
- Versioned real-world identity corpora include homonyms, renames, historical
  classifications, namesakes, same-kind polysemy, multilingual aliases,
  external-ID redirects/merges, conflicting sources, and relation direction.
- Permanent full-merge corpora cover unrelated products with shared ownership,
  independent same-domain frameworks, and a direct mathematics/toolchain
  dependency. All use the production planner interface and golden changesets.

### Security

- Arbitrary `source`, owner, project, team, and space IDs cannot escalate.
- Membership expiry/revocation invalidates cache and outstanding context before
  mutation.
- Symlink/path traversal cannot select another root or staging directory.
- Merge confirmation is bound to principal, target, plan hash, and expiry.
- Logs/events/backups redact content and local paths according to policy.

## Performance gates

Retain the validated relative gates and add production-scale fixtures:

- Two-space warm recall p95 <= `1.25x` slower component + 5 ms.
- Fusion of 512 candidates to 128 < 2 ms p95.
- Recall time independent of 1,000/10,000 registered closed spaces.
- Cached project-only orchestration overhead < 1 ms.
- Compact audited small-write p95 overhead <= 25% versus the same canonical
  transaction without history, and absolute overhead < 2 ms on the reference
  machine.
- Compact audit durable storage <= 1.75x canonical control for small revisions;
  publish bytes per revision for 1 KB, 16 KB, and 256 KB content.
- Proposal approval does not hold writer/rebuild locks during embedding.
- Three-way planning of 100k semantic objects < 250 ms p95 and bounded memory;
  switch to streaming sorted iterators if the full-map implementation misses.
- Until the 100k production export exists, publish repeated 100/1k/10k
  contribution-preserving plan-plus-materialize measurements. The validated
  prototype is near-linear through 10k entities and relations; this is design
  evidence, not a relaxation of the 100k production gate.
- Material merge publishes throughput (objects/s and bytes/s), pause duration,
  disk amplification, verification time, and recovery time. No fixed target
  until 100k and 1M fixtures establish the envelope.
- Registry handles/open connections remain bounded and idle CPU is zero.
- Exact identity-candidate lookup remains indexed rather than O(A*B), bounded
  under adversarial homonym/untrusted-ID fanout, and proof-prioritized;
  deterministic evidence analysis p95 < 100 us for bounded packets; durable
  packet-bound proposal and review transactions p95 < 2 ms on the reference
  machine.
- Agent calls are absent from ordinary recall/capture latency and run
  asynchronously. Publish candidate coverage, abstention, human-review rate,
  false-same/false-different rate, cost, and latency by entity kind on a
  versioned representative corpus.

Run Criterion/CodSpeed for algorithmic paths and the existing release stress
harness style for latency percentiles, concurrent recall/write, durability lag,
and lost-write checks. Measurements run on macOS and Linux with fixed toolchain,
fixture seed, feature set, domain count, and cold/warm state. A single noisy run
does not pass or fail a relative gate; interleave controls and require repeated
CI/reference runs.

## Release blockers

- Any cross-space recall, traversal, cache, backup, or restore leak.
- Any automatic identity merge based only on label, embedding, model
  confidence, or wall-clock order; any agent path that writes canonical graph
  state or authorizes its own team decision.
- Any unique-ID proof without source verification and a compatible namespace
  resolver generation; any stale proposal/decision suppression based only on
  entity revisions rather than the complete policy/evidence packet.
- Any relation edge created from generic overlap or agent/user assertion
  without source-verified directional evidence and a separate changeset.
- Enabling a model adapter without a representative offline/adversarial corpus,
  explicit abstention behavior, prompt-injection tests, and a human review
  study measuring time and mistaken approvals.
- Authorization based on caller-controlled provenance.
- Applied canonical mutation without durable history, or history without its
  canonical mutation.
- Recall returning a revision through stale index content without a correct
  degraded path.
- Lost update, non-idempotent retry, or partial multi-object changeset.
- Relation edit/merge before canonical relation IDs and mirror repair exist.
- Material promotion without common base/conflict handling, semantic/index
  verification, same-filesystem preflight, or tested recovery.
- Unbounded catalog fan-out, open handles, job memory, or disk staging.
- Migration that makes startup O(corpus) or strands an older supported client
  without a typed schema-too-new error.

## Decisions required before the corresponding phase

1. Team membership authority, offline expiry, and default reviewer policy
   (before Phase 4).
2. Applied/rejected history retention and legal hard-delete obligations
   (before Phase 2 deletion ships).
3. Whether agents may propose team promotion autonomously or only humans may
   initiate it (before team write API).
4. Backup encryption/ownership behavior for team spaces (before team backup).
5. Recovery backup retention quota and cleanup UX (before Phase 6).
6. Windows filesystem promotion contract (before advertising Windows merge).

These policy decisions do not block Phases 0–3. They must not be guessed inside
storage code.

## Production acceptance criteria

The complete feature is ready only when:

- Every semantic request can state exactly which authorized spaces it read and
  which single space it wrote.
- Project A/B, personal/team, single/four-domain, and keyword/vector fixtures
  show exact isolation and explainable overlay.
- New semantic writes have compact immutable history; proposal, approval,
  rejection, edit, delete, restore, and revert survive concurrency and crashes.
- Users can inspect and modify memory through typed APIs/UI without direct SQL.
- Copy and merge preserve logical identity and provenance; divergent edits are
  surfaced, never silently won.
- Cross-space identity decisions are explainable from a packet-bound audit;
  claimed IDs/relations never become proof; conflicting proof is reviewed; and
  source, canonicalizer, ontology, or policy changes reopen stale decisions.
- Every tested merge crash recovers to verified old or new target and retains a
  usable backup; source remains byte/semantic-hash unchanged.
- Backup/restore includes all spaces, catalog, memberships, pending changesets,
  revisions, and lineage without broadening authorization.
- Fixed release benchmarks pass repeatedly on macOS and Linux.
- Workspace default/all/no-default tests, Clippy, rustdoc, formatting, deny,
  migration fixtures, stress tests, and Desktop contract/E2E gates pass.
