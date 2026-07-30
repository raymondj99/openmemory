# Changesets and Manual Editing

## Model and limits

Canonical rows remain the fast current projection. Immutable revisions and
compact changeset events explain every new semantic mutation; this is not a
generic event-sourcing framework.

```rust
pub enum ObjectKind { Entity, Observation, Relation }
pub struct ObjectRef { kind: ObjectKind, logical_id: LogicalId }

pub enum ChangeOperation {
    Remember(RememberChange),
    SupersedeObservation(SupersedeObservation),
    SetObservationTier(SetObservationTier),
    UpdateEntity(UpdateEntity),
    UpdateRelation(UpdateRelation),
    Retire(ObjectMutation),
    Restore(ObjectMutation),
    RevertToRevision(RevertToRevision),
    CherryPick(CherryPickChange),
}

pub struct ChangeSetDraft {
    idempotency_key: IdempotencyKey,
    space_id: SpaceId,
    actor: ActorRef,
    authority: AuthoritySnapshot,
    reason: ChangeReason,
    source: ChangeSource,
    operations: ChangeOperations,
}

pub enum SubmitMode { ApplyImmediately, Propose }
pub enum ChangeSetState { Proposed, Applied, Rejected, Conflicted, Reverted }
```

`IdempotencyKey`, `ChangeReason`, `ChangeSource`, and `ChangeOperations` are
small concrete validated types, not a generic collection framework. Expose
read-only accessors and constructors that validate and canonicalize before
admission or transaction:

- idempotency key: 1–128 bytes;
- reason/source: 1–1,024 bytes;
- operations: 1–256, one engine domain;
- requested payload: configured 1 MiB, hard 4 MiB;
- existing per-field observation/entity limits;
- no unknown fields, duplicate operations on one logical head, invalid enum,
  NaN/Infinity, or unbounded map/depth.

Persist typed versioned payloads. Canonical request hashing explicitly binds
schema version, submit mode, space, resolved home domain, actor, authority,
reason, source, operation order, expected heads/versions, and every semantic
field. JSON bytes are not the hash input.

Material merge does not construct an oversized `ChangeSetDraft`. Its private
streaming import accepts only planner-issued merge put/delete/contribution
actions, applies bounded SQL batches, and emits generated merge changesets and
events without weakening interactive limits.

## State transitions

```text
new + immediate --------------------------> applied
new + review ----------> proposed --------> applied
                                  \-------> rejected
                                   \------> conflicted
applied + forward inverse changeset ------> reverted
```

Every transition supplies expected state and `state_version`; SQL must affect
exactly one row. A stale approval conflicts the complete proposal and mutates
no canonical state. Editing a conflict creates a new idempotency key against
current heads. Revert is a new forward changeset; history is never deleted or
silently repointed.

## Routing

- New entity: `home_domain` derived from its immutable `EntityId` at
  creation, then immutable for the entity's lifetime.
- Observation: the home domain of its owning entity.
- Existing object: its recorded home domain; a name change never
  reroutes anything.
- Canonical relation: the immutable source entity's home domain.
- Resolve all homes before inserting a proposal. More than one domain returns
  `changeset_cross_domain`.

A rename is one domain-local audited mutation. The previously specified
staged whole-space `entity_rehome` job is **removed**: it existed only to
repair routing that had been keyed on a mutable label, and routing is now
keyed on the immutable `EntityId` (see `01-contract-and-invariants.md`).
Repartitioning remains a separately staged whole-space domain-count
migration.

An explicitly non-atomic batch assigns stable child IDs, groups children by
domain, executes domain groups through bounded write-class admission, and
returns every child receipt in caller order. Failure of one group does not
erase successful siblings and is never described as atomic.

## Immediate apply

Compatibility remember/batch methods build trusted typed drafts and call the
same transaction helper.

1. Validate, route, canonicalize, hash, and precompute embeddings.
2. Daemon holds a current `AuthorizationLease`; graph verifies the bound
   `SpaceId` and supplied authority digest.
3. Acquire rebuild barrier; begin `IMMEDIATE`.
4. Resolve `(space_id, idempotency_key)`:
   - same canonical request: return existing state/receipt;
   - different request or submit mode: conflict.
5. Recheck expected heads, row versions, lifecycle, and any explicit
   whole-domain generation precondition.
6. Insert changeset/request records and any lazy legacy baselines.
7. Apply transaction-local mutations; insert revisions, events,
   contributions, and outbox rows.
8. Increment semantic generation once; mark applied; commit once.
9. Drain index work under the barrier and return explicit readiness.

No success receipt is published until the canonical transaction is durable.
Post-commit index failure returns an applied-but-repair-required receipt; it
does not pretend the mutation rolled back.

For context-engine ingestion, one shard maps to one home domain. Its canonical
write and journal checkpoint remain one domain transaction; cross-domain work
is represented by separate shard/child receipts.

## Proposal, review, and races

Proposal records request, base generation, expected heads/lifecycle, actor,
authority, reason/source, and canonical hash. It creates no revision, event,
semantic generation, embedding, outbox, or index mutation.

Approval:

1. acquire a current human Reviewer/Maintainer authorization lease; reject
   agents and prohibited self-approval;
2. load immutable proposal payload and precompute embeddings outside locks;
3. begin the graph transaction and require exact `proposed` state/version;
4. recheck request hash, heads, lifecycle, any explicit whole-domain
   generation precondition, and supplied authority;
5. apply everything or transition the proposal to `conflicted` with no
   canonical mutation.

Concurrent approvals yield one commit and the same terminal receipt. Competing
proposals against one head yield at most one apply. Revocation takes the
exclusive authority gate and cannot return while an old authorization lease is
still publishing.

Rejection records reviewer, authority, bounded reason, and time without
canonical/derived work. Default rejected payload retention is 30 days, after
which a maintenance task retains only canonical hash and safe metadata.
Applied revisions are retained by default.

## Revision semantics

Any recall-relevant change creates a new immutable revision, advances the
logical object's head, and increments its row version. Projection update uses
the expected prior head/version and requires exactly one affected row.

- Observation semantic fields: content, title, summary, concepts, source files
  and kind, confidence, importance, tier, validity, and semantic provenance.
- Entity semantic fields: name, controlled type, confidence, aliases, and
  verified/claimed identifiers.
- Relation semantic fields: endpoints, type, weight, validity, source, and
  directional evidence.
- Access counts and ranking telemetry are not semantic revisions.
- Endpoint changes retire the old relation and add a new relation.

Lazy migration creates a synthetic baseline plus requested revision in the
same transaction. Background backfill uses the identical canonical builder.

## Lifecycle

Typed lifecycle is `active|retired|destroyed`; only active objects participate
in default recall/traversal.

- `retire`: audited, reversible, and enqueues index removal.
- `restore`: requires the expected retired head and enqueues index insertion.
- `revert`: copies an earlier semantic value into a new head revision.
- `destroy`: irreversible Maintainer operation after a preview enumerates
  canonical rows, revisions, contributions, relation history, and backup
  impact. It requires a short-lived actor/scope/hash-bound confirmation and
  leaves a content-free destruction receipt when policy requires.

Preserve existing MCP forget tool names, inputs, successful “absent from normal
recall” behavior, and response compatibility, but route them through audited
retirement once v3/v4 migration is ready. They never expose hard destruction.
Irreversible destroy exists only on human admin/CLI with confirmation.

## Diff and query contract

Compute typed diffs from revisions/events, never stored before/after object
blobs. A diff includes space, object, from/to revisions, lifecycle, sorted field
changes, provenance, expected/current head, and staleness.

- Set diffs are sorted and duplicate-free.
- Large content returns hash/length and a bounded preview; full content uses a
  separately authorized detail route.
- List pagination is keyset `(created_at, id)`, never offset.
- Space authorization constrains SQL before filtering.
- Logical IDs are never assumed globally unique.
- Actor identity comes from authentication, not caller `source`.
- Logs/SSE contain IDs, counts, states, redacted errors—not semantic payloads.

Required views: proposal versus current head, two revisions, applied
changeset, merge before/predicted/actual, and base/source/target conflict.

## Manual flows

- Edit: client supplies expected head/version; personal policy applies, team
  policy proposes; stale tabs receive current head and a fresh diff base.
- Entity rename: always a same-domain audited revision. Names are
  assertions, not identity, so a rename never triggers a re-home and
  never requires a uniqueness check against other entities.
- Relation edit: disabled until canonical-ID/mirror backfill is ready; edit the
  canonical source-home relation and enqueue idempotent mirror work.
- Cherry-pick: destination changeset retains exact source space/logical/revision
  provenance.
- Bulk review: grouped domain calls with independent results; never one
  space-wide transaction.

## Acceptance

- Proposals are absent from recall/indexes until approval.
- Forced transaction failure leaves canonical, revisions, events, generation,
  and outbox all present or all absent.
- Same idempotency retry is byte-equivalent across reopen; changed mode/content
  conflicts.
- Stale edit/review and zero-row optimistic transition cannot overwrite.
- Edit → retire → restore → revert has coherent immutable lineage.
- Legacy baseline is lazy; future graph/request schema is refused.
- Persistent index failure is explicit before recall success.
- Relation edit cannot run before mirror readiness.
- Diff and child-receipt order are deterministic across completion order and
  reopen.
