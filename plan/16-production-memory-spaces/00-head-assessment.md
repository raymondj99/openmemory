# `main` HEAD Assessment

## Reviewed revision

| Field | Value |
|---|---|
| Repository | `openmemory` |
| Branch | `main` |
| Commit | `bcd10fd3f736290c536ac94daf5a9014ef711828` |
| Subject | `feat: harden context engine for desktop production` |
| Workspace version | `0.4.4` |
| Edition / MSRV | Rust 2021 / 1.85.0 |
| Committed files | 170 |
| Workspace crates | 12 |

The review used committed `HEAD` paths (`git show HEAD:<path>` and
`git grep ... HEAD`) so the untracked prototype, logbook, plans, and image
assets did not masquerade as production code.

## Crate ownership at HEAD

| Crate | Current responsibility | Relevance to this feature |
|---|---|---|
| `openmemory-core` | Clock, config, shared error, retry, schema migrator, test doubles. | Own validated identifiers and small scope/role value types only. Do not put workflow services here. |
| `openmemory-admin` | Serializable daemon/admin DTOs and stable error codes. | Add modular DTO files and re-export them; keep transport and storage out. |
| `openmemory-index` | Vector/keyword stores, metadata, hybrid fusion, cache, persistence. | Remains derived search infrastructure. It does not learn authorization or merge semantics. |
| `openmemory-embed` | Optional ONNX embedding and cache. | Used for retrieval/candidate discovery only; never identity proof. |
| `openmemory-graph` | SQLite entity/observation/relation truth plus synchronized search. | Own revisions, changesets, lifecycle, canonical export/import/hash, and durable index/mirror outboxes. |
| `openmemory-engine` | Write-behind engine, journals, performance domains, migration, source adapters. | Own space roots/manifests, layered recall, consistent snapshots, staged materialization, and promotion recovery. |
| `openmemory-mcp` | Custom JSON-RPC/MCP server and graph/index/maintenance tools. | Consume an authorized request context; preserve existing tool contract and add provenance/proposal tools. |
| `openmemory-daemon` | Loopback admin server, persistent active store, jobs/events, backups, integrations. | Own catalog, local authority, context resolver, lazy registry, review/identity/merge services, and jobs. |
| `openmemory-cli` | Setup, direct scriptable commands, MCP/daemon lifecycle, ingest/watch/eval. | Add space/audit/review/merge commands, normally through daemon admin API. |
| `openmemory-watch` | Filesystem scan/event indexing. | Must bind each watcher to one explicit space; no catalog guessing. |
| `openmemory-eval` | Retrieval-quality datasets, metrics, runner. | Add scoped retrieval/merge fixture evaluation without making external datasets mandatory. |
| `openmemory-bench` | Central Criterion/CodSpeed benchmarks. | Add layered recall, audited write, candidate analysis, planning, and admin contract groups. |

This plan adds a thirteenth crate, `openmemory-merge`, for a narrow pure
algorithm boundary. It avoids placing a generic planner in the daemon or adding
another thousand lines to `partition.rs`.

## Code-quality baseline

### Strengths to preserve

- The workspace forbids unsafe code in major crate roots and warns on unsafe
  workspace-wide. No unsafe implementation is needed for this feature.
- Dependencies and feature flags are centralized and deliberately constrained
  by the Rust 1.85 MSRV. The custom MCP implementation is an explicit MSRV
  decision, not accidental reinvention.
- `MemoryStore` has a clear concurrency contract: one mutex-protected writer,
  a WAL read pool, and a separate index-rebuild visibility barrier.
- SQLite configuration uses WAL, foreign keys, a 5-second busy timeout, and
  forward-only schema checks that refuse newer databases.
- Graph writes validate before transaction entry, batch embeddings before the
  writer lock, and keep canonical multi-row mutations transactional.
- `ContextEngine` has bounded queues, backpressure, shard-local tickets,
  durable watermarks, journal replay, torn-line handling, cross-process journal
  ownership, admission pause, and maintenance checkpoints.
- Domain-count migration already uses raw canonical export, verified staging,
  an intent sentinel, same-root promotion, retained backup, and recovery tests.
  That is the correct primitive to generalize for material space merge.
- Tests use `TempDir`, fixed clocks, deterministic generated data, explicit
  barriers/condition variables, and Tokio paused time instead of arbitrary
  sleeps.
- Production paths return typed errors and deliberately keep request handlers
  free of expected panics.
- Security details are treated seriously: constant-time token comparison,
  redacted debug/log behavior, owner-only token files, model checksums,
  loopback daemon binding, and `cargo-deny`.

### Weaknesses and debt relevant to this plan

- Several modules are now too large for safe feature accretion:
  `openmemory-daemon/src/lib.rs` is 2,164 lines,
  `openmemory-graph/src/remember.rs` 1,619,
  `openmemory-engine/src/partition.rs` 1,606,
  `openmemory-engine/src/engine.rs` 1,561,
  `openmemory-graph/src/store.rs` 1,429, and
  `openmemory-mcp/src/tools/memory.rs` 1,591. New functionality must land in
  focused modules; preparatory extraction is part of implementation.
- `openmemory-admin` holds every DTO in one 774-line `lib.rs`. Additive modules
  with root re-exports preserve wire names without extending the monolith.
- Current graph/index synchronization is recoverable only in intent, not in
  durable state. `remember` commits SQLite and then warns if search insertion
  fails; comments mention a future `rebuild_if_stale`, but no such implementation
  exists at HEAD. Manual edits require a real durable outbox/generation.
- Cross-domain relation mirrors are best-effort warn-and-continue. That is
  acceptable for current traversal degradation but insufficient for editable,
  auditable relations. Canonical relation IDs and a mirror outbox must precede
  relation edit/merge.
- Entity-name fuzzy normalization runs inside one space and can auto-merge.
  Reusing it across spaces would conflate homonyms; the merge planner must have
  a separate identity policy.
- `DomainStore` is already the name of a physical performance partition
  facade. Calling a user-visible scope a “domain” would be ambiguous and must
  be avoided in APIs and storage.
- Product database initialization uses a hand-written version check and one
  `CREATE TABLE IF NOT EXISTS` batch. Before adding catalog/ledger tables,
  convert it to the shared ordered `Migrator` pattern or an equally tested
  ordered migration list.
- Contributor docs have minor drift: `docs/crates.md` says eleven workspace
  members while HEAD has twelve, and it describes benchmark files under the
  index crate that are no longer committed. The implementation must update
  architecture/storage/crate/MCP docs from source truth.
- The eval workflow is non-gating and skips when external fixtures are absent.
  Permanent small scoped/merge fixtures must live in the repository and gate
  correctness; large external evaluations remain optional.

## Test rigor at HEAD

Static inspection found 858 explicit `#[test]`, `#[tokio::test]`, and
`proptest!` annotations/blocks across committed Rust sources. Counts by crate
are a useful density indicator, not the number reported by one feature-unified
test executable:

| Crate | Test annotations / property blocks |
|---|---:|
| `openmemory-admin` | 4 |
| `openmemory-cli` | 126 |
| `openmemory-core` | 51 |
| `openmemory-daemon` | 67 |
| `openmemory-embed` | 99 |
| `openmemory-engine` | 52 |
| `openmemory-eval` | 18 |
| `openmemory-graph` | 181 |
| `openmemory-index` | 144 |
| `openmemory-mcp` | 74 |
| `openmemory-watch` | 42 |

Coverage is not merely happy-path coverage. Existing tests include migration
rollback/future-version rejection, concurrent readers/writers, normalization
properties over arbitrary Unicode, batch rollback, raw export byte equality,
journal replay/torn records, cross-domain routing, migration sentinels, auth,
secret redaction, backup/restore, CLI process output, and MCP end-to-end calls.

### Baseline execution on 2026-07-18

Command:

```text
cargo test --workspace --locked --all-features
```

All completed core/admin/CLI/daemon/embed/engine/eval/graph/index/MCP test
executables passed. The run then failed in `openmemory-watch` integration
testing: two of four real filesystem backend tests reported `watcher backend
never became live after 8 pokes`. An immediate isolated rerun passed three and
failed a different one with the same readiness error:

```text
cargo test -p openmemory-watch --test integration --locked --all-features
```

This is a pre-existing, environment-sensitive FSEvents readiness flake, not a
tracked-code change. `LOGBOOK.md` records that a serial isolated run previously
passed all four. The feature implementation must not hide this baseline and
must not build its own tests on real-time watcher readiness. Fixing the watcher
flake is a small independent prerequisite before calling the final whole-
workspace gate green.

## Performance and release rigor at HEAD

- `openmemory-bench/benches/openmemory.rs` centralizes deterministic synthetic
  Criterion benchmarks for flat/HNSW vector search, hybrid search, end-to-end
  recall, spreading activation, dirty/clean consolidation, and daemon admin
  routes.
- `.github/workflows/bench.yml` builds and runs the crate under CodSpeed for
  Rust/manifest/benchmark changes.
- `openmemory-engine` keeps release stress/read-path examples with correctness
  assertions, latency percentiles, durability lag, concurrent recall, and
  no-lost-write checks. This style is appropriate for material merge and
  layered-read load tests that Criterion cannot model well.
- `.github/workflows/ci.yml` gates default builds/tests on Linux and macOS,
  no-default/all-feature tests, rustfmt, all-target Clippy with warnings denied,
  and rustdoc default/all features with warnings denied.
- `.github/workflows/audit.yml` runs `cargo-deny` on pushes, pull requests, and
  weekly.
- `.github/workflows/eval.yml` runs retrieval evaluation when optional fixtures
  exist, but is non-gating by design.
- Release profile uses opt-level 3, fat LTO, one codegen unit, stripping, and
  aborting panics. Profiling retains symbols and disables LTO.

## Consequences for implementation

1. Match existing test depth: pure unit tests near algorithms, SQLite migration
   and transaction tests near stores, HTTP/MCP contract tests at surfaces,
   subprocess abort tests for durable boundaries, and performance evidence in
   the existing bench/stress systems.
2. Extend existing ownership instead of bypassing it: graph owns canonical
   rows, engine owns multi-store orchestration, daemon owns policy/control
   metadata, and admin owns only wire types.
3. Reuse `FixedClock`, `TempDir`, `Migrator`, `pause_admissions`, raw export,
   staging verification, and typed error conventions.
4. Preserve the single-space fast path. A legacy/default recall must not pay a
   catalog scan, thread fan-out, or heap-heavy merge.
5. Treat module readability as a release gate: focused files, private helpers,
   direct state machines, documented invariants, and no speculative framework.

