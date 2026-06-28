# Filesystem watcher

The `openmemory-watch` crate gives `openmemory` an opt-in
filesystem watcher. It walks a directory tree once on startup
(BLAKE3-deduped against the existing metadata store), then tails
`notify-debouncer-full` events to re-index only what changed. The
CLI exposes it as `openmemory watch <PATH>`.

The crate ships behind a default-on `watch` build feature on
`openmemory-cli`; the rest of the workspace builds without it.

## What it does

- **Initial scan.** On startup, walk the tree under `<PATH>` using
  the `ignore` crate (which respects `.gitignore`, `.ignore`, and
  `.openmemory-ignore` files in precedence order). For each
  surviving file: read it, BLAKE3-hash the contents, look up the
  hash in the metadata store. If the hash matches, skip; if not,
  write the file's text into the index and store the new hash.
- **Event loop.** Subscribe to filesystem events via
  `notify-debouncer-full` (200 ms default debounce). For each
  Create / Modify / Remove batch, run `process_file` or
  `remove_path` on each affected path. The same BLAKE3 dedup that
  the initial scan uses applies to events, so a save with no
  byte-level change is a no-op.
- **Concurrent friendliness.** The watcher takes an
  `Arc<MemoryStore>`. A future `openmemory mcp --watch DIR` mode
  can share the running MCP server's store handle without opening
  a second SQLite connection.

## URI shape

Files are indexed under `file://<canonical-absolute-path>`. The
helper:

```rust
pub fn path_to_uri(path: &Path) -> String;
```

The `Watcher::new` constructor canonicalises the watch root once
on startup, so every path the helper sees is already absolute and
the URI is stable across runs. Symlinks and relative paths
normalise to one URI per inode. This means deleting and re-creating
the same file (with the same canonical path) replaces the previous
index entry rather than creating a stale duplicate.

The watcher writes one chunk per file (`chunk_index = 0`) for v0.2.
Multi-chunk file ingestion is tracked for a future release.

## Default extensions

`DEFAULT_EXTENSIONS` (in
[`crates/openmemory-watch/src/lib.rs`](../crates/openmemory-watch/src/lib.rs))
is the curated allowlist for the watcher when no other configuration
overrides it:

```text
md, markdown, mdx, txt, org, rst,
rs, py, js, ts, tsx, jsx,
go, java, c, h, cpp, hpp,
toml, yaml, yml, json
```

The list is biased toward plain-text and source-code formats. PDFs,
DOCX, images, and binaries are not in scope; the watcher reads the
raw bytes directly with no parser stage.

Override sources, in order of precedence:

1. `--exts md,txt,rs` (CLI flag): overrides everything.
2. `[watch] extensions = [...]` in `config.toml`.
3. `DEFAULT_EXTENSIONS` (the compiled fallback).

A file whose extension is not in the active list is filtered out
by the directory walker before `process_file` runs, so it never
produces a `ProcessOutcome` at all.

## Always-ignore paths

Two sets bypass the configurable allowlist entirely:

- `ALWAYS_IGNORE_DIRS = [".git", "target", "node_modules", ".venv", "__pycache__"]`.
  the watcher never enters these directories regardless of
  `.gitignore` content.
- `ALWAYS_IGNORE_GLOBS`: lock files, Python bytecode, editor
  swap/autosave files, `.DS_Store`, and `watchexec.*.log`. These
  names are filtered before extension matching, so permissive custom
  extension lists still do not route scratch files into hashing,
  UTF-8 decoding, SQLite writes, or embedding.

Repository owners can add per-tree rules via a
`.openmemory-ignore` file (`IGNORE_FILE_NAME`). It uses the same
syntax as `.gitignore`. Note: `.openmemory-ignore` rules are
honoured by the **initial scan** but not re-evaluated on every
event in v0.2; that is tracked for v0.3.

## Size cap

Files larger than `WatchOptions::max_size` (default 10 MiB) are
skipped with `SkipReason::TooLarge`. The cap exists so a watched
tree containing a giant log file or video does not stall the event
loop on a multi-second read. CLI override: `--max-size <BYTES>`.

## Dedup with BLAKE3

The metadata store (`metadata.sqlite`, owned by
`openmemory-index`) carries a `content_hash BLOB` column. The
watcher writes the BLAKE3 hash of the file contents on each
successful index. Subsequent scans (including a fresh `openmemory
watch` invocation over the same tree) compare incoming hashes
against the stored hash and skip when they match.

Concretely: a re-run over an unchanged tree is **free**; the only
work is the directory walk plus a hash per file.

## Process outcomes

`process_file` returns one of:

- `ProcessOutcome::Inserted`: first time the watcher has seen this
  URI. Text was read, hashed, written to the index, and the
  metadata-store row was inserted.
- `ProcessOutcome::Updated`: the URI was already in the metadata
  store but the BLAKE3 hash differed. The old engine entry was
  dropped and the new content was indexed.
- `ProcessOutcome::Unchanged`: the BLAKE3 hash matched the stored
  hash; no engine work was done.
- `ProcessOutcome::Skipped(SkipReason::TooLarge | UnreadableUtf8 | EmptyContent)`:
  the file was rejected by the size cap, was not valid UTF-8, or
  had zero bytes.

Read or index failures bubble up as `WatchError` from the
`WatchResult` return value; the caller (`Watcher::scan_initial` and
the event loop) logs them at warn level and continues with the
next path.

`remove_path` returns `WatchResult<bool>`: `true` if either the
search engine or the metadata store had a row for the URI, `false`
if the path was never indexed.

## Initial scan vs. event loop

```rust
pub struct Watcher { /* memory store, root, options */ }

impl Watcher {
    pub fn new(memory: Arc<MemoryStore>, root: PathBuf, options: WatchOptions) -> Self;
    pub fn run(self) -> WatchResult<()>;  // blocks; returns when notified to stop
}
```

`Watcher::run` does:

1. (Unless `--no-initial-scan`) Walk the tree with `ignore::WalkBuilder`
   honouring all the precedence rules above. Call `process_file`
   on each surviving entry. Emit a `ScanReport { files_indexed,
   files_skipped, files_errored }` to the tracing logs at info
   level.
2. Spin up a `notify-debouncer-full` watcher and subscribe to
   create / modify / remove events for `<root>`.
3. For each batch, derive a `BatchSummary { duration,
   events_processed, files_indexed, files_removed }` and dispatch
   `process_file` or `remove_path` per affected path.
4. Loop until the process is signalled (SIGINT / SIGTERM in the
   CLI's case). Every write goes through a SQLite transaction in
   WAL mode, so an abrupt termination is safe.

## CLI surface

See [cli.md](cli.md#openmemory-watch-path) for the full flag
reference. Quick summary:

```bash
openmemory watch ~/notes \
    --exts md,txt \
    --debounce-ms 250 \
    --max-size 5242880   # 5 MiB
```

## Tests

`tests/integration.rs` covers:

- Create / modify / delete event handling.
- BLAKE3 dedup on restart (re-running over an unchanged tree is a
  no-op).
- Ignore-precedence (`.gitignore` and `.openmemory-ignore`).
- Latency smoke test that prints p50/p99 numbers for each
  operation. The numbers are not gated; they are recorded in the
  test output for trend tracking.

The test suite synchronises on the `BatchSummary` notifier channel
rather than `thread::sleep` so tests are deterministic on
contended CI runners.

## Limits and v0.3 follow-ups

- One chunk per file (`chunk_index = 0`). Multi-chunk per-file
  ingestion is tracked for v0.3.
- `.openmemory-ignore` is honoured by the initial scan but not
  re-evaluated on every event.
- No graceful-shutdown signal hook on the CLI side. SIGINT /
  SIGTERM kills the process cleanly because every write goes
  through a SQLite transaction in WAL mode; no half-applied state.
