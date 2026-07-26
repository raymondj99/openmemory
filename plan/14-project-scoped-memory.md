# Project-Scoped Memory

## Status

Proposed implementation specification for engine, daemon, MCP, admin API,
and OpenMemory Desktop.

This document closes the workspace/scope gap left intentionally open in
the product architecture. It is the normative design for individual,
local project separation. Team sharing and remote sync remain later work.

## Decision Summary

OpenMemory will use a layered graph model:

```text
active project graph + profile-global graph -> layered recall
```

The two layers are physically independent `DomainStore` families. They
do not share SQLite tables, vector indexes, journals, relations, or
entity identifiers.

- A **profile** remains the hard user-controlled isolation boundary
  (`default`, `work`, `personal`).
- Every profile has exactly one **global graph**, using the profile's
  current store layout.
- A mapped local workspace may have one **project graph**, stored under
  that profile.
- Reads in a workspace are layered over only two graphs: the active
  project and global.
- Writes target exactly one graph. There is no implicit dual-write.
- Relations and spreading activation never cross graph boundaries.
- No request fans out over every known project.

This design is preferred over adding `workspace_id` columns to the
existing graph because the current vector index has no exact namespace
filter. Post-filtering approximate-nearest-neighbor results can lose
valid hits, while independent graph stores provide exact isolation,
cheap deletion, and predictable performance using existing code.

## Goals

- Prevent facts from one project appearing in another project by
  default.
- Preserve useful user-global memory across projects.
- Keep current CLI and MCP behavior unchanged when no workspace mapping
  exists.
- Reuse `MemoryStore`, `DomainStore`, `ContextEngine`, and current search
  indexes without creating a second implementation.
- Make the active memory boundary visible and inspectable in Desktop.
- Keep layered recall proportional to two graph queries, regardless of
  how many projects the user has.
- Make deleting or backing up one project graph straightforward.

## Non-Goals

- Team or organization sharing.
- Cloud sync.
- Cross-project recall by default.
- Automatic merging of similarly named entities across graphs.
- Cross-graph relations.
- Per-observation ACLs or enterprise RBAC.
- Inferring that a memory is global from its prose.

## Vocabulary

| Term | Meaning |
|------|---------|
| Profile | A hard local isolation domain selected by the user. |
| Workspace | Product metadata mapping a local directory to a project graph. |
| Graph reference | `global` or `project:<workspace_id>`. |
| Global graph | Memory intentionally available across projects in one profile. |
| Project graph | Memory isolated to one mapped workspace. |
| Layered recall | Parallel recall from the active project and global graph, followed by deterministic fusion. |
| Session context | Resolved profile, optional workspace, readable graphs, and default write target for one client session. |

Use **graph** in engine and API code. Use **project** and **global** in
user-facing UI. Avoid using `domain` for project separation; engine
domains are performance partitions inside one graph.

## Architecture

```text
Codex / Claude Code / CLI / Desktop
                |
                v
       WorkspaceContextResolver
                |
       +--------+---------+
       |                  |
       v                  v
Project DomainStore   Global DomainStore
       |                  |
       +--------+---------+
                |
                v
        LayeredRecallMerger
```

The daemon owns workspace resolution and graph-handle lifecycle. Engine
crates continue to operate on one graph at a time. A small orchestration
layer composes existing graph operations rather than teaching every
storage primitive about workspaces.

## Repository Change Map

| Area | Ownership |
|------|-----------|
| `openmemory-core` | Validated `WorkspaceId`, `GraphRef`, `ReadScope`, and `MemoryContext` types. |
| `openmemory-engine` | Shared two-graph recall merger and graph-targeted runtime helpers usable by daemon and direct CLI/MCP fallback. |
| `openmemory-admin` | Workspace, graph, scoped search, recall explanation, health, error, and event DTOs. |
| `openmemory-daemon` | Product-store migration, workspace resolver, bounded graph registry, admin handlers, jobs, and lifecycle. |
| `openmemory-mcp` | Optional target/scope inputs and graph provenance in outputs. |
| `openmemory-cli` | Workspace-aware MCP fallback, graph selectors for administration, status, backup, and migration. |
| `openmemory-desktop` | Workspace selection, scoped Overview/Memory/Search/Connections/Review UX. |

The layered merge algorithm belongs in `openmemory-engine`, not in an
HTTP handler or frontend. The daemon owns handle caching because it owns
long-lived process lifecycle. Direct CLI/MCP fallback may open the two
required stores for one command and invoke the same engine merger.

## On-Disk Layout

Keep the current profile root as the global graph so existing installs
do not require a data move:

```text
~/.openmemory/
+-- data/
|   +-- default/                    # existing global graph root
|       +-- memory.sqlite
|       +-- vectors.bin / hnsw/
|       +-- engine-journal/
|       +-- domains.toml
|       +-- projects/
|           +-- <workspace-id>/     # independent DomainStore root
|               +-- memory.sqlite
|               +-- vectors.bin / hnsw/
|               +-- engine-journal/
|               +-- domains.toml
+-- product/
    +-- product.sqlite              # workspace mappings and daemon metadata
```

Rules:

- `workspace-id` is an opaque lowercase UUID, never a path or repository
  name.
- A project directory is passed directly to `DomainStore::open`; its
  internal layout remains opaque.
- Domain count is pinned independently for each graph. Initially a new
  project inherits the profile's configured domain count.
- The model/runtime cache remains shared at the OpenMemory home level.
- Product metadata is not stored in any graph database.
- Project graph paths must be constructed from validated IDs, never
  joined from caller-provided strings.

## Product Metadata

Increment the daemon product schema and add:

```sql
CREATE TABLE workspaces (
    id                    TEXT PRIMARY KEY,
    profile               TEXT NOT NULL,
    display_name          TEXT NOT NULL,
    canonical_path        TEXT NOT NULL,
    git_remote_fingerprint TEXT,
    created_at_unix_secs  INTEGER NOT NULL,
    updated_at_unix_secs  INTEGER NOT NULL,
    last_seen_unix_secs   INTEGER NOT NULL,
    state                 TEXT NOT NULL DEFAULT 'active',
    UNIQUE(profile, canonical_path)
);

CREATE INDEX idx_workspaces_profile
    ON workspaces(profile, state, display_name);
```

`state` is one of `active`, `missing`, or `detached`. Detaching a
workspace removes path-based activation but does not delete its graph.

The raw canonical path is local product metadata. It must never enter
telemetry or diagnostic exports unless the user explicitly includes
paths.

## Workspace Identity Resolution

Resolve a client session to a workspace in this order:

1. An admin-controlled explicit workspace ID.
2. A single MCP root supplied by the client.
3. The client's launch working directory.
4. No workspace.

For path resolution:

1. Convert to an absolute normalized path.
2. Resolve symlinks for comparison when the path exists.
3. Match the longest registered ancestor in the active profile.
4. Never auto-create a mapping during a recall or write.
5. If two mappings are equally specific, return `workspace_ambiguous`.

Git remote fingerprints assist with moved-repository suggestions only.
They must not silently merge or activate a graph because forks commonly
share remotes.

Unknown directories operate global-only until the user creates or
accepts a workspace mapping.

Workspace resolution produces an immutable context snapshot at request
start. A concurrent rename, detach, or remap affects the next request,
never a request already executing. Remapping a path preserves the opaque
workspace ID and project graph; it does not copy memory.

## Core Types

Add shared types outside the graph store implementation:

```rust
pub struct WorkspaceId(String);

pub enum GraphRef {
    Global,
    Project(WorkspaceId),
}

pub enum ReadScope {
    Layered,
    ProjectOnly,
    GlobalOnly,
}

pub struct MemoryContext {
    pub profile: String,
    pub workspace_id: Option<WorkspaceId>,
    pub read_scope: ReadScope,
    pub default_write_graph: GraphRef,
}
```

Constructors validate profile and workspace identifiers. Do not expose
unchecked tuple-field construction across crates.

Every result crossing MCP or admin boundaries includes:

```json
{
  "profile": "default",
  "graph": "project:018f...",
  "workspace_id": "018f..."
}
```

`workspace_id` is omitted for global results.

## Write Semantics

### Defaults

| Session | Default target |
|---------|----------------|
| Mapped workspace | Project graph |
| No mapped workspace | Global graph |
| Desktop project view | Selected project graph |
| Desktop global view | Global graph |

An agent may explicitly request `target: "global"`. It may not name an
arbitrary project graph through MCP. Global writes must be explicit when
a workspace is active; OpenMemory does not classify prose into global
memory automatically.

`remember` gains an optional additive field:

```json
{
  "target": "default" | "project" | "global"
}
```

- `default` preserves session behavior and is the wire default.
- `project` requires a resolved workspace or returns
  `workspace_required`.
- `global` always targets the active profile's global graph.

A single request writes to one `ContextEngine`. Batches containing mixed
targets are rejected. Promotion from project to global is an explicit
copy-then-optional-delete operation with provenance linking the source
observation.

## Recall Semantics

`recall` gains:

```json
{
  "scope": "default" | "layered" | "project" | "global"
}
```

Resolution:

- `default` becomes `layered` when a workspace is active, otherwise
  `global`.
- `layered` without a workspace degrades to global-only.
- `project` without a workspace returns `workspace_required`.
- MCP cannot request all projects.
- Admin search may request one explicit project or a deliberately
  confirmed all-project diagnostic search.

Layered recall runs project and global recall concurrently with the same
query, filters, time, and search mode.

For requested `top_k = K`, each graph retrieves:

```text
candidate_k = clamp(max(16, 3 * K), 16, 256)
```

Merge rules:

1. Apply the existing graph-local recall score first.
2. Multiply project scores by `1.00` and global scores by `0.92`.
3. Deduplicate exact normalized `(entity_type, entity_name, content)`
   matches, keeping the project result.
4. Sort by adjusted score descending.
5. Break ties by project before global, newer `observed_at`, then stable
   observation ID.
6. Return the first `K` results.

The layer prior is configuration-owned but not user-tunable in V1. It
must be included in recall explanations.

Deduplication reuses the engine's canonical text normalization and is
exact after normalization; it must not perform fuzzy semantic merging on
the recall hot path.

Relation spreading occurs independently inside each graph before merge.
There is no cross-graph traversal.

## Search And Indexing

Each graph owns its current keyword and vector indexes. No index entry
requires a workspace field and no ANN result is post-filtered by scope.

Free-text indexing follows the same target rules as memory writes.
Indexed URI uniqueness is graph-local. Search results include their
graph reference so identical URIs in global and project graphs remain
unambiguous.

Cache keys must include:

- Profile.
- Graph reference or layered graph pair.
- Query and `top_k`.
- Search mode and existing filters hash.
- Workspace mapping generation, so detach/remap invalidates context
  caches.

## Graph Registry And Lifecycle

Add a daemon-owned `GraphRegistry`:

```text
GraphRegistry
+-- global: always-open GraphRuntime
+-- projects: bounded LRU<WorkspaceId, GraphRuntime>
+-- in_flight: per-graph open coordination
```

`GraphRuntime` owns one `DomainStore`, optional `ContextEngine`, health
state, and last-used timestamp.

Requirements:

- Open the global graph at daemon startup.
- Open a project graph lazily on first use.
- Coalesce concurrent opens for the same graph.
- Default maximum open project runtimes: `8`.
- Never evict a runtime with active requests or writes.
- Quiesce its `ContextEngine`, flush indexes, and checkpoint WAL before
  eviction.
- Negative-cache open failures briefly to avoid retry storms.
- Do not hold the registry mutex while opening stores or performing
  recall.
- Layered recall borrows both handles, then runs them concurrently.

The first version may keep the existing single active profile. Profile
hot-switching is a separate lifecycle feature; project switching must
not require daemon restart.

## MCP Session Context

The daemon-proxied MCP path resolves session context once and refreshes
it when client roots change. Stdio fallback resolves from the launch
working directory using the same product metadata reader.

MCP responses must identify the resolved context in `status` and in
memory/search result metadata. A client should be able to answer:

- Which profile am I using?
- Which project graph am I using?
- Are reads layered with global memory?
- Where will an unqualified write go?

If daemon and stdio fallback resolve different contexts, MCP startup
fails with a typed diagnostic rather than silently writing elsewhere.

## Admin API

Add typed contracts in `openmemory-admin`.

### Workspaces

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/workspaces?profile=` | List mappings and graph health/counts. |
| `POST /admin/workspaces` | Create a mapping after path validation. |
| `PATCH /admin/workspaces/{id}` | Rename, remap path, or change state. |
| `DELETE /admin/workspaces/{id}` | Detach mapping; never delete graph implicitly. |
| `POST /admin/workspaces/{id}/delete-graph` | Destructive job with impact preview and confirmation token. |

### Graphs And Memory

| Endpoint | Purpose |
|----------|---------|
| `GET /admin/graphs?profile=` | Global and project graph summaries. |
| `GET /admin/entities?graph=` | Browse exactly one graph. |
| `GET /admin/entities/{id}?graph=` | Resolve an entity in one graph. |
| `GET /admin/search?scope=&workspace_id=` | Global, project, or layered search. |
| `POST /admin/recall/explain` | Include graph-local score and layer prior. |
| `GET /admin/connections?scope=&workspace_id=` | Graph visualization payload. |

Collection endpoints never default to all projects. `graph` is required
for entity mutation routes.

New error codes:

- `workspace_not_found`
- `workspace_required`
- `workspace_ambiguous`
- `workspace_path_invalid`
- `workspace_path_conflict`
- `graph_not_found`
- `graph_open_failed`
- `graph_busy`
- `graph_delete_confirmation_required`

New events:

- `workspace.created`
- `workspace.updated`
- `workspace.detached`
- `graph.opened`
- `graph.evicted`
- `graph.deleted`

Paths and memory content are redacted before events enter the persisted
event stream.

## Desktop UX

### Sidebar

The sidebar shows the active profile and project context without turning
workspace selection into permanent visual noise.

- Search defaults to `Project + Global` when a project is active.
- Project selection lives next to the profile status at the bottom of
  the rail or in a compact profile/workspace popover.
- Unmapped folders show `Global only` and an action to create a project
  graph.

### Overview

Show:

- Active profile.
- Active project or `Global only`.
- Project observation/entity counts.
- Global observation/entity counts as secondary context.
- A warning when workspace resolution is ambiguous or detached.

Do not combine project and global counts into one unexplained number.

### Memory And Search

- Default view: active project plus global results.
- Scope control: `Project + Global`, `Project`, `Global`.
- Every row has a graph provenance value, shown on hover/detail rather
  than as a permanent badge when the distinction is obvious.
- Moving a memory between graphs is explicit and previews duplicates.

### Connections

- Default canvas: project graph only.
- Optional `Include global` overlay.
- Project nodes use the normal fill; global nodes use an outlined or
  quieter treatment.
- Relations never render across layers because none exist.
- Entity detail always states `Project` or `Global`.
- Selecting a global node opens its detail in the global graph, not a
  same-named project entity.

### Review

Every candidate carries a proposed graph target. Candidates captured in
a workspace default to project. The user may change the target before
approval. Approval persists to exactly one graph.

## Privacy And Safety Rules

- A project request never reads another project graph.
- Global memory is readable in layered mode by design and is visibly
  identified in explanations.
- Global writes from a project require the explicit MCP target or a
  direct user action.
- Detaching a path does not delete memory.
- Deleting a project graph requires count/size preview, a short-lived
  confirmation token, and a backup recommendation.
- File paths remain in product metadata and provenance only; no path is
  used as a graph identifier.
- Logs record graph kind and opaque workspace ID, not memory content.

## Backup And Restore

The default profile backup includes:

- Global graph.
- Every project graph.
- Workspace product metadata for that profile.
- A manifest relating opaque graph IDs to workspace display names.

A full-profile backup takes a short profile-wide write barrier, captures
one mapping generation, quiesces and checkpoints every open graph, and
then snapshots product metadata and graph roots. The barrier is released
after stable snapshots are established, not after archive compression.
This prevents a backup from containing a workspace mapping without its
corresponding graph generation. Reads may continue where the SQLite
snapshot mechanism is safe.

Also support a project-only export. Project-only restore must choose:

- Restore into its original detached workspace ID.
- Restore as a new project graph with a new ID.

Restore never overwrites an open graph. The registry must evict and
close the target before an approved replacement job begins.

## Compatibility And Migration

No graph SQL migration is required for the initial release.

- Existing profile root data becomes the global graph in place.
- No `projects/` directory is created until a workspace mapping is
  explicitly created.
- With zero mappings, all commands behave exactly as today.
- Existing MCP JSON remains valid because `target` and `scope` are
  optional.
- Export/import remains graph-local; profile backup becomes recursive.
- `migrate-domains` gains an optional graph selector and defaults to the
  global graph for compatibility.
- `status` reports global plus project summaries additively without
  removing current fields during the compatibility window.

Rollout must be gated behind `[workspaces] enabled = false` until daemon,
MCP, backup, and recovery tests pass. Desktop may expose workspace setup
only when daemon capability discovery reports support.

## Performance Budgets

Performance gates use both absolute and relative budgets:

- Layered warm recall touches exactly two graphs.
- Layered recall p95 must be no more than `1.25x` the slower component
  query plus `5 ms` merge overhead on the same fixture.
- Merge time for 512 total candidates must remain below `2 ms` p95.
- A cached project handle adds no more than `1 ms` orchestration overhead
  to project-only recall.
- Opening an uncached 100k-observation project graph must not block
  unrelated global recall.
- Idle workspace support adds zero polling and effectively zero CPU.
- Registry memory is bounded by the configured open-project limit.
- No benchmark may fan out with the total workspace count.

Add benchmark fixtures for:

- Global 100k observations + active project 100k observations.
- 1,000 registered projects with only one open.
- Duplicate content across global and project.
- Four-domain global and project graphs.
- Concurrent layered recall and project writes.

## Test Matrix

### Unit

- Path canonicalization and longest-ancestor resolution.
- Ambiguous and missing workspace behavior.
- Graph reference validation and safe path construction.
- Layered score prior, deduplication, and deterministic tie breaks.
- Default target/scope resolution.
- LRU eviction never removes borrowed or dirty runtimes.

### Integration

- Project A cannot recall Project B content.
- Global memory appears in A and B layered recall.
- Project-specific duplicate overrides the identical global result.
- Project-only and global-only modes are exact.
- Relations and spreading never cross graphs.
- Daemon and stdio fallback resolve the same workspace.
- Detach preserves the project graph.
- Project deletion removes only the selected graph.
- Crash recovery replays journals independently per graph.

### Contract

- MCP old requests retain global-only behavior without a mapping.
- New response metadata round-trips.
- Admin graph selectors are required for mutations.
- Error codes are stable and paths are redacted.
- Capability discovery gates Desktop features.

### Backup

- Full profile round-trip restores all graph stores and mappings.
- Project-only restore under a new ID is isolated.
- Restore refuses an open target graph.
- Domain-partitioned project stores round-trip.

## Delivery Sequence

1. Add shared graph/context types and product-store workspace schema.
2. Implement workspace mapping CRUD and resolver with unit tests.
3. Introduce `GraphRegistry` around the existing global runtime.
4. Add lazy project runtime open, bounded LRU, and health summaries.
5. Add project-targeted remember/index operations.
6. Add layered recall and deterministic merge benchmarks.
7. Extend MCP optional fields and context/status metadata.
8. Extend admin contracts and endpoints.
9. Make backup/restore and domain migration graph-aware.
10. Enable Desktop workspace selection, scoped search, and graph overlay.
11. Run isolation, crash-recovery, performance, and compatibility gates.
12. Enable `[workspaces]` by default only after two-version-old backup
    fixtures restore successfully.

## Acceptance Criteria

The feature is production-ready when:

- Two projects with identical entity names remain fully isolated.
- Layered recall returns only active-project and global memory.
- An unqualified write in a mapped workspace lands only in its project
  graph.
- A global write from a mapped workspace requires explicit targeting.
- Existing users with no workspace mappings observe no behavior change.
- Desktop always displays the active project/global boundary.
- Backup and restore cover global plus project graphs.
- The relative performance budgets pass in CI on fixed fixtures.
- Full workspace tests, clippy, docs, no-default-features tests, and
  locked builds pass.

## Rejected Alternatives

### One `workspace_id` Column In The Existing Graph

Rejected for the initial implementation. SQLite filtering is easy, but
the current vector index cannot guarantee exact scope filtering during
ANN search. Overfetch-and-filter is correctness-sensitive and degrades
as irrelevant scopes grow.

### One Existing Profile Per Project

Useful today as a manual workaround, but insufficient as the product
model. Profiles are user-visible hard boundaries and the daemon has one
active profile; repurposing them as projects makes global-plus-project
recall and profile UX awkward.

### One Unified Graph With Workspace Relations

Rejected because entity identity, relation spreading, consolidation,
and deletion can all leak across projects unless every operation becomes
scope-aware. Physical graph separation provides a smaller correctness
surface.

### Query Every Project And Rank Globally

Rejected for privacy and performance. Cross-project search must be an
explicit admin operation, never the agent-facing default.
