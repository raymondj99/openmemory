# Contract and Invariants

## Vocabulary

- **Profile**: existing CLI/config namespace under `data/<profile>`.
- **Memory space**: user-visible semantic silo backed by one complete
  `DomainStore`.
- **Engine domain**: internal hash partition inside a space; never an
  authorization scope.
- **Context**: global or one stable `ProjectId`; paths activate a project but
  are not its identity.
- **Read set**: immutable, ordered, authorized spaces consulted by a request.
- **Write target**: the one authorized space a request may mutate.
- **Changeset**: durable intent, decision, immutable revisions, and compact
  result metadata for one semantic mutation.
- **Overlay**: temporary multi-space read with no mutation.
- **Cherry-pick**: provenance-bearing copy into a destination changeset.
- **Material merge**: directional source-to-target replacement through a
  verified staged root.
- **Origin contribution**: immutable source material retained after
  coalescence.

## Required types

Add private, validated types in `openmemory-core::space`. IDs use canonical
lowercase hyphenated UUIDv7. This is stricter than the prototype's portable
component grammar and satisfies its case-insensitive/Windows-path findings.
Principal/team external IDs are bounded opaque strings and are never path
components.

```rust
pub struct SpaceId(/* UUIDv7 */);
pub struct ProjectId(/* UUIDv7 */);
pub struct ChangeSetId(/* UUIDv7 */);
pub struct RevisionId(/* UUIDv7 */);
pub struct MergeJobId(/* UUIDv7 */);
pub struct PrincipalId(/* 1..=128 bytes */);
pub struct TeamId(/* 1..=128 bytes */);
pub struct ProfileName(/* canonical existing profile grammar */);
pub struct WorkspacePathKey(/* platform + normalizer version + reversible path */);
pub struct AuthoritySnapshot(/* version + canonical generation digest */);

pub enum SpaceOwner { User(PrincipalId), Team(TeamId) }
pub enum SpaceContext { Global, Project(ProjectId) }
pub enum SpaceRole { Reader, Contributor, Reviewer, Maintainer }
pub enum ActorKind { Human, Agent, System }
pub enum SelectionSource { Explicit, WorkspaceMapping, ProductDefault }

pub struct SelectionProvenance {
    project: SelectionSource,
    team: SelectionSource,
    read_scope: SelectionSource,
    write_target: SelectionSource,
}

pub struct SpaceRef {
    id: SpaceId,
    owner: SpaceOwner,
    context: SpaceContext,
}

pub struct SpaceGrant {
    space: SpaceRef,
    role: SpaceRole,
    authority: AuthoritySnapshot,
}

pub struct ReadSet(/* 1..=4 unique grants in precedence order */);

pub struct MemoryContext {
    principal: PrincipalId,
    actor_kind: ActorKind,
    profile: ProfileName,
    project: Option<ProjectId>,
    active_team: Option<TeamId>,
    read_set: ReadSet,
    default_write: SpaceId,
    authority: AuthoritySnapshot,
    selection: SelectionProvenance,
}
```

Expose read-only accessors and validated constructors. Implement canonical
`Display`, `FromStr`, and serde string encodings. Reject
noncanonical case, separators, controls, dot components, Windows device names
where a type can become a component, leading/trailing dots, oversize input,
unknown enum values, and non-finite numbers.

`AuthoritySnapshot` is a versioned, canonical digest of the installation
credential, catalog, team, and applicable membership generations. Do not
collapse unrelated generations into an increment that can alias after restore.
`ReadSet::new` rejects empty, duplicate, unauthorized, and more than four
entries while preserving explicit order.

## Representation boundaries and explicit intent

- Semantic content, wire JSON, IDs, enum tags, reasons, and sources are
  validated UTF-8 with documented byte/depth limits. Invalid input returns a
  typed caller error; it is never replaced or normalized lossily.
- Managed roots are derived from fixed ASCII names and canonical IDs. An OS
  path remains `Path`/`OsStr` until the workspace boundary. This release
  accepts only workspace paths with a reversible UTF-8 representation;
  unsupported platform-native encodings return
  `workspace_path_encoding_unsupported` before catalog mutation. A display
  rendering is never an identity, authorization, cache-key, or hash input.
- Canonical hashes consume validated semantic types, never display strings,
  filesystem iteration order, platform path separators, or lossy conversions.
- `SelectionProvenance` survives context normalization for each independent
  choice. An explicit space mode,
  project/global restriction, team choice, path, or security option cannot be
  silently replaced by workspace inference or a product default. Invalid
  explicit intent fails rather than falling through.
- Internal errors retain source error kind, operation, and safe structured
  context through service translation. Wire surfaces expose the stable
  redacted envelope without destroying the trace-linked internal cause.

## Context precedence and writes

Contextual recall uses present, authorized layers in this exact order:

1. personal-project;
2. active-team-project;
3. personal-global;
4. active-team-global.

An unmapped workspace resolves only global layers. Explicit project-only,
global-only, or overlay modes may omit layers but may not add unauthorized
ones.

| Selection | Write target |
|---|---|
| Default with mapped project | personal-project |
| Default without project | personal-global |
| `personal` | personal-project if mapped, otherwise personal-global |
| `team` | active team-project when present, otherwise active team-global |
| Admin concrete ID | that space after current-role reauthorization |

MCP accepts `default|personal|team`, never an arbitrary space ID. Human admin
routes may name a space because they authenticate and reauthorize it.
Project mappings become active only after their personal-project space is
verified, so a mapped default never silently falls back to global.

## Local authority and revocation

- First startup creates one random installation principal.
- Maintainers create local teams and grant bounded roles to known opaque
  principals. No remote authority or synchronization is claimed.
- Membership, team state, catalog state, credential rotation, profile switch,
  and promotion increment their owning generation and invalidate affected
  contexts.
- Daemon authorization returns an RAII `AuthorizationLease`. The coordinator
  holds it through recall or graph commit. Revocation takes the corresponding
  exclusive authority gate, increments generation, then invalidates cached
  contexts; after revocation returns, no old lease can begin or publish work.
- Long jobs do not retain authority indefinitely. They reauthorize at every
  human transition and immediately before publication; revocation cancels or
  leaves a non-published staged result.
- Agents may read authorized spaces and apply ordinary personal writes. Team
  writes are proposals. Agents cannot review, establish identity, manage
  membership, confirm promotion, or perform hard destruction.
- Team review requires a human Reviewer/Maintainer. Membership, destructive
  lifecycle, and material promotion require Maintainer.

All team-capable writes flow through the daemon. Legacy direct mode is fixed to
personal-global and cannot obtain team authority.

## Semantic mutation contract

Every new semantic mutation records:

- concrete space and home domain;
- actor principal/kind, reason, source, and current authority snapshot;
- caller idempotency key plus canonical request hash including submit mode,
  scoped to the resolved space and home domain;
- expected row version/revision/lifecycle;
- one checked changeset transition;
- immutable revisions and compact typed events;
- one incremented semantic generation; and
- durable index/mirror outbox work.

An immediate personal write applies one changeset in one SQLite transaction.
A proposal writes no canonical row or derived work. Approval reauthorizes and
rechecks payload, expected heads, lifecycle, and state before one atomic apply.
Same idempotency key plus same request returns the original receipt; different
canonical request or submit mode in that routed domain is a conflict. Surfaces
generate UUID-backed globally unique keys; non-atomic batches derive unique
child keys and never intentionally reuse one across domains.

Interactive changesets contain 1–256 operations and must route to one domain
before proposal insertion. Cross-domain bulk work is explicitly non-atomic and
returns one independent child receipt per operation. Material merge uses a
separate internal, merge-only streamed import path; it never raises the
interactive bound.

## Identity contract

### Entity identity is an immutable ID, not a label

Every entity has an immutable `EntityId` assigned at creation. Names and
aliases are versioned, provenance-bearing assertions *about* an entity;
they are never its identity. Two entities of the same type may share a
name. A name resolves to a bounded candidate set through a derived
name/alias directory, never directly to one entity.

This is a correction, and the defect it fixes is demonstrated rather
than hypothetical. At the time of writing, `schema.rs` declares
`idx_entities_name_type` UNIQUE over `(name, entity_type)`. Writing two
unrelated people under one name — a payments engineer in Lisbon and a
pediatric surgeon — produced **one entity row carrying both sets of
observations**. The store silently merged two different people because
they shared a label, in direct contradiction of the invariant stated
below.

That is unrecoverable corruption, not a ranking nuisance: once the rows
are merged, no later merge receipt, revision, or audit trail can
separate them, because the fact that they were ever distinct was never
recorded.

The correctness argument stands on its own. The cost argument is
secondary, has now been measured twice under wider conditions, and both
earlier figures are withdrawn. Details and limits are in
`11-entity-identity-migration.md` §8; the two facts that matter here:

- **Ambiguity-awareness has a floor cost.** On a population with
  effectively no homonyms, resolving a name to a candidate set costs
  **4.22 µs against 3.07 µs** for today's unique-index lookup — +1.15 µs,
  +37%, non-overlapping 95% intervals. A UNIQUE index may stop at the
  first match; a candidate set may not, because "is there a second one?"
  is the question being asked. No index layout removes this.
- **The covering directory index is what makes the bound real.** A
  bounded, *deterministic* candidate set taken from `entities` sorts the
  whole homonym group (`USE TEMP B-TREE FOR ORDER BY`), because
  `idx_entities_name_type` does not order by `id`. At a hottest-name
  fan-out of 20,438 that is **3,146 µs against 8.1 µs** through the
  directory, for the identical result set. `LIMIT` bounds the result, not
  the work.

Two earlier revisions of this document reported figures that did not
survive re-measurement: an extra 7 µs (from a benchmark that fetched a
column the covering index already held), and **3.89 µs against 4.64 µs**
showing the directory slightly faster (from a comparison of a
non-covering direct read against a covering directory read — a
difference in projection, not in identity model). Both are withdrawn.

Required behaviour:

- duplicate `(name, entity_type)` pairs are permitted and stay distinct;
- an ambiguous legacy name lookup returns a typed candidate set, never
  an arbitrary row chosen by SQL order;
- unambiguous legacy lookups keep their current output;
- entity creation and candidate indexing stay bounded at 100,000
  homonyms.

The migration that delivers this is specified and prototyped in
`11-entity-identity-migration.md`. Two of its findings qualify the text
above. First, `entities.id` already is the immutable `EntityId` — a
UUIDv7 primary key that nothing rewrites — so no row is rekeyed. Second,
the defect is not only within a type: the real store at
`~/.openmemory/data/default` already holds `ProjectAlpha` as both a
`concept` and a `project`, and `get_entity(name)` returns whichever row
SQLite reaches first. One of the two is unreachable by name today.

### Physical routing follows the immutable ID

`home_domain` is **recorded** at creation and is thereafter immutable.
For an entity created after the migration it is derived from `EntityId`.
For an entity that predates the migration it is the domain the entity is
already in, which is what `domain_for(name)` chose.

That distinction is not pedantry. Deriving `home_domain` from the id for
legacy rows would declare each of them to live in a file it is not in
whenever the two hashes disagree, which for `n` domains is a `1 - 1/n`
share of the store — measured at more than 150 of 200 rows at 16 domains
(`experiments/entity-identity`, `phase_a_records_this_files_own_domain_not_the_id_hash`).
Recording the observed domain moves nothing.

Observations route through their owning entity; canonical relations
route through the immutable source-entity home.

A rename is therefore one domain-local audited mutation and never moves
an entity, its observations, or its relations. The `entity_rehome`
staged job described in `04-changesets-and-manual-editing.md` exists
only to repair name-hash routing and is removed. Repartitioning remains
a separately staged whole-space domain-count migration, not an ordinary
semantic edit.

### Establishing identity

Equal labels, including two concepts named `cerpheus`, create candidates only.
Identity may be established only by:

1. exact revision-bound lineage;
2. equal source-verified canonical values in a configured unique namespace
   with compatible controlled kinds and current resolver generation; or
3. a current human decision bound to both revisions, complete evidence packet,
   policy/ontology/resolver generations, and proposed effect.

Conflicting trusted identifiers require human revalidation and cannot be
overridden by an agent. Missing evidence, timeout, abstention, or malformed
proposal leaves concepts separate. `Undetermined` is a deferral, not a final
same/different receipt.

## Merge contract

- Direction is explicit; source is immutable and target is replaceable.
- Planning consumes verified immutable snapshots and current review receipts.
- Each candidate has exactly one current resolution.
- Each source entity has exactly one disposition: coalesce with one target or
  add under a deterministic source-qualified ID.
- One target cannot absorb multiple source entities in one plan.
- Coalescence preserves the target projection and appends source
  contributions; it never uses last-write-wins.
- Every relation assertion retains provenance and maps both endpoints through
  the disposition table. Missing endpoints block the plan.
- A verified lineage base enables typed three-way classification; without one,
  use conservative union and explicit conflict.
- Plan input hashes, receipt hashes, expected target generation, complete
  accounting, predicted result hash, action-stream hash, and plan hash are
  canonical and independently reverified.
- Materialization streams into a new root, rebuilds derived state, verifies,
  pauses target admission, rechecks target state, and promotes under an fsynced
  intent. It never mutates live target files in place.

## Non-negotiable invariants

1. Graph APIs receive one already authorized, space-bound handle.
2. Every object/result/history/diff/identity/merge value carries space
   provenance at the service boundary.
3. Catalog size cannot affect recall work.
4. Traversal and spreading activation never cross roots.
5. Proposed/rejected/conflicted/stale changes alter neither canonical nor
   derived state.
6. Canonical mutation, revision, event, generation, and outbox enqueue are one
   SQLite transaction.
7. Cross-SQLite atomicity is never implied.
8. Stale revision, authority, snapshot, receipt, plan, or confirmation fails
   closed.
9. Telemetry is excluded from semantic revisions and hashes.
10. Names, paths, repo URLs, caller source, embeddings, and agents are neither
    identity nor authorization proof.
11. Stale indexes are repaired or reported; they are never served as current.
12. Derived state is rebuilt, never semantically merged.
13. Recovery is idempotent and yields a verified old or verified new target.
14. Source snapshot hash is unchanged after all outcomes.
15. No model/network call is on recall, ordinary remember, approval, snapshot,
    or promotion critical paths.
16. Ambient config, environment, current directory, and platform probes are
    resolved before the operation; hot storage/executor/planner code cannot
    reinterpret them.
17. A specialized path may claim eligibility only conservatively. Ineligible
    or unavailable optional acceleration uses the general path; required user
    operations fail loudly when no correct implementation is available.
18. No lossy representation participates in identity, authorization, routing,
    cache keys, persistence keys, canonical hashes, or provenance.
19. A cache entry is visible only after complete publication, and its key plus
    validity generations cover every input that can shape its result.
20. Every acquired lease, permit, lock, file, staged artifact, child process,
    and stream has one named owner and an idempotent terminal cleanup path.

## Stable failure envelope

Internal errors are typed and exhaustive. Admin exposes stable `snake_case`
codes with a safe message, retryability, optional repair hint, trace ID, and
bounded redacted details. Required families include:

```text
space_not_found, space_closed, space_busy, space_manifest_mismatch, space_access_denied
read_set_too_large, context_expired, authority_generation_moved
changeset_not_pending, changeset_stale, changeset_cross_domain
idempotency_conflict, object_retired, index_repair_required
identity_candidate_stale, identity_evidence_invalid, identity_revalidation_required
merge_conflicts_unresolved, merge_target_moved, merge_disk_budget_exceeded
merge_cross_filesystem, merge_recovery_required
execution_saturated, execution_budget_exceeded, execution_deadline
execution_stuck, shutdown_incomplete
operation_outcome_unknown
workspace_path_encoding_unsupported, platform_capability_unavailable
```

MCP maps caller errors to tool/invalid-params errors and internal durability
failures to redacted trace-bearing errors. Never expose tokens, content, or
absolute paths by default.

## Non-goals

Hosted sync, distributed consensus, remote membership, arbitrary cross-space
edges, automatic fuzzy identity, generic byte-level version control,
interactive cross-domain atomicity, raw Desktop/CLI SQL editing, and Windows
material promotion without platform proof.
