# CLI reference

The `open-memory` binary is a thin clap surface. Every subcommand
is a small adapter to a function in
[`crates/open-memory-cli/src/commands/`](../crates/open-memory-cli/src/commands/)
that does the real work. The clap definitions live in
[`crates/open-memory-cli/src/cli.rs`](../crates/open-memory-cli/src/cli.rs):

## Top-level invocation

```text
open-memory [--home <PATH>] [--profile <NAME>] <SUBCOMMAND> [ARGS...]
```

| Global flag | Effect |
|-------------|--------|
| `--home <PATH>` | Override the data root. Defaults to `$OPEN_MEMORY_HOME` or `~/.open-memory`. Setting it also exports `OPEN_MEMORY_HOME` to the process so subprocesses inherit the override. |
| `--profile <NAME>` | Memory profile under the data root. Defaults to `default`. |

Built-in `--help`, `--version`, and shell completions all work.

## `open-memory init`

Initialise the data directory and write a default `config.toml`.

```text
open-memory init [--force]
```

| Flag | Effect |
|------|--------|
| `--force` | Overwrite an existing `config.toml`. Without it, `init` refuses to clobber. |

Creates `~/.open-memory/`, `~/.open-memory/data/<profile>/`, and
writes a populated `config.toml` from the
`Config::default()` shape. Idempotent across re-runs unless
something changed.

## `open-memory status`

Print summary of memory and index state. No flags.

```text
open-memory status
```

Output (human-readable; one short line per fact):

- Entity, observation, relation counts.
- Tombstoned-observation count.
- Schema versions per database.
- Oldest/newest observation timestamps.
- Per-entity-type counts.
- Per-tier counts.
- Vector index size.
- Reader-pool size (multi-agent concurrency surface).
- Last consolidation timestamp.

## `open-memory mcp`

Start the MCP server.

```text
open-memory mcp [--http <ADDR>]
```

| Flag | Effect |
|------|--------|
| (none) | Default. Stdio transport: read JSON-RPC requests from stdin, write responses to stdout. |
| `--http <ADDR>` | Bind a Streamable HTTP listener on `<ADDR>` (e.g. `127.0.0.1:7800`). Requires the `mcp-http` build feature. |

Examples:

```bash
# Stdio (the default OpenClaw entry point)
open-memory mcp

# HTTP (requires --features mcp-http)
open-memory mcp --http 0.0.0.0:7800

# HTTP with bearer-token auth (recommended for any non-loopback bind)
export OPEN_MEMORY_HTTP_TOKEN="$(openssl rand -hex 32)"
open-memory mcp --http 0.0.0.0:7800
```

Stdio mode runs Tokio current-thread; HTTP mode runs Tokio
multi-thread. See [mcp.md](mcp.md#transports) for transport detail
including the bearer-token contract.

## `open-memory consolidate`

Run dedup plus decay-prune once and print the report.

```text
open-memory consolidate [--dedup-threshold <F>] [--prune-floor <F>] [--min-age-secs <SECS>]
```

| Flag | Effect |
|------|--------|
| `--dedup-threshold <F>` | Override the Jaccard text-similarity threshold (0.0–1.0). Default from `[memory] dedup_threshold`, normally `0.95`. |
| `--prune-floor <F>` | Override the decay-prune score floor. Default from `[memory] prune_floor`, normally `0.05`. |
| `--min-age-secs <SECS>` | Minimum age (seconds) for an observation to be eligible for pruning. |

Idempotent: a second call right after the first reports zero work.
See [search.md](search.md#consolidation) for the math.

## `open-memory integrate openclaw`

Register `open-memory` in OpenClaw's MCP config. The defining
"out-of-the-box" command.

```text
open-memory integrate openclaw [--config <PATH>] [--http <ADDR>] [--binary <PATH>]
```

| Flag | Effect |
|------|--------|
| `--config <PATH>` | Override the OpenClaw config path. Defaults to `$OPENCLAW_CONFIG_PATH` or `~/.openclaw/openclaw.json`. |
| `--http <ADDR>` | Emit an HTTP-transport entry (`streamable-http`) pointing at the given address (e.g. `127.0.0.1:7821`) instead of the default stdio entry. |
| `--binary <PATH>` | Override the binary path written into the entry. Defaults to the bare `open-memory`, which OpenClaw resolves via `$PATH`. |

Examples:

```bash
# Default: stdio entry under mcp.servers.open-memory
open-memory integrate openclaw

# HTTP-transport entry pointing at a locally-running mcp --http server
open-memory integrate openclaw --http 127.0.0.1:7821

# Pin the binary path explicitly (for non-PATH-resolvable installs)
open-memory integrate openclaw --binary /opt/open-memory/bin/open-memory
```

Idempotent: a re-run with no changes reports "no changes". A re-run
with a changed entry prints the diff. Full integration contract in
[openclaw.md](openclaw.md):

## `open-memory remember`

Append observations to an entity from the command line. Mainly for
scripting and testing; the MCP `open_memory_remember` tool is the
in-agent path.

```text
open-memory remember <ENTITY>
    --observation <TEXT>... (repeatable, at least one required)
    [--entity-type <TYPE>]
    [--relation <TYPE=NAME[:ENTITY_TYPE]>...]
    [--source <ORIGIN>]
    [--json]
```

| Flag | Effect |
|------|--------|
| `<ENTITY>` (positional) | Entity name to attach observations to. |
| `--observation <TEXT>` | Repeatable. At least one required. |
| `--entity-type <TYPE>` | Defaults to `concept`. Must be one of `person`, `project`, `concept`, `tool`, `preference`, `fact`, `event`, `location`, `organization`. |
| `--relation <TYPE=NAME[:ENTITY_TYPE]>` | Repeatable. Format: `relation_type=target_entity_name[:target_entity_type]`. |
| `--source <ORIGIN>` | Origin tag for audit. Defaults to `"cli"`. |
| `--json` | Emit a single-line JSON result instead of human text. |

Example:

```bash
open-memory remember Raymond \
    --entity-type person \
    --observation 'prefers Rust' \
    --observation 'maintains open-memory' \
    --relation 'maintains=open-memory:project'
```

## `open-memory recall`

Search memory by natural-language query. CLI counterpart to
`open_memory_recall`.

```text
open-memory recall <QUERY>
    [--limit <N>]
    [--entity-type <TYPE>]
    [--source <ORIGIN>]
    [--min-confidence <F>]
    [--json]
```

| Flag | Effect |
|------|--------|
| `<QUERY>` (positional) | Natural-language query. |
| `--limit <N>` | Maximum results. |
| `--entity-type <TYPE>` | Restrict to a single entity type. |
| `--source <ORIGIN>` | Restrict to observations from this source tag. |
| `--min-confidence <F>` | Minimum confidence in `[0.0, 1.0]`. |
| `--json` | Emit a JSON array instead of human text. |

Hybrid mode (vector + keyword + RRF + Ebbinghaus decay + spreading
activation) is hard-coded for the CLI; for finer-grained control,
use the MCP tool. See [search.md](search.md):

## `open-memory list-entities`

Browse entities, optionally filtered by type.

```text
open-memory list-entities
    [--entity-type <TYPE>]
    [--limit <N>]
    [--offset <N>]
    [--json]
```

## `open-memory forget-entity`

Hard-delete an entity and its observations and relations.

```text
open-memory forget-entity <ENTITY> --yes
```

`--yes` is required to confirm the destructive action; the command
aborts otherwise.

## `open-memory completions <SHELL>`

Emit shell completions for the named shell. Behind the
default-on `completions` feature.

```text
open-memory completions <bash|fish|zsh|elvish|powershell>
```

The output goes to stdout; redirect into the appropriate location
for your shell:

```bash
open-memory completions fish > ~/.config/fish/completions/open-memory.fish
open-memory completions bash > /etc/bash_completion.d/open-memory
open-memory completions zsh  > "${fpath[1]}/_open-memory"
```

## `open-memory watch <PATH>`

Watch a directory and incrementally re-index changed files. Behind
the default-on `watch` feature. Full crate detail in
[watcher.md](watcher.md):

```text
open-memory watch <PATH>
    [--debounce-ms <MS>]
    [--exts <LIST>]
    [--max-size <BYTES>]
    [--no-initial-scan]
```

| Flag | Effect |
|------|--------|
| `<PATH>` (positional) | Directory to watch. Walked recursively on startup, then tailed for create/modify/delete events. |
| `--debounce-ms <MS>` | Debounce window in milliseconds. Defaults to `[watch] debounce_ms` in `config.toml` (200 ms by default). |
| `--exts <LIST>` | Comma-separated extension list (no leading dot). Overrides the curated default and any `[watch] extensions` value. Example: `--exts md,txt,rs`. |
| `--max-size <BYTES>` | Skip files larger than this. Defaults to 10 MiB. |
| `--no-initial-scan` | Skip the initial-tree walk and only react to events. Mostly for debugging; production runs leave this off so the index is consistent on startup. |

Example:

```bash
open-memory watch ~/notes --exts md,txt
# Walks ~/notes once, then tails create/modify/delete events.
# BLAKE3-deduped against the metadata store, so a re-run over an
# unchanged tree is free.
```

The watcher takes an `Arc<MemoryStore>` so a future
`open-memory mcp --watch DIR` mode can share the MCP server's
handle without opening a second SQLite connection.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `1` | Generic failure. The error message goes to stderr. |
| `2` | clap argument parsing error (`open-memory --help` covers most cases). |

The scriptable subcommands (`remember`, `recall`, `list-entities`,
`forget-entity`) preserve `0` / `1` semantics so shell pipelines
can chain them. `--json` emits structured output suitable for
`jq`.
