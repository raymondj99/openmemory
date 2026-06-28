# Evidence Model

## Purpose

Evidence is the durable source record from which memory candidates are derived.
It is not itself a graph memory. It is the audit layer between external data and
the semantic graph.

The evidence model must support:

- Stable identity across reruns.
- Cheap changed/unchanged detection.
- Raw source search.
- Extraction caching.
- Candidate idempotency.
- Review and debugging.
- Future retraction or supersession policies.

## EvidenceRecord

Initial Rust shape:

```rust
pub struct EvidenceRecord {
    pub uri: String,
    pub source_type: String,
    pub external_id: Option<String>,
    pub parent_uri: Option<String>,
    pub chunk_index: u32,
    pub text: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub observed_at: Option<i64>,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub source_files: Vec<String>,
    pub metadata_json: serde_json::Value,
    pub content_hash: [u8; 32],
}
```

Field rules:

| Field | Rule |
|-------|------|
| `uri` | Stable, unique source-unit identifier. Human-debuggable where possible. |
| `source_type` | Source family tag, for example `file:markdown` or `chat:jsonl`. |
| `external_id` | Vendor or source-native ID when one exists. |
| `parent_uri` | Parent file, thread, conversation, ticket, or transcript. |
| `chunk_index` | Zero-based index within the parent. |
| `text` | Canonical UTF-8 text used for indexing and extraction. |
| `title` | Source-native title or derived heading, not a graph entity guess. |
| `author` | Source-native author/speaker/sender when unambiguous. |
| `observed_at` | When source evidence was observed by OpenMemory, if known. |
| `valid_from` | Source-native start timestamp, if known. |
| `valid_until` | Source-native end timestamp, if known. |
| `source_files` | Local file paths or source URLs that establish provenance. |
| `metadata_json` | Structured source metadata that should not be flattened into text. |
| `content_hash` | BLAKE3 over canonical text plus extraction-relevant metadata. |

## URI Conventions

URIs should be stable and easy to inspect.

Recommended forms:

| Source | URI |
|--------|-----|
| Markdown file | `file://<canonical-path>#chunk=<n>` |
| Markdown heading chunk | `file://<canonical-path>#heading=<slug>&chunk=<n>` |
| Chat JSONL row | `chat-jsonl://<canonical-path>#line=<n>` |
| Chat thread chunk | `chat-jsonl://<canonical-path>#channel=<c>&thread=<id>&chunk=<n>` |
| Transcript chunk | `transcript://<canonical-path>#chunk=<n>` |
| Calendar event | `calendar://<calendar-id>/<event-id>` |
| GitHub issue | `github://<owner>/<repo>/issues/<number>` |
| GitHub comment | `github://<owner>/<repo>/issues/<number>#comment=<id>` |

URI rules:

- Use canonical absolute file paths for local files.
- Preserve vendor IDs where available.
- Do not use random IDs unless the source has no stable identifier.
- Do not include secrets or tokens.
- Keep URI generation deterministic and covered by tests.

## Hashing

Use BLAKE3 for evidence content hashes, matching the watcher precedent.

The hash input should include:

- Canonical text.
- Source type.
- External ID, if present.
- Title, author, timestamps, and source-native metadata that can affect
  extraction.
- Chunk boundaries.

The hash input should not include:

- Local scan time.
- Ingestion batch number.
- Non-semantic ordering noise.
- Absolute path when a stable vendor ID is the true identity, unless path is the
  source identity.

The hash must be stable across process runs and platforms for the same canonical
source.

## EvidenceBatch

```rust
pub struct EvidenceBatch {
    pub records: Vec<EvidenceRecord>,
    pub checkpoint: Option<EvidenceCheckpoint>,
}
```

Batch rules:

- Empty batch means exhausted.
- Batches should be bounded by record count and memory footprint.
- Streaming adapters can attach checkpoints.
- File adapters can initially leave checkpoints empty.

## Evidence Outcomes

Upserting evidence should produce explicit outcomes:

```rust
pub enum EvidenceOutcome {
    Inserted,
    Updated,
    Unchanged,
    Deleted,
    Skipped(EvidenceSkipReason),
}
```

Skip reasons should be structured:

```rust
pub enum EvidenceSkipReason {
    TooLarge,
    EmptyContent,
    UnreadableUtf8,
    UnsupportedFormat,
    IgnoredPath,
    MalformedRecord,
}
```

These outcomes feed CLI reports, logs, tests, and review workflows.

## Evidence Ledger Responsibilities

The ledger owns:

- Upsert by `uri`.
- Current `content_hash`.
- Source type and metadata.
- Created and updated timestamps.
- Deleted or stale status.
- Extraction status per evidence hash and extractor identity.
- Candidate commit status.
- Links from graph rows back to evidence.

The ledger must answer:

- Is this evidence new, changed, unchanged, or deleted?
- Which extractor versions have already processed this hash?
- Which candidates were produced?
- Which candidates were accepted, rejected, committed, or superseded?
- Which graph observations or relations came from this evidence?

## Storage Phase 1

Start by reusing existing index metadata concepts where possible:

- URI.
- Source kind.
- Source type.
- Content hash.
- Size bytes.
- Status.
- Created and updated timestamps.

Raw evidence text can be indexed in the existing hybrid index so `openmemory`
can search imported evidence even before extraction commits graph memories.

This reduces migration risk for the first implementation pass.

## Storage Phase 2

Add explicit evidence tables after the shape is proven.

```sql
CREATE TABLE evidence_records (
  uri TEXT PRIMARY KEY,
  source_type TEXT NOT NULL,
  external_id TEXT,
  parent_uri TEXT,
  chunk_index INTEGER NOT NULL,
  content_hash BLOB NOT NULL,
  title TEXT,
  author TEXT,
  observed_at INTEGER,
  valid_from INTEGER,
  valid_until INTEGER,
  source_files_json TEXT NOT NULL,
  metadata_json TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
```

```sql
CREATE TABLE extraction_runs (
  id TEXT PRIMARY KEY,
  evidence_uri TEXT NOT NULL,
  evidence_hash BLOB NOT NULL,
  extractor_name TEXT NOT NULL,
  extractor_version TEXT NOT NULL,
  model TEXT,
  prompt_hash BLOB,
  status TEXT NOT NULL,
  error TEXT,
  created_at INTEGER NOT NULL,
  completed_at INTEGER,
  UNIQUE (
    evidence_uri,
    evidence_hash,
    extractor_name,
    extractor_version,
    model,
    prompt_hash
  )
);
```

```sql
CREATE TABLE extraction_candidates (
  candidate_hash BLOB PRIMARY KEY,
  run_id TEXT NOT NULL,
  evidence_uri TEXT NOT NULL,
  evidence_hash BLOB NOT NULL,
  candidate_json TEXT NOT NULL,
  confidence REAL NOT NULL,
  status TEXT NOT NULL,
  rejection_reason TEXT,
  created_at INTEGER NOT NULL
);
```

```sql
CREATE TABLE evidence_memory_links (
  candidate_hash BLOB NOT NULL,
  evidence_uri TEXT NOT NULL,
  observation_id TEXT,
  relation_id TEXT,
  entity_id TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(candidate_hash, evidence_uri)
);
```

## Placement Decision

There are two reasonable homes:

| Option | Benefit | Cost |
|--------|---------|------|
| Index metadata database | Aligns evidence with raw searchable source records and watcher precedent. | Graph-row links cross database boundaries. |
| Graph database | Allows transactional links between candidates and graph rows. | Pulls raw source lifecycle closer to graph semantics. |

Recommendation:

- Phase 1: reuse index metadata and raw index.
- Phase 2: decide after idempotent extraction and candidate linking are proven.
- If retraction becomes a core feature, move evidence-memory links closer to the
  graph transaction.

## Provenance On Graph Writes

Every committed observation should carry:

- `source_kind`: extraction source family, for example `chat-message` or
  `meeting-note`.
- `source_files`: file path, vendor URL, or evidence URI where appropriate.
- `source`: extractor or ingestion source tag.

The link table should provide stronger provenance than `source_files` alone:

- Candidate hash.
- Evidence URI.
- Graph observation ID.
- Graph relation ID.
- Graph entity ID.

## Deletion And Staleness

Default behavior should be non-destructive:

- Deleted evidence is marked deleted.
- Raw index rows for that evidence are removed.
- Existing graph memories stay in place with provenance.

Explicit modes can later support:

- `--retract-deleted`: tombstone graph rows linked only to deleted evidence.
- `--supersede-changed`: mark candidates from old hashes stale.
- Review queue: show memories whose evidence is gone or changed.

Do not silently delete graph memory when source evidence disappears.
