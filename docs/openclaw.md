# OpenClaw integration

This is the contract between `openmemory` and OpenClaw. Everything
in this file is observable from outside the binary: file paths,
config snippets, MCP tool names, error shapes. Anything we change
here is a breaking change.

## Why OpenClaw is the first-class consumer

OpenClaw is an open-source personal AI assistant. It speaks MCP for
external tool integration, ships its config at
`~/.openclaw/openclaw.json` (JSON5; comments and trailing commas
allowed) with managed outbound MCP server definitions under
`mcp.servers`, and exposes a CLI for programmatic config (`openclaw
mcp list | show | set | unset`). That gives us a clean
"drop-in-and-go" target without ad-hoc IPC or bespoke transports.

The OpenClaw contract surfaces we depend on (sourced from
[`docs.openclaw.ai/gateway/configuration`](https://docs.openclaw.ai/gateway/configuration)
and [`docs.openclaw.ai/cli/mcp`](https://docs.openclaw.ai/cli/mcp)):

- **Config file.** `~/.openclaw/openclaw.json`, JSON5, overridable
  with `OPENCLAW_CONFIG_PATH`.
- **MCP server section.** `mcp.servers.<name>`, a map keyed by
  server name. Stdio entries carry `command`, `args`, `env`. Remote
  entries carry `url`, `transport` (`streamable-http` or `sse`),
  and optional `headers` (env-var substitution allowed,
  e.g. `${MCP_REMOTE_TOKEN}`).
- **CLI.** `openclaw mcp set <name> <json>` accepts a single JSON
  object value (the entry body). `unset` removes by name. `list`
  prints names. `show [name]` prints one entry or the whole `mcp`
  object. Changes under `mcp.*` hot-apply: the next tool discovery
  recreates the cached MCP session.

Anything outside that surface (a separate `~/.openclaw/mcp.json`,
top-level `mcpServers`, `openclaw mcp ls`) is **not** part of the
contract and `openmemory` does not produce it.

## "Out of the box" definition

A user installs `openmemory` and runs **one command**:

```bash
openmemory integrate openclaw
```

After that command:

1. `~/.openmemory/data/default/` exists with empty SQLite
   databases.
2. `~/.openclaw/openclaw.json` exists (created if absent) and its
   `mcp.servers["openmemory"]` entry points at the local binary.
3. The next OpenClaw session has all `openmemory_*` tools
   available with no further setup.
4. If the binary was compiled with `--features embeddings` and no
   model is present yet, the server logs a warning and runs in
   keyword-only mode; recall still works. Run
   `openmemory model download` to cache the default model
   (`nomic-embed-text-v1.5`) before starting OpenClaw when semantic
   recall is desired. Builds without `embeddings` skip model loading
   entirely.

No environment variables are required. No "edit JSON" steps. No
shell scripts.

## Config resolution

`openmemory integrate openclaw` resolves and writes the OpenClaw
config in this order:

1. **Path resolution.** Use `$OPENCLAW_CONFIG_PATH` if set;
   otherwise `~/.openclaw/openclaw.json`. The legacy
   `~/.openclaw/mcp.json` filename is **not** probed; it is not
   part of OpenClaw's documented config surface.
2. **Write strategy.** When the `openclaw` binary is on `PATH`,
   delegate to `openclaw mcp set openmemory '<json>'`. This lets
   OpenClaw normalize legacy aliases (e.g. `type: "http"` →
   `transport: "streamable-http"`), validate the entry, and
   hot-apply without restart. The CLI returns non-zero on
   rejection; we surface that error verbatim.
3. **Fallback strategy.** When `openclaw` is not on `PATH`, edit
   `openclaw.json` in place: parse as JSON5, ensure the top-level
   `mcp` object and its `servers` map exist, set the `openmemory`
   key, write back atomically (temp file + rename), preserving
   comments and trailing commas where the JSON5 round-trip allows.
4. **Idempotence.** Running the command twice is a no-op when the
   resolved entry is byte-equivalent. When fields differ, the
   command prints a diff and applies the update.

`--profile <name>` is honored: the data directory becomes
`~/.openmemory/data/<name>/` and the entry name in `mcp.servers`
becomes `openmemory-<name>` so multiple profiles can coexist.
Without `--profile`, both default to `default` and the entry name
is just `openmemory`.

## CLI flags

The integrator subcommand:

```text
openmemory integrate openclaw [--config PATH] [--http ADDR] [--binary PATH]
```

- `--config PATH` overrides the default config path (defaults to
  `$OPENCLAW_CONFIG_PATH` or `~/.openclaw/openclaw.json`).
- `--http ADDR` emits an HTTP-transport entry (`streamable-http`)
  pointing at the given address (e.g. `127.0.0.1:7821`) instead of
  the default stdio entry.
- `--binary PATH` overrides the binary path written into the
  entry. Defaults to the bare `openmemory`, which OpenClaw
  resolves via `$PATH`.

## MCP entry written into OpenClaw config

The default invocation produces (shown in context; only the
`openmemory` key is added or updated by the integrator, sibling
entries are preserved):

```json5
{
  // ... other openclaw.json content unchanged ...
  "mcp": {
    "servers": {
      "openmemory": {
        "command": "openmemory",
        "args": ["mcp"],
        "env": {
          "OPENMEMORY_HOME": "/Users/<user>/.openmemory",
          "OPENMEMORY_PROFILE": "default"
        }
      }
    }
  }
}
```

`OPENMEMORY_HOME` is set explicitly so that re-locating the config
elsewhere does not silently break the integration.
`OPENMEMORY_PROFILE` is set so a multi-profile OpenClaw user can
pin which memory store is attached to which OpenClaw profile.

For HTTP transport (only emitted when the user passes `--http`):

```json5
{
  "mcp": {
    "servers": {
      "openmemory": {
        "url": "http://127.0.0.1:7821/mcp",
        "transport": "streamable-http"
      }
    }
  }
}
```

The default port the integrator suggests is 7821. The CLI does not
start a server itself; the user runs `openmemory mcp --http <addr>`
separately. The bearer-token auth setup for that HTTP server is
documented in [mcp.md](mcp.md#bearer-token-authentication):

## First-run bootstrap

On first MCP `initialize`:

1. Create `~/.openmemory/` if absent (config and data directories).
2. Run schema migrations on `memory.sqlite`, the index database
   files, and `embeddings/cache.sqlite`. If a database is at a
   higher schema version than the binary, refuse to start with a
   clear error pointing at the migration mismatch.
3. If the `embeddings` feature is enabled, check for the default
   model in `~/.openmemory/models/`. Startup never downloads model
   files; tool calls are answered keyword-only until the user runs
   `openmemory model download`.
4. Log output is diagnostic only. OpenClaw-facing machine state
   should come from MCP JSON-RPC responses or daemon/admin JSON
   routes, not stderr log parsing.

## Verification

After running `openmemory integrate openclaw`, the user can
verify end-to-end:

```bash
openmemory status
# -> memory: 0 entities, 0 observations
# -> index: 0 documents, 0 chunks
# -> schema: memory v2, index v1, embeddings v1
# -> openclaw: entry present at mcp.servers.openmemory in
#              ~/.openclaw/openclaw.json

openclaw mcp list
# -> openmemory

openclaw mcp show openmemory
# -> {
# ->   "command": "openmemory",
# ->   "args": ["mcp"],
# ->   "env": {
# ->     "OPENMEMORY_HOME": "/Users/<user>/.openmemory",
# ->     "OPENMEMORY_PROFILE": "default"
# ->   }
# -> }
```

Then a user-level smoke test from an OpenClaw agent:

```text
> remember that I prefer Rust over Python
< (openmemory_remember called; 1 observation written for entity "User")

> what do you remember about my language preferences?
< (openmemory_recall called; 1 result, score 0.91)
< I have on record that you prefer Rust over Python.
```

## Compatibility commitments

The following are part of the public contract from v0.1.0:

- Tool **names** under `openmemory_*` will not be renamed without
  a major version bump. Adding new tools is a minor version bump.
- Tool input **field names** are stable; renaming a field is
  breaking.
- The SQLite **schema versions** advance forward only.
- The OpenClaw config **JSON keys** (`mcp.servers`, `command`,
  `args`, `env`, `url`, `transport`, `headers`) follow OpenClaw's
  spec.

The following are **not**:

- The internal Rust API.
- The on-disk directory layout under `~/.openmemory/data/<profile>/`.
- Log line wording.
