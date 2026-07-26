# Changesets and Manual Editing

## Goal

Make every new semantic memory inspectable and correctable without turning the
graph into an event-sourced framework. Canonical rows remain the fast current
projection; immutable revisions plus compact changeset events explain how the
projection changed.

## Public model

```rust
pub enum ObjectKind {
    Entity,
    Observation,
    Relation,
}

pub struct ObjectRef {
    pub kind: ObjectKind,
    pub logical_id: String,
}

pub enum ChangeOperation {
    Remember(RememberChange),
    SupersedeObservation(SupersedeObservation),
    SetObservationTier(SetObservationTier),
    Retire(ObjectMutation),
    Restore(ObjectMutation),
    RevertToRevision(RevertToRevision),
    UpdateEntity(UpdateEntity),
    UpdateRelation(UpdateRelation),
    CherryPick(CherryPickChange),
    MergeContribution(MergeContributionChange),
}

pub struct ChangeSetDraft {
    pub idempotency_key: String,
    pub space_id: SpaceId,
    pub actor_principal: PrincipalId,
    pub actor_kind: ActorKind,
    pub authorization_generation: u64,
    pub reason: String,
    pub source: String,
    pub operations: Vec<ChangeOperation>,
}

pub enum SubmitMode {
    ApplyImmediately,
    Propose,
}

pub enum ChangeSetState {
    Proposed,
    Applied,
    Rejected,
    Conflicted,
    Reverted,
}
```

All strings and vectors have hard bounds validated before any transaction:

- Idempotency key: 1–128 bytes.
- Reason/source: 1–1,024 bytes each.
- Operations: 1–256, and all must route to one domain.
- One requested payload: at most configured 1 MiB and hard max 4 MiB.
- Entity name/title/summary/identifiers/aliases use existing or explicit
  per-field bounds; content follows the observation limit.

Payloads use versioned typed serde structs. Persisted request/event JSON has no
arbitrary maps except explicitly sorted key/value identifier lists. Deserialize
with unknown-field rejection for mutation inputs and length checks after parse.

## State machine

```text
create + immediate policy ───────────────> applied
create + review policy ──> proposed ──┬──> applied
                                      ├──> rejected
                                      └──> conflicted
applied + forward inverse changeset ─────> reverted
```

There is no mutable draft row. Clients construct drafts locally and submit
them. A stale approval sets the proposal to `conflicted` with a compact reason;
it does not partially apply. Editing a conflicted proposal creates a new
proposal/idempotency key against current heads.

Revert is a new forward applied changeset that selects prior revision content
or lifecycle. The original changeset's state becomes `reverted` only after the
new inverse changeset commits; history is not deleted.

## Domain routing

- Add entity/observation: route by destination entity name using the current
  `DomainStore::domain_for` hash.
- Existing entity/observation/relation: locate canonical object and use its
  home domain. Do not route an entity rename by the new name.
- Relation canonical history belongs to the source entity's home domain.
- Before proposal insertion, compute every operation's home domain. More than
  one unique domain returns `ChangeSetCrossDomain`.
- Entity rename that changes the name hash is an administrative re-home job,
  not a normal changeset. It uses staged whole-space rebuild/promotion.

## Immediate write algorithm

The compatibility `MemoryStore::remember` and `remember_batch` methods remain,
but build trusted `ChangeSetDraft`s and call the same transaction helper.

1. Validate draft and canonicalize request encoding.
2. Hash the canonical request; precompute embeddings for desired observation
   revisions outside graph locks.
3. Resolve one domain and take its rebuild write barrier.
4. Begin an immediate SQLite transaction.
5. Query `(space_id, idempotency_key)`:
   - Same request hash and terminal receipt: return it without mutation.
   - Same key, different hash: `IdempotencyConflict`.
   - Same key still proposed: return proposal receipt; do not apply implicitly.
6. Recheck expected row/revision/lifecycle values and insert the changeset.
7. Create lazy baseline revisions for legacy objects as needed.
8. Apply canonical operations through transaction-local helpers, insert new
   immutable revisions and compact events, and enqueue derived work.
9. Increment semantic generation once, mark changeset applied, and commit.
10. Drain index work under the same rebuild barrier; report indexed or repair
    required as specified in the persistence plan.

The existing context-engine journal checkpoint still commits atomically with
the batch. Each grouped request gets a stable internal child changeset ID and
the batch checkpoint advances only if every child mutation succeeds.

## Proposal and approval

Proposal transaction:

- Recheck idempotency.
- Record base domain generation, expected revision/row versions, actor,
  authorization generation, reason, and requested payloads.
- Do not create canonical revisions, events, generation increments, outbox
  rows, embeddings, or index entries.

Approval flow:

1. Daemon verifies human Reviewer/Maintainer role and current membership
   generation. Agent actor kinds are rejected.
2. Graph transaction loads the proposal and requires `proposed`.
3. Graph rechecks expected revisions/row versions/lifecycle and the supplied
   authorization generation/current decision metadata.
4. Embeddings are computed before taking the writer lock from immutable
   proposal payload; approval rechecks payload hash after acquiring the lock.
5. Apply all canonical operations, revisions, events, generation, and outbox in
   one transaction; or mark conflicted without canonical mutation.

Two concurrent approvals yield one applied receipt and one idempotent view of
that receipt. Two competing proposals against one head yield at most one apply;
the other becomes conflicted. No last writer silently wins.

Rejection records reviewer, reason, time, and authorization generation. It does
not touch graph projections or indexes. Rejected payloads are retained 30 days
by default, then compacted to hash/metadata; applied revisions are retained by
default.

## Immutable revision semantics

Observation `id` remains logical identity. Any field affecting semantic recall
creates a new `observation_revisions` row whose `parent_revision_id` is the
current head. The transaction copies the new revision into existing projection
columns, sets `current_revision_id`, increments `row_version`, and adds an index
outbox upsert.

These are semantic fields:

- content, title, summary, concepts, source files, source kind;
- confidence, importance, memory tier;
- valid-from/valid-until;
- semantic source/provenance.

`access_count` is telemetry and does not revise. `observed_at` is retained from
the original assertion unless a specific corrective operation changes its
meaning and records that fact.

Entity semantic fields are name, controlled entity type, confidence, aliases,
and identifiers. Entity rename may require re-home; aliases are the normal
cheap alternative.

Relation semantic fields are endpoints, controlled/free relation type, weight,
validity, source, and verified directional evidence. Endpoint changes are
modeled as retire old relation plus add new relation, never in-place identity
mutation.

## Lifecycle and deletion

Use `active`, `retired`, and `destroyed` lifecycle values in typed APIs. Only
`active` objects participate in default recall/traversal.

- `retire`: semantic, audited, reversible, index delete/outbox; current
  revisions remain.
- `restore`: audited, requires expected retired head, index upsert/outbox.
- `revert`: select an earlier revision as a new head event; never repoint
  history silently.
- `destroy`: irreversible administrative operation. Preview enumerates
  canonical rows, revisions, contributions, related history, and backups. It
  requires Maintainer, a short-lived confirmation hash, and explicit scope.
  It leaves a content-free destruction receipt if policy permits.

Keep existing MCP `openmemory_forget_entity` behavior compatible for the
release in which migration lands, but route new admin UI/CLI operations through
retire. Mark legacy hard deletion clearly and do not advertise it as reversible.
Before enabling history guarantees by default, decide whether the legacy MCP
tool becomes a deprecated alias for retire in a future major protocol version.

## Typed diff

`MemoryDiff` is computed from immutable revisions and lifecycle events, not
stored full before/after object JSON.

```rust
pub struct MemoryDiff {
    pub space_id: SpaceId,
    pub object: ObjectRef,
    pub from_revision: Option<RevisionId>,
    pub to_revision: Option<RevisionId>,
    pub lifecycle: Option<ValueChange<Lifecycle>>,
    pub fields: Vec<FieldChange>,
    pub provenance: Vec<OriginRef>,
    pub expected_current_revision: Option<RevisionId>,
    pub current_revision: Option<RevisionId>,
    pub stale: bool,
}

pub enum FieldChange {
    Text { field: FieldName, before: Option<String>, after: Option<String> },
    Scalar { field: FieldName, before: ScalarValue, after: ScalarValue },
    Set { field: FieldName, added: Vec<String>, removed: Vec<String> },
    Relation { before: Option<RelationValue>, after: Option<RelationValue> },
    Contribution { added: Vec<OriginRef> },
}
```

Set diffs are sorted and duplicate-free. Large content responses include a
bounded inline preview plus content hash/length and a separate authorized
detail endpoint; do not put multi-megabyte bodies into event streams. The
daemon/admin layer redacts local paths according to endpoint policy.

Diff endpoints support:

- proposal desired state vs current head;
- two revisions of one logical object;
- changeset applied events;
- merge target before vs predicted/actual target;
- conflict view with base/source/target values.

## Manual editing flows

### Edit observation

1. Client GETs detail/history and receives head revision + row version.
2. Client edits fields and POSTs expected head/version plus idempotency key.
3. Personal policy may apply immediately; team policy proposes.
4. Response contains changeset state, typed diff, new/current head, and index
   readiness.
5. A stale tab gets `changeset_stale` with current head and a fresh diff base.

### Entity rename

- If lowercased hash remains in same domain, apply a normal entity revision
  after uniqueness and relation/index checks.
- If hash changes domain, create an administrative `entity_rehome` job. Build a
  staged space copy, re-derive relation stubs/mirrors/indexes, verify, and
  promote using the root promotion primitive.
- UI shows that rename is a short maintenance job; no hidden cross-domain
  partial update.

### Relation editing

Disabled until canonical relation ID/mirror backfill reports ready. Then:

- Edit canonical revision in source home domain.
- Enqueue idempotent mirror upsert/delete with canonical ID and target domain.
- Surface traversal degradation if mirror work lags.
- Relation suggestions from identity review are separate changesets and require
  source-verified directional evidence; they are never a side effect of
  deciding entity identity.

### Bulk review

“Approve all” is a client convenience that submits independent approval calls
grouped by domain and returns each result. It is never presented as one atomic
space-wide transaction.

## Audit query behavior

- Cursor pagination uses `(created_at, id)`, not offset, for changesets/history.
- Default list queries return metadata/diff summaries without full requested
  content.
- Space and principal filters are authorization constrained before SQL.
- History queries never infer space from globally non-unique logical IDs.
- Audit events include real actor kind/principal, not caller-controlled source.
- Product job/event logs contain identifiers/counts/states and redacted errors;
  semantic content remains behind authenticated detail routes.

## Acceptance cases

- Proposal invisible to recall before approval.
- Applied write, revision, event, generation, and outbox are all present or all
  absent after forced transaction failure.
- Stale approval and stale edit cannot overwrite.
- Same idempotency retry returns byte-equivalent receipt across reopen.
- Edit -> retire -> restore -> revert produces a coherent immutable lineage.
- Legacy row lazily gains baseline plus new head without startup scan.
- Index failure never makes new canonical content silently unsearchable as a
  successful recall.
- Relation edit cannot run before mirror readiness.
- Diff is deterministic across insertion order and reopen.

