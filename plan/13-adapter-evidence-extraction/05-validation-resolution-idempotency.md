# Validation, Resolution, And Idempotency

## Purpose

Validation protects the graph from malformed or unsupported candidates.
Resolution converts accepted candidates into `RememberRequest`s. Idempotency
prevents reruns from appending duplicates.

This stage is the trust boundary. All extraction output, deterministic or
agent-generated, passes through the same checks.

## Validation Inputs

Validator input:

- Candidate.
- Evidence ledger lookup.
- Current evidence hash.
- Extraction policy.
- Optional relation allowlist.
- Optional confidence threshold.
- Optional duplicate preflight settings.

Validator output:

```rust
pub enum CandidateDecision {
    Accept,
    Reject(CandidateRejection),
    HoldForReview(CandidateReviewReason),
    AlreadyCommitted,
    StaleEvidence,
}
```

## Required Validation Rules

Reject when:

- `entity_name` is empty after trim.
- Observation content is present but empty after trim.
- No observation and no relation are present.
- Relation type is empty after trim.
- Relation target name is empty after trim.
- Entity type is unknown.
- Relation target type is unknown.
- Confidence is outside `0.0..=1.0`.
- Evidence URI is missing.
- Evidence URI is not found in the ledger.
- Candidate evidence hash does not match the ledger.
- Span is outside evidence text.
- Candidate hash does not match canonical candidate content.
- Candidate count for a record exceeds policy.
- Agent extractor output omits required provenance.

Hold for review when:

- Confidence is below auto-commit threshold but above reject threshold.
- Relation type is not in an allowlist but policy permits review.
- Entity name normalization would merge with a surprising candidate.
- Duplicate preflight finds a close but not exact match.

## Stale Evidence

A candidate is stale when:

- It cites an evidence URI that still exists, but the content hash changed.
- It cites a deleted evidence URI.
- It was produced by an extractor run superseded by policy.

Stale candidates must not commit automatically. They can be retained for audit or
shown in review.

## Candidate Idempotency

Every candidate has a canonical hash. Commit state is keyed by this hash.

Before converting to `RememberRequest`:

1. Check whether candidate hash has already committed.
2. Check whether candidate hash is currently pending commit.
3. Check whether evidence hash still matches.
4. Only then resolve and submit.

This gives exact idempotency for repeated extraction runs.

Semantic duplicates remain a separate concern. Existing consolidation can still
merge near-duplicate observations later.

## Resolver Rules

The resolver maps accepted candidates to graph requests:

```rust
RememberRequest::new(candidate.entity_name, candidate.entity_type)
    .with_observations(observations)
    .with_relations(relations)
    .with_source(source)
```

Observation fields:

- `content`: candidate observation content.
- `title`: candidate title.
- `summary`: candidate summary.
- `importance`: clamped.
- `source_kind`: derived from source type or candidate.
- `concepts`: candidate concepts.
- `source_files`: source files plus evidence URI where useful.
- `valid_from`: candidate or evidence timestamp.
- `valid_until`: candidate or evidence timestamp.

Relation fields:

- `relation_type`: candidate relation type.
- `target_name`: candidate target name.
- `target_type`: candidate target type.
- `source`: extraction source tag where supported.

Source tag:

- Deterministic extractor: `ingest:<source_type>:deterministic`.
- Agent extractor: `ingest:<source_type>:agent:<extractor_name>`.
- Compatibility path: preserve current source labels where behavior is being
  kept stable.

## Provenance Links

Each committed graph row should be linkable to:

- Candidate hash.
- Evidence URI.
- Evidence hash.
- Extractor identity.
- Observation ID or relation ID.

Phase 1 can rely on observation fields and candidate commit records. Phase 2
should add explicit links if candidate review or retraction needs stronger
queries.

## Duplicate Preflight

Exact idempotency is mandatory. Semantic duplicate prevention is optional.

Optional preflight:

- Recall the candidate title/content scoped to the entity.
- If a high-scoring observation already exists, hold or skip.
- Use source provenance to avoid suppressing independent evidence too eagerly.

Default recommendation:

- Implement exact candidate-hash idempotency first.
- Keep semantic dedup in consolidation until candidate workflows mature.

## Commit Transaction Boundaries

The context engine commits `RememberRequest`s in batches. Candidate commit state
must stay consistent with graph writes.

Options:

1. Mark candidate committed after engine durability.
2. Write candidate commit state in the same graph transaction.
3. Use a pending state plus reconciliation after crash.

Recommendation:

- Phase 1: mark committed after durable engine acknowledgement and reconcile by
  candidate hash on restart.
- Phase 2: move candidate link writes into the same transaction if evidence
  tables live with graph storage.

## Crash Recovery

Recovery rules:

- Evidence upsert completed, extraction not started: extract later.
- Extraction completed, candidate not committed: validate and commit later.
- Candidate pending commit, graph row unknown: reconcile using candidate state
  and provenance.
- Candidate committed, duplicate run appears: return `AlreadyCommitted`.

No crash point should create unbounded duplicate writes.

## Deletion And Retraction Policy

Default policy:

- Source deletion does not delete graph memory.
- Changed source does not delete old graph memory.
- Both conditions mark evidence/candidates stale.

Explicit future policies:

- Retract graph rows linked only to deleted evidence.
- Supersede old observations when replacement candidates commit.
- Hold stale memories for review.

Retraction must be opt-in because local memory may remain valuable even when the
original source file was moved or deleted.

## Review States

Candidate states:

- New.
- Accepted.
- Rejected.
- Held for review.
- Committed.
- Already committed.
- Stale evidence.
- Failed validation.
- Superseded.

Review decisions should be idempotent and auditable. Rejected candidates should
store a reason.
