# Implementation Roadmap

## Phase 0: Preserve Current Behavior

Purpose:

- Pin current adapter behavior before refactoring.
- Avoid accidental semantic drift.

Deliverables:

- Tests for Markdown H1, sections, attendees, empty files, and source files.
- Tests for chat JSONL timestamps, malformed lines, source tags, and exhaustion.
- Tests for `openmemory ingest` format detection.

Definition of done:

- Existing behavior is documented in tests.
- No user-visible behavior changes.

## Phase 1: Add Evidence Core Types

Purpose:

- Introduce the new seam without changing CLI behavior.

Deliverables:

- `EvidenceRecord`.
- `EvidenceBatch`.
- `EvidenceAdapter`.
- `EvidenceOutcome`.
- `EvidenceSkipReason`.
- URI helpers.
- BLAKE3 hash helpers.
- Canonical serialization helper for hashes.

Definition of done:

- Unit tests prove stable URI and hash generation.
- Types are serializable if they need to be persisted or reported.
- No graph write path changes.

## Phase 2: Implement Markdown And Chat Evidence Adapters

Purpose:

- Move existing source reading behind evidence without changing committed graph
  output.

Deliverables:

- `MarkdownEvidenceAdapter`.
- `ChatJsonlEvidenceAdapter`.
- Compatibility conversion to existing `RememberRequest` output.
- Case-insensitive extension handling.
- Iterative file traversal or proven walker.
- Source metadata preservation.

Definition of done:

- Current adapter tests still pass through compatibility wrappers.
- New evidence tests verify URIs, hashes, metadata, and chunk indexes.
- Large empty input trees do not recurse unboundedly.

## Phase 3: Evidence Ledger Facade

Purpose:

- Make reruns cheap and safe.

Deliverables:

- Upsert evidence records by URI.
- Return inserted, updated, unchanged, skipped outcomes.
- Raw evidence text index for changed evidence.
- Reuse existing metadata store where possible.
- JSON report counts.

Definition of done:

- Re-ingesting unchanged evidence creates no graph writes in evidence-first mode.
- Changed evidence updates raw index once.
- Evidence outcomes are tested.

## Phase 4: Deterministic Extraction Framework

Purpose:

- Separate interpretation from source reading.

Deliverables:

- `Extractor` trait.
- `ExtractorId`.
- `ExtractionCandidate`.
- Candidate hash.
- Deterministic Markdown extractor.
- Deterministic chat compatibility extractor.

Definition of done:

- Deterministic extraction is pure for the same evidence hash.
- Candidate hashes are stable.
- Existing Markdown/chat graph output remains compatible.

## Phase 5: Validator, Resolver, And Idempotency Gate

Purpose:

- Prevent malformed, stale, or duplicate candidate commits.

Deliverables:

- Candidate validator.
- Rejection reasons.
- Review states.
- Resolver to `RememberRequest`.
- Candidate commit state.
- Exact candidate-hash idempotency.

Definition of done:

- Re-running the same import commits zero duplicate graph writes.
- Stale evidence candidates do not commit.
- Invalid agent-like candidates reject cleanly.

## Phase 6: CLI Modes

Purpose:

- Expose evidence-first behavior explicitly.

Deliverables:

- `--mode index`.
- `--mode deterministic`.
- `--mode review`.
- `--dry-run`, if feasible without mutation.
- Structured JSON report.
- Human report.

Definition of done:

- Existing default remains compatible.
- New modes are documented.
- CLI tests cover JSON report fields.

## Phase 7: Agent Extraction Feature

Purpose:

- Let agents handle messy interpretation without becoming commit authority.

Deliverables:

- Optional feature flag.
- Provider-neutral `AgentClient` trait.
- Strict JSON schema.
- Prompt template and prompt hash.
- Timeout and concurrency limits.
- Extraction cache.
- Fake-agent tests.

Definition of done:

- Default CI does not need network.
- Invalid agent output cannot commit.
- Duplicate agent output does not duplicate graph writes.
- Agent failures are bounded and reported.

## Phase 8: Review And Operations

Purpose:

- Make lower-confidence extraction usable in product flows.

Deliverables:

- Candidate list state.
- Accept/reject operations.
- Rejection reason persistence.
- Daemon job stage model.
- Event progress model.

Definition of done:

- A product UI can build a review inbox without accessing storage internals.
- CLI can inspect and resolve candidates.

## Migration Strategy

Short term:

- Keep `SourceAdapter`.
- Add evidence adapters beside it.
- Use compatibility wrappers.

Medium term:

- Recommend evidence-first modes in docs.
- Keep old behavior available.
- Add explicit deprecation notice only after evidence-first parity.

Long term:

- Treat direct semantic adapters as compatibility only.
- Share evidence semantics with watcher and future source connectors.

## Storage Migration Strategy

Avoid schema migration in the first slice if possible.

If explicit tables are needed:

- Add forward-only migrations.
- Add backup preflight coverage.
- Add restore coverage.
- Add schema-too-new diagnostics.
- Do not couple migration to a default behavior switch.

## Pull Request Sizing

Recommended PR order:

1. Evidence types and hash helpers.
2. Markdown/chat evidence adapters with compatibility wrappers.
3. Ledger facade and raw index upsert.
4. Deterministic extraction and candidate hash.
5. Validator/resolver/idempotency.
6. CLI modes and reports.
7. Agent extractor feature.
8. Review/job operations.

Each PR should include:

- Focused tests.
- Docs updates only for shipped behavior.
- Benchmark or load test when performance-sensitive paths change.
- No unrelated refactors.
