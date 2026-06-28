# Principles And Boundaries

## Current Problem

The current engine adapter contract is direct:

```rust
pub trait SourceAdapter {
    fn name(&self) -> &'static str;
    fn next_batch(&mut self) -> MemoryResult<Vec<RememberRequest>>;
}
```

That makes a source connector responsible for three jobs:

- Reading an external source.
- Interpreting the source into entities, observations, and relations.
- Committing those interpreted memories into the graph through the engine.

This works for small reference adapters, but it is not the right production
boundary. Real sources are messy. Chat rows, meeting notes, transcripts, issue
threads, logs, and calendar entries do not share a single generic semantic
shape. Forcing every adapter to emit `RememberRequest`s makes the connector
pretend it knows what should become durable memory.

The graph write path is append-only by design. Identical observations are
appended again and later consolidation can deduplicate near matches. That is a
good contract for an explicit memory write, but a bulk import needs stronger
rerun safety before it calls the graph write path.

## First Principles

### Preserve Evidence First

The system should be able to answer:

- Which source produced this memory?
- What exact row, chunk, file, message, or event was used?
- What was the source hash when extraction happened?
- Has the source changed since then?
- Can this source be re-extracted or reviewed?

Direct `RememberRequest` adapters lose too much of that chain.

### Keep Connectors Boring

A source adapter should decode and normalize source evidence. It should not be a
semantic authority.

Good adapter work:

- Walk a directory.
- Read JSONL rows.
- Decode vendor fields.
- Preserve timestamps, authors, IDs, paths, and URLs.
- Emit stable URIs and content hashes.
- Split large input into bounded evidence chunks.

Bad adapter work:

- Invent durable facts from ambiguous text.
- Guess arbitrary entity boundaries.
- Pick relation types from prose.
- Commit graph memories directly.
- Call a model.

### Put Interpretation Behind A Policy

Interpretation belongs in an extractor policy. The policy can be:

- Index-only: keep evidence searchable, create no graph memory.
- Deterministic: apply source-specific rules with no model calls.
- Agent: ask a model or agent to propose candidate memories.
- Hybrid: run deterministic extraction first, then use an agent for selected
  evidence.
- Review: store candidates for approval without committing.

### Agents Propose, Validators Decide

Agents can help with the hard part: deciding which facts matter. They should not
be allowed to write memory directly.

An agent extractor must return structured candidates with:

- Evidence URI.
- Evidence hash.
- Source span or row range.
- Proposed entity and type.
- Proposed observation and relations.
- Confidence.
- Extractor identity and version.

A deterministic validator then decides whether each candidate is admissible.

### Idempotency Comes Before Throughput

The engine can write very fast. That is only useful if imports avoid duplicate
work. Every bulk path should be safe to rerun:

- Unchanged evidence is skipped.
- Changed evidence is re-indexed and marked for re-extraction.
- Candidates already committed are not committed again.
- Stale candidates tied to old source hashes cannot commit.

### The Context Engine Stays Narrow

The context engine should continue to accept validated `RememberRequest`s and
commit them quickly. It should not own:

- Source-specific parsing.
- Prompt formats.
- Agent retries.
- Review state.
- Evidence ledgers.
- UI approval workflows.

This keeps the engine easy to reason about and protects existing MCP and CLI
write paths.

## Ownership Boundaries

### Evidence Adapter

Owns:

- Source discovery.
- Source decoding.
- Stable URI construction.
- Canonical content hash.
- Batch boundaries.
- Skip reasons.

Does not own:

- Graph entity resolution.
- Relation semantics.
- Agent prompts.
- Memory commit.

### Evidence Ledger

Owns:

- Upsert-by-URI.
- Changed/unchanged detection.
- Raw evidence text indexing.
- Extraction run state.
- Candidate commit state.
- Provenance links.

Does not own:

- Graph recall ranking.
- Entity normalization.
- Consolidation.

### Extractor

Owns:

- Turning evidence into candidate memories.
- Attaching confidence and provenance.
- Identifying itself and its version.

Does not own:

- Final validation.
- Duplicate commit prevention.
- Direct writes.

### Validator And Resolver

Own:

- Candidate schema validation.
- Evidence hash checks.
- Policy checks.
- Candidate hash idempotency.
- Conversion into `RememberRequest`s.

Do not own:

- Source crawling.
- Model calls.
- Background job orchestration.

### Context Engine

Owns:

- Queueing.
- Journaling.
- Shard routing.
- Batched graph commits.
- Durability watermarks.

Does not own:

- Source interpretation.
- Evidence storage.
- Candidate review.

## Non-Goals

This plan does not introduce:

- A required hosted service.
- A required model dependency.
- A universal ontology.
- Automatic deletion of graph memories when source evidence disappears.
- A rewrite of the graph store.
- A default behavior change before compatibility is proven.
- A product UI implementation inside this repository.

## Invariants

- Every candidate memory must cite evidence.
- Every candidate memory must be reproducibly hashable.
- Every committed candidate must be idempotent by hash.
- Every agent result must pass the same validator as deterministic output.
- Network-backed extraction must be optional.
- Existing local CLI and MCP usage must keep working.
- Source adapters must bound memory and avoid unbounded parallelism.
