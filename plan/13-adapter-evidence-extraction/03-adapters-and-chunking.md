# Adapters And Chunking

## EvidenceAdapter Contract

New source connectors should implement an evidence contract:

```rust
pub trait EvidenceAdapter {
    fn name(&self) -> &'static str;
    fn next_batch(&mut self) -> MemoryResult<EvidenceBatch>;
}
```

Rules:

- `Ok(batch)` yields a bounded set of evidence records.
- `Ok(empty batch)` means exhausted.
- Errors must include path, line, row, or source context where available.
- The adapter must not commit graph memory.
- The adapter must not call an agent or model.
- The adapter must not perform unbounded recursion or unbounded buffering.

## Adapter Responsibilities

Adapters own:

- Source discovery.
- Source-specific decoding.
- Canonical text construction.
- Stable URI construction.
- Hash construction.
- Chunk boundaries.
- Metadata preservation.
- Skip decisions.

Adapters do not own:

- Entity extraction.
- Relation extraction.
- Memory tier decisions.
- Confidence scoring.
- Candidate validation.
- Commit idempotency.

## Markdown Evidence Adapter

Current behavior turns one file into one event entity and `##` sections into
observations. The evidence-first behavior should start earlier.

### Source Units

Small Markdown files:

- One evidence record per file.
- URI: `file://<canonical-path>#chunk=0`.
- `title`: first H1 if present.
- `text`: canonical Markdown text.
- `source_files`: canonical path.
- `metadata_json`: frontmatter fields, headings, file size, modified time if
  needed for audit but not content hash.

Large Markdown files:

- Split by heading boundaries first.
- URI includes heading slug and chunk index.
- Preserve parent file URI.
- Include heading path in metadata.

### Deterministic Metadata

The adapter can preserve:

- H1 title.
- Frontmatter.
- Heading hierarchy.
- Local path.
- File stem.
- Explicit `Attendees:` line as source metadata, not automatically as a graph
  relation at the adapter layer.

### Skip Rules

Skip:

- Empty files.
- Non-UTF-8 files.
- Files above configured max size unless chunking can safely stream them.
- Always-ignored directories such as `.git`, `target`, `node_modules`, `.venv`,
  and `__pycache__`.

Extension matching should be case-insensitive.

## Chat JSONL Evidence Adapter

The current adapter creates one channel entity per message. That is too eager for
production imports. Chat should first become evidence.

### Source Units

If thread IDs are available:

- Group rows by channel and thread ID.
- Chunk long threads by token or byte budget.
- URI: `chat-jsonl://<canonical-path>#channel=<channel>&thread=<id>&chunk=<n>`.

If only flat rows are available:

- Start with one evidence record per row for compatibility.
- Add a grouping mode by channel and time window before enabling agent
  extraction by default.
- URI: `chat-jsonl://<canonical-path>#line=<n>`.

### Metadata

Preserve:

- Channel.
- User or sender.
- Timestamp.
- Thread ID.
- Message ID.
- Row number.
- Source file path.
- Raw source object fields not included in canonical text.

### Validation At Adapter Layer

The adapter can reject malformed source rows:

- Missing channel when the source format requires it.
- Missing user when the source format requires it.
- Missing text when no other extractable payload exists.
- Invalid timestamp type.

It should not decide whether a message is memory-worthy.

## Transcript Adapter

Future transcript evidence should be chunked by:

- Speaker turn boundaries.
- Timestamp windows.
- Max token or byte budget.
- Topic boundaries when reliable markers exist.

Metadata:

- Speaker.
- Start and end timestamps.
- Recording or transcript URI.
- Segment index.
- Confidence from transcription if available.

Agent extraction is likely useful here, but it remains an extractor policy, not
an adapter feature.

## Ticket And Issue Adapter

Future issue tracker evidence should preserve source-native structure.

Evidence units:

- Issue body.
- Comment.
- Review thread.
- Status transition.
- Label change.

Metadata:

- Repository.
- Issue number.
- Comment ID.
- Author.
- Created and updated timestamps.
- Labels.
- State.
- URL.

Deterministic extraction may be sufficient for status and labels. Agent
extraction may be useful for decisions, action items, and rationale.

## Calendar Adapter

Evidence units:

- Event.
- Event update.
- Meeting notes attachment.

Metadata:

- Calendar ID.
- Event ID.
- Organizer.
- Attendees.
- Start and end time.
- Location.
- Recurrence ID.

Calendar attendees are source-native metadata. Whether they become graph
relations is an extraction policy decision.

## Chunking Rules

Chunking should preserve semantic boundaries when possible:

1. Native object boundary.
2. Thread or event boundary.
3. Heading or section boundary.
4. Speaker turn boundary.
5. Time window.
6. Token or byte window with overlap.

Every chunk must preserve:

- Parent URI.
- Chunk index.
- Source span or row range.
- Hash over canonical chunk content.

Avoid:

- Splitting in the middle of UTF-8 characters.
- Splitting in a way that loses source timestamp context.
- One model call per tiny message when grouping would preserve context.
- One unbounded chunk for large sources.

## Discovery And Ignore Rules

File-based adapters should reuse watcher lessons:

- Case-insensitive extension allowlists.
- Standard ignore files where appropriate.
- Always-skip noisy directories.
- Max-size guardrails.
- Non-UTF-8 rejection.
- Deterministic sorted traversal when order matters.

Directory scans should use iterative walking or a proven walker, not unbounded
recursive calls.

## Batch And Resource Rules

Adapters should:

- Bound batch size.
- Bound total text bytes per batch.
- Avoid loading an entire huge corpus before yielding.
- Avoid one OS thread per source file.
- Use worker pools only where they provide measurable value.
- Preserve deterministic output order unless streaming source semantics require
  otherwise.

Initial defaults:

| Setting | Default |
|---------|---------|
| Evidence batch size | 256 records |
| Max file read workers | `min(available_parallelism, 8)` |
| Max evidence text per batch | Configurable, start conservative |
| Extension matching | Case-insensitive |

## Compatibility Wrappers

During migration, the existing `SourceAdapter` can be preserved as a wrapper:

```text
EvidenceAdapter -> deterministic compatibility extractor -> RememberRequest
```

This allows tests to prove that Markdown and chat produce the same graph writes
as today while the internal seam changes.
