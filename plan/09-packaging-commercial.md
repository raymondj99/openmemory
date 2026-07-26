# Packaging And Commercial Plan

This file describes work owned primarily by the `openmemory-desktop`
product repository. The `open-memory` repository must provide stable
platform contracts and installable binaries that the product can ship
or detect.

## Packaging Direction

Ship macOS first, then Windows, then Linux.

macOS public beta requirements:

- Signed app.
- Notarized build.
- Installer or disk image with clear upgrade behavior.
- Preserves existing CLI/MCP installs.
- Preserves user data.
- Can install without account creation.

The CLI distribution remains separate and scriptable from this
repository. Homebrew or curl installer paths should not require the
desktop app.

## Install And Upgrade

Install flow:

1. Install app.
2. Start or install daemon.
3. Detect existing `openmemory` binary and data root.
4. Detect supported MCP clients.
5. Preview config changes.
6. Verify one client.

Upgrade flow:

- Preserve data root.
- Preserve profile/store.
- Preserve client config unless repair is explicitly accepted.
- Run migrations with backup preflight where needed.
- Recover daemon if previous version is still running.
- Show typed diagnostics on failure.

## Migration From CLI-Only Installs

The desktop app must treat existing users carefully:

- Detect current binary path.
- Detect `OPENMEMORY_HOME` where possible.
- Detect configured clients.
- Detect installed model.
- Detect existing profiles/stores.
- Avoid rewriting config until preview is accepted.
- Offer backup before product migrations.

No existing CLI/MCP user should lose a working setup by installing the
desktop product.

## Update Channels

Channels:

- Stable.
- Beta.
- Internal.

Updates should be explicit until the public update path is proven.
Release notes should call out migrations, config changes, and network
behavior changes.

## Free And Pro Boundary

Free:

- CLI.
- MCP server.
- Local engine.
- Basic desktop dashboard.
- Memory explorer.
- Manual edit/delete.
- Client setup and `doctor`.

Pro:

- Review inbox.
- Advanced recall explanations.
- Automatic transcript capture.
- Encrypted local backups.
- Optional encrypted cloud backup/sync.
- Advanced filters and saved searches.
- Priority updates.

The boundary should be easy to explain. Free users must retain access to
their local memory and all core OSS surfaces.

## License Behavior

License activation:

- Does not block local CLI/MCP after activation expires.
- Supports reasonable offline grace period.
- Avoids sending memory content.
- Stores tokens in OS keychain or encrypted local secret store.
- Surfaces current license state in settings.

Failure modes:

- Expired license disables Pro-only UI actions.
- Local memory remains readable through free surfaces.
- Backup restore of local user data remains available enough to avoid
  lock-in or data loss.

## Optional Cloud

Cloud can add value only as optional encrypted backup/sync and license
management. It should not become the memory system of record for V1.

Cloud backup/sync rules:

- Off by default.
- End-to-end encryption before upload.
- Clear account and network state in settings.
- Local app remains useful with sync disabled.
- Restore path works from a local backup artifact.

## Support Surface

Support docs needed before V1:

- Install.
- First-run setup.
- Codex integration.
- Claude Code integration.
- Claude Desktop integration.
- OpenClaw integration.
- Model download.
- Keyword-only mode.
- Backup and restore.
- Privacy and network behavior.
- Migration from CLI-only installs.
- Troubleshooting `doctor` codes.

Support tooling:

- Local debug bundle export.
- Redacted logs.
- Integration verification report.
- Store health report.
