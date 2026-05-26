# Integrations

`openmemory setup` is the recommended path. It runs `init`, detects
every supported MCP client on the machine, and registers `openmemory`
with each one in a single command:

```bash
openmemory setup
```

It is idempotent. Re-run after upgrading the binary or after
installing a new client and it picks up the new state.

The per-client `openmemory integrate <target>` commands stay available
for explicit, scripted control (one client at a time).

## Supported clients

| Client | Command | Config written |
|--------|---------|----------------|
| Claude Code | `openmemory integrate claude-code` | `~/.claude.json` |
| Claude Desktop | `openmemory integrate claude-desktop` | platform-specific (see below) |
| Codex CLI | `openmemory integrate codex` | `~/.codex/config.toml` |
| OpenClaw | `openmemory integrate openclaw` | `~/.openclaw/openclaw.json` |

All targets share the same flags:

```text
--http ADDR      Emit an HTTP-transport entry (streamable-http) instead of stdio
--binary PATH    Override the binary path in the entry (default: openmemory)
--config PATH    Override the target config file path
```

## Claude Code

```bash
openmemory integrate claude-code
```

When the `claude` CLI is on PATH, the integrator delegates to
`claude mcp add-json` for validated, hot-applied registration. When
`claude` is not available, it writes directly to `~/.claude.json`.

Pass `--no-cli` to skip the CLI and always write the file directly.

The entry is written under `mcpServers`:

```json
{
  "mcpServers": {
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
```

## Claude Desktop

```bash
openmemory integrate claude-desktop
```

Platform-specific config paths:

| OS | Path |
|----|------|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Linux | `$XDG_CONFIG_HOME/Claude/claude_desktop_config.json` (default: `~/.config/Claude/`) |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |

The entry shape is the same as Claude Code (under `mcpServers`).
Restart Claude Desktop after running the command.

## Codex CLI

```bash
openmemory integrate codex
```

Codex stores MCP servers in `~/.codex/config.toml` (override via
`$CODEX_HOME`) under `[mcp_servers.<name>]` blocks. The integrator
edits that file in place using `toml_edit`, so sibling tables and
comments survive the rewrite untouched.

The entry shape, in TOML:

```toml
[mcp_servers.openmemory]
command = "openmemory"
args = ["mcp"]

[mcp_servers.openmemory.env]
OPENMEMORY_HOME = "/Users/<user>/.openmemory"
OPENMEMORY_PROFILE = "default"
```

Restart `codex` after running the command so it re-reads the config.

## OpenClaw

```bash
openmemory integrate openclaw
```

See [openclaw.md](openclaw.md) for the full integration contract,
config resolution strategy, and compatibility commitments.

## Manual configuration

If you prefer to configure your client by hand, add this to its MCP
server config (the key path varies by client):

```json
{
  "openmemory": {
    "command": "openmemory",
    "args": ["mcp"]
  }
}
```

| Client | Config file | Key path |
|--------|-------------|----------|
| Claude Code | `~/.claude.json` | `mcpServers` |
| Claude Desktop | platform-specific (above) | `mcpServers` |
| Codex CLI | `~/.codex/config.toml` | `[mcp_servers.<name>]` (TOML) |
| OpenClaw | `~/.openclaw/openclaw.json` | `mcp.servers` |

## HTTP transport

Any target accepts `--http <addr>` to emit an HTTP-transport entry
instead of the default stdio entry:

```bash
openmemory integrate claude-code --http 127.0.0.1:7800
```

This writes a streamable-http entry:

```json
{
  "openmemory": {
    "url": "http://127.0.0.1:7800/mcp",
    "transport": "streamable-http"
  }
}
```

The integrator does not start the server. Run
`openmemory mcp --http 127.0.0.1:7800` separately. See
[mcp.md](mcp.md#streamable-http-behind-mcp-http) for bearer-token
auth setup.

## Profiles

`--profile <name>` isolates a memory store and renames the server
entry so multiple profiles can coexist:

```bash
openmemory --profile work integrate claude-code
# writes entry as "openmemory-work" pointing at ~/.openmemory/data/work/
```

## Other MCP clients

Any MCP client that accepts stdio server entries can use openmemory.
Add this to your client's server config:

```json
{
  "command": "openmemory",
  "args": ["mcp"],
  "env": {
    "OPENMEMORY_HOME": "/path/to/.openmemory",
    "OPENMEMORY_PROFILE": "default"
  }
}
```

The env block is optional; defaults are `~/.openmemory` and `default`.

If your client is popular enough to warrant a first-class `integrate`
target, open an issue.
