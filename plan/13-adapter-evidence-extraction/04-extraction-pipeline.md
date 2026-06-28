# Extraction Pipeline

## Purpose

Extraction turns evidence into candidate memories. It is the first stage where
the system is allowed to interpret source text.

Extraction output is not durable memory. It is a proposal that must pass
validation, idempotency checks, and resolution before the graph write path sees
it.

## Extractor Modes

### Index Only

Evidence is stored and indexed. No candidates are produced.

Use this mode for:

- First-time large imports.
- Sources where raw search is enough.
- Cost-sensitive users.
- Debugging adapter output.

### Deterministic

Deterministic extractors use source-specific rules.

Examples:

- A Markdown H1 can become a candidate title.
- An explicit `Attendees:` field can become candidate participant relations.
- Chat sender metadata can become source attribution.
- Ticket status fields can become candidate facts.

Deterministic extraction must be:

- Pure for the same evidence hash.
- Fast.
- Network-free.
- Covered by fixture tests.

### Agent

Agent extractors call a model or agent to propose candidates from evidence.

Use this mode for:

- Messy chat threads.
- Long transcripts.
- Meeting notes without consistent formatting.
- Tickets with important rationale buried in prose.

Agent extraction must be:

- Optional.
- Feature-gated.
- Timeout-bounded.
- Concurrency-bounded.
- Cached by evidence hash and extractor identity.
- Validated before commit.

### Hybrid

Hybrid extraction runs deterministic rules first, then uses an agent only where
policy says the extra cost is justified.

Examples:

- Deterministically parse participants and timestamps, then ask an agent for
  durable decisions and action items.
- Deterministically parse issue status, then ask an agent for root cause and
  resolution summary.

### Review

Review mode stores candidates without committing graph memory.

Use this mode for:

- New extractor rollouts.
- Low-confidence candidates.
- Sources with high privacy or correctness risk.
- Product UI review inboxes.

## Extractor Trait

Initial shape:

```rust
pub trait Extractor {
    fn id(&self) -> ExtractorId;
    fn extract(&self, evidence: &[EvidenceRecord]) -> MemoryResult<Vec<ExtractionCandidate>>;
}
```

Async agent extractors can be adapted behind a job runner. Do not require async
in the first core trait unless the surrounding implementation already needs it.

## Extractor Identity

```rust
pub struct ExtractorId {
    pub name: String,
    pub version: String,
    pub model: Option<String>,
    pub prompt_hash: Option<[u8; 32]>,
}
```

Identity rules:

- Deterministic extractors must bump `version` when output can change.
- Agent extractors must include model and prompt hash.
- Extractor identity participates in extraction-run caching.
- Candidate hash includes extractor identity.

## Candidate Shape

```rust
pub struct ExtractionCandidate {
    pub evidence_uri: String,
    pub evidence_hash: [u8; 32],
    pub span: Option<TextSpan>,
    pub entity_name: String,
    pub entity_type: EntityType,
    pub observation: Option<ObservationCandidate>,
    pub relations: Vec<RelationCandidate>,
    pub confidence: f32,
    pub extractor: ExtractorId,
    pub candidate_hash: [u8; 32],
}
```

Observation candidate:

```rust
pub struct ObservationCandidate {
    pub content: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub importance: Option<f32>,
    pub source_kind: Option<String>,
    pub concepts: Vec<String>,
    pub memory_tier: Option<MemoryTier>,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
}
```

Relation candidate:

```rust
pub struct RelationCandidate {
    pub relation_type: String,
    pub target_name: String,
    pub target_type: EntityType,
    pub confidence: Option<f32>,
}
```

## Candidate Hash

The candidate hash should be computed from canonical data:

- Evidence URI.
- Evidence hash.
- Span.
- Entity name and type.
- Observation fields.
- Relation fields in stable order.
- Extractor identity.

Use a canonical serialization, not ad hoc string concatenation.

The hash should not include:

- Wall-clock extraction time.
- Batch number.
- Non-semantic JSON field order.
- Transient error or retry state.

## Agent Request Contract

The agent receives:

- Evidence URI.
- Source type.
- Canonical text.
- Source metadata.
- Allowed entity types.
- Allowed relation vocabulary, if configured.
- Maximum candidate count.
- Examples of acceptable and rejected candidates.
- Instruction to emit no candidates when the evidence is not durable memory.

The agent must return strict JSON.

Example:

```json
{
  "candidates": [
    {
      "evidence_uri": "chat-jsonl:///tmp/export.jsonl#line=42",
      "span": {"start": 12, "end": 97},
      "entity": {"name": "Context Engine", "type": "project"},
      "observation": {
        "content": "Context Engine rollout is blocked on journal replay validation.",
        "title": "Journal replay validation blocks rollout",
        "source_kind": "chat-message",
        "importance": 0.72,
        "memory_tier": "episodic"
      },
      "relations": [
        {
          "type": "blocked_by",
          "target": {"name": "Journal Replay Validation", "type": "concept"}
        }
      ],
      "confidence": 0.82
    }
  ]
}
```

## Agent Guardrails

Agent extraction must enforce:

- Strict JSON schema.
- Maximum candidates per evidence record.
- Maximum text bytes or tokens per request.
- Timeout per request.
- Retry budget.
- Concurrency limit.
- Provider-neutral error mapping.
- Cache lookup before model call.
- No graph write permission.

Reject responses that:

- Are not valid JSON.
- Contain unknown entity types.
- Omit evidence URI.
- Cite evidence that was not supplied.
- Cite spans outside the evidence text.
- Exceed candidate count.
- Fall below confidence threshold.
- Contain empty names, observations, or relation fields.

## Prompt Versioning

Prompt changes can alter output. Treat prompt text as code:

- Store prompt templates in versioned files.
- Hash the exact prompt body used for extraction.
- Include prompt hash in `ExtractorId`.
- Add golden tests for fake agent responses.
- Document prompt purpose and non-goals.

## Extraction Caching

Cache key:

```text
(evidence_uri, evidence_hash, extractor_name, extractor_version, model, prompt_hash)
```

Cache states:

- Pending.
- Completed.
- Failed retryable.
- Failed terminal.
- Skipped by policy.

Failed agent calls should not block deterministic extraction. Retry behavior
must be explicit and bounded.

## Policy Selection

Policy should be configurable by source type:

```toml
[ingest.markdown]
mode = "deterministic"

[ingest.chat_jsonl]
mode = "hybrid"
agent_concurrency = 2
min_confidence = 0.75
max_candidates_per_record = 8
```

The first implementation can expose this through CLI flags before adding config
surface.
