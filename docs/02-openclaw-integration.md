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
[`docs.openclaw.ai/gateway/configuration`](https://docs.openclaw.ai/gateway/configuration),
[`docs.openclaw.ai/gateway/configuration-reference`](https://docs.openclaw.ai/gateway/configuration-reference),
and [`docs.openclaw.ai/cli/mcp`](https://docs.openclaw.ai/cli/mcp)):

- **Config file.** `~/.openclaw/openclaw.json`, JSON5, overridable
  with `OPENCLAW_CONFIG_PATH`. There is **no** separate
  `~/.openclaw/mcp.json` and **no** top-level `mcpServers` key —
  those were never part of OpenClaw's documented surface and the
  integrator does not produce them.
- **MCP server section.** `mcp.servers.<name>` — a map keyed by
  server name. Stdio entries carry `command`, `args`, `env`. Remote
  entries carry `url`, `transport` (`streamable-http` or `sse`), and
  optional `headers` (env-var substitution allowed, e.g.
  `${MCP_REMOTE_TOKEN}`). Changes under `mcp.*` hot-apply: OpenClaw
  disposes cached MCP runtimes and the next tool discovery picks the
  new entry up without a restart.
- **CLI.** `openclaw mcp set <name> <json>` accepts a single JSON
  object (the entry body). `unset <name>` removes by name. `list`
  prints names. `show [name]` prints one entry or the whole `mcp`
  object. `openclaw mcp set` and `openclaw doctor --fix` normalize
  the legacy `type: "http"` alias to the canonical `transport`
  field.

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
   without restart. The CLI's exit code and stderr are surfaced
   verbatim on rejection.
3. **Fallback strategy.** When `openclaw` is not on `PATH`, edit
   `openclaw.json` in place: parse as JSON5, ensure the top-level
   `mcp` object and its `servers` map exist, set the `open-memory`
   key, write back atomically (temp file + rename), preserving
   comments and trailing commas where the JSON5 round-trip allows.
4. **Validation.** After write, the integrator re-reads the file and
   asserts `mcp.servers["open-memory"]` round-trips byte-equivalent
   to the intended entry. If `openclaw` is on `PATH`, also runs
   `openclaw mcp show open-memory` and compares.
5. **Idempotence.** Running the command twice is a no-op when the
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

The default port is 7821 (one above sift's 7820).

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

### Tool descriptions in OpenClaw's tool inspector

For agents, the per-server description is keyed to OpenClaw's
tool-list rendering: `ServerHandler::get_info()` returns a short
markdown summary so that OpenClaw's tool inspector renders cleanly:

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

### Tool input/output schemas

Every input is JSON Schema 2020-12, generated from a `serde` +
`schemars` Rust struct via rmcp's `schema_for_type::<Parameters<T>>()`.
The schemas below are the public contract — the Rust structs in
`open-memory-mcp/src/tools/*.rs` exist to **match** these shapes, not
to define them. Implementations may add nothing, may rename
**nothing** without a major version bump, and reject unknown
top-level fields (`additionalProperties: false`).

#### Shared types

```ts
// All ids are UUIDv7 strings (RFC 9562). Time-ordered, lexicographic.
type Id = string;
// /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

// RFC 3339 / ISO 8601 with offset, e.g. "2026-05-04T15:30:00Z".
type Timestamp = string;

// Bounded entity kinds. Mirrors sift's retained EntityType enum exactly.
type EntityType =
  | "person" | "project" | "concept" | "tool"
  | "preference" | "fact" | "event" | "location"
  | "organization";

type Observation = {
  id: Id;
  entity_id: Id;
  text: string;             // 1..=4096 chars
  observed_at: Timestamp;   // when the fact was learned
  valid_from: Timestamp;    // bi-temporal validity start
  valid_until: Timestamp | null; // null = still valid
  source: string | null;    // free-form provenance
  tags: string[];           // optional, ASCII slugs <=32 chars each
};

type Relation = {
  id: Id;
  from_entity_id: Id;
  to_entity_id: Id;
  relation: string;         // 1..=64 chars, snake_case verb
  observed_at: Timestamp;
  valid_until: Timestamp | null;
};

type Entity = {
  id: Id;
  name: string;             // unique within store, 1..=128 chars
  entity_type: EntityType;
  created_at: Timestamp;
  updated_at: Timestamp;
};

type ScoredObservation = Observation & {
  entity_name: string;
  entity_type: EntityType;
  score: number;            // 0..=1, RRF + decay
};
```

#### Memory tools

`open_memory_remember` — write, idempotent on `(entity_name, text)`
within a 1-second observation window.

```ts
// Input
{
  entity_name: string;          // required, 1..=128
  entity_type?: EntityType;     // default "concept"
  observations?: {              // 0..=64 per call
    text: string;               // 1..=4096
    source?: string;
    tags?: string[];
    valid_from?: Timestamp;     // default = server now()
    valid_until?: Timestamp;
  }[];
  relations?: {                 // 0..=32 per call
    target_entity_name: string;
    target_entity_type?: EntityType; // ensured if target absent
    relation: string;           // 1..=64
    valid_until?: Timestamp;
  }[];
}

// Output
{
  entity: Entity;
  created_observations: Observation[];
  created_relations: Relation[];
  // Pre-existing duplicates that suppressed creation:
  deduped_observations: { id: Id; text: string }[];
}
```

`open_memory_recall` — read.

```ts
// Input
{
  query: string;                          // 1..=512
  limit?: number;                         // 1..=50, default 10
  entity_type?: EntityType;               // filter
  entity_name?: string;                   // restrict to one entity
  tags?: string[];                        // any-of match
  mode?: "hybrid" | "vector" | "keyword"; // default "hybrid"
  include_invalidated?: boolean;          // default false
  spread?: number;                        // 0..=2 hops, default 0
}

// Output
{
  results: ScoredObservation[];           // ordered by score desc
  // When spread > 0, the entities reached via relations:
  expanded_entities: Entity[];
  // Diagnostics for callers building UIs:
  vector_used: boolean;                   // false in keyword-only build
  total_candidates: number;
}
```

`open_memory_list_entities` — read, paginated.

```ts
// Input
{
  entity_type?: EntityType;
  name_prefix?: string;
  limit?: number;       // 1..=200, default 50
  cursor?: string;      // opaque, from previous response
}

// Output
{
  entities: (Entity & { observation_count: number })[];
  next_cursor: string | null;     // null when exhausted
}
```

`open_memory_get_entity` — read.

```ts
// Input — exactly one of entity_name / entity_id required
{
  entity_name?: string;
  entity_id?: Id;
  include_invalidated?: boolean;  // default false
  observation_limit?: number;     // 1..=500, default 100
}

// Output (or JSON-RPC -32000 EntityNotFound)
{
  entity: Entity;
  observations: Observation[];           // newest first
  relations_out: Relation[];             // entity -> *
  relations_in: Relation[];              // * -> entity
}
```

`open_memory_forget` — write, soft-delete.

```ts
// Input
{ observation_id: Id; }

// Output
{ observation_id: Id; valid_until: Timestamp; }
```

`open_memory_forget_entity` — write, hard-delete.

```ts
// Input — exactly one of entity_name / entity_id; confirm must be true
{
  entity_name?: string;
  entity_id?: Id;
  confirm: true;
}

// Output
{
  entity_id: Id;
  deleted_observations: number;
  deleted_relations: number;
}
```

`open_memory_consolidate` — write, idempotent.

```ts
// Input
{
  dedup?: boolean;          // default true
  decay?: boolean;          // default true
  prune?: boolean;          // default true
  dry_run?: boolean;        // default false
}

// Output
{
  dedup_merged: number;
  decay_applied: number;
  pruned_observations: number;
  pruned_entities: number;
  duration_ms: number;
}
```

`open_memory_status` — read.

```ts
// Input: {} (empty object accepted)

// Output
{
  entities: number;
  observations: number;             // active (not soft-deleted)
  relations: number;
  index_documents: number;
  index_chunks: number;
  fulltext_backend: "fts5" | "bm25"; // runtime-probed at startup
  schema: { memory: number; index: number; embeddings: number };
  embeddings: {
    enabled: boolean;
    model: string | null;           // e.g. "nomic-embed-text-v1.5"
    cached_vectors: number;
  };
  last_consolidate_at: Timestamp | null;
}
```

#### Index tools

`open_memory_index_text` — write, idempotent on `(uri, content_hash)`.

```ts
// Input
{
  uri: string;              // 1..=1024, must contain a scheme ":"
  content: string;          // 1..=1_048_576 (1 MB) chars
  content_type?: string;    // MIME-style, default "text/plain"
  chunk?: {                 // chunking knobs; defaults applied otherwise
    target_chars?: number;  // 256..=4096, default 1024
    overlap_chars?: number; // 0..=512, default 128
  };
  metadata?: Record<string, string>; // <=16 keys, value <=512 chars
}

// Output
{
  uri: string;
  chunk_count: number;
  content_hash: string;     // BLAKE3 hex
  was_unchanged: boolean;   // true when (uri, hash) already present
}
```

`open_memory_search` — read.

```ts
// Input
{
  query: string;                          // 1..=512
  limit?: number;                         // 1..=50, default 10
  uri_prefix?: string;                    // restrict to URI namespace
  content_type?: string;
  mode?: "hybrid" | "vector" | "keyword"; // default "hybrid"
  min_score?: number;                     // 0..=1, default 0
}

// Output
{
  hits: {
    uri: string;
    chunk_index: number;
    snippet: string;        // <=512 chars, server-rendered
    score: number;          // 0..=1
    content_type: string;
    metadata: Record<string, string>;
  }[];
  vector_used: boolean;
}
```

`open_memory_delete` — write, destructive.

```ts
// Input — exactly one of uri / uri_prefix required
{
  uri?: string;             // exact match
  uri_prefix?: string;      // namespace delete
  confirm: true;            // must be literal true when uri_prefix is used
}

// Output
{
  deleted_documents: number;
  deleted_chunks: number;
}
```

#### Response envelope

All success responses ride the standard MCP `tools/call` result:

```ts
{
  content: [{ type: "text", text: <JSON.stringify of the schema above> }];
  isError: false;
  // Tools whose schema declares structured output also include:
  structuredContent: <the schema above>;
}
```

The `structuredContent` field is the canonical machine-readable form;
`content[0].text` is its JSON serialization for clients that ignore
structured output. Both must agree byte-for-byte after re-parse.

#### Validation rules common to every tool

- Unknown top-level fields are **rejected**
  (`additionalProperties: false`). Forward-compatibility happens via
  new optional fields, not via silent ignore.
- String length and array size limits above are enforced on the
  request path; oversize requests get `-32602 Invalid Params` with a
  machine-readable `data.field` pointer (e.g.
  `{"field": "observations[3].text", "limit": 4096}`).
- Whole-request size cap: 1 MB JSON-RPC payload.

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
# → fulltext: fts5   # or bm25 if host SQLite lacks FTS5
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
