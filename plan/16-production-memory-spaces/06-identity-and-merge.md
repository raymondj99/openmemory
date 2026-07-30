# Identity and Merge

## Three separate operations

| Operation | Mutation | Use |
|---|---|---|
| Overlay | none | temporary authorized multi-space recall |
| Cherry-pick | destination changeset | selected provenance-bearing promotion |
| Material merge | verified replacement target; source unchanged | deliberate durable composition |

Surfaces recommend them in that order.

## Immutable inputs

Capture source and target with the quiesced generation-bound snapshot protocol
in `03-persistence-and-migrations.md`. A snapshot contains sorted canonical
active/history records, lifecycle, revisions, identifiers, relation assertions,
and origin contributions; it excludes telemetry and derived state.
The target snapshot additionally carries the independently hashed audit/control
preservation view defined in the persistence plan; source control rows never
cross spaces.

Before use, verify:

- private immutable artifact path and file hashes;
- space/manifest/schema/hash versions;
- numeric domain generations and combined semantic hash;
- unique objects/revisions/sets, valid lineage, endpoints, contributions, and
  configured size limits.

Planning receives snapshot readers, never live store handles. Target may reopen
during review, but confirmation and pre-rename checks require the exact preview
target version. Source may advance later; those writes are explicitly “not
included” and never modified.

## Candidate discovery

Discovery is deterministic, target-space-indexed, and bounded; never Cartesian.
Signals:

1. exact lineage/origin reference;
2. compatible source-verified unique identifiers;
3. source-bound directional relation target identifiers;
4. exact normalized label and compatible controlled kind;
5. bounded aliases;
6. bounded semantic retrieval over descriptions/claims.

Discovery is a conservative candidate-producing optimization behind an
authoritative bounded reference scan used in tests. It may admit contextual
false positives, because the exact evidence packet and receipt path rejects
them. It must not omit a lineage or currently verified compatible unique-ID
candidate; such a false negative is a semantic defect, not an acceptable index
tradeoff. Unsupported or stale index generations fall back to the general
reference mechanism when bounded, or fail with a typed unavailable result
rather than claiming a complete candidate set.

Each source entity gets a receipt containing discovery/index/policy generation,
ordered candidates, evidence references, total lower bound, cap, and
`truncated`. Default cap is 32, hard 128. Strong lineage/verified-ID evidence
cannot be displaced by homonym buckets.

No candidate means add distinct. An unresolved page produces a canonical
`KeepDistinctReceipt` bound to the source revision, complete emitted candidate
hashes, discovery generation/truncation, and policy. It makes no ontological
`different` claim but explicitly consumes those candidates for this merge.
A truncated page may proceed only through this conservative receipt; it can
never support coalescence or imply omitted candidates are different. Refined/
manual discovery invalidates the receipt.

Duplicate entity/relation insertion must fail atomically so label/identifier
indexes cannot retain stale entries. External namespace uniqueness is monitored
and versioned; an observed duplicate revokes deterministic proof for that
namespace until revalidation.

## Evidence policy

```rust
pub enum AssertionTrust {
    Claimed,
    SourceVerified {
        source_snapshot: SnapshotId,
        verifier_version: String,
        resolver_generation: u64,
    },
}

pub struct IdentityPacket {
    left: EntityRevisionRef,
    right: EntityRevisionRef,
    policy_generation: u64,
    ontology_generation: u64,
    resolver_generations: ResolverGenerations,
    evidence: IdentityEvidenceSet,
    hash: PacketHash,
}
```

A namespace can prove identity only with a deterministic canonicalizer,
declared uniqueness scope, controlled-kind compatibility, current resolver
generation, and source-bound verification on both assertions. Labels, aliases,
descriptions, embeddings, neighborhoods, unknown namespaces, claimed IDs, and
prose are context.

Deterministic analysis:

- consistent lineage or compatible verified unique ID, no contradiction:
  `proof_same`;
- conflicting verified IDs in one uniqueness scope: `proof_different`;
- simultaneous trusted same/different evidence: `conflicting_proofs`;
- context only: `review_or_separate`.

Unknown ontology compatibility remains context. Trusted contradiction cannot be
overridden by a human or agent without a new evidence-policy generation and
revalidated packet.

For material coalescence, even `proof_same` requires a current human
Reviewer/Maintainer decision receipt. Deterministic proof controls what review
may accept; it is not a silent merge policy. `proof_different` may produce a
system contradiction receipt that only permits separation.

## Agent boundary

The optional `IdentityAgent` is asynchronous review assistance:

- disabled without explicit provider configuration and outbound-data consent;
- receives one bounded immutable packet after deterministic analysis;
- persists packet/prompt/model/version/temperature/tool-policy provenance;
- returns `same|different|undetermined`, cited evidence IDs, and bounded
  rationale;
- may explain any packet, including deterministic-different packets, but cannot
  reverse proof, issue a receipt, write graph state, create relations, approve
  team work, or authorize itself.

Reject wrong packet hash, unknown evidence, invalid schema, excess size, hard
proof override, or unsupported enum. Timeout, unavailable model, prompt
injection, parse failure, or abstention leaves entities separate and reviewable.
No provider is called from recall/write/approval/promotion.

Relation suggestions require a source-bound verified directional assertion for
the exact endpoints and become separate changesets after review.

The agent is an optional adapter at the same immutable-packet seam as
deterministic review assistance, not a parallel identity pipeline. Optional
mode falls back to deterministic `review_or_separate`; an explicitly required
provider fails loudly with a typed unavailable result. Both modes preserve the
same packet, proof, receipt, authority, and mutation contract.

## Durable human review

Review displays deterministic evidence, contradictions, optional agent
proposal, and exact projected effects. The transaction rebinds both current
entity revisions, packet, discovery/policy/ontology/resolver generations,
membership generation, and actor role.

```rust
pub struct IdentityResolutionReceipt {
    candidate_id: CandidateId,
    left_revision: RevisionId,
    right_revision: RevisionId,
    packet_hash: PacketHash,
    decision_event: DecisionEventId,
    decision: SameOrDifferent,
    policy_generation: u64,
    receipt_hash: ReceiptHash,
}
```

Two reviewers produce one head decision and one conflict/idempotent receipt.
Revising a decision appends a superseding event against a newly current packet.
New hard evidence reopens review. `Undetermined` is a defer action, not a final
receipt.

## Pure streaming planner

`openmemory-merge` consumes sorted canonical iterators, current discovery
receipts, current resolution receipts, optional verified lineage base, and a
versioned policy. It emits actions to a bounded sink while computing accounting
and predicted hash. It has no I/O or authorization.

Phase 1 first pins a simple bounded, test-only materialized reference
implementation of these semantics. It is an oracle, not a production path.
The streaming planner is an execution-shape specialization behind the same
typed contract and must be observably equivalent to that reference for action
order, hashes, accounting, errors, and terminal state. Chunk size, allocation
strategy, and sink implementation cannot select different semantic rules.

The source/sink protocol is explicit:

```text
validate header -> begin sink -> emit ordered actions -> finish accounting/hash
                                    \-> abort on any error
```

Only `finish` may yield a usable plan. Parse, validation, sink, cancellation,
deadline, or accounting failure calls idempotent `abort` and cannot leave a
published or previewable plan. Test hash, paged-artifact, materialization,
verifier, and reference sinks all consume this one protocol.

Before emitting:

- validate versions, bounds, sorted uniqueness, endpoints, and snapshot hashes;
- validate discovery coverage and exact candidate hashes;
- consume every emitted candidate exactly once through a current human-approved
  same receipt, proven/reviewed different receipt, or source-level
  `KeepDistinctReceipt`; no extra/missing/overlapping consumption is valid;
- permit truncated discovery only through `KeepDistinctReceipt`;
- compute final one-to-one dispositions on the coordinator;
- reject one source to multiple targets and multiple sources to one target;
- ensure every source entity and relation endpoint has one disposition.

```rust
pub enum EntityDisposition {
    Coalesce { source: EntityAddress, target: EntityAddress, receipt: ReceiptHash },
    AddDistinct { source: EntityAddress, result: QualifiedObjectId, reason: DistinctReason },
}
```

`QualifiedObjectId` is a typed, domain-separated, length-framed hash of source
space and source logical ID. Its projection encoding occupies a reserved
versioned namespace; new local IDs cannot use that namespace. Legacy/prior
imports are scanned for collisions and every generated ID is checked against
target/result IDs.

### Semantic actions

- Coalesced entity keeps target ID/projection and appends exact source revision
  as an origin contribution. Source fields do not overwrite target scalars.
- Distinct entity copies source projection under the qualified ID with exact
  origin.
- Equal semantic observations under a coalesced entity deduplicate content but
  add origin; distinct/contradictory claims remain provenance-bearing.
- Stable logical collisions use verified three-way classification when a base
  exists; otherwise target is preserved and collisions are explicit.
- Relations map both endpoints through dispositions, retain each assertion and
  origin, and deduplicate only equal canonical assertion keys.
- Duplicate input assertions are rejected, not hidden by a set.
- Coalescence-created self-loops require explicit relation-type policy;
  otherwise they are review conflicts.
- Mirrors/stubs are absent from planner input and rebuilt later.

### Three-way rules

| Source vs base | Target vs base | Result |
|---|---|---|
| unchanged | unchanged | target/base |
| changed | unchanged | source change |
| unchanged | changed | target change |
| identical change | identical change | one value, both provenance |
| disjoint fields | disjoint fields | deterministic combination |
| same field differs | changed | conflict |
| retire/delete | edit | conflict unless explicit typed policy |

No timestamp winner. Conflict resolution binds base/source/target revisions;
movement invalidates it.

## Plan artifact and preview

The durable plan is a small canonical header plus an immutable paged action
stream:

```rust
pub struct MergePlan {
    version: u32,
    source_space: SpaceId,
    target_space: SpaceId,
    source_snapshot_hash: SnapshotHash,
    target_snapshot_hash: SnapshotHash,
    expected_target_version: SpaceVersion,
    action_stream_hash: ActionStreamHash,
    action_counts: ActionCounts,
    accounting: MergeAccounting,
    predicted_result_hash: SnapshotHash,
    plan_hash: PlanHash,
}
```

Accounting proves every target retained/explicitly revised; every source
entity/observation/relation added, coalesced, no-op, or conflicted exactly once;
every candidate consumed exactly once; every endpoint mapped; all
origins retained; no silent delete; and expected result/contribution counts.

Hash the semantic action structure independently from its file encoding.
Materialization parses bounded records and revalidates every action and
accounting invariant; recomputing a forged plan hash is insufficient.

Preview is paginated and deterministic: summary/hashes, identities,
add/coalesce/change/conflict/no-op sections, provenance, accounting, and
predicted result. Identical inputs yield byte-equivalent canonical plan
artifacts.

## Materialization

1. Preflight distinct source/target, Maintainer, current receipts, no recovery,
   health/readiness, schema/features, operation/memory/disk quotas, same
   filesystem, and backup retention.
2. Capture fresh snapshots before accepting confirmation. Any preview movement
   restarts discovery/planning.
3. Require human confirmation, default TTL 15 minutes and hard maximum 1 hour,
   bound to principal, authority, source/target, snapshot/plan/action/predicted
   hashes, and expiry.
4. Stream verified snapshots/actions into bounded deterministic per-domain
   spools, then add engine-derived mirror/stub instructions; do not retain
   complete graphs.
5. Build new domains through the shared executor, default concurrency two:
   preserve target audit/control rows, import semantic actions, revalidate
   pending proposals, derive mirrors/stubs, rebuild indexes, drain outboxes,
   checkpoint, close, and verify.
6. At the full barrier, perform coordinator-only global accounting, combined
   generation/hash, predicted-result, and sampled deterministic recall
   verification.
7. Promote only through the serialized protocol in
   `03-persistence-and-migrations.md`.

Authorization, final identity accounting, intent, rename/fsync, recovery, and
catalog/lineage/job commit never run as independent domain tasks.

## Recovery and source immutability

The private source snapshot is read-only and hash-verified before planning,
materialization, and final report. No source write handle is acquired.

Recovery verifies intent phase plus actual live/staging/backup hashes. It may
finish verified promotion or restore verified backup; ambiguous/mismatched
state returns `merge_recovery_required`. Repeated recovery is idempotent.
Nothing is cleaned until live target, product catalog, lineage, and job agree.

## Permanent fixtures and acceptance

Port sanitized, licensed/provenanced fixtures for:

- Codex/Homebrew Tools;
- Axum/Actix Web;
- Mathlib/Lean;
- adversarial homonyms/verified identifiers.

They use the production discovery/packet/review/planner/materializer path with
no fixture branches. Goldens assert candidates, decisions, dispositions,
action/accounting hashes, relation rewiring, origin preservation, staged
recall, and no deletion.

Release acceptance:

- label/embedding/claimed ID/agent output alone never coalesces;
- human-approved proof preserves target projection and source contribution;
- stale receipt/snapshot/policy/plan/confirmation fails before publication;
- incomplete/duplicate accounting or dangling endpoint fails;
- many-to-one coalescence fails pending source normalization;
- exact rerun is deterministic and source remains unchanged;
- planner and materializer stream within budgets;
- every injected durable-boundary crash on macOS/Linux recovers verified old or
  new target.
