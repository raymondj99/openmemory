# Persistence and Migrations

## Storage authority

- Graph SQLite rows and immutable revisions are semantic truth.
- Product SQLite is control-plane truth for catalog, local authority, context
  capabilities, identity decisions, lineages, and jobs.
- FTS/vector files, metadata indexes, access counts, engine journals, partition
  stubs, and mirror relations are derived or operational state.
- A space manifest binds an opaque catalog identity to one physical root. It
  does not replace either database.

## Final layout

```text
<home>/
├── config.toml
├── product/
│   └── product.sqlite
└── data/<profile>/
    ├── space.toml                     # personal-global binding; legacy root
    ├── .space-id                      # compatibility identity before binding
    ├── memory.sqlite / domains/ ...   # existing DomainStore family
    ├── spaces/
    │   └── <space-uuid>/
    │       ├── space.toml
    │       └── store/
    │           ├── memory.sqlite ...  # or domains/domain-NN families
    │           └── ...
    ├── .merge-staging/<job-uuid>/
    ├── .merge-backup/<job-uuid>/
    └── .merge-intents/<job-uuid>.json
```

Opaque canonical IDs, never display names, construct new paths. Validate every
resolved root remains beneath `data/<profile>` with no symlink escape. Staging,
target, and backup must be on the same filesystem before work begins.

The existing profile root stays a valid `DomainStore` root. New space roots use
`spaces/<id>/store` so manifest/control files cannot be mistaken for index
files. Backup and domain migration must understand both shapes.

Promotion therefore has two explicit store-layout implementations:

- `DirectoryStoreRoot` for new spaces: rename the complete `store/` directory
  to backup and staging into `store/`.
- `LegacyStoreRoot` for personal-global at the profile root: move only the
  enumerated `DomainStore` artifacts (`memory.sqlite`, search/metadata/vector/
  embedding artifacts, or `domains/` plus `domains.toml`) under an open-blocking
  intent, exactly as domain-count migration does. Never rename the profile
  directory, because it also contains sibling spaces and merge control state.

Both implementations checkpoint/close first, fsync every rename parent, retain
the verified old artifact set, block open while intent exists, and share one
exhaustively tested recovery state machine.

### Store identity before catalog binding

Audited graph rows need one stable `SpaceId`, including when legacy/direct CLI
code opens a store before the daemon catalog exists.

- Add `DomainStore::open_scoped(config, root, domains, SpaceId)` and
  `MemoryStore::open_scoped(config, domain_dir, SpaceId)`. Every domain writes
  or verifies `memory_meta['space_id']`; a mismatch fails closed.
- Compatibility `DomainStore::open/open_existing` loads `space.toml` when
  present. Otherwise it atomically loads or mints one UUIDv7 in root `.space-id`
  before opening any member database, then passes the same ID to every domain.
- Compatibility single `MemoryStore::open` may load/verify the database meta
  ID and mint one only for a truly standalone single database. Production
  `DomainStore` and `SpaceHandle` paths always pass an explicit root ID.
- First catalog binding adopts this existing ID instead of minting another,
  writes the full manifest, and retains `.space-id` as a harmless compatibility
  marker. New spaces use their catalog/manifest ID from creation.
- Opening a partitioned root whose domain databases disagree on bound space ID
  is corruption; never repair it by choosing one silently.

## `space.toml`

Write atomically (same-directory temp, file fsync, rename, parent fsync) and
verify on every open.

```toml
format_version = 1
space_id = "018f..."
profile = "default"
owner_kind = "user"       # user | team
owner_id = "local:..."
context_kind = "global"   # global | project
project_id = ""            # empty only for global
domain_count = 1
created_at_unix_secs = 0
catalog_binding_hash = "blake3:..."
```

The binding hash is a domain-separated hash of format version, space/profile,
owner/context, project key, domain count, and catalog root key. It detects a
directory copied beneath another catalog row. Display name is deliberately
absent so renaming a UI label does not rewrite storage identity.

## Product database migrations

Refactor `product_store.rs` into ordered migrations. Preserve v1 jobs/events
tables byte-for-byte. Use `PRODUCT_SCHEMA_VERSION = 4` after this feature.

### Product v2: spaces and local authority

Use a non-null `project_key` because SQLite uniqueness treats `NULL` values as
distinct.

```sql
CREATE TABLE memory_spaces (
    id                    TEXT PRIMARY KEY,
    profile               TEXT NOT NULL,
    owner_kind            TEXT NOT NULL CHECK(owner_kind IN ('user','team')),
    owner_id               TEXT NOT NULL,
    context_kind          TEXT NOT NULL CHECK(context_kind IN ('global','project')),
    project_key            TEXT NOT NULL DEFAULT '',
    display_name          TEXT NOT NULL,
    root_key              TEXT NOT NULL UNIQUE,
    domain_count          INTEGER NOT NULL CHECK(domain_count >= 1),
    format_version        INTEGER NOT NULL,
    manifest_hash         BLOB NOT NULL,
    state                 TEXT NOT NULL CHECK(state IN
                              ('creating','active','closed','deleting','error')),
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    CHECK((context_kind = 'global' AND project_key = '') OR
          (context_kind = 'project' AND project_key <> '')),
    UNIQUE(profile, owner_kind, owner_id, context_kind, project_key)
);

CREATE INDEX idx_memory_spaces_profile_state
    ON memory_spaces(profile, state, updated_at);

CREATE TABLE local_principals (
    id                    TEXT PRIMARY KEY,
    display_name          TEXT NOT NULL,
    state                 TEXT NOT NULL CHECK(state IN ('active','disabled')),
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);

CREATE TABLE local_teams (
    id                    TEXT PRIMARY KEY,
    profile               TEXT NOT NULL,
    display_name          TEXT NOT NULL,
    authority_generation  INTEGER NOT NULL DEFAULT 1,
    state                 TEXT NOT NULL CHECK(state IN ('active','archived')),
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);

CREATE TABLE space_memberships (
    space_id              TEXT NOT NULL REFERENCES memory_spaces(id),
    principal_id          TEXT NOT NULL REFERENCES local_principals(id),
    role                  TEXT NOT NULL CHECK(role IN
                              ('reader','contributor','reviewer','maintainer')),
    authority_generation  INTEGER NOT NULL,
    expires_at            INTEGER,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    PRIMARY KEY(space_id, principal_id)
) WITHOUT ROWID;

CREATE INDEX idx_space_memberships_principal
    ON space_memberships(principal_id, space_id);

CREATE TABLE projects (
    id                    TEXT PRIMARY KEY,
    profile               TEXT NOT NULL,
    display_name          TEXT NOT NULL,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);

CREATE TABLE workspace_projects (
    workspace_id          TEXT PRIMARY KEY,
    profile               TEXT NOT NULL,
    canonical_path        TEXT NOT NULL,
    project_id            TEXT NOT NULL REFERENCES projects(id),
    vcs_fingerprint       TEXT,
    state                 TEXT NOT NULL CHECK(state IN ('active','moved','detached')),
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    UNIQUE(profile, canonical_path)
);

CREATE TABLE context_capabilities (
    token_hash            BLOB PRIMARY KEY,
    principal_id          TEXT NOT NULL REFERENCES local_principals(id),
    actor_kind            TEXT NOT NULL CHECK(actor_kind IN ('human','agent','system')),
    profile               TEXT NOT NULL,
    project_id            TEXT,
    active_team_id        TEXT,
    authorization_generation INTEGER NOT NULL,
    context_json          TEXT NOT NULL,
    created_at            INTEGER NOT NULL,
    expires_at            INTEGER NOT NULL,
    revoked_at            INTEGER
);

CREATE INDEX idx_context_capabilities_expiry
    ON context_capabilities(expires_at);
```

Store only a BLAKE3 hash of an opaque random capability. `context_json` is a
bounded versioned encoding of the concrete read set and default write target;
load revalidates current membership/generation before returning handles.

### Product v3: identity, lineage, and merge jobs

```sql
CREATE TABLE identity_candidates (
    id                    TEXT PRIMARY KEY,
    merge_job_id          TEXT NOT NULL,
    left_space_id         TEXT NOT NULL,
    left_entity_id        TEXT NOT NULL,
    left_revision_id      TEXT NOT NULL,
    right_space_id        TEXT NOT NULL,
    right_entity_id       TEXT NOT NULL,
    right_revision_id     TEXT NOT NULL,
    policy_generation     INTEGER NOT NULL,
    packet_hash           BLOB NOT NULL,
    packet_json           TEXT NOT NULL,
    deterministic_state   TEXT NOT NULL,
    proposal_state        TEXT NOT NULL,
    current_decision      TEXT,
    current_event_id      TEXT,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    UNIQUE(merge_job_id, left_space_id, left_entity_id,
           right_space_id, right_entity_id)
);

CREATE INDEX idx_identity_candidates_job_state
    ON identity_candidates(merge_job_id, proposal_state, id);

CREATE TABLE identity_decision_events (
    id                    TEXT PRIMARY KEY,
    candidate_id          TEXT NOT NULL REFERENCES identity_candidates(id),
    supersedes_event_id   TEXT,
    decision              TEXT NOT NULL CHECK(decision IN
                              ('same','different','undetermined')),
    decision_source       TEXT NOT NULL CHECK(decision_source IN
                              ('lineage','verified_identifier','human','agent_proposal')),
    principal_id          TEXT,
    actor_kind            TEXT NOT NULL,
    packet_hash           BLOB NOT NULL,
    rationale_json        TEXT NOT NULL,
    model_provenance_json TEXT,
    created_at            INTEGER NOT NULL
);

CREATE TABLE merge_jobs (
    id                    TEXT PRIMARY KEY,
    profile               TEXT NOT NULL,
    source_space_id       TEXT NOT NULL,
    target_space_id       TEXT NOT NULL,
    requested_by          TEXT NOT NULL,
    state                 TEXT NOT NULL CHECK(state IN
                              ('discovering','awaiting_review','planned','confirmed',
                               'staging','verified','promoting','succeeded',
                               'cancelled','conflicted','failed','recovery_required')),
    source_snapshot_hash  BLOB,
    target_snapshot_hash  BLOB,
    target_generation_json TEXT,
    plan_hash             BLOB,
    predicted_result_hash BLOB,
    confirmation_hash     BLOB,
    confirmation_expires_at INTEGER,
    report_json           TEXT NOT NULL DEFAULT '{}',
    error_json            TEXT,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);

CREATE INDEX idx_merge_jobs_profile_state
    ON merge_jobs(profile, state, updated_at);

CREATE TABLE merge_resolutions (
    merge_job_id          TEXT NOT NULL REFERENCES merge_jobs(id),
    candidate_id          TEXT NOT NULL REFERENCES identity_candidates(id),
    decision_event_id     TEXT NOT NULL REFERENCES identity_decision_events(id),
    receipt_hash          BLOB NOT NULL,
    PRIMARY KEY(merge_job_id, candidate_id)
) WITHOUT ROWID;

CREATE TABLE space_lineages (
    id                    TEXT PRIMARY KEY,
    source_space_id       TEXT NOT NULL,
    target_space_id       TEXT NOT NULL,
    merge_job_id          TEXT NOT NULL REFERENCES merge_jobs(id),
    source_snapshot_hash  BLOB NOT NULL,
    prior_target_hash     BLOB NOT NULL,
    result_target_hash    BLOB NOT NULL,
    manifest_relpath      TEXT NOT NULL,
    created_at            INTEGER NOT NULL
);
```

Candidate packets and reports are bounded JSON control records. Semantic object
content/revisions stay in graph roots.

### Product v4: migration/backfill progress and retention

```sql
CREATE TABLE maintenance_tasks (
    id                    TEXT PRIMARY KEY,
    profile               TEXT NOT NULL,
    space_id              TEXT,
    kind                  TEXT NOT NULL,
    state                 TEXT NOT NULL,
    cursor_json           TEXT NOT NULL DEFAULT '{}',
    counters_json         TEXT NOT NULL DEFAULT '{}',
    last_error_json       TEXT,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);

CREATE INDEX idx_maintenance_tasks_state
    ON maintenance_tasks(profile, state, updated_at);
```

Backfill jobs, cleanup, and retained backup deletion use this table. Do not
overload `daemon_jobs` with resumable internal cursors.

## Graph database migrations

Raise `MEMORY_SCHEMA_VERSION` from 2 to 4 with two ordered migrations. Test
fresh creation, v1->v4, v2->v4 with populated rows, rollback of each failed
step, idempotent reopen, and future-version refusal.

### Graph v3: observation history, changesets, and index outbox

```sql
ALTER TABLE observations ADD COLUMN current_revision_id TEXT;
ALTER TABLE observations ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE observations ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';

CREATE TABLE domain_state (
    singleton             INTEGER PRIMARY KEY CHECK(singleton = 1),
    semantic_generation   INTEGER NOT NULL DEFAULT 0,
    indexed_generation    INTEGER NOT NULL DEFAULT 0,
    mirror_generation     INTEGER NOT NULL DEFAULT 0,
    backfill_state        TEXT NOT NULL DEFAULT 'pending'
);
INSERT OR IGNORE INTO domain_state(singleton) VALUES(1);

CREATE TABLE change_sets (
    id                    TEXT PRIMARY KEY,
    space_id              TEXT NOT NULL,
    idempotency_key       TEXT NOT NULL,
    request_hash          BLOB NOT NULL,
    actor_principal       TEXT NOT NULL,
    actor_kind            TEXT NOT NULL,
    authorization_generation INTEGER NOT NULL,
    reason                TEXT NOT NULL,
    source                TEXT NOT NULL,
    state                 TEXT NOT NULL CHECK(state IN
                              ('proposed','applied','rejected','conflicted','reverted')),
    base_generation       INTEGER NOT NULL,
    committed_generation  INTEGER,
    created_at            INTEGER NOT NULL,
    decided_at            INTEGER,
    decided_by            TEXT,
    UNIQUE(space_id, idempotency_key)
);

CREATE INDEX idx_change_sets_state_created
    ON change_sets(state, created_at, id);

CREATE TABLE change_requests (
    change_set_id         TEXT NOT NULL REFERENCES change_sets(id),
    ordinal               INTEGER NOT NULL,
    payload_version       INTEGER NOT NULL,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    operation             TEXT NOT NULL,
    expected_revision_id  TEXT,
    expected_row_version  INTEGER,
    requested_payload_json TEXT NOT NULL,
    PRIMARY KEY(change_set_id, ordinal)
) WITHOUT ROWID;

CREATE TABLE change_events (
    change_set_id         TEXT NOT NULL REFERENCES change_sets(id),
    ordinal               INTEGER NOT NULL,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    operation             TEXT NOT NULL,
    before_revision_id    TEXT,
    after_revision_id     TEXT,
    before_lifecycle      TEXT,
    after_lifecycle       TEXT,
    compact_payload_json  TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(change_set_id, ordinal)
) WITHOUT ROWID;

CREATE TABLE observation_revisions (
    id                    TEXT PRIMARY KEY,
    observation_id        TEXT NOT NULL REFERENCES observations(id),
    parent_revision_id    TEXT,
    semantic_hash         BLOB NOT NULL,
    content               TEXT NOT NULL,
    observed_at           INTEGER NOT NULL,
    valid_from            INTEGER,
    valid_until           INTEGER,
    confidence            REAL NOT NULL,
    source                TEXT NOT NULL,
    memory_tier           TEXT NOT NULL,
    title                 TEXT,
    summary               TEXT,
    importance            REAL,
    source_kind           TEXT,
    created_by_change_set TEXT NOT NULL REFERENCES change_sets(id),
    created_at            INTEGER NOT NULL,
    UNIQUE(observation_id, semantic_hash, created_by_change_set)
);

CREATE INDEX idx_observation_revisions_object_created
    ON observation_revisions(observation_id, created_at, id);

CREATE TABLE observation_revision_concepts (
    revision_id           TEXT NOT NULL REFERENCES observation_revisions(id),
    concept               TEXT NOT NULL,
    PRIMARY KEY(revision_id, concept)
) WITHOUT ROWID;

CREATE TABLE observation_revision_source_files (
    revision_id           TEXT NOT NULL REFERENCES observation_revisions(id),
    file_path             TEXT NOT NULL,
    PRIMARY KEY(revision_id, file_path)
) WITHOUT ROWID;

CREATE TABLE index_outbox (
    generation            INTEGER NOT NULL,
    object_kind           TEXT NOT NULL,
    logical_id            TEXT NOT NULL,
    operation             TEXT NOT NULL CHECK(operation IN ('upsert','delete')),
    revision_id           TEXT,
    attempts              INTEGER NOT NULL DEFAULT 0,
    last_error            TEXT,
    PRIMARY KEY(generation, object_kind, logical_id)
) WITHOUT ROWID;
```

Legacy observations keep `current_revision_id = NULL`. First semantic mutation
creates a synthetic baseline changeset/revision and the requested revision in
the same transaction. A resumable backfill fills untouched rows later; startup
does not scan them.

### Graph v4: entity/relation history and merge provenance

```sql
ALTER TABLE entities ADD COLUMN current_revision_id TEXT;
ALTER TABLE entities ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE entities ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';

ALTER TABLE relations ADD COLUMN canonical_relation_id TEXT;
ALTER TABLE relations ADD COLUMN mirror_role TEXT NOT NULL DEFAULT 'canonical';
ALTER TABLE relations ADD COLUMN current_revision_id TEXT;
ALTER TABLE relations ADD COLUMN row_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE relations ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';

CREATE UNIQUE INDEX idx_relations_canonical_role
    ON relations(canonical_relation_id, mirror_role)
    WHERE canonical_relation_id IS NOT NULL;

CREATE TABLE entity_revisions (
    id                    TEXT PRIMARY KEY,
    entity_id             TEXT NOT NULL REFERENCES entities(id),
    parent_revision_id    TEXT,
    semantic_hash         BLOB NOT NULL,
    name                  TEXT NOT NULL,
    entity_type           TEXT NOT NULL,
    confidence            REAL NOT NULL,
    source                TEXT NOT NULL,
    created_by_change_set TEXT NOT NULL REFERENCES change_sets(id),
    created_at            INTEGER NOT NULL
);

CREATE TABLE entity_revision_aliases (
    revision_id           TEXT NOT NULL REFERENCES entity_revisions(id),
    alias                 TEXT NOT NULL,
    PRIMARY KEY(revision_id, alias)
) WITHOUT ROWID;

CREATE TABLE entity_revision_identifiers (
    revision_id           TEXT NOT NULL REFERENCES entity_revisions(id),
    namespace             TEXT NOT NULL,
    raw_value             TEXT NOT NULL,
    canonical_value       TEXT,
    trust                 TEXT NOT NULL CHECK(trust IN ('claimed','source_verified')),
    source_snapshot_id    TEXT,
    verifier_version      TEXT,
    resolver_generation   INTEGER,
    PRIMARY KEY(revision_id, namespace, raw_value)
) WITHOUT ROWID;

CREATE INDEX idx_verified_identifier_lookup
    ON entity_revision_identifiers(namespace, canonical_value, revision_id)
    WHERE trust = 'source_verified' AND canonical_value IS NOT NULL;

CREATE TABLE relation_revisions (
    id                    TEXT PRIMARY KEY,
    relation_id           TEXT NOT NULL,
    parent_revision_id    TEXT,
    semantic_hash         BLOB NOT NULL,
    from_entity           TEXT NOT NULL,
    to_entity             TEXT NOT NULL,
    relation_type         TEXT NOT NULL,
    weight                REAL NOT NULL,
    valid_from            INTEGER,
    valid_until           INTEGER,
    source                TEXT NOT NULL,
    evidence_json         TEXT NOT NULL DEFAULT '{}',
    created_by_change_set TEXT NOT NULL REFERENCES change_sets(id),
    created_at            INTEGER NOT NULL
);

CREATE TABLE origin_contributions (
    object_kind           TEXT NOT NULL,
    target_logical_id     TEXT NOT NULL,
    origin_space_id       TEXT NOT NULL,
    origin_logical_id     TEXT NOT NULL,
    origin_revision_id    TEXT NOT NULL,
    semantic_hash         BLOB NOT NULL,
    contribution_json     TEXT NOT NULL,
    imported_by_change_set TEXT NOT NULL REFERENCES change_sets(id),
    created_at            INTEGER NOT NULL,
    PRIMARY KEY(object_kind, target_logical_id,
                origin_space_id, origin_logical_id, origin_revision_id)
) WITHOUT ROWID;

CREATE TABLE mirror_outbox (
    generation            INTEGER NOT NULL,
    canonical_relation_id TEXT NOT NULL,
    operation             TEXT NOT NULL CHECK(operation IN ('upsert','delete')),
    target_domain         INTEGER NOT NULL,
    payload_json          TEXT NOT NULL,
    attempts              INTEGER NOT NULL DEFAULT 0,
    last_error            TEXT,
    PRIMARY KEY(generation, canonical_relation_id, target_domain)
) WITHOUT ROWID;
```

Backfill sets `canonical_relation_id = id` for canonical rows. Partition mirror
rebuild identifies mirrors from the canonical side, writes new explicitly
marked mirrors idempotently, verifies traversal, then removes legacy ambiguous
mirrors. Relation edit/merge remains disabled until that job completes.

## Transaction and generation rules

For one domain-local applied changeset:

1. Validate bounded payload and compute embeddings/semantic hashes outside the
   writer lock where possible.
2. Take the graph rebuild write barrier and begin `IMMEDIATE` transaction.
3. Recheck changeset/idempotency, authorization generation supplied by the
   daemon, expected revisions/row versions, lifecycle, and current generation.
4. Create lazy baseline revisions if needed.
5. Insert new immutable revisions, update canonical projection/head/lifecycle,
   append change events and origin contributions, enqueue index/mirror rows.
6. Increment `domain_state.semantic_generation` once and stamp the changeset's
   committed generation.
7. Commit once.
8. Still under the rebuild barrier, drain index work through that generation.
   Update `indexed_generation` only after success.
9. Release the barrier and return a receipt including semantic/index generation
   and degraded state.

If step 8 fails, committed truth/history remains valid and the outbox remains.
The store is not “ready for recall.” The next open/recall attempts repair under
the write barrier; persistent failure returns `IndexRepairRequired`. This is
stricter and more honest than serving stale search state.

Mirror outbox work may cross domain databases and therefore cannot share the
canonical transaction. It is at-least-once and idempotent; the canonical edge
is truth. APIs expose `mirror_generation`/degraded traversal until repair.

## Canonical semantic hashes

Use versioned domain-separated BLAKE3 encodings:

```text
openmemory/entity/v1
openmemory/observation/v1
openmemory/relation/v1
openmemory/domain-snapshot/v1
openmemory/space-snapshot/v1
openmemory/identity-packet/v1
openmemory/merge-plan/v1
```

Encode type tag, field name/version where necessary, length, and raw canonical
bytes in fixed order. Sort set-like aliases, identifiers, concepts, source
files, contributions, and relation assertions by canonical keys before hashing.
Reject duplicates. Do not hash serde JSON bytes, SQL row order, display-only
labels, access counts, cache state, index files, journals, partition stubs,
mirror rows, or timestamps that are not semantic fields.

A `SpaceVersion` is an ordered vector of domain index plus semantic/index/
mirror generations and the combined canonical hash. Domain order is numeric,
not completion order.

## Legacy binding and backfill

First space-aware daemon startup performs a small recoverable control-plane
transaction:

1. Load/create local installation principal.
2. Detect active profile's existing root, stable `.space-id` (mint atomically if
   absent), and pinned domain count.
3. If no catalog binding exists, create `memory_spaces` row in `creating`
   state for personal-global using the existing root `SpaceId` and root key
   `legacy-root`.
4. Atomically write `data/<profile>/space.toml` and fsync parent.
5. Reopen/verify manifest plus `DomainStore` status.
6. Mark catalog row `active` with manifest hash.

On interruption, startup reconciles `creating` plus manifest combinations
deterministically. It never creates a second personal-global row.

Graph v3/v4 migrations are O(schema), not O(rows). Resumable maintenance jobs
backfill revisions, canonical relation IDs, and mirrors in stable ID pages. Each
page records cursor/counters, commits independently, can be rerun, and never
changes current recall semantics. Feature capability reports distinguish:

- spaces ready,
- observation audit ready,
- entity/relation history backfill pending,
- relation edit/merge ready,
- material merge ready.

## Backup and restore

A full-profile backup captures, under coordinated admission pauses:

- Product catalog generation and relevant product rows.
- Every space manifest/root and generation vector.
- Pending/proposed changesets, revisions, contributions, outboxes, identity
  decisions, merge jobs, and lineage.
- Recovery intents/backups when present, or fails preflight with explicit
  recovery-required status.

Restore never overwrites an open root. It stages and verifies using the same
promotion primitive as merge, then updates catalog binding. A space-only import
always asks whether to create a new space or merge/cherry-pick into an existing
authorized destination; it never trusts imported owner/membership as local
authorization.
