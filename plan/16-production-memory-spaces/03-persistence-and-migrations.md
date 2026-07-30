# Persistence and Migrations

## Authority and layout

- Graph SQLite canonical rows plus immutable revisions are semantic truth.
- Product SQLite is control-plane truth for catalog, local authority,
  capabilities, identity review, lineage, jobs, and maintenance cursors.
- FTS/vector/metadata indexes, access telemetry, journals, mirrors, and stubs
  are derived or operational state.
- A manifest binds one catalog identity to one root and generation vector.

```text
<home>/
├── product/product.sqlite
└── data/<profile>/
    ├── space.toml                    # legacy personal-global binding
    ├── .space-id                     # pre-catalog compatibility identity
    ├── memory.sqlite | domains/ ...  # existing DomainStore family
    ├── spaces/<space-uuid>/
    │   ├── space.toml
    │   ├── .space.lock                # stable; never promoted
    │   └── store/<DomainStore artifacts>
    ├── .snapshots/<snapshot-uuid>/
    ├── .merge-staging/<job-uuid>/
    ├── .merge-backup/<job-uuid>/
    └── .merge-intents/<job-uuid>.json
```

The legacy root uses a stable `.personal-global.lock`. Normal
`DomainStore`/standalone opens hold the corresponding advisory lock shared for
their lifetime. Snapshot, restore, domain migration, and promotion require it
exclusive. Lock acquisition is bounded and identity-checked; a supported
external process that remains open yields `space_busy`. Manual tools that
ignore product locks are outside the supported contract and are detected by
post-open integrity/generation checks where possible.

New paths are derived only from canonical UUID IDs and fixed directory names.
Catalog rows store a `RootKey` (`legacy-root` or `space:<uuid>`), never an
arbitrary path.

Registry-owned parents use restrictive permissions. Opening/creating:

- rejects any symlink in the managed path and any existing non-directory;
- proves the canonical root remains below the profile root;
- rejects two catalog rows resolving to one canonical root, including closed
  rows;
- uses no-follow/directory-relative operations from a reviewed safe dependency
  where the platform requires protection from path-swap races;
- rejects unexpected hard-linked managed artifacts on supported Unix targets;
- requires target, staging, intent, and backup on the same filesystem.

Unsupported filesystem/platform combinations disable snapshot/material
promotion capability. Canonicalization alone is not accepted as TOCTOU proof.

## Store identity and manifests

Add `DomainStore::open_scoped(..., SpaceId)` and
`MemoryStore::open_scoped(..., SpaceId)`. Every domain stores the same
`space_id` in `memory_meta`; mismatch or disagreement fails as corruption.

Compatibility open:

1. load `space.toml` if present;
2. otherwise atomically load/create one UUIDv7 in `.space-id` before any
   member database opens;
3. pass that ID to all domains.

First catalog binding adopts the existing ID. It never mints a replacement.

`space.toml` is a bounded, versioned structure:

```toml
format_version = 1
space_id = "..."
profile = "default"
owner_kind = "user"
owner_id = "..."
context_kind = "global"
project_id = ""
domain_count = 1
created_at_unix_secs = 0
catalog_binding_hash = "blake3:..."
```

The binding hash uses explicit domain-separated field encoding over format,
space/profile, owner/context, project, domain count, and root key. Display names
are absent. Write with same-directory exclusive temp creation, file `sync_all`,
rename, then parent-directory `sync_all`; verify on every open.

Pinned domain count is 1–64 and must satisfy the existing shard/divisibility
rules. A larger or changed manifest fails before any member database opens.

## Product migrations

Refactor `product_store.rs` to the repository `Migrator`. Preserve v1
`daemon_jobs` and `daemon_events` behavior. Apply one transaction per ordered
step and set both metadata and `user_version` consistently:

- v2: spaces, local authority, projects, capabilities;
- v3: identity review, merge jobs, lineage;
- v4: resumable maintenance.

Every migration validates the exact prior version, rolls back on fault, is
idempotent on reopen, and refuses a future version.

### Product v2

| Table | Required keys and invariants |
|---|---|
| `memory_spaces` | `id` PK; owner/context enums; non-null `project_key`; unique `(profile, owner_kind, owner_id, context_kind, project_key)`; unique `root_key`; positive pinned `domain_count`; manifest hash; `creating|active|closed|deleting|error`; catalog generation |
| `local_principals` | opaque ID PK; display label; `active|disabled`; generation/timestamps |
| `local_teams` | ID PK; profile; label; `active|archived`; monotonic authority generation |
| `team_memberships` | `(team_id, principal_id)` PK; role; authority generation; optional expiry; FK to team/principal; applies only to spaces whose owner is that team |
| `projects` | ID PK; profile; label; timestamps |
| `workspace_projects` | workspace ID PK; unique `(profile, canonical_path)`; path platform/normalizer version; project FK; optional VCS fingerprint; `active|moved|detached` |
| `context_capabilities` | token-hash PK; principal/actor/profile; bounded versioned context; authority digest; expiry/revocation; never plaintext token |

Use a non-null empty `project_key` for global uniqueness. All list/lookup paths
have covering indexes by profile/state, principal/space, expiry, and
`(created_at, id)`. Membership/team/catalog mutations and generation
increments are one product transaction. This release has team-wide roles; it
does not add a second per-space ACL whose precedence could become ambiguous.

`canonical_path` is constructed once from a validated `WorkspacePathKey`, not
from a display string. The key records the platform/path-normalization version
and requires a reversible UTF-8 OS path in this release. A non-UTF-8 path is
rejected before insert; no lossy rendering enters uniqueness, authorization,
project inference, or persistence. A full-profile restore onto an incompatible
platform marks the mapping detached and requires an explicit new local mapping
rather than reinterpreting path bytes.

### Product v3

| Table | Required keys and invariants |
|---|---|
| `identity_candidates` | job and canonical pair unique; exact left/right revisions; policy/ontology/resolver generations; canonical packet hash; bounded packet payload; deterministic state; current review head |
| `identity_agent_proposals` | candidate/revision/packet bound; model/prompt/tool provenance; bounded structured proposal; validity; never an authoritative decision |
| `identity_decision_events` | immutable lineage/verified-ID/human decisions; packet hash; supersedes pointer; actor/rationale; checked current-head update |
| `merge_jobs` | source/target; state machine; immutable snapshot manifests; target version; action-stream/plan/predicted hashes; confirmation binding/expiry; bounded report/error |
| `merge_resolutions` | one current identity or conservative keep-distinct receipt per `(job, candidate)`; exact no-overlap coverage |
| `space_lineages` | source/target/job; source/prior-target/result hashes; immutable manifest reference |

Do not store `agent_proposal` as a decision source and do not issue a receipt
from it. Candidate packets and reports may use bounded versioned JSON for
inspection, but their identity hashes use canonical binary field encoding.
Semantic content remains in private snapshot/graph storage, not product logs.

Merge action streams and immutable snapshots live in daemon-owned job
directories. Product rows store only validated relative artifact keys, size,
hash, and state. A row cannot advance until its referenced artifact has been
fsynced and verified.

### Product v4

`maintenance_tasks` stores ID, profile, optional space, typed kind/state,
versioned cursor, bounded counters/error, and timestamps. It owns resumable
backfill, repair, cleanup, and retained-backup deletion. Cursor advancement is
in the same transaction as each completed page's control-plane result.

## Graph migrations

Raise `MEMORY_SCHEMA_VERSION` from 2 to 4 through ordered v3/v4 steps.
Migration changes schema only; no startup corpus scan.

### Graph v3: observation audit and index visibility

Add to observations: nullable `current_revision_id`, `row_version NOT NULL
DEFAULT 1`, and lifecycle default `active`.

Add:

- singleton `domain_state` with semantic/index/mirror generations and backfill
  readiness;
- `change_sets` with space, idempotency key, request version/hash, submit mode,
  actor/authority/reason/source, base/committed generation, state and
  `state_version`; unique `(space_id, idempotency_key)`;
- ordered `change_requests` with typed versioned bounded payload and expected
  head/version/lifecycle;
- ordered compact `change_events`;
- immutable `observation_revisions`, concepts, and source-file sets;
- ordered `index_outbox` entries keyed by generation and ordinal with
  operation, revision, attempts, and bounded last error.

State enum: `proposed|applied|rejected|conflicted|reverted`. Every transition
uses an exact expected state/version and requires exactly one affected row.
Submit mode is part of the canonical request hash.

### Graph v4: entity/relation history and provenance

Add current revision, row version, and lifecycle to entities/relations. Add:

- immutable entity revisions, aliases, and identifiers;
- identifier trust and source-verification metadata;
- canonical relation IDs, canonical/mirror role, immutable relation revisions,
  and evidence;
- immutable origin contributions keyed by target and exact origin revision;
- durable mirror outbox keyed by canonical relation and target domain.

Backfill canonical relation IDs from canonical rows, write explicitly marked
mirrors idempotently, verify traversal, then remove ambiguous legacy mirrors.
Relation edit/merge readiness remains false until this completes.

Migration tests cover fresh creation, populated v1/v2 to v4, every failed-step
rollback, exact schema/index/constraint shape, reopen, and future refusal.

## Domain transaction protocol

For one applied domain-local changeset:

1. Validate all bounds, canonicalize/hash the request, route, and precompute
   embeddings outside graph locks.
2. Acquire the graph rebuild barrier and `BEGIN IMMEDIATE`.
3. Recheck idempotency, state/version, expected heads/lifecycle, bound space,
   and supplied current authority; read current generation for the receipt and
   enforce equality only for an operation with an explicit whole-domain
   precondition.
4. Lazily create a baseline revision for touched legacy rows.
5. Insert immutable revisions/contributions/events, update projections with
   exact affected-row checks, and enqueue index/mirror work.
6. Increment semantic generation once; mark the changeset applied.
7. Commit once.
8. Under the same rebuild barrier, drain index work through that generation.
9. Advance indexed generation only after durable index success and return an
   explicit readiness receipt.

If derived work fails after graph commit, truth remains committed and the
outbox remains pending; recall returns `index_repair_required` instead of
silently serving stale state. Mirror application is at-least-once because it
crosses databases; the canonical edge remains truth and traversal reports its
degraded mirror generation.

## Canonical hashes and versions

Use versioned, domain-separated BLAKE3 encodings:

```text
openmemory/entity/v1
openmemory/observation/v1
openmemory/relation/v1
openmemory/domain-snapshot/v1
openmemory/space-snapshot/v1
openmemory/identity-packet/v1
openmemory/identity-receipt/v1
openmemory/merge-actions/v1
openmemory/merge-plan/v1
```

Encode tag/version and every semantic field in fixed order with checked
length-framing. Canonically sort set-like fields and reject duplicates.
Exclude JSON formatting, SQL/file order, labels used only for display,
timestamps without semantic meaning, telemetry, caches, indexes, journals,
stubs, and mirror rows.

`SpaceVersion` is the numeric-domain-ordered vector of semantic/index/mirror
generations plus combined canonical hash. Completion order never affects it.
Hash-version changes require explicit migration/revalidation; receipts are
never silently reinterpreted.

Every durable artifact distinguishes semantic identity from file encoding.
Canonical stream/action hashes cover typed semantic records; file hashes cover
the exact stored representation. A completed row or cache entry is published
only after its referenced immutable artifact is fully written, synced, hashed,
and atomically named. Atomic publication is the completeness signal; an
independent “done” flag may summarize state but cannot make a partial artifact
valid.

## Snapshot protocol

Only one coordinator may snapshot a space:

1. authorize; reserve execution, memory, and disk budgets; create a new private
   staging directory exclusively;
2. pause local admission, quiesce every accepted writer, drain/repair index and
   mirror outboxes, checkpoint journals, close local store handles, and release
   their shared process lock;
3. acquire the stable process lock exclusively and reopen the space under that
   owner; this excludes daemon-less supported writers;
4. capture the numeric ordered domain generation vector;
5. export canonical domain streams and checkpoint domain families through the
   shared executor (default concurrency 1);
6. at the full barrier, verify every database integrity/FK, canonical hash,
   generation, index/mirror readiness, manifest binding, size, and file set
   against the capture;
7. write/fsync one space snapshot manifest containing every domain hash,
   generation, schema, file size/hash, and combined semantic hash;
8. atomically rename the staged snapshot to its immutable published ID, fsync
   the parent, close exclusive snapshot handles, release the process lock,
   reopen the registry runtime, then release admissions.

Any failure publishes nothing. Safe unpublished staging is removed
idempotently. If deadline cancellation cannot stop a domain task, keep the
space closed/degraded and require recovery; do not resume writers around an
unknown snapshot boundary.

Snapshots are daemon-private, non-symlink, read-only after publication, and
re-hashed before every planning/materialization use. No live source handle is
given to a materializer.

Each domain snapshot has two independently hashed views:

- the canonical semantic stream consumed by identity/planning; and
- a target-preservation stream for graph audit/control state, including
  idempotency receipts, requests, events, revisions, lifecycle history,
  contributions, and pending/rejected changesets.

Source audit/control state is never imported into another space. Target
preservation is mandatory so replacement cannot erase review/history. Drained
derived outboxes, caches, mirrors, indexes, telemetry, and journals are not
semantic merge input.

## Material staging and generation barrier

The coordinator reads immutable source/target manifests and streams sorted
canonical records through the pure planner. It emits a hash-bound action stream
and external, bounded per-domain spools. After final dispositions, the engine
coordinator deterministically adds mirror/stub instructions to the appropriate
spools. Those instructions are derived from canonical relation actions and are
excluded from planner input and semantic hashes.

Staging opens through a crate-private job-token API, not normal catalog open.
It derives the root beneath `.merge-staging`, marks the manifest with job/state,
uses the target `SpaceId`, and cannot yield a live `SpaceHandle`.

Each domain worker owns one new SQLite/index family and performs:

1. batched target semantic/audit preservation and merge-only actions;
2. revision/contribution/event linkage;
3. mirror/stub instruction application and durable-outbox drain;
4. FTS/vector/metadata rebuild;
5. WAL checkpoint/close;
6. integrity/FK/count/generation/canonical/index verification.

Default material concurrency is two. A full barrier verifies all domain
receipts, the numeric generation vector, global one-to-one accounting,
action-stream hash, expected counts, and predicted result hash before the
staged-root manifest is fsynced. No complete source+target+result graph is held
in memory.

After applying actions, revalidate every pending target proposal against the
result heads/lifecycle. Preserve an unaffected proposal and its idempotency
receipt unchanged; transition a moved proposal to `conflicted` with a compact
system event. Never silently rebase it. Applied/rejected audit history is
preserved exactly, and merge-generated idempotency keys occupy a separate
versioned namespace.

## Promotion and recovery

Create and fsync a bounded intent before staging. Intent paths are validated
profile-relative keys derived from job/space IDs, never caller strings.

```text
prepared -> staging_verified -> target_backed_up -> staging_promoted
         -> promoted_verified -> catalog_committed -> complete
```

Immediately before rename:

- reauthorize the Maintainer and confirmation;
- acquire the registry's exclusive target admission guard used by every
  writer/read handle;
- quiesce/checkpoint/close target;
- release the daemon's shared process lock and acquire the stable target lock
  exclusively with a deadline;
- re-snapshot target and require the exact expected hash/version;
- close the recheck handles while retaining the exclusive process lock;
- reverify staging and disk/filesystem preflight.

Promotion implementations:

- new space: rename complete live `store/` to backup and staging `store/` to
  live;
- legacy personal-global: move only an explicit versioned allowlist of
  `DomainStore` artifacts, never the profile directory.

Every intent update, rename, and affected parent directory is fsynced in order.
Open and verify the new target before one product transaction commits catalog,
lineage, and job state. Clear/fsync intent last. The old verified target remains
under retention. Default merge-backup retention is 7 days (validated 1–365);
quota pressure blocks a new merge rather than deleting the only recovery path.

Recovery runs before opening an affected space and exhaustively handles all
live/staging/backup combinations. It verifies hashes before roll-forward or
rollback, never creates an empty target, never guesses on ambiguous state, and
is idempotent after repeated process death. Cleanup is a later maintenance job.

## Disk, corruption, backup, and backfill

- Preflight uses actual allocated bytes for live target, predicted staged
  canonical/index data, SQLite WAL/temp/index scratch, retained backups, and a
  configurable safety reserve. A simple multiplier of old live bytes is not
  sufficient when source is larger.
- Recheck free space before every high-amplification phase. SQLite `FULL` and
  low-level I/O faults are injected through a controllable VFS/test filesystem;
  no successful receipt is returned before required sync.
- Integrity, FK, generation, manifest, and canonical-hash mismatch fails closed
  with operator repair/recovery diagnostics.
- Backfills use stable ID pages, bounded transactions, persisted cursors, and
  idempotent writes. They yield between pages through maintenance admission.
- Full-profile backup freezes product/job publication, captures each space as
  an independently consistent version in numeric `SpaceId` lock order, then
  records one backup manifest binding those versions to the product snapshot.
  It does not claim a cross-SQLite transaction. It includes manifests,
  revisions, proposals, outboxes, identity/merge state, snapshots required by
  active jobs, and recovery intents/backups.
- Restore never overwrites an open root. It stages, verifies, promotes, and
  rebinds through the same primitives. Space-only/external imports never import
  membership as authority. A trusted full-profile restore may restore local
  team records, but rotates the installation/credential authority epoch,
  revokes all restored context capabilities and confirmations, and requires
  fresh authentication before any job may publish.
