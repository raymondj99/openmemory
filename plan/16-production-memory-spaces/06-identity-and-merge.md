# Identity and Merge

## Three different operations

Do not collapse these into one “merge” button.

| Operation | Mutation | Default use |
|---|---|---|
| Overlay | None | Temporarily recall from multiple authorized spaces. Safe and reversible. |
| Cherry-pick | Destination changeset only | Promote selected facts/entities with explicit provenance and review. |
| Material merge | Replaces target root with a staged verified result; source unchanged | Deliberately combine complete spaces and preserve lineage. |

The UI/CLI should recommend overlay first, cherry-pick second, material merge
only when the user wants one durable combined space.

## Immutable input snapshots

Before identity discovery:

1. Acquire source and target runtime leases.
2. Pause admissions for each engine, quiesce accepted writes, checkpoint WAL,
   drain index/mirror outboxes, and record ordered domain generation vectors.
3. Export canonical active and historical semantic records plus origin
   contributions. Exclude access counts and all derived rows/files.
4. Validate uniqueness, endpoints, revision lineage, contribution references,
   and bounds in `openmemory-merge::canonical`.
5. Compute domain-separated source/target snapshot hashes.
6. Release source pause; material merge never writes source. The target may
   reopen during review, but confirmation later requires the same target hash
   and generation or re-plans.

The snapshot contains enough data to reproduce the predicted target without
opening either live database. Every entity/claim/relation is in stable canonical
order.

## Candidate discovery

Candidate generation is deterministic, indexed, bounded, and separate from
decision. Never compare the Cartesian product.

Signals in priority order:

1. Existing lineage/origin references to the other space/logical ID.
2. Compatible source-verified identifiers in unique namespaces.
3. Source-bound verified relation target identifiers.
4. Exact normalized label plus controlled compatible kind.
5. Bounded aliases.
6. Bounded semantic retrieval over descriptions/claims.

Each source entity receives a deterministic candidate page with:

- candidates ordered by strongest signal, then canonical IDs;
- hard cap (default 32, maximum 128);
- signal/evidence references;
- total lower-bound and `truncated` flag;
- no interpretation that omitted candidates are `different`.

Lineage and verified unique-ID matches cannot be pushed out by a large homonym
bucket. Adversarial repeated labels and claimed identifiers remain bounded.

If no candidate exists, disposition is `add_source_qualified`. If candidates
exist but none is proven/reviewed same, source remains distinct. Candidate
review does not have to block a merge: `undetermined` conservatively adds a
separate source-qualified entity, provided complete accounting records that
choice.

## Evidence model

```rust
pub enum AssertionTrust {
    Claimed,
    SourceVerified {
        source_snapshot_id: String,
        verifier_version: String,
        resolver_generation: u64,
    },
}

pub enum EvidenceClass {
    ProofSame,
    ProofDifferent,
    Context,
    DirectionalRelation,
}

pub struct IdentityPacket {
    pub left: EntityRevisionRef,
    pub right: EntityRevisionRef,
    pub policy_generation: u64,
    pub ontology_generation: u64,
    pub resolver_generations: Vec<(String, u64)>,
    pub evidence: Vec<IdentityEvidence>,
    pub binding_hash: PacketHash,
}
```

Namespace policy is versioned data in code/fixtures. A namespace is allowed to
prove identity only if it has:

- a deterministic canonicalizer;
- an explicit uniqueness scope;
- compatible controlled entity kinds;
- current resolver generation; and
- source-verification metadata bound to both assertions.

Unknown namespaces, claimed values, incomparable resolver generations, labels,
aliases, descriptions, embeddings, graph neighborhoods, and user/agent prose
are context. Kind inequality is a contradiction only when the versioned
ontology says the pair is incompatible; unknown kind compatibility requires
review/separation.

Deterministic analysis:

- Consistent lineage/same proof with no contradiction -> `same` receipt.
- Compatible verified unique identifier match with no contradiction -> `same`
  receipt if policy permits; team policy may still require human confirmation.
- Conflicting verified unique identifiers in the same uniqueness scope ->
  `different` proof.
- Any simultaneous trusted same and different proof -> `conflicting_proofs`,
  human review only.
- Context-only -> unresolved review/separate.

## Agent boundary

An agent is optional review assistance, not identity authority.

- It receives only one bounded immutable packet after deterministic analysis.
- Prompt/model/version/temperature/tool policy and packet hash are persisted.
- Output schema is `same|different|undetermined`, cited evidence IDs, and a
  bounded rationale.
- Output referencing unknown evidence, wrong packet hash, excessive strings,
  hard-proof override, or invalid enum is rejected.
- The call is asynchronous and absent from recall/write/approval latency.
- It cannot write graph state, issue a receipt, authorize itself, approve a team
  decision, resolve conflicting proof, or create a relation.
- Timeout, model absence, prompt injection, parse failure, or abstention leaves
  the entities separate and available for human review.

Human review displays deterministic evidence first, agent suggestion second,
then the exact effect on contributions/claims/relations. Review action rechecks
packet, revisions, policy, resolver, ontology, membership generation, and actor
role. A changed input creates a new packet hash and reopens review.

## Resolution receipt

Only current receipts enter planning:

```rust
pub struct IdentityResolutionReceipt {
    pub candidate_id: String,
    pub left_revision: RevisionId,
    pub right_revision: RevisionId,
    pub packet_hash: PacketHash,
    pub decision_event_id: String,
    pub decision: IdentityDecision,
    pub policy_generation: u64,
    pub receipt_hash: ReceiptHash,
}
```

Receipt validation checks every field and current decision event. Reversing a
decision appends a superseding event; it never edits history. Old receipts then
fail planning as stale.

## The `cerpheus` example

Suppose Project A and Project B independently contain a concept named
`cerpheus`.

1. Exact normalized label discovers an identity candidate.
2. It does not merge automatically, even if embeddings/descriptions are close.
3. If A means an internal query planner and B means a mythological spelling,
   incompatible identifiers/context leads to `different`; both remain.
4. If both carry a source-verified same canonical registry ID under a unique
   compatible namespace, deterministic proof may yield `same`.
5. If evidence is contextual only, an agent may suggest an answer but a human
   must approve `same`; no decision/abstention keeps them separate.
6. Approved same maps B's entity to A's target identity in this directional
   merge, appends B as an origin contribution, unions non-conflicting claims,
   and rewires B relation assertions. It does not replace A's label/description
   with B's projection.

This behavior is the central identity acceptance test.

## Pure semantic planner

Inputs:

```rust
pub struct PlanRequest {
    pub source: CanonicalSpaceSnapshot,
    pub target: CanonicalSpaceSnapshot,
    pub lineage_base: Option<CanonicalSpaceSnapshot>,
    pub candidates: Vec<IdentityCandidate>,
    pub resolutions: Vec<IdentityResolutionReceipt>,
    pub policy: MergePolicy,
}
```

Validation before planning:

- Snapshot hashes, schema/policy versions, bounds, uniqueness, and endpoints.
- Candidate set exactly covers discovered pairs: no missing, extra, duplicate,
  or truncated-unacknowledged page.
- One current resolution per candidate; stale/invalid receipts fail.
- Every source entity gets one disposition.
- No two source entities coalesce into one target in the same plan.
- No generated source-qualified ID collides with target/source/result IDs.
- All source relations have both endpoints represented.

Deterministic disposition:

```rust
pub enum EntityDisposition {
    Coalesce {
        source: EntityAddress,
        target: EntityAddress,
        receipt: ReceiptHash,
    },
    AddDistinct {
        source: EntityAddress,
        result_id: String,
        reason: DistinctReason,
    },
}
```

Source-qualified IDs use a domain-separated hash of source space ID and source
logical ID, encoded as a valid logical ID. They do not depend on label, order,
or timestamps.

## Entity, claim, and relation planning

### Coalesced entity

- Keep the target logical ID and current target projection.
- Append one immutable origin contribution for the exact source revision.
- Union aliases/verified identifiers only as contributions until an explicit
  target revision policy promotes them.
- Never overwrite target scalar fields merely because source is newer.

### Distinct entity

- Add a canonical copy under the deterministic source-qualified ID.
- Preserve source origin space/logical/revision/semantic hash.
- Preserve source label/type/projection because this is not coalescence.

### Observations/claims

- Exact same semantic revision under a coalesced entity becomes a no-op plus
  origin contribution; do not duplicate recall text.
- Non-conflicting distinct claims are added with deterministic result IDs and
  provenance.
- Same logical ID with different semantic content uses lineage-aware three-way
  classification when a base exists; otherwise it is a collision requiring an
  explicit resolution or source-qualified addition according to policy.
- Contradictory claims are retained as separate provenance-bearing assertions
  or surfaced as semantic conflict; entity equivalence never silently resolves
  them.

### Relations

- Treat each canonical relation revision as a provenance-bearing assertion.
- Map both endpoints through the entity disposition table.
- Deduplicate only exact canonical assertion keys while retaining all origins.
- A same identity self-loop created by endpoint coalescence is dropped only if
  relation policy explicitly marks that relation type reflexively meaningless;
  otherwise surface it for review.
- Rebuild partition mirror/stub rows after import. They are not planner input.
- A candidate's contextual relation similarity never becomes a new edge.

## Three-way merge

If a lineage base is available and hash-verified, classify each stable logical
object/field:

| Base -> source | Base -> target | Result |
|---|---|---|
| unchanged | unchanged | target/base |
| changed | unchanged | source change |
| unchanged | changed | target change |
| same change | same change | one change + both provenance |
| disjoint fields | disjoint fields | combine deterministically |
| different same field | changed | conflict |
| delete/retire | edit | conflict unless policy explicitly resolves |

Conflict records contain base/source/target revision hashes and typed field
diffs. No timestamp winner. Resolutions are explicit plan inputs bound to those
three revisions; moved input invalidates them.

Without a base, target is never overwritten. Conservative union plus explicit
collisions is the only behavior.

## Plan and preview output

```rust
pub struct MergePlan {
    pub version: u32,
    pub source_space: SpaceId,
    pub target_space: SpaceId,
    pub source_snapshot_hash: SnapshotHash,
    pub target_snapshot_hash: SnapshotHash,
    pub expected_target_version: SpaceVersion,
    pub dispositions: Vec<EntityDisposition>,
    pub entity_actions: Vec<EntityAction>,
    pub observation_actions: Vec<ObservationAction>,
    pub relation_actions: Vec<RelationAction>,
    pub conflicts: Vec<MergeConflict>,
    pub accounting: MergeAccounting,
    pub predicted_result_hash: SnapshotHash,
    pub plan_hash: PlanHash,
}
```

`MergeAccounting` proves:

- all target objects retained/explicitly revised;
- all source objects added/coalesced/no-op/conflicted exactly once;
- all candidate decisions consumed exactly once;
- all relation endpoints mapped;
- no silent deletes;
- expected result counts and contribution counts.

Preview returns typed sections analogous to a git diff:

```text
SUMMARY
  source / target / base / plan hashes
  added / coalesced / unchanged / conflicted counts

IDENTITIES
  A:cerpheus -> B:cerpheus  SAME (receipt ..., evidence ...)
  A:parser   -> new src:<hash> DISTINCT

ENTITIES / CLAIMS / RELATIONS
  + additions with provenance
  = coalesced target plus contribution
  ~ three-way changes
  ! conflicts with base/source/target
  · exact no-ops with new origin

ACCOUNTING / PREDICTED RESULT
  complete=true, no deletes, result hash ...
```

Sort every section by canonical IDs. Re-running from identical inputs produces
byte-equivalent structured output and plan hash.

## Materialization

Material merge is a durable daemon job:

1. Preflight actor Maintainer, distinct source/target, no active recovery,
   source/target health, same filesystem, staging quota, backup retention quota,
   and compatible schema/features.
2. Require zero unresolved conflicts and current identity receipts. Immediately
   before accepting confirmation, capture fresh source and target snapshots; if
   either differs from preview, invalidate the preview and re-run discovery/
   planning rather than applying a stale plan.
3. Require a short-lived human confirmation bound to principal, source/target,
   plan hash, predicted result hash, and expiry.
4. Create unique staging root beneath `.merge-staging/<job>`; never reuse dirty
   staging without recovery validation.
5. Build a fresh target `DomainStore` using the target's pinned domain count.
6. Import target canonical truth, apply plan actions as generated merge
   changesets/revisions/contributions, and never import derived state.
7. Repartition by canonical entity names, re-derive relation mirrors/stubs,
   rebuild FTS/vector/metadata, drain outboxes, checkpoint, and close.
8. Verify schema versions, foreign keys, canonical uniqueness/endpoints,
   revision/events/contribution linkage, complete accounting, expected counts,
   per-domain generation, index counts/generation, predicted semantic hash, and
   deterministic sampled recall fixtures.
9. Pause target admission, drain/close registry runtime, and re-snapshot target.
   If hash/version moved, abort staging and reopen old target.
10. Write/fsync promotion intent and parent directory.
11. For a new-space target, rename its live `store/` directory to
    `.merge-backup/<job>`; for the legacy personal-global target, move only the
    enumerated `DomainStore` artifacts into that backup while the intent blocks
    open. Never rename the whole profile root. Fsync all affected parents.
12. Promote the staged directory or staged artifact set into the live layout
    and fsync all affected parents.
13. Open and verify promoted target; update catalog/lineage/job in product DB.
14. Clear/fsync intent and reopen registry. Keep backup for retention window.

The job never acquires a source write handle and never writes the live source.
Verify the private source snapshot artifact hash again in the final report. If
the live source legitimately advances after the confirmation snapshot, report
its newer version as “not included” rather than attributing it to the merge or
silently extending the plan.

## Promotion intent and recovery

Intent JSON is versioned, bounded, and contains absolute-canonical validated
root keys (serialized as profile-relative paths), job/source/target IDs, old
target hash, new target hash, plan hash, and phase:

```text
prepared -> target_backed_up -> staging_promoted -> promoted_verified
         -> catalog_committed -> complete
```

Filesystem state plus intent phase has an exhaustive transition table in code
and tests. Recovery runs before opening an affected space:

- Live old target + staging + intent prepared: verify old/staging; resume or
  cancel per job state.
- Backup exists, live missing, staging exists: promote verified staging or
  restore verified backup; never create empty live root.
- Backup exists, live new verified: finish catalog/lineage and clear intent.
- Ambiguous/mismatched hashes: fail closed with `MergeRecoveryRequired` and
  report safe relative artifact paths to `doctor`.
- Repeated recovery/process death is idempotent.

No cleanup runs until live target, product catalog, lineage, and job all agree.
Backup cleanup is a separate resumable maintenance task.

## Permanent real-world fixtures

Port sanitized, source-attributed versions of:

- `github_codex_homebrew_tools.json`: shared Homebrew ecosystem but distinct
  Codex and OpenAI-tools distribution concepts.
- `github_axum_actix_web.json`: two same-domain Rust web frameworks with shared
  ecosystem concepts and distinct framework identities.
- `github_mathlib_lean4.json`: mathematics library and directly related Lean
  coding/toolchain project, containing both true shared concepts and distinct
  modules.

Each fixture must use the same production candidate/packet/planner path. Goldens
assert candidate coverage, reviewed decisions, dispositions, relation rewiring,
origin contributions, no silent deletes, deterministic hashes, and final recall
from the staged graph. Do not add fixture-name branches or expected-decision
shortcuts to production code.

## Merge acceptance cases

- Shared name alone never coalesces.
- Proven same identity preserves target projection and source contribution.
- Proven/reviewed different creates deterministic distinct ID.
- Stale receipt, target generation, source snapshot, plan hash, confirmation,
  or policy generation fails before promotion.
- Missing/extra/duplicate candidate resolution or source accounting fails.
- Many-to-one source coalescence fails and asks for source normalization.
- No relation has dangling endpoints; every imported assertion has provenance.
- Exact rerun is deterministic and idempotent.
- Source bytes/semantic hash remain unchanged.
- Abort after every durable transition recovers verified old or new target on
  macOS and Linux.
