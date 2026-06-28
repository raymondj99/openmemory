# Quality And Performance

## Quality Bar

The adapter redesign touches ingestion, provenance, and graph writes. It must be
held to the same bar as the engine:

- Deterministic tests for contracts.
- No panics on source input.
- No duplicate graph writes on rerun.
- Bounded memory and concurrency.
- No required network dependency.
- Explicit error states.
- Forward-only migration discipline.

## Required Local Checks

Baseline:

```bash
cargo fmt --all
cargo test -p openmemory-engine --all-features adapter
cargo test -p openmemory-cli --all-features ingest
cargo clippy -p openmemory-engine --all-targets --all-features -- -D warnings
```

Before merging wider changes:

```bash
cargo test --workspace --locked --all-features
cargo test --workspace --locked --no-default-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

Use narrower checks during early slices, but the full gates must pass before the
feature is considered production-ready.

## Unit Test Matrix

Evidence:

- Stable file URI generation.
- Stable chat row URI generation.
- Stable chunk URI generation.
- Content hash is stable across reruns.
- Hash changes when extraction-relevant metadata changes.
- Hash ignores scan-time-only metadata.
- Non-UTF-8 source is rejected.
- Empty source is skipped.
- Oversize source is skipped or chunked.

Adapters:

- Markdown H1 title metadata.
- Markdown frontmatter preservation.
- Markdown heading chunking.
- Markdown case-insensitive extension matching.
- Markdown noisy directory skip.
- Chat valid row parsing.
- Chat malformed row line-number error.
- Chat timestamp preservation.
- Chat thread grouping.
- Chat flat row compatibility.

Extraction:

- Deterministic extractor is pure.
- Candidate hash is stable.
- Candidate hash ignores JSON field order.
- Extractor ID changes candidate hash when version changes.
- Agent fake client valid output.
- Agent fake client invalid JSON.
- Agent fake client missing evidence URI.
- Agent fake client duplicate candidates.

Validation:

- Empty entity rejected.
- Empty observation rejected.
- Empty relation fields rejected.
- Unknown entity type rejected.
- Missing evidence rejected.
- Stale evidence rejected.
- Invalid span rejected.
- Low confidence held or rejected by policy.
- Candidate already committed is skipped.

Resolution:

- Observation fields map correctly.
- Relation fields map correctly.
- Source provenance is attached.
- Valid timestamps are preserved.
- Candidate hash commit state prevents duplicates.

## Integration Test Matrix

- Markdown import, rerun unchanged, zero duplicate graph writes.
- Markdown changed source, only changed evidence re-extracts.
- Chat JSONL import, rerun unchanged, zero duplicate graph writes.
- Chat malformed rows report line numbers and do not partially commit in
  fail-fast mode.
- Index-only mode writes evidence and raw index, not graph memory.
- Review mode writes candidates, not graph memory.
- Deterministic mode writes accepted graph memory.
- Crash after evidence upsert can resume extraction.
- Crash after extraction can resume validation and commit.
- Crash after commit can reconcile candidate state.

## Benchmarks

Add benchmarks for:

- Evidence hash throughput.
- Evidence ledger upsert throughput.
- Unchanged rerun throughput.
- Markdown chunking throughput.
- Chat JSONL parsing throughput.
- Candidate validation throughput.
- Candidate hash throughput.
- Resolver throughput.

Existing context-engine write benchmarks should remain the source of truth for
commit-lane performance.

## Load Scenarios

Run real-world load tests before defaulting to evidence-first ingest:

| Scenario | Purpose |
|----------|---------|
| 50,000 Markdown files | Directory walk, hashing, metadata, unchanged rerun. |
| 1,000,000 chat rows | Streaming, grouping, batch memory bounds. |
| Mixed repository tree | Ignore rules, binary rejection, noisy directories. |
| Large transcript | Chunking and extraction boundaries. |
| Agent timeout storm | Bounded failures and deterministic progress. |
| Crash at each stage | Resume and duplicate prevention. |

## Performance Budgets

Initial targets:

| Path | Budget |
|------|--------|
| Unchanged rerun | Dominated by walk/hash; zero graph writes. |
| Small deterministic import | Same order of magnitude as current ingest. |
| Candidate validation | CPU-local and batch-friendly. |
| Agent extraction | Bounded by configured concurrency and timeout. |
| Memory growth | Bounded by batch size and max text bytes. |

Agent extraction has no universal latency budget because provider latency varies.
The production requirement is bounded concurrency, clear progress, cache hits on
rerun, and no blocking of deterministic extraction.

## Concurrency Rules

- Use bounded worker pools.
- Cap file read/hash workers.
- Cap agent calls separately.
- Do not spawn one thread per file, row, or candidate.
- Do not let agent backpressure block raw evidence indexing.
- Preserve deterministic ordering where tests or compatibility require it.

Suggested defaults:

| Worker class | Default |
|--------------|---------|
| File read/hash | `min(available_parallelism, 8)` |
| Deterministic extraction | `available_parallelism` |
| Agent extraction | 2 |
| Evidence batch size | 256 records |

## Failure Rules

No panic or `unwrap()` on:

- Source decoding.
- Path traversal.
- UTF-8 conversion.
- JSONL parsing.
- Agent response parsing.
- Candidate validation.
- Candidate resolution.
- Commit reconciliation.

Errors should be typed and include stage plus source context.

## Release-Blocking Failures

Block release for:

- Duplicate graph writes on unchanged rerun.
- Missing evidence provenance on committed memories.
- Agent output bypassing validation.
- Network dependency in default build.
- Unbounded memory growth on large sources.
- Panic on malformed source input.
- Data loss during migration.
- Existing ingest default regression before announced migration.

## Production Checklist

- Evidence records are stable and auditable.
- Unchanged imports do not create graph writes.
- Changed imports re-extract only changed evidence.
- Candidate hashes prevent exact duplicate commits.
- Agent output is optional, validated, cached, and bounded.
- Every committed memory links back to evidence.
- Existing Markdown/chat behavior has compatibility coverage.
- Load tests cover large files, large chat exports, reruns, and crash recovery.
- Docs describe shipped behavior only.
- Benchmarks show no material regression to deterministic ingest throughput.
