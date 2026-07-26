# Architecture and File Plan

## Dependency direction

Keep dependencies one-way and policy-free below the daemon:

```text
openmemory-core
   ├── openmemory-merge (pure; no I/O)
   └── openmemory-graph ──> openmemory-index
             │    └──────> openmemory-merge canonical/hash types
             └───────────> openmemory-engine
                                ├──> openmemory-mcp backend contracts
                                └──> openmemory-daemon ──> openmemory-admin DTOs
                                             └──────────> openmemory-cli
```

Actual Cargo edges need not form the visual's bidirectional admin label:
`openmemory-daemon` and `openmemory-cli` depend on `openmemory-admin`;
`openmemory-admin` depends only on serde/serde_json.

The new merge crate depends on `openmemory-core`, `serde`, `thiserror`, and
`blake3`. It must not depend on graph, SQLite, Tokio, Axum, MCP, daemon,
embeddings, or filesystem libraries. `openmemory-graph` may depend on the merge
crate's canonical/hash types to export one source of truth;
`openmemory-engine` invokes the planner and performs I/O.

## New workspace crate: `openmemory-merge`

Add to root `Cargo.toml` members and workspace dependencies.

```text
crates/openmemory-merge/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── canonical.rs
│   ├── error.rs
│   ├── hash.rs
│   ├── identity.rs
│   ├── planner.rs
│   └── three_way.rs
└── tests/
    ├── fixtures.rs
    ├── planner_properties.rs
    └── real_world.rs
```

Responsibilities:

- `canonical.rs`: bounded, ordered immutable snapshot records and origin
  contributions. Constructors validate uniqueness, endpoints, size bounds, and
  sorted canonical order.
- `hash.rs`: versioned domain-separated BLAKE3 semantic/snapshot/packet/plan
  hashes. No serde JSON hashing. Write explicit fields in fixed order with
  length prefixes.
- `identity.rs`: typed evidence, namespace trust, controlled kind compatibility,
  packet binding, deterministic decisions, reviewed receipts, and receipt
  validation.
- `three_way.rs`: pure common-base classification and field-level conflicts.
- `planner.rs`: candidate coverage validation, disposition mapping,
  contribution-preserving entity/claim/relation planning, accounting, and
  predicted result hash.
- `error.rs`: exhaustive typed validation/planning errors.

The crate returns data, never side effects. It does not decide authorization or
call an agent.

## `openmemory-core`

Add:

```text
crates/openmemory-core/src/space.rs
```

Modify:

- `src/lib.rs`: export `space` types.
- `src/config.rs`: add small runtime sections after services exist:
  `SpacesSection`, `AuditSection`, `MergeSection`. Defaults preserve the legacy
  path during staged rollout; the final rollout commit enables spaces/audit for
  new installs while upgrades bind the legacy root in place.
- `src/error.rs`: add only shared validation errors if needed. Workflow errors
  stay in their owning crates.

Do not add a general repository/service framework to core.

Suggested config fields:

```rust
pub struct SpacesSection {
    pub enabled: bool,
    pub max_read_set: usize,      // validated 1..=4; default 4
    pub max_open_spaces: usize,   // validated 1..=64; default 8
    pub idle_close_secs: u64,     // default 300
}

pub struct AuditSection {
    pub enabled: bool,
    pub rejected_payload_ttl_days: u32, // default 30
    pub max_payload_bytes: usize,       // default 1 MiB
}

pub struct MergeSection {
    pub enabled: bool,
    pub staging_disk_multiplier: f32,   // default 2.2
    pub backup_retention_days: u32,     // default 7
    pub confirmation_ttl_secs: u64,     // default 900
}
```

Hard caps remain constants even if config asks for more.

## `openmemory-graph`

Add focused modules:

```text
crates/openmemory-graph/src/
├── canonical.rs        # canonical export/import and semantic hash adapter
├── changeset.rs        # public types + submit/approve/reject state machine
├── diff.rs             # typed revision/event diff
├── history.rs          # revision reads, legacy baseline creation
├── lifecycle.rs        # retire/restore/revert and guarded hard destruction
├── mutation.rs         # transaction-local canonical mutation helpers
├── outbox.rs           # index generation/outbox drain and repair
├── provenance.rs       # origin contribution persistence/read APIs
├── relation_history.rs # canonical IDs, revisions, mirror-outbox records
└── revision.rs         # semantic hashes and revision record types
```

Modify:

- `schema.rs`: ordered v3/v4 migrations and fixture tests.
- `types.rs`: add row version, lifecycle, and current revision fields with serde
  defaults where public compatibility requires it. Prefer separate detail
  structs if adding fields would make legacy constructors awkward.
- `remember.rs` and `batch.rs`: retain validation/embedding/index payload
  construction, but route transaction-local writes through `mutation.rs` so an
  immediate changeset, revisions, generation, canonical rows, and outbox commit
  together. Keep old public methods as compatibility wrappers.
- `forget.rs`: keep observation soft-delete compatibility; implement audited
  lifecycle operations separately. Guard irreversible entity hard-delete and
  document its history destruction.
- `export.rs`: keep raw domain migration export unchanged. Add a separate
  canonical semantic snapshot API; do not change raw byte-preserving migration
  semantics.
- `store.rs`: add readiness/index-generation state and small accessors only.
  Add `open_scoped(config, data_dir, SpaceId)` and retain the verified bound
  `SpaceId` on the handle; do not place workflows here.
- `error.rs`: add typed changeset/revision/index errors.
- `lib.rs`: re-export stable feature types.

Core public APIs to implement:

```rust
impl MemoryStore {
    pub fn submit_changeset(
        &self,
        draft: &ChangeSetDraft,
        mode: SubmitMode,
    ) -> MemoryResult<ChangeSetReceipt>;

    pub fn decide_changeset(
        &self,
        decision: &ChangeSetDecision,
    ) -> MemoryResult<ChangeSetReceipt>;

    pub fn changeset(&self, id: &ChangeSetId) -> MemoryResult<Option<ChangeSet>>;
    pub fn history(&self, object: &ObjectRef) -> MemoryResult<ObjectHistory>;
    pub fn diff(&self, request: &DiffRequest) -> MemoryResult<MemoryDiff>;
    pub fn ensure_index_current(&self) -> MemoryResult<IndexGeneration>;
    pub fn canonical_snapshot(&self, space: &SpaceId) -> MemoryResult<DomainSnapshot>;
}
```

Only graph-internal trusted callers may construct low-level transaction
operations. Surface code uses typed drafts.

## `openmemory-engine`

Add:

```text
crates/openmemory-engine/src/
├── space/
│   ├── mod.rs
│   ├── handle.rs       # SpaceHandle around one DomainStore
│   ├── manifest.rs     # space.toml read/write/verify
│   ├── snapshot.rs     # admission pause + generation vector + canonical export
│   └── layered.rs      # bounded multi-space recall + deterministic fusion
└── merge/
    ├── mod.rs
    ├── materialize.rs  # build a complete staged target from MergePlan
    ├── promotion.rs    # intent/swap/fsync state machine
    ├── recovery.rs     # exhaustive state recovery
    └── verify.rs       # canonical/index/FK/count/fixture verification
```

Modify:

- `lib.rs`: document semantic spaces separately from performance domains.
- `partition.rs`: add narrowly scoped operations needed for canonical domain
  snapshots and changesets plus `open_scoped`. Every member `MemoryStore`
  receives the same `SpaceId`; do not put a space catalog here.
- `engine.rs`: reuse public `pause_admissions`; add a read-only generation
  snapshot accessor if necessary. Preserve hot submission behavior.
- `migrate.rs`: extract generic verified store-layout promotion primitives into
  `merge/promotion.rs` or a shared private module. New space stores promote by
  directory rename. The legacy personal-global root promotes the enumerated
  `DomainStore` artifact set while leaving sibling `spaces/` and control files
  in place, using the existing sentinel/recovery approach. Domain migration and
  space merge must use these same tested primitives, not near-copies.
- `journal.rs`: journal records gain concrete `SpaceId` only at the enclosing
  engine/runtime boundary; do not let replay route into another space.

Key APIs:

```rust
pub struct SpaceHandle {
    pub id: SpaceId,
    // private manifest + Arc<DomainStore> + optional Arc<ContextEngine>
}

pub fn layered_recall(
    read_set: &[SpaceReadHandle],
    request: &LayeredRecallRequest,
) -> MemoryResult<LayeredRecallResponse>;

pub fn capture_space_snapshot(
    space: &SpaceHandle,
) -> MemoryResult<SpaceSnapshotBundle>;

pub fn materialize_merge(
    target: &SpaceSnapshotBundle,
    source: &SpaceSnapshotBundle,
    plan: &MergePlan,
    staging: &Path,
) -> MemoryResult<MaterializationReport>;

pub fn promote_verified_root(intent: &PromotionIntent) -> MemoryResult<()>;
pub fn recover_promotions(profile_root: &Path) -> MemoryResult<Vec<RecoveryReport>>;
```

`layered_recall` has a direct single-space branch. For 2–4 spaces it uses a
bounded worker pool/adaptive scheduler; it must not nest unbounded threads on
top of `DomainStore` fan-out.

## `openmemory-daemon`

First extract the existing 2,164-line `lib.rs` without behavioral changes:

```text
crates/openmemory-daemon/src/
├── lib.rs              # public construction/re-exports only
├── auth.rs
├── health.rs
├── runtime_files.rs
├── routes/
│   ├── mod.rs
│   ├── existing.rs     # or focused health/memory/jobs/integration/backup files
│   ├── spaces.rs
│   ├── changesets.rs
│   ├── identity.rs
│   └── merges.rs
├── services/
│   ├── mod.rs
│   ├── context.rs
│   ├── changesets.rs
│   ├── identity.rs
│   ├── merges.rs
│   └── spaces.rs
├── space_registry.rs
└── product_store/
    ├── mod.rs
    ├── schema.rs
    ├── jobs.rs
    ├── spaces.rs
    ├── identity.rs
    └── merges.rs
```

Keep `backup.rs`, `integrations.rs`, and `state.rs` focused; update them to use
services/registry rather than opening arbitrary profile roots.

Responsibilities:

- `product_store/schema.rs`: ordered forward migrations using the shared
  migration pattern and future-version refusal.
- `product_store/spaces.rs`: catalog, project/workspace mappings, teams,
  membership, context capability hashes.
- `space_registry.rs`: lazy `SpaceRuntime` open/lease/eviction, max-open bound,
  health, close-for-promotion, and generation-aware cache invalidation.
- `services/context.rs`: canonicalize a workspace, map to project, load current
  grants, resolve read set/write target, and mint short-lived opaque context
  capability.
- `services/changesets.rs`: authorization/reviewer policy around graph atomic
  APIs. It never writes graph SQL.
- `services/identity.rs`: candidate lifecycle, deterministic evidence analysis,
  asynchronous optional agent proposal adapter, human review, receipt issue.
- `services/merges.rs`: snapshot, preview, resolve, confirm, stage, promote,
  recover, event/job updates.
- route modules: deserialize, authorize, call one service method, map result.
  No SQL or planning logic in HTTP handlers.

The registry replaces the single `StoreRuntime` for space-aware paths but keeps
a compatibility accessor for the legacy active personal-global space.

## `openmemory-admin`

Add modules without breaking root imports:

```text
crates/openmemory-admin/src/
├── spaces.rs
├── changesets.rs
├── identity.rs
└── merges.rs
```

`lib.rs` declares modules and `pub use`s their types so current consumers can
continue importing from `openmemory_admin::*`. Add error codes, pagination, and
capability discovery. DTOs use strings for opaque IDs at the wire boundary and
derive serde plus equality/debug. Contract tests pin `snake_case`, omitted
optional fields, and backward-compatible deserialization.

## `openmemory-mcp`

Add:

```text
crates/openmemory-mcp/src/
├── context.rs           # request context capability and backend trait
└── tools/
    ├── context.rs
    └── changeset.rs
```

Split `tools/memory.rs` during the change if it remains over roughly 1,000 lines
(e.g. remember/recall/entities/lifecycle modules). Preserve registry-derived
descriptors/instructions/golden tests.

Refactor `OpenMemoryMcpServer` to hold a backend abstraction that accepts a
resolved request context. Provide:

- A fixed single-space adapter for direct/legacy stdio use and current tests.
- A space-aware backend used by daemon and context-capability stdio proxy.
- `handle_with_context` while preserving `handle` as the fixed-context wrapper.

Do not give MCP graph/store handles or catalog queries. All target selection is
resolved by the backend from a bounded capability.

## `openmemory-cli`

Add command modules:

```text
crates/openmemory-cli/src/commands/
├── space.rs
├── context.rs
├── changeset.rs
├── memory.rs
└── merge.rs
```

Administrative commands call authenticated daemon endpoints. If no daemon is
running, print one precise repair instruction instead of opening product and
graph databases concurrently. Existing scriptable direct commands continue to
work on personal-global; add `--target personal|team` only when a daemon context
is available.

`openmemory mcp` resolves the canonical startup working directory by default,
asks the daemon for a context capability when proxying, and sends the opaque
capability header on every proxied MCP request. `--workspace <path>`,
`--no-project-context`, and `--team <id>` are explicit overrides.

## `openmemory-watch`

Change `Watcher::new` to receive one explicit `SpaceHandle`/fixed store already
authorized by its caller. Watch metadata URI/hash state must live with that
space's index family. Add a space ID to reports. Do not auto-route each file by
catalog path during events.

Fix or quarantine the pre-existing FSEvents startup flake independently; new
space tests use fake event batches and deterministic scan calls.

## `openmemory-bench` and `openmemory-eval`

- Add the new crate dependencies and benchmark groups described in
  [08-test-performance-and-release.md](08-test-performance-and-release.md).
- Move the three validated real-world merge fixtures from experiment-only
  evidence into small versioned production test fixtures after checking their
  licensing/provenance and removing unnecessary source text.
- Keep large external retrieval datasets optional. Small isolation, identity,
  and merge goldens are committed and gating.

## Compatibility boundary

- Existing Rust APIs remain wrappers during this release. Rust API is not the
  stable public contract, but keeping wrappers makes the migration reviewable.
- Existing MCP tool names, required fields, JSON-RPC behavior, HTTP auth, and
  direct stdio setup remain valid.
- Existing admin endpoints and payloads remain valid. New provenance fields are
  optional/defaulted until the API version is deliberately advanced.
- Existing profile paths open without a catalog. First daemon startup creates
  the personal-global catalog/manifest binding transactionally without moving
  the store.
- No old client can broaden scope by omitting context. Omission yields its
  legacy personal-global view, not “all spaces.”
