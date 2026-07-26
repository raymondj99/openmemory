# Repository Split

## Decision

Use two repositories:

```text
open-memory/              # open-source platform repo
openmemory-desktop/       # desktop product repo
```

`open-memory` remains the open-source local memory platform. It owns the
engine, CLI, MCP server, local daemon, admin API contract, diagnostics,
setup/integration logic, backup primitives, and tests that protect those
surfaces.

`openmemory-desktop` owns the app product. It contains the Tauri shell,
frontend UI, product workflows, commercial feature gates, updater,
licensing, signing/notarization automation, optional sync client, and
product support assets.

## Why Split

The split keeps the open-source engine trustworthy and useful on its
own while allowing commercial product work to move without mixing closed
assets, license checks, packaging secrets, and release credentials into
the platform repository.

It also gives the desktop app a clean dependency model: it consumes the
platform through released binaries, crates, and typed local API
contracts instead of reaching into storage internals.

## `open-memory` Owns

- Core Rust crates.
- CLI.
- MCP server.
- Local daemon.
- Admin API request/response types.
- Admin API handlers for free local operations.
- `doctor` diagnostics.
- Client detection and setup/integration logic.
- Profile/workspace mapping when it affects recall or setup behavior.
- Backup/restore primitives.
- Store migrations.
- Engine and daemon performance fixtures.
- Public documentation for OSS usage.

## `openmemory-desktop` Owns

- Tauri app shell.
- Frontend UI.
- First-run desktop workflow.
- Dashboard, memory explorer, search UI, recall explanation UI.
- Review inbox UI and Pro gating.
- Licensing and activation UI.
- Auto-update integration.
- macOS signing and notarization assets.
- Windows/Linux packaging when added.
- Optional encrypted cloud backup/sync client.
- Product website/demo/support assets if kept in-repo.

## Dependency Direction

The product repo may depend on the platform repo. The platform repo must
not depend on the product repo.

Allowed product dependencies:

- Released `openmemory` binary.
- Published or path-pinned `openmemory-*` crates during development.
- `openmemory-admin` typed contract.
- Local admin API over loopback.
- CLI JSON output for setup/bootstrap where appropriate.

Disallowed product dependencies:

- Direct SQLite edits.
- Private engine modules.
- Forked MCP behavior.
- Product-only changes to stable MCP tool names or field names.
- Hidden patches that make the desktop app work differently from the
  public CLI/MCP runtime.

## Release Coordination

The platform repo should release first when a desktop feature needs new
daemon/admin behavior. The product repo pins that platform version and
ships after contract tests pass.

Minimum coordination checks:

- Admin API fixture compatibility.
- CLI JSON output compatibility.
- MCP tool compatibility.
- Migration compatibility.
- Backup/restore compatibility.
- Installer upgrade compatibility with existing CLI installs.

## Open-Core Boundary

Free local capability belongs in the platform repo unless it requires
commercial infrastructure or a product-only UI.

Good platform candidates:

- Daemon health.
- Integration verification.
- Typed `doctor` output.
- Manual memory edit/delete through local API.
- Backup artifact creation and restore primitives.

Good product repo candidates:

- Pro trial and license state.
- Review inbox UI gating.
- Advanced saved-search UX.
- Auto-update UX.
- Signed desktop installer workflows.
- Optional cloud sync account flow.

