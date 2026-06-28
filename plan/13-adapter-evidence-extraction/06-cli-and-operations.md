# CLI And Operations

## CLI Goals

The CLI should let users and automation choose how far an import should go:

- Store raw evidence only.
- Run deterministic extraction.
- Run agent extraction.
- Review candidates before commit.
- Commit validated candidates to memory.

The default must remain compatible until the evidence-first path is proven.

## Proposed Command Shape

```text
openmemory ingest <PATH>
  [--format auto|markdown|chat]
  [--mode index|deterministic|agent|hybrid|review]
  [--dry-run]
  [--force-reindex]
  [--force-extract]
  [--max-agent-calls N]
  [--agent-concurrency N]
  [--min-confidence F]
  [--no-normalize]
  [--json]
```

Modes:

| Mode | Behavior |
|------|----------|
| `index` | Upsert evidence and raw index only. No candidates, no graph writes. |
| `deterministic` | Upsert evidence, run deterministic extractor, commit accepted candidates. |
| `agent` | Upsert evidence, run agent extractor, commit accepted candidates. |
| `hybrid` | Run deterministic extraction first, then agent extraction where policy allows. |
| `review` | Upsert evidence and candidates, but do not commit graph writes. |

Compatibility:

- Existing `openmemory ingest <PATH>` should preserve current output and graph
  behavior while compatibility wrappers are in place.
- New modes can be opt-in first.
- The eventual default can become `deterministic` after parity and idempotency
  are proven.

## JSON Report

All ingest modes should produce a structured report under `--json`.

```json
{
  "source": "/tmp/export.jsonl",
  "format": "chat",
  "mode": "hybrid",
  "elapsed_ms": 1234,
  "evidence": {
    "inserted": 10,
    "updated": 2,
    "unchanged": 300,
    "deleted": 0,
    "skipped": 4
  },
  "extraction": {
    "runs": 12,
    "cached": 300,
    "candidates": 22,
    "rejected": 3,
    "held_for_review": 2
  },
  "commit": {
    "requests": 17,
    "already_committed": 0,
    "stale": 0
  }
}
```

Human output should stay concise:

```text
indexed 312 evidence records (300 unchanged); extracted 22 candidates; committed 17 memories
```

## Dry Run

`--dry-run` should:

- Read source.
- Build evidence records.
- Report insert/update/unchanged predictions.
- Optionally run extraction depending on mode.
- Validate candidates.
- Not mutate evidence ledger, raw index, candidate store, or graph.

If a true no-write dry run is too costly in the first implementation, call that
out explicitly and start with `--mode review` for non-graph mutation.

## Force Flags

`--force-reindex`:

- Rebuild raw evidence index even when hashes match.
- Does not imply re-extraction.

`--force-extract`:

- Re-run extractor for current evidence hashes even when cached results exist.
- Does not commit duplicate candidates because candidate hashes still gate
  writes.

`--max-agent-calls`:

- Hard cap for a command invocation.
- When exceeded, remaining evidence is skipped or held depending on policy.

## Review Operations

Initial CLI review surface can be simple:

```text
openmemory ingest candidates list [--source <URI-prefix>] [--status held]
openmemory ingest candidates accept <CANDIDATE_HASH>
openmemory ingest candidates reject <CANDIDATE_HASH> --reason <TEXT>
```

The desktop product can later build a richer review inbox on top of the same
state.

## Job Model

Long-running imports should become daemon jobs when invoked from the daemon or
desktop product.

Job stages:

- Discover.
- Read/hash.
- Evidence upsert.
- Raw index update.
- Extract.
- Validate.
- Commit.
- Reconcile.

Each stage should emit progress:

```json
{
  "job_id": "job_...",
  "stage": "extract",
  "processed": 1200,
  "total": 5000,
  "message": "running deterministic extractor"
}
```

CLI can run synchronously first. The internal stage model should still be clear
so daemon integration does not require a rewrite.

## Observability

Counters:

- Evidence inserted, updated, unchanged, skipped, deleted.
- Bytes read.
- Hash time.
- Raw index time.
- Extraction runs, cached hits, failures.
- Agent calls, retries, timeouts.
- Validation accepted, rejected, held.
- Commit requests and durable latency.

Logs:

- Source path or URI.
- Structured skip reason.
- Extractor ID.
- Candidate rejection reason.
- Agent provider errors without memory content.

Do not log:

- Full memory content by default.
- Raw chat text.
- Secrets.
- Bearer tokens.
- Full local paths in telemetry.

## Error Shape

Errors should include source context:

- File path.
- Line number.
- URI.
- Source type.
- Stage.

Examples:

```text
chat export line 42: missing required field `text`
markdown file /notes/q3.md: invalid UTF-8
extractor openai:gpt-... timed out after 30s for evidence URI ...
candidate ... rejected: evidence hash is stale
```

Errors in one evidence record should not necessarily abort the whole import.
Policy should choose fail-fast or best-effort behavior.

## Configuration

CLI flags should come first. Config can follow once behavior settles.

Possible config:

```toml
[ingest]
default_mode = "deterministic"
fail_fast = false

[ingest.agent]
enabled = false
concurrency = 2
timeout_ms = 30000
max_candidates_per_record = 8
min_confidence = 0.75

[ingest.markdown]
mode = "deterministic"
max_chunk_bytes = 32768

[ingest.chat_jsonl]
mode = "hybrid"
grouping = "thread-or-time-window"
```

No config should make network-backed extraction mandatory for local operation.
