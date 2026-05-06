# Integrations

`open-memory integrate <target>` registers the MCP server in a client's
config file. One command, no manual JSON editing. Run it again after
upgrading and it updates the entry idempotently.

## Supported clients

| Client | Command | Config written |
|--------|---------|----------------|
| Claude Code | `open-memory integrate claude-code` | `~/.claude.json` |
| Claude Desktop | `open-memory integrate claude-desktop` | platform-specific (see below) |
| OpenClaw | `open-memory integrate openclaw` | `~/.openclaw/openclaw.json` |

All targets share the same flags:

```text
--http ADDR      Emit an HTTP-transport entry (streamable-http) instead of stdio
--binary PATH    Override the binary path in the entry (default: open-memory)
--config PATH    Override the target config file path
```

## Claude Code

```bash
open-memory integrate claude-code
```

When the `claude` CLI is on PATH, the integrator delegates to
`claude mcp add-json` for validated, hot-applied registration. When
`claude` is not available, it writes directly to `~/.claude.json`.

Pass `--no-cli` to skip the CLI and always write the file directly.

The entry is written under `mcpServers`:

```json
{
  "mcpServers": {
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
```

## Claude Desktop

```bash
open-memory integrate claude-desktop
```

Platform-specific config paths:

| OS | Path |
|----|------|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Linux | `$XDG_CONFIG_HOME/Claude/claude_desktop_config.json` (default: `~/.config/Claude/`) |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |

The entry shape is the same as Claude Code (under `mcpServers`).
Restart Claude Desktop after running the command.

## OpenClaw

```bash
open-memory integrate openclaw
```

See [openclaw.md](openclaw.md) for the full integration contract,
config resolution strategy, and compatibility commitments.

## HTTP transport

Any target accepts `--http <addr>` to emit an HTTP-transport entry
instead of the default stdio entry:

```bash
open-memory integrate claude-code --http 127.0.0.1:7800
```

This writes a streamable-http entry:

```json
{
  "open-memory": {
    "url": "http://127.0.0.1:7800/mcp",
    "transport": "streamable-http"
  }
}
```

The integrator does not start the server. Run
`open-memory mcp --http 127.0.0.1:7800` separately. See
[mcp.md](mcp.md#streamable-http-behind-mcp-http) for bearer-token
auth setup.

## Profiles

`--profile <name>` isolates a memory store and renames the server
entry so multiple profiles can coexist:

```bash
open-memory --profile work integrate claude-code
# writes entry as "open-memory-work" pointing at ~/.open-memory/data/work/
```

## Other MCP clients

Any MCP client that accepts stdio server entries can use open-memory.
Add this to your client's server config:

```json
{
  "command": "open-memory",
  "args": ["mcp"],
  "env": {
    "OPEN_MEMORY_HOME": "/path/to/.open-memory",
    "OPEN_MEMORY_PROFILE": "default"
  }
}
```

The env block is optional; defaults are `~/.open-memory` and `default`.

If your client is popular enough to warrant a first-class `integrate`
target, open an issue.
