# Research Basis

Research date: 2026-06-28.

## Verdict

The evidence-first adapter plan checks out against current agent-memory research
and production ingestion practice.

The strongest alignment is with systems that separate raw episodic storage from
semantic memory extraction. In the plan's terms:

```text
EvidenceRecord ~= episode / source document / raw memory event
ExtractionCandidate ~= proposed semantic memory
Validator + Resolver ~= write policy and idempotency gate
ContextEngine ~= durable graph commit lane
```

This is consistent with recent memory systems such as Zep/Graphiti and Mem0,
and with production RAG ingestion systems such as LlamaIndex and LangChain,
which use stable source IDs, content hashes, document managers, and upsert or
skip logic to avoid duplicate indexing work.

The plan should keep the term `evidence` because it is clearer for audit and
source provenance, but implementation docs should explicitly map evidence to
the "episode" terminology used in temporal agent-memory systems.

## Sources Reviewed

| Source | Type | Relevant finding |
|--------|------|------------------|
| [Memory for Autonomous LLM Agents: Mechanisms, Evaluation, and Emerging Frontiers](https://arxiv.org/abs/2603.07670) | 2026 survey | Frames agent memory as a write-manage-read loop and calls out engineering realities around write filtering, contradiction handling, latency, and privacy. |
| [Mem0: Building Production-Ready AI Agents with Scalable Long-Term Memory](https://arxiv.org/abs/2504.19413) | 2025 paper | Uses dynamic extraction, consolidation, retrieval, and graph memory; reports major latency and token-cost reductions versus full-context approaches. |
| [Zep: A Temporal Knowledge Graph Architecture for Agent Memory](https://arxiv.org/html/2501.13956v1) | 2025 paper | Uses raw episodic nodes as a non-lossy store, then extracts semantic entities and relations with source links and temporal validity. |
| [Graphiti overview](https://help.getzep.com/graphiti/getting-started/overview) | Official docs | Describes dynamic temporal context graphs built from structured and unstructured data, with changing relationships and historical context. |
| [LangGraph memory overview](https://docs.langchain.com/oss/python/concepts/memory) | Official docs | Separates short-term thread state from long-term memory and describes semantic, episodic, and procedural memory types. |
| [LangMem conceptual guide](https://langchain-ai.github.io/langmem/concepts/conceptual_guide/) | Official docs | Describes memory operations as accepting conversations and current state, prompting an LLM to update or consolidate memory, then returning updated state. |
| [LlamaIndex ingestion pipeline document management](https://developers.llamaindex.ai/python/examples/ingestion/document_management_pipeline/) | Official docs | Uses doc IDs and document hashes to detect duplicates, reprocess changed documents, and skip unchanged documents. |
| [LangChain indexing API announcement](https://www.langchain.com/blog/syncing-data-sources-to-vector-stores) | Official docs/blog | Uses a record manager with document hash, write time, and source ID to avoid duplicate content, unchanged rewrites, and redundant embeddings. |
| [Weaviate batch import docs](https://docs.weaviate.io/weaviate/manage-objects/import) | Official docs | Recommends deterministic UUIDs for object IDs to prevent duplicate IDs during batch import. |
| [Qdrant points docs](https://qdrant.tech/documentation/manage-data/points/) | Official docs | Point loading is idempotent; re-uploading the same ID overwrites the point, useful even with non-exactly-once queues. |
| [Pinecone upsert docs](https://docs.pinecone.io/reference/api/2026-01.alpha/data-plane/upsert) | Official docs | Upsert overwrites an existing record with the same ID and recommends batching up to documented limits. |
| [Unstructured chunking docs](https://docs.unstructured.io/open-source/core-functionality/chunking) | Official docs | Chunking should use document elements and metadata, preserving semantic units before falling back to text splitting. |
| [Microsoft GraphRAG provenance note](https://www.microsoft.com/en-us/research/blog/graphrag-unlocking-llm-discovery-on-narrative-private-data/) | Research blog | Emphasizes source grounding and provenance so users can audit LLM output against original source material. |

## Alignment With Agent-Memory Research

### Write, Manage, Read Loop

The 2026 agent-memory survey frames memory as a loop:

```text
write -> manage -> read
```

The plan's stages map cleanly:

| Research concept | Plan stage |
|------------------|------------|
| Write | Evidence adapter, ledger upsert, candidate extraction. |
| Manage | Validator, resolver, idempotency gate, review states, consolidation. |
| Read | Existing graph recall, raw evidence search, future provenance traversal. |

This supports the decision to avoid writing raw adapter output directly into the
semantic graph. The write path needs policy, not just throughput.

### Episodic Plus Semantic Memory

Zep/Graphiti is the closest architecture match. Its paper describes raw
episodes containing messages, text, or JSON as a non-lossy store. Semantic
entities and relations are extracted from those episodes. It also keeps links
between semantic artifacts and source episodes so facts can be traced back for
citation or quotation.

That directly supports:

- `EvidenceRecord` as raw source evidence.
- `ExtractionCandidate` as proposed semantic memory.
- Evidence URI and hash required for every candidate.
- Provenance links from graph rows back to evidence.
- Temporal fields on evidence and observations.

The plan should explicitly preserve both directions:

- Given evidence, find candidates and committed memories.
- Given a memory, find its source evidence and source hash.

### Dynamic Extraction And Consolidation

Mem0 validates selective extraction and consolidation rather than full-context
storage. The paper reports large latency and token-cost improvements compared
with processing entire histories. It also adds graph memory for relational
structure.

That supports:

- Agent extraction as a bounded optional stage.
- Hybrid deterministic plus agent extraction.
- Candidate consolidation and duplicate prevention before commit.
- Graph relations as first-class output, but only after validation.

The plan's "agent proposes, validator decides" stance is stricter than many
prototype memory systems, and that is appropriate for a local persistent memory
tool.

### Application-Specific Memory

LangGraph and LangMem both emphasize that long-term memory is not one-size-fits
all. LangMem describes a pattern where conversations and current memory state
are passed to an LLM to decide how memory should expand or consolidate.

That supports:

- Source-specific adapter metadata.
- Policy-based extraction modes.
- Per-source configuration.
- Review mode for low-confidence or domain-sensitive memories.

It also argues against a single generic adapter schema that pretends all sources
are equivalent.

## Alignment With Production Ingestion Systems

### Stable IDs And Hashes

LlamaIndex document management stores a mapping from document ID to document
hash. When the same ID appears again, changed hashes trigger reprocessing and
unchanged hashes are skipped.

LangChain's indexing API uses a record manager that stores document hash, write
time, and source ID. Its goal is to avoid duplicate writes, avoid rewriting
unchanged content, and avoid recomputing embeddings.

This directly supports:

- Stable evidence URI.
- BLAKE3 evidence hash.
- Inserted, updated, unchanged outcomes.
- Evidence ledger before graph writes.
- Rerun safety as a required production property.

### Upsert And Idempotency

Vector databases converge on the same operational pattern:

- Weaviate recommends deterministic UUIDs to prevent duplicate IDs during batch
  import.
- Qdrant documents point loading as idempotent when IDs are reused.
- Pinecone upsert overwrites existing records with the same ID.

This supports candidate hashes and source URIs as first-class IDs. The graph
write path currently appends observations, so OpenMemory needs its own
idempotency gate before `RememberRequest` submission. Relying on the graph write
path alone would be behind production ingestion practice.

### Chunk By Source Semantics First

Unstructured's chunking documentation argues for using document-format knowledge
to partition into semantic units, falling back to text splitting only when an
element is too large.

This supports:

- Markdown chunking by headings before byte windows.
- Chat chunking by thread or time window before line-by-line extraction.
- Transcript chunking by speaker turns and timecodes.
- Source metadata preserved with every chunk.

This also weakens the idea that adapters should emit a generic memory shape.
Adapters should preserve source-native structure and hand coherent evidence
chunks to extraction.

### Provenance Is Product-Critical

Microsoft's GraphRAG writeup emphasizes source grounding and provenance so users
can audit LLM output against the source material. Graphiti similarly links
semantic artifacts back to raw episodes.

This supports:

- Evidence URI and hash on every candidate.
- Source span or row range.
- Link table from committed graph rows to evidence.
- Review workflows that show source text beside proposed memory.

## Where The Plan Is Strong

The current plan is well aligned on these points:

- Evidence-first ingestion.
- Strict separation between connector, extractor, validator, resolver, and
  commit lane.
- Stable source identity and content hashing.
- Changed/unchanged detection.
- Agent extraction as optional and bounded.
- Candidate hashes for exact idempotency.
- Review mode before graph commit.
- Source-specific chunking.
- Temporal fields and provenance links.
- Existing context engine remains narrow.

## Recommended Plan Refinements

### 1. Name The Evidence Layer As Episodic Evidence

Add a short glossary:

```text
EvidenceRecord: OpenMemory's local term for a raw episodic source unit.
Episode: research term used by Zep/Graphiti for raw message/text/JSON memory.
Semantic memory: graph entity, observation, or relation derived from evidence.
```

This helps reviewers map the plan to the literature.

### 2. Make Source Hashes Transformation-Aware

LangChain and LlamaIndex both account for transformed documents and metadata.
OpenMemory should hash:

- Canonical text.
- Source metadata used by extraction.
- Chunking strategy version.
- Adapter version.

If chunking changes, old evidence chunks should not silently reuse the same
extraction cache.

### 3. Add Cleanup Modes Explicitly

LangChain exposes cleanup behavior because stale source-derived records are a
hard production problem. OpenMemory should name its modes:

- `none`: never retract graph memory automatically.
- `raw-index`: delete or replace raw evidence index rows only.
- `stale-candidates`: mark old candidates stale.
- `review`: enqueue memories whose evidence disappeared or changed.
- `retract`: opt-in tombstoning of graph rows linked only to deleted evidence.

The current plan has the behavior, but naming the modes will make CLI and daemon
implementation cleaner.

### 4. Keep Agent Extraction Off The Hot Path By Default

LangGraph's docs call out tradeoffs between updating memory in the hot path and
as background work. OpenMemory should default bulk imports to deterministic or
review mode unless the user explicitly enables agent extraction.

Agent extraction belongs in a bounded job with progress and resume state.

### 5. Treat Temporal Validity As A First-Class Extraction Concern

Zep/Graphiti's temporal model is a major differentiator. OpenMemory already has
`valid_from` and `valid_until`; adapter evidence should preserve source
timestamps and extraction should prefer temporal observations when the source
supports them.

### 6. Add Evaluation Fixtures For Memory Quality

Mem0 and Zep evaluate against long-memory benchmarks. OpenMemory should not
only benchmark ingestion throughput. It should add small quality fixtures:

- Chat conversation -> expected durable facts.
- Meeting notes -> expected decisions and action items.
- Changed source -> stale candidate behavior.
- Conflicting source -> review or contradiction handling.

These do not need to become a full LoCoMo runner immediately, but the design
should create hooks for quality evaluation.

## Does Anything Contradict The Plan?

No major source contradicted the architecture.

The main tension is terminology and default behavior:

- Research systems often call raw evidence "episodes".
- Production RAG systems talk about source documents, records, and docstores.
- OpenMemory's plan uses "evidence" to emphasize auditability.

That is acceptable, but the implementation should keep the mapping explicit.

The other tension is graph retraction. Production vector systems often replace
or delete stale indexed records aggressively. Persistent agent memory is more
sensitive: deleting a source file should not silently delete a user memory. The
plan's conservative default is correct, as long as raw evidence index cleanup
and stale candidate marking are still explicit.

## Implementation Implications

The research supports these near-term implementation choices:

1. Build evidence types before agent extraction.
2. Use stable URIs and BLAKE3 hashes as source identity.
3. Include adapter and chunking version in the hash input.
4. Store raw evidence separately from semantic graph memory.
5. Preserve links from graph memories back to evidence.
6. Make extraction cache keys include evidence hash and extractor identity.
7. Keep direct `SourceAdapter -> RememberRequest` only as compatibility.
8. Add exact idempotency before semantic dedup.
9. Add source-specific chunking before generic text windows.
10. Gate agent extraction behind config, feature flags, and reviewable output.

## Final Assessment

The plan is not over-engineered relative to the state of the art. It is the
minimum production version of the same pattern now appearing in serious memory
systems:

- Raw episodic/evidence store.
- Stable source identity and hash.
- Semantic extraction as a managed stage.
- Graph memory with provenance and time.
- Idempotent ingestion and update handling.
- Optional agent reasoning behind validation.

The design is directionally correct. The most important next step is to
implement the smallest evidence seam without changing current user-visible
ingest behavior, then prove rerun idempotency before adding agent extraction.
