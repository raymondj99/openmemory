# MCP server reference

The MCP (Model Context Protocol) server is the public contract.
Eleven `openmemory_*` tools cover the entire knowledge-graph and
free-text-index API. The same `OpenMemoryMcpServer::handle` path
serves both stdio and Streamable HTTP transports.

## Protocol

`openmemory-mcp` ships a hand-rolled JSON-RPC 2.0 server in
[`src/protocol.rs`](../crates/openmemory-mcp/src/protocol.rs). It
does **not** depend on the upstream `rmcp` Rust SDK because every
published rmcp release uses Rust 1.88+ if-let chain syntax that
breaks the workspace's MSRV pin at 1.85. The `Tool` and `ToolRouter`
shapes mirror rmcp closely; swapping upstream in once MSRV catches
up is a mechanical change.

| Field | Value |
|-------|-------|
| Protocol version (advertised in `initialize`) | `"2024-11-05"` |
| Server name | `"openmemory"` |
| Server version | `0.2.1` (workspace version) |
| Wire format | JSON-RPC 2.0 |
| Default transport | stdio (line-buffered, one JSON object per line) |
| Optional transport | Streamable HTTP (POST `/mcp`) behind `mcp-http` feature |

The protocol surface a client sees:

- `initialize`. Handshake. Returns `ServerCapabilities`,
  `ServerInfo`, plus the rendered `instructions` block from
  `server_instructions()`.
- `tools/list`. Every registered tool with its descriptor (name,
  description, JSON-Schema input, MCP annotations). Registration
  order is deterministic (memory tools first, then index, then
  maintenance).
- `tools/call`. Dispatch by `name` to the registered handler with
  the supplied `arguments` payload.

The `OpenMemoryMcpServer::handle` method takes a `JsonRpcRequest`
and returns `Option<JsonRpcResponse>`. It returns `None` for
notification requests (no `id` field) and `Some` for everything
else.

## The eleven tools

All tools use `snake_case`. All tools are prefixed `openmemory_`:
short to keep them under any 64-char tool-name budget the agent
runner imposes, and namespaced cleanly against `memoclaw_*` and
other memory MCPs.

### Memory tools (knowledge graph)

| Tool | Type | Purpose |
|------|------|---------|
| `openmemory_remember` | write | Create or update an entity, append observations and relations atomically. Fuzzy-matches incoming names against existing entities of the same type to prevent duplicates (configurable thresholds in `[normalization]`). |
| `openmemory_recall` | read | Hybrid (vector + keyword) search over observations, scored with Ebbinghaus decay. Optional spreading-activation expansion to related entities. |
| `openmemory_list_entities` | read | Browse entities. Optional filter by `entity_type`; pagination via `limit` / `offset`. |
| `openmemory_get_entity` | read | All observations and relations for one entity, lookup by `entity_id` or `name`. Used after `recall` to drill in. |
| `openmemory_forget` | destructive | Soft-delete a single observation by id. Lineage preserved (the row is tomb-stoned, not removed). |
| `openmemory_forget_entity` | destructive | Hard-delete an entity and its observations and relations. Irreversible. |
| `openmemory_status` | read | Counts, schema versions, oldest/newest observation timestamps, entity-type and tier breakdowns, vector count, reader-pool size. |

### Index tools (free-text URI store)

| Tool | Type | Purpose |
|------|------|---------|
| `openmemory_index_text` | write | Upsert plain text under a caller-supplied URI (e.g. `note://2026-05-04/standup`). Returns the inserted chunk count. |
| `openmemory_search` | read | Hybrid search over the URI corpus. Filter by URI prefix, content type, score threshold, search mode. |
| `openmemory_delete` | destructive | Remove all chunks for a URI (or URI prefix). |

### Maintenance tools

| Tool | Type | Purpose |
|------|------|---------|
| `openmemory_consolidate` | write | Run dedup (Jaccard text similarity within an entity) plus decay-prune (Ebbinghaus scoring with floor). Idempotent. |

### Why split graph vs. index

The graph is for structured agent memory: named entities with
bounded observations and relations. The index is for unstructured
caller-owned text under arbitrary URIs (notes, transcripts,
scratchpads). Both ride the same hybrid search engine under the
hood, but they have different schemas, different write semantics,
and different authorization stories. Keeping them separate at the
MCP boundary keeps each tool description short and unambiguous.

## Tool registration

All eleven tools are registered in one place, the `registry()`
function in
[`crates/openmemory-mcp/src/tools/mod.rs`](../crates/openmemory-mcp/src/tools/mod.rs).
Adding a new tool is a one-line change to that registry.

```rust
fn registry() -> Vec<Entry> {
    let mut v = Vec::new();
    memory::register_all(&mut v);
    index::register_all(&mut v);
    maintenance::register_all(&mut v);
    v
}
```

Each tool implements the `Tool` trait, which keeps three concerns
colocated:

```rust
pub trait Tool: Send + Sync + 'static {
    const NAME: &'static str;          // wire name
    const SUMMARY: &'static str;       // for `instructions` block
    const GROUP: ToolGroup;            // Memory / Index / Maintenance

    fn descriptor() -> ToolDescriptor;
    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError>;
}
```

Because `descriptor()` and `call()` come from the same impl block,
an agent can never see a tool advertised in `tools/list` that the
router cannot dispatch (or vice versa). The `server_instructions()`
function derives the human-readable index from the same registry,
so the on-the-wire description and the rendered tool listing stay
in lockstep.

## Tool input schemas

Every tool input is JSON Schema 2020-12, generated from a `serde +
schemars` Rust struct in [`tools/memory.rs`](../crates/openmemory-mcp/src/tools/memory.rs),
[`tools/index.rs`](../crates/openmemory-mcp/src/tools/index.rs), and
[`tools/maintenance.rs`](../crates/openmemory-mcp/src/tools/maintenance.rs)
via `schema_for::<T>()`. Wire enums use camelCase names to keep
JSON-Schema output ergonomic for OpenClaw's tool inspector:

- `EntityTypeParam`: `person`, `project`, `concept`, `tool`,
  `preference`, `fact`, `event`, `location`, `organization`.
- `MemoryTierParam`: `episodic`, `semantic`, `procedural`.
- `SearchModeParam`: `hybrid` (default), `vector_only`,
  `keyword_only`.

The Rust source is the single source of truth. Read the per-tool
input structs to see exactly what fields are required, optional,
or have defaults.

## Tool annotations

Each tool sets MCP `ToolAnnotations` so an MCP client can surface
risk in its UI. The three patterns:

```rust
// read tools (recall, search, list, get, status)
ToolAnnotations { read_only: Some(true),  destructive: Some(false), idempotent: Some(true)  }

// write tools (remember, index_text, consolidate)
ToolAnnotations { read_only: Some(false), destructive: Some(false), idempotent: Some(false) }

// destructive tools (forget, forget_entity, delete)
ToolAnnotations { read_only: Some(false), destructive: Some(true),  idempotent: Some(true)  }
```

These are produced by the `read_only_annotations`,
`write_annotations`, and `destructive_annotations` helpers in
`tools/mod.rs`.

## Error shape

Errors are returned as a `JsonRpcError` with an MCP `code` and
`message`. Five codes are emitted by the server:

| Code | Constant | Trigger |
|------|----------|---------|
| `-32602` | `INVALID_PARAMS` | Caller passed a malformed input. Rejected before any DB work. |
| `-32603` | `INTERNAL_ERROR` | Unexpected SQLite, I/O, or model failure. Logged with a unique trace id. |
| `-32601` | `METHOD_NOT_FOUND` | Unknown method name or unknown tool name in `tools/call`. |
| `-32700` | `PARSE_ERROR` | The wire payload was not valid JSON. |
| `-32600` | (generic application) | Bearer-token auth failure on the HTTP transport. |

`openmemory` never panics on the request path. Every panic in any
tool is treated as a CI-blocking bug. The release-hardening pass
(`Unreleased` in `CHANGELOG.md`) replaced a stray
`Response::builder().unwrap()` in `http::handle_mcp` with
`StatusCode::NO_CONTENT.into_response()` precisely to keep that
contract.

## Transports

### Stdio (always available)

The default. `OpenMemoryMcpServer` reads JSON-RPC requests from
stdin (one object per line) and writes responses to stdout. This
is what OpenClaw runs by default and what `cargo install
openmemory` ships out of the box.

```bash
openmemory mcp        # equivalent to: openmemory mcp (stdio)
```

The transport implementation is `run_stdio_server` in
[`src/stdio.rs`](../crates/openmemory-mcp/src/stdio.rs). Tokio
runs single-threaded current-thread; one stdio session is plenty.

### Streamable HTTP (behind `mcp-http`)

The HTTP transport serves the same `OpenMemoryMcpServer::handle`
path under `POST /mcp`, plus `GET /healthz` for load-balancer
liveness probes. Built with `--features mcp-http`:

```bash
cargo build --release --features mcp-http
openmemory mcp --http 0.0.0.0:7800
```

The implementation lives in
[`src/http.rs`](../crates/openmemory-mcp/src/http.rs). It uses
`axum` plus `tower-http` for CORS and tracing middleware. Each
request still goes through `handle()`, so behaviour is identical
to stdio modulo the wire framing.

#### Bearer-token authentication

For anything bound to a non-loopback address, set
`OPENMEMORY_HTTP_TOKEN` before launching the server. Each `/mcp`
request must carry a matching `Authorization: Bearer <token>`
header; missing or wrong tokens get a `401` with
`WWW-Authenticate: Bearer` and a JSON-RPC `-32600` error envelope.
`/healthz` is **never** auth-gated so liveness probes keep working
without the token.

```bash
export OPENMEMORY_HTTP_TOKEN="$(openssl rand -hex 32)"
openmemory mcp --http 0.0.0.0:7800
```

With the env var unset (or empty), the server logs a warning and
serves unauthenticated. That is fine for `127.0.0.1` deployments
and never appropriate on a public address.

The token comparison is constant-time over the byte payload via
`BearerToken::matches`. The `BearerToken` type's `Debug` impl
redacts the secret (`BearerToken { len: <n> }`) so it never lands
in a log accidentally.

## Initialize / get_info

`OpenMemoryMcpServer::initialize_result()` returns the `initialize`
response. The `instructions` field is the rendered
`server_instructions()` output: the human-readable tool index that
OpenClaw and other inspectors render to the user.

The instructions look approximately like this:

```text
openmemory is a local persistent agent memory + hybrid text search engine.

MEMORY TOOLS:
- openmemory_remember: store entities, observations, relations
- openmemory_recall: semantic search over stored memory
- openmemory_list_entities: browse entities by type
- openmemory_get_entity: full record for one entity
- openmemory_forget: soft-delete one observation
- openmemory_forget_entity: hard-delete an entity
- openmemory_status: store statistics

INDEX TOOLS:
- openmemory_index_text: store text under a URI
- openmemory_search: hybrid search over indexed text
- openmemory_delete: remove text by URI or prefix

MAINTENANCE TOOLS:
- openmemory_consolidate: run dedup + decay-prune

WORKFLOW:
1. Use openmemory_remember to store facts about named entities.
2. Use openmemory_recall to find facts by natural-language query.
3. Use openmemory_index_text + openmemory_search for free-text content.
4. Run openmemory_consolidate periodically to dedup + decay-prune.
```

The exact text is generated from the registry; if you add a new
tool to the registry, the instructions update automatically.

## Tool naming conventions

- `snake_case` everywhere.
- All tools prefixed `openmemory_`. Short to keep them under
  informal 64-char tool-name budgets.
- Verb-second naming (`*_remember`, `*_recall`, `*_search`) keeps
  related tools alphabetically grouped in agent listings.
- Read-only tools surface `read_only: true` in their annotations
  so OpenClaw can decide how to gate them in agent permission UIs.

## Compatibility commitments

The following are part of the public contract from v0.1.0 onwards:

- Tool **names** under `openmemory_*` are stable across minor
  version bumps. Renaming requires a major bump. Adding new tools
  is a minor bump.
- Tool input **field names** are stable. Renaming a field
  (e.g. `entity_name` → `name`) is breaking.
- The SQLite **schema versions** advance forward only. A v1
  database opened by a newer binary always works after migration.
- The OpenClaw config **JSON keys** track OpenClaw's spec; we
  follow upstream changes there.

The following are **not** part of the contract:

- The internal Rust API (any `pub` symbol in any crate). Library
  consumers should pin patch versions.
- The on-disk directory layout under `~/.openmemory/data/<profile>/`.
  Treat the data directory as opaque.
- Log line wording. `OPENMEMORY_LOG=json` is stable; the
  human-readable text is not.
