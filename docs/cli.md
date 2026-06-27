# CLI reference

The `openmemory` binary is a thin clap surface. Every subcommand
is a small adapter to a function in
[`crates/openmemory-cli/src/commands/`](../crates/openmemory-cli/src/commands/)
that does the real work. The clap definitions live in
[`crates/openmemory-cli/src/cli.rs`](../crates/openmemory-cli/src/cli.rs):

## Top-level invocation

```text
openmemory [--home <PATH>] [--profile <NAME>] <SUBCOMMAND> [ARGS...]
```

| Global flag | Effect |
|-------------|--------|
| `--home <PATH>` | Override the data root. Defaults to `$OPENMEMORY_HOME` or `~/.openmemory`. Setting it also exports `OPENMEMORY_HOME` to the process so subprocesses inherit the override. |
| `--profile <NAME>` | Memory profile under the data root. Defaults to `default`. |

Built-in `--help`, `--version`, and shell completions all work.

## `openmemory setup`

One-shot end-to-end onboarding. Runs `init`, detects every supported
MCP client on the machine, and registers `openmemory` with each.
Idempotent: re-run any time after installing a new client or
upgrading the binary.

```text
openmemory setup [--client <LIST>] [--all] [--with-model] [--yes]
```

| Flag | Effect |
|------|--------|
| `--client <LIST>` | Comma-separated subset of `claude-code,claude-desktop,codex,openclaw`. Defaults to every detected client. |
| `--all` | Register with every known client even if not detected on the machine. |
| `--with-model` | Also download the default embedding model. Requires the `embeddings` build feature. |
| `--yes` | Non-interactive; assume yes to every prompt. |

Exit code is non-zero if every targeted integration failed or if MCP
startup verification failed. Partial integration success returns zero
when verification succeeds, and the per-client status is reported.

The verification step (spawning `openmemory mcp` and confirming it
boots) can be skipped by setting `OPENMEMORY_SETUP_SKIP_VERIFY=1` in
the environment, which is useful inside test harnesses.

## `openmemory init`

Initialise the data directory and write a default `config.toml`.

```text
openmemory init [--force]
```

| Flag | Effect |
|------|--------|
| `--force` | Overwrite an existing `config.toml`. Without it, `init` refuses to clobber. |

Creates `~/.openmemory/`, `~/.openmemory/data/<profile>/`, and
writes a populated `config.toml` from the
`Config::default()` shape. Idempotent across re-runs unless
something changed.

## `openmemory status`

Print summary of memory and index state. No flags.

```text
openmemory status
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

## `openmemory daemon`

Start, inspect, and gracefully stop the optional local admin daemon.
The daemon binds loopback only, requires the per-home bearer token on
admin endpoints, and writes discovery metadata under
`<home>/run/daemon.json`.

```text
openmemory daemon start [--foreground] [--addr <ADDR>]
openmemory daemon status [--json]
openmemory daemon stop [--json]
```

| Command | Effect |
|---------|--------|
| `start --foreground` | Starts the admin API in the current process. Port `0` lets the OS choose an available loopback port. |
| `status --json` | Reads runtime discovery, authenticates to `/admin/health`, and emits `running`, `not_started`, or `unreachable`. |
| `stop --json` | Sends authenticated `POST /admin/shutdown`, waits for graceful server exit, and removes stale runtime metadata. |

The daemon is not required for CLI or stdio MCP usage. It is the
desktop/admin control plane for health, memory browsing, search,
integrations, durable jobs/events, backup, and restore.

## `openmemory mcp`

Start the MCP server.

```text
openmemory mcp [--http <ADDR>]
```

| Flag | Effect |
|------|--------|
| (none) | Default. Stdio transport: read JSON-RPC requests from stdin, write responses to stdout. |
| `--http <ADDR>` | Bind a Streamable HTTP listener on `<ADDR>` (e.g. `127.0.0.1:7800`). Requires the `mcp-http` build feature. |

Examples:

```bash
# Stdio (the default OpenClaw entry point)
openmemory mcp

# HTTP (requires --features mcp-http)
openmemory mcp --http 0.0.0.0:7800

# HTTP with bearer-token auth (recommended for any non-loopback bind)
export OPENMEMORY_HTTP_TOKEN="$(openssl rand -hex 32)"
openmemory mcp --http 0.0.0.0:7800
```

Stdio mode runs Tokio current-thread; HTTP mode runs Tokio
multi-thread. See [mcp.md](mcp.md#transports) for transport detail
including the bearer-token contract.

## `openmemory consolidate`

Run dedup plus decay-prune once and print the report.

```text
openmemory consolidate [--dedup-threshold <F>] [--prune-floor <F>] [--min-age-secs <SECS>]
```

| Flag | Effect |
|------|--------|
| `--dedup-threshold <F>` | Override the Jaccard text-similarity threshold (0.0–1.0). Default from `[memory] dedup_threshold`, normally `0.95`. |
| `--prune-floor <F>` | Override the decay-prune score floor. Default from `[memory] prune_floor`, normally `0.05`. |
| `--min-age-secs <SECS>` | Minimum age (seconds) for an observation to be eligible for pruning. |

Idempotent: a second call right after the first reports zero work.
See [search.md](search.md#consolidation) for the math.

## `openmemory integrate openclaw`

Register `openmemory` in OpenClaw's MCP config. The defining
"out-of-the-box" command.

```text
openmemory integrate openclaw [--config <PATH>] [--http <ADDR>] [--binary <PATH>]
```

| Flag | Effect |
|------|--------|
| `--config <PATH>` | Override the OpenClaw config path. Defaults to `$OPENCLAW_CONFIG_PATH` or `~/.openclaw/openclaw.json`. |
| `--http <ADDR>` | Emit an HTTP-transport entry (`streamable-http`) pointing at the given address (e.g. `127.0.0.1:7821`) instead of the default stdio entry. |
| `--binary <PATH>` | Override the binary path written into the entry. Defaults to the bare `openmemory`, which OpenClaw resolves via `$PATH`. |

Examples:

```bash
# Default: stdio entry under mcp.servers.openmemory
openmemory integrate openclaw

# HTTP-transport entry pointing at a locally-running mcp --http server
openmemory integrate openclaw --http 127.0.0.1:7821

# Pin the binary path explicitly (for non-PATH-resolvable installs)
openmemory integrate openclaw --binary /opt/openmemory/bin/openmemory
```

Idempotent: a re-run with no changes reports "no changes". A re-run
with a changed entry prints the diff. Full integration contract in
[openclaw.md](openclaw.md):

## `openmemory integrate codex`

Register `openmemory` in Codex CLI's TOML config. Edits
`~/.codex/config.toml` (or `$CODEX_HOME/config.toml`) in place using
`toml_edit`, preserving every sibling table and comment.

```text
openmemory integrate codex [--config <PATH>] [--http <ADDR>] [--binary <PATH>]
```

| Flag | Effect |
|------|--------|
| `--config <PATH>` | Override the Codex config path. Defaults to `$CODEX_HOME/config.toml` or `~/.codex/config.toml`. |
| `--http <ADDR>` | Emit an HTTP-transport entry (`streamable-http`) pointing at the given address (e.g. `127.0.0.1:7800`) instead of stdio. |
| `--binary <PATH>` | Override the binary path written into the entry. Defaults to the bare `openmemory`, which Codex resolves via `$PATH`. |

Restart `codex` after registering so it re-reads the config.

## `openmemory remember`

Append observations to an entity from the command line. Mainly for
scripting and testing; the MCP `openmemory_remember` tool is the
in-agent path.

```text
openmemory remember <ENTITY>
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
openmemory remember Raymond \
    --entity-type person \
    --observation 'prefers Rust' \
    --observation 'maintains openmemory' \
    --relation 'maintains=openmemory:project'
```

## `openmemory recall`

Search memory by natural-language query. CLI counterpart to
`openmemory_recall`.

```text
openmemory recall <QUERY>
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

## `openmemory list-entities`

Browse entities, optionally filtered by type.

```text
openmemory list-entities
    [--entity-type <TYPE>]
    [--limit <N>]
    [--offset <N>]
    [--json]
```

## `openmemory forget-entity`

Hard-delete an entity and its observations and relations.

```text
openmemory forget-entity <ENTITY> --yes
```

`--yes` is required to confirm the destructive action; the command
aborts otherwise.

## `openmemory ingest <PATH>`

Bulk-load a data source through the concurrent write-behind context
engine (journaled, batched commits; partitioned per `[engine] domains`
when configured).

```text
openmemory ingest <PATH> [--format auto|markdown|chat] [--no-normalize] [--json]
```

| Flag | Effect |
|------|--------|
| `--format` | Source shape. `auto` (default) picks `chat` for `.jsonl` files and `markdown` for directories. Markdown: one entity per note (H1 title or file stem), one observation per `##` section, `Attendees:` lines become `has_participant` relations. Chat: one entity per channel, one observation per `{channel, user, ts, text}` line. |
| `--no-normalize` | Skip fuzzy entity-name normalization for the bulk load; offline consolidation still dedups. |
| `--json` | Emit a single-line JSON report. |

## `openmemory migrate-domains`

Re-home a profile to a different storage-domain count (see
`[engine] domains` in [configuration](configuration.md)), including
1 → K (partition a single-store profile) and K → 1 (restore the
classic layout).

```text
openmemory migrate-domains --domains <N> --yes
```

Offline only: stop any MCP server, watcher, or TUI holding the profile
first, and shut the engine down cleanly so its journals are empty (a
non-empty journal aborts the migration). The new layout is built in a
staging directory, verified by raw-count reconciliation, then swapped
in under a crash-safe intent sentinel; ids, timestamps, tombstones,
access counts, embedding vectors, and free-text index entries are
preserved byte-exactly (nothing is re-embedded), while cross-domain
stubs and mirror edges are re-derived for the new boundaries. The old
layout is parked in `.migrate-backup/` inside the profile and is never
deleted automatically; remove it after verifying.

`--yes` is required to confirm; the command aborts otherwise.

## `openmemory completions <SHELL>`

Emit shell completions for the named shell. Behind the
default-on `completions` feature.

```text
openmemory completions <bash|fish|zsh|elvish|powershell>
```

The output goes to stdout; redirect into the appropriate location
for your shell:

```bash
openmemory completions fish > ~/.config/fish/completions/openmemory.fish
openmemory completions bash > /etc/bash_completion.d/openmemory
openmemory completions zsh  > "${fpath[1]}/_openmemory"
```

## `openmemory watch <PATH>`

Watch a directory and incrementally re-index changed files. Behind
the default-on `watch` feature. Full crate detail in
[watcher.md](watcher.md):

```text
openmemory watch <PATH>
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
openmemory watch ~/notes --exts md,txt
# Walks ~/notes once, then tails create/modify/delete events.
# BLAKE3-deduped against the metadata store, so a re-run over an
# unchanged tree is free.
```

The watcher takes an `Arc<MemoryStore>` so a future
`openmemory mcp --watch DIR` mode can share the MCP server's
handle without opening a second SQLite connection.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `1` | Generic failure. The error message goes to stderr. |
| `2` | clap argument parsing error (`openmemory --help` covers most cases). |

The scriptable subcommands (`remember`, `recall`, `list-entities`,
`forget-entity`) preserve `0` / `1` semantics so shell pipelines
can chain them. `--json` emits structured output suitable for
`jq`.
