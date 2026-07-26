# OpenMemory Desktop Plan

This directory is the cross-repository plan for turning `openmemory`
from a local memory engine into a small, production-quality desktop
product. It is intentionally separate from
[`docs/roadmap.md`](../docs/roadmap.md), which tracks crate and release
history.

The product thesis:

> OpenMemory Desktop is the local-first memory control plane for AI
> coding agents. It gives users one inspectable, correctable, portable
> memory across Codex, Claude Code, Claude Desktop, OpenClaw, and other
> MCP clients.

The engine stays open source. This repository owns the local platform:
CLI, MCP server, daemon, admin API contracts, diagnostics, setup,
profile/workspace plumbing, and backup primitives. The sibling
`openmemory-desktop` repository owns the Tauri app, frontend UX,
commercial packaging, licensing, updater, and Pro-only product flows.

## Document Map

Read in this order for a cold start. Each file is scoped enough to be
useful on its own.

| Document | Purpose |
|----------|---------|
| [00-repository-split.md](00-repository-split.md) | Repository boundary, ownership rules, dependency direction, release coordination, and anti-fork constraints. |
| [01-product-brief.md](01-product-brief.md) | Positioning, target users, jobs to be done, V1 non-goals, and the open-core product boundary. |
| [02-product-principles.md](02-product-principles.md) | Product and design principles that keep the app local-first, simple, inspectable, and consistent with the engine. |
| [03-system-architecture.md](03-system-architecture.md) | Process model, workspace additions, daemon boundaries, data ownership, and migration strategy. |
| [04-admin-api.md](04-admin-api.md) | Local admin API contract, endpoint groups, auth, error shape, pagination, and event stream rules. |
| [05-desktop-ux.md](05-desktop-ux.md) | First-run setup, dashboard, memory explorer, search, recall explanation, integrations, settings, and tray behavior. |
| [06-capture-and-review.md](06-capture-and-review.md) | Assisted capture pipeline, candidate model, review inbox, suppressions, and acceptance criteria. |
| [07-security-privacy.md](07-security-privacy.md) | Threat model, network policy, secrets, logs, backups, permissions, telemetry, and data deletion. |
| [08-quality-performance.md](08-quality-performance.md) | Engineering gates, test matrix, performance budgets, observability, and release-blocking checks. |
| [09-packaging-commercial.md](09-packaging-commercial.md) | Installers, update paths, migration from CLI installs, licensing, free/Pro boundary, and support surface. |
| [10-delivery-roadmap.md](10-delivery-roadmap.md) | Phases, milestones, definitions of done, risks, metrics, and open decisions. |
| [11-daemon.md](11-daemon.md) | Local daemon purpose, lifecycle, auth, crate boundaries, API build order, job model, and milestones. |
| [12-daemon-production-hardening.md](12-daemon-production-hardening.md) | Research anchors, implemented daemon hardening, production gate, and remaining product-repo boundaries. |
| [13-adapter-evidence-extraction/INDEX.md](13-adapter-evidence-extraction/INDEX.md) | Evidence-first adapter redesign: source records, extraction, validation, CLI, rollout, and production gates. |
| [14-project-scoped-memory.md](14-project-scoped-memory.md) | Per-project graph isolation, workspace identity, layered global recall, daemon lifecycle, APIs, Desktop UX, migration, and production gates. |
| [15-memory-spaces-audit-and-merge.md](15-memory-spaces-audit-and-merge.md) | Validated generalization of scoped graphs to personal/team memory spaces, compact audit history, manual editing, copy, three-way merge, recovery, and phased production gates. |
| [16-production-memory-spaces/INDEX.md](16-production-memory-spaces/INDEX.md) | Implementation-ready production package for memory spaces, local team review, immutable audit/manual editing, conservative identity resolution, contribution-preserving merge, exact crate/schema/API changes, exhaustive validation, delivery sequence, and agent prompt. |

## Product Constraints

- Local-first behavior is the product, not a deployment option.
- The desktop app must not become required for CLI or MCP usage.
- The product repo depends on this repo through released binaries,
  crates, and typed admin contracts; it must not fork engine internals.
- The UI must explain and repair local setup failures without exposing
  raw internal errors.
- Automatic capture is review-first by default.
- All schema changes are additive or forward-only.
- Desktop code never edits SQLite directly; it uses engine/admin APIs.
- No feature silently sends memory content, logs, or identifiers over
  the network.
- Idle CPU should be effectively zero.

## Initial Scope

V1 is a local desktop app for individual AI coding power users.
It supports setup, health, inspection, search, edit/delete, client
integration repair, reviewed capture, and backup/restore.

Small team features can follow only after individual retention is
proven. Hosted memory APIs, general document management, enterprise
RBAC, mobile, and legal e-discovery are not V1 goals.
