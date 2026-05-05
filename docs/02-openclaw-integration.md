# OpenClaw integration

This is the contract between `open-memory` and OpenClaw. Everything in
this file is observable from outside the binary: file paths, config
snippets, MCP tool names, error shapes. Anything we change here is a
breaking change.

## Why OpenClaw is the first-class consumer

OpenClaw is an open-source personal AI assistant. It speaks MCP for
external tool integration, ships its config at `~/.openclaw/openclaw.json`
(JSON5; comments + trailing commas allowed) with managed outbound MCP
server definitions under `mcp.servers`, and exposes a CLI for
programmatic config (`openclaw mcp list | show | set | unset`). That
gives us a clean "drop in and go" target without ad-hoc IPC or bespoke
transports.

The OpenClaw contract surfaces we depend on (sourced from
[`docs.openclaw.ai/gateway/configuration`](https://docs.openclaw.ai/gateway/configuration)
and [`docs.openclaw.ai/cli/mcp`](https://docs.openclaw.ai/cli/mcp)):

- **Config file.** `~/.openclaw/openclaw.json`, JSON5, overridable
  with `OPENCLAW_CONFIG_PATH`.
- **MCP server section.** `mcp.servers.<name>` — a map keyed by
  server name. Stdio entries carry `command`, `args`, `env`. Remote
  entries carry `url`, `transport` (`streamable-http` or `sse`), and
  optional `headers` (env-var substitution allowed, e.g.
  `${MCP_REMOTE_TOKEN}`).
- **CLI.** `openclaw mcp set <name> <json>` accepts a single JSON
  object value (the entry body). `unset` removes by name. `list`
  prints names. `show [name]` prints one entry or the whole `mcp`
  object. Changes under `mcp.*` hot-apply — the next tool discovery
  recreates the cached MCP session.

Anything outside that surface (a separate `~/.openclaw/mcp.json`,
top-level `mcpServers`, `openclaw mcp ls`) is **not** part of the
contract and `open-memory` does not produce it.

## "Out of the box" definition

A user installs `open-memory` and runs **one command**:

```bash
open-memory integrate openclaw
```

After that command:

1. `~/.open-memory/data/default/` exists with empty SQLite databases.
2. `~/.openclaw/openclaw.json` exists (created if absent) and its
   `mcp.servers["open-memory"]` entry points at the local binary.
3. The next OpenClaw session has all `open_memory_*` tools available
   with no further setup.
4. If the binary was compiled with `--features embeddings` and no
   model exists yet, the first MCP call triggers a one-time download
   of the default model (`nomic-embed-text-v1.5`). If the user is
   offline, the server logs a warning and runs in keyword-only mode
   — recall still works. The default install (no embeddings feature)
   skips this step entirely.

No env vars are required. No "edit JSON" steps. No shell scripts.

## Config resolution

`open-memory integrate openclaw` resolves and writes the OpenClaw
config in this order:

1. **Path resolution.** Use `$OPENCLAW_CONFIG_PATH` if set; otherwise
   `~/.openclaw/openclaw.json`. (`~/.openclaw/mcp.json` is **not**
   probed — it is not part of OpenClaw's documented config surface.)
2. **Write strategy.** When the `openclaw` binary is on `PATH`,
   delegate to `openclaw mcp set open-memory '<json>'`. This lets
   OpenClaw normalize legacy aliases (e.g. `type: "http"` →
   `transport: "streamable-http"`), validate the entry, and hot-apply
   without restart. The CLI returns non-zero on rejection; we
   surface that error verbatim.
3. **Fallback strategy.** When `openclaw` is not on `PATH`, edit
   `openclaw.json` in place: parse as JSON5, ensure the top-level
   `mcp` object and its `servers` map exist, set the
   `open-memory` key, write back atomically (temp file + rename),
   preserving comments and trailing commas where the JSON5
   round-trip allows.
4. **Idempotence.** Running the command twice is a no-op when the
   resolved entry is byte-equivalent. When fields differ, the
   command prints a diff and applies the update.

`--profile <name>` is honored — the data directory becomes
`~/.open-memory/data/<name>/` and the entry name in `mcp.servers`
becomes `open-memory-<name>` so multiple profiles can coexist.
Without `--profile`, both default to `default` and the entry name is
just `open-memory`.

## MCP entry written into OpenClaw config

The default invocation produces (shown in context — only the
`open-memory` key is added/updated by the integrator; sibling entries
are preserved):

```json5
{
  // ... other openclaw.json content unchanged ...
  "mcp": {
    "servers": {
      "open-memory": {
        "command": "open-memory",
        "args": ["mcp"],
        "env": {
          "OPEN_MEMORY_HOME": "/Users/<user>/.open-memory",
          "OPEN_MEMORY_PROFILE": "default"
        }
      }
    }
  }
}
```

`OPEN_MEMORY_HOME` is set explicitly so that re-locating the config
elsewhere does not silently break the integration. `OPEN_MEMORY_PROFILE`
is set so a multi-profile OpenClaw user can pin which memory store is
attached to which OpenClaw profile.

For HTTP transport (only emitted when the user passes `--http`):

```json5
{
  "mcp": {
    "servers": {
      "open-memory": {
        "url": "http://127.0.0.1:7821/mcp",
        "transport": "streamable-http"
      }
    }
  }
}
```

The default port is 7821.

## MCP tool surface

Eleven tools, all `open_memory_*`. The prefix is intentional — it is
short, namespaces cleanly against `memoclaw_*` and other memory MCPs,
and matches the binary name. Names use `snake_case` to follow MCP
convention.

### Memory tools (entity-graph)

| Tool | Type | Purpose |
|------|------|---------|
| `open_memory_remember` | write | Create or update an entity, append observations and relations atomically. |
| `open_memory_recall` | read | Hybrid (vector + keyword) search over observations, scored with temporal decay. Optional spreading-activation expansion to related entities. |
| `open_memory_list_entities` | read | Browse entities, optional filter by `entity_type`, paginated. |
| `open_memory_get_entity` | read | All observations + relations for one entity. Used after `recall` to drill in. |
| `open_memory_forget` | write | Soft-delete a single observation by id. Lineage preserved. |
| `open_memory_forget_entity` | write | Hard-delete an entity and its observations + relations. Irreversible. |
| `open_memory_consolidate` | write | Run dedup + decay/prune on the graph. Idempotent. |
| `open_memory_status` | read | Counts, schema versions, last-consolidation timestamp. |

### Index tools (free-text URI store)

| Tool | Type | Purpose |
|------|------|---------|
| `open_memory_index_text` | write | Upsert plain text under a caller-supplied URI (e.g. `note://2026-05-04/standup`). Returns chunk count. |
| `open_memory_search` | read | Hybrid search over the URI corpus. Filter by URI prefix, content type, score threshold. |
| `open_memory_delete` | write | Remove all chunks for a URI (or URI prefix). |

### Why split graph vs. index

The graph is for structured agent memory: named entities with bounded
observations and relations. The index is for unstructured caller-owned
text under arbitrary URIs (notes, transcripts, scratchpads). Both ride
the same hybrid search engine under the hood, but they have different
schemas, write semantics, and authorization stories — keeping them
separate at the MCP boundary keeps the tool descriptions short and
unambiguous.

### Tool input schemas

Every tool input is JSON Schema 2020-12, generated from a `serde` +
`schemars` Rust struct via rmcp's `schema_for_type::<Parameters<T>>()`.
See
`open-memory-mcp/src/tools/*.rs` for the source-of-truth structs.

For agents, the description is keyed to OpenClaw's tool-list rendering:
each tool gets a one-line summary in `ServerHandler::get_info()` so
that OpenClaw's tool inspector renders cleanly. Example summary block:

```
open-memory is a persistent knowledge graph + hybrid text index.

MEMORY TOOLS:
- open_memory_remember: store entities, observations, relations
- open_memory_recall: semantic search over stored memory
- open_memory_list_entities: browse entities by type
- open_memory_get_entity: full record for one entity
- open_memory_forget: soft-delete one observation
- open_memory_forget_entity: hard-delete an entity
- open_memory_consolidate: run dedup + decay
- open_memory_status: store statistics

INDEX TOOLS:
- open_memory_index_text: store text under a URI
- open_memory_search: hybrid search over indexed text
- open_memory_delete: remove text by URI or prefix
```

## Tool naming conventions

- All tools use `snake_case`.
- All tools are prefixed `open_memory_`. Short prefix to keep them
  under the (informal) 64-char tool-name budget some agents impose.
- Verb-second naming: `*_remember`, `*_recall`, `*_search`. Keeps
  related tools alphabetically grouped in agent listings.
- Read-only tools surface `READ_ONLY = true` in the `Tool` trait so
  OpenClaw can decide how to gate them in agent permission UIs.

## Tool annotations (MCP)

Each tool sets `ToolAnnotations` per the MCP spec:

```rust
ToolAnnotations::new()
    .with_title("Recall memory")
    .read_only(true)               // for read tools
    .destructive(true)             // for forget_entity, delete
    .idempotent(true)              // for index_text, remember
    .open_world(false)             // memory is closed-world
```

These annotations matter to OpenClaw and other MCP clients that surface
tool risk in their UIs.

## Error shape

Errors are returned as `JsonRpcError` with an MCP `code` and `message`.
Three codes:

| Code | Trigger |
|------|---------|
| `-32602` (Invalid Params) | Caller passed a malformed input. Rejected before any DB work. |
| `-32603` (Internal Error) | Unexpected SQLite / I/O / model failure. Logged with a unique trace id. |
| `-32000` (Application) | A semantic memory error, e.g. `EntityNotFound`. The `message` field is the user-readable reason. |

`open-memory` never panics on the request path — every panic in any
tool is treated as a CI-blocking bug.

## First-run bootstrap

On first MCP `initialize`:

1. Create `~/.open-memory/` if absent (config + data dirs).
2. Run schema migrations on `memory.sqlite`, `index.sqlite`,
   `embeddings/cache.sqlite`. If a database is at a higher schema
   version than the binary, refuse to start with a clear error
   pointing at the migration mismatch.
3. If the `embeddings` feature is enabled and no model is present in
   `~/.open-memory/data/<profile>/embeddings/models/`, kick off a
   download of the default model in the background. Tool calls are
   answered keyword-only until the model is ready.
4. Log to stderr in human-friendly form by default; `OPEN_MEMORY_LOG=json`
   switches to JSON lines for OpenClaw's log capture.

## Compatibility commitments

The following are part of the public contract from v0.1.0 onwards:

- Tool **names** under `open_memory_*` will not be renamed without a
  major version bump. Adding new tools is a minor version bump.
- Tool input **field names** are stable — renaming a field (e.g.
  `entity_name` → `name`) is breaking.
- The SQLite **schema versions** advance forward only. A v1 database
  opened by a newer binary always works after migration.
- The OpenClaw config **JSON keys** (`mcp.servers`, `command`,
  `args`, `env`, `url`, `transport`, `headers`) follow OpenClaw's
  spec; we will track upstream changes there.

The following are **not** part of the contract:

- The internal Rust API (any `pub` symbol in any crate). Library
  consumers should pin patch versions.
- The on-disk directory layout under `~/.open-memory/data/<profile>/`.
  Treat the data directory as opaque.
- Log line wording. `--log-format json` is stable; the human-readable
  text is not.

## Verification

After running `open-memory integrate openclaw`, the user can verify
end-to-end:

```bash
open-memory status
# → memory: 0 entities, 0 observations
# → index: 0 documents, 0 chunks
# → schema: memory v4, index v3, embeddings v1
# → openclaw: ✔ entry present at mcp.servers.open-memory in
#             ~/.openclaw/openclaw.json

openclaw mcp list
# → open-memory

openclaw mcp show open-memory
# → {
# →   "command": "open-memory",
# →   "args": ["mcp"],
# →   "env": {
# →     "OPEN_MEMORY_HOME": "/Users/<user>/.open-memory",
# →     "OPEN_MEMORY_PROFILE": "default"
# →   }
# → }
```

Then a user-level smoke test from an OpenClaw agent:

```
> remember that I prefer Rust over Python
< (open_memory_remember called; 1 observation written for entity "User")

> what do you remember about my language preferences
< (open_memory_recall called; 1 result, score 0.91)
< I have on record that you prefer Rust over Python.
```
