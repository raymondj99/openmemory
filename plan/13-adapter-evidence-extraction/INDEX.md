# Adapter Evidence And Extraction Plan

This plan redesigns `openmemory ingest` and future source adapters around
evidence-first ingestion. It replaces the current habit of turning external
sources directly into graph writes with a safer pipeline:

```text
source -> evidence adapter -> evidence ledger -> extractor -> validator
       -> resolver -> context engine -> graph memory
```

The goal is not to add complexity for its own sake. The goal is to make imports
auditable, idempotent, reversible, and suitable for both deterministic parsing
and agent-assisted extraction.

## Product And Engine Thesis

OpenMemory should be the memory substrate agents can trust. That means source
imports must preserve provenance before interpretation. A connector that reads a
Markdown file, chat export, ticket, transcript, or calendar feed should first
record what was seen, where it came from, and whether it changed. Only after that
should a deterministic extractor or agent propose durable memories.

The context engine remains a high-throughput commit lane for validated
`RememberRequest`s. It should not learn source-specific parsing rules, model
prompt semantics, or review workflow state.

## Document Map

Read in this order for implementation work.

| Document | Purpose |
|----------|---------|
| [01-principles-and-boundaries.md](01-principles-and-boundaries.md) | Current adapter problem, first principles, ownership boundaries, non-goals, and invariants. |
| [02-evidence-model.md](02-evidence-model.md) | Evidence records, URIs, hashing, source metadata, ledger responsibilities, and storage options. |
| [03-adapters-and-chunking.md](03-adapters-and-chunking.md) | Evidence adapter contract, Markdown/chat behavior, future connectors, chunking, skip rules, and streaming. |
| [04-extraction-pipeline.md](04-extraction-pipeline.md) | Deterministic, agent, and hybrid extraction policies, extractor identity, schemas, caching, and prompts. |
| [05-validation-resolution-idempotency.md](05-validation-resolution-idempotency.md) | Candidate validation, resolver rules, duplicate prevention, provenance links, stale evidence, and deletion policy. |
| [06-cli-and-operations.md](06-cli-and-operations.md) | CLI modes, JSON reports, review workflows, jobs, observability, errors, and operator controls. |
| [07-implementation-roadmap.md](07-implementation-roadmap.md) | Phased implementation sequence, compatibility plan, migrations, and definitions of done. |
| [08-quality-performance.md](08-quality-performance.md) | Test matrix, benchmarks, load scenarios, performance budgets, release gates, and production checklist. |
| [09-research-basis.md](09-research-basis.md) | Research and production-system check against current agent-memory and ingestion practice. |

## Core Decisions

- Adapters emit evidence, not graph writes.
- Extractors propose memory candidates, not commits.
- Agent extraction is optional, feature-gated, bounded, cached, and validated.
- Every committed memory must link back to source evidence.
- Unchanged evidence must not produce graph writes on rerun.
- Changed evidence must be re-extractable without losing the audit trail.
- Existing Markdown and chat ingest behavior remains compatible until the new
  path is proven.

## Target Shape

```text
EvidenceAdapter
  reads one source family and yields EvidenceRecord batches

EvidenceLedger
  upserts records by URI/hash, indexes raw text, and records changed state

ExtractorPolicy
  picks index-only, deterministic, agent, hybrid, or review behavior

Extractor
  produces ExtractionCandidate values with evidence URI/hash/span

Validator
  rejects malformed, stale, unsupported, low-confidence, or duplicate candidates

Resolver
  turns accepted candidates into RememberRequest batches

ContextEngine
  commits validated requests through the existing sharded write-behind path
```

## Compatibility Rule

Do not break existing `openmemory ingest <PATH>` behavior during the first
implementation slices. Add the evidence seam behind the current adapters, prove
parity with tests, then expose new modes behind explicit flags.

## Immediate First Slice

1. Add evidence types and stable URI/hash helpers.
2. Add `EvidenceAdapter` beside the existing `SourceAdapter`.
3. Implement Markdown and chat evidence adapters.
4. Add compatibility conversion back to the current `RememberRequest` output.
5. Add tests for stable evidence and rerun idempotency at the evidence layer.

No schema migration or user-visible CLI default change is required for this
first slice.

## Success Criteria

- Source imports are repeatable and audit-friendly.
- Agent extraction can improve quality without being trusted as the writer.
- Large reruns avoid duplicate observations by design.
- The adapter layer can grow to tickets, calendar events, transcripts, and
  vendor exports without turning every connector into a bespoke graph writer.
- The implementation keeps the repository's existing posture: local-first,
  explicit contracts, deterministic tests, bounded resources, and no hidden
  network dependency.
