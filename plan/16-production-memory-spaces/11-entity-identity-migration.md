# Entity Identity Migration

Status: **step 1 (the lookup adapter) is landed in production; the schema
change and the write-path change are prototyped, verified on a copy of a
real store, and staged.** See §9 for the delivery order and §11 for what
shipped.
Owner document for the amendment recorded in `INDEX.md` ("Entity identity:
unique `(name, entity_type)` → immutable `EntityId`") and for the required
behaviour listed in `01-contract-and-invariants.md`.

Evidence: `experiments/entity-identity` (T11), `LOGBOOK.md` 2026-07-28.
Prior evidence: T4a (the coalescence), T4b and its repair (the first cost
estimate, now superseded).

## 1. The defect, restated exactly

`schema.rs` v1 declares:

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_name_type
    ON entities(name, entity_type);
```

so a mutable display label is the identity key. T4a wrote two unrelated
people under one name and type into a real store and got **one entity row
carrying both sets of observations**. That is unrecoverable: no later
receipt, revision, or audit trail can separate them, because the fact
that they were ever distinct was never recorded.

Two further facts were established by T11 on the live store copy and
change the shape of the fix:

- **The defect is already present in the user's own data, across types.**
  `~/.openmemory/data/default` holds 59 entities under 58 distinct names.
  `ProjectAlpha` exists twice — once as `concept`, once as `project`.
  `MemoryStore::get_entity(name)` runs `SELECT ... WHERE name = ?1`
  through `query_row`, which takes the first row SQLite reaches and drops
  the rest. One of those two entities is unreachable by name today, and
  nothing tells the caller.
- **Dropping the index is not sufficient.** `remember.rs:679` resolves an
  entity with `SELECT id FROM entities WHERE name = ?1 AND entity_type = ?2`
  and appends to whatever comes back. With the constraint relaxed and two
  `Alex Chen` rows present, that statement still returns exactly one of
  them. The schema change makes distinctness *representable*; the write
  path is what currently makes it impossible.

## 2. `entities.id` already is the immutable `EntityId`

No new identifier is needed and no row is rekeyed.

- `entities.id` is `TEXT PRIMARY KEY`, minted by `types::new_id()` as a
  UUIDv7 at creation.
- `grep 'UPDATE entities SET'` across `crates/` finds only `updated_at`.
  Nothing rewrites an id.
- On the live store, all 59 ids parse as 36-character hyphenated hex.

The migration therefore promotes an observed habit to an enforced
invariant (a `BEFORE UPDATE OF id` trigger that aborts), rather than
introducing a column.

## 3. `home_domain` is *recorded*, not derived

`01-contract-and-invariants.md` currently says `home_domain` is "derived
from `EntityId` at creation". For a new entity that is correct. For a
legacy entity it is wrong, and applying it literally would be
destructive.

Existing rows live in the domain that `domain_for(name, n)` chose. If the
migration stamped `domain_for(id, n)` on them, it would assert that an
entity lives in a file it is not in. Measured on a synthetic 200-entity
legacy domain (`tests/migration.rs`), more than 150 of 200 rows would be
mis-declared at 16 domains — the two hashes are independent, so the
expected mis-declaration rate is `1 - 1/n`.

The rule the migration implements:

- **Legacy rows**: `home_domain` is the index of the database file the
  row is already in. Nothing moves.
- **New rows**: `home_domain = domain_for(entity_id, n)`, so routing
  follows the immutable id from creation.
- **Thereafter**: immutable, enforced by trigger, with one explicit
  escape hatch (§6).

This should be corrected in `01-contract-and-invariants.md`: "recorded at
creation and thereafter immutable; derived from `EntityId` for entities
created after the migration".

## 4. Schema: two phases, only one of which is one-way

### Phase A — additive, reversible, behaviour-preserving

Adds:

| object | purpose |
|---|---|
| `entities.home_domain INTEGER` | nullable until backfilled |
| `entity_names(name, entity_id, entity_type, home_domain, name_kind, asserted_at, source)` | the name/alias directory |
| `idx_entity_names_lookup(name, entity_id, home_domain, entity_type, name_kind)` | covering: a name resolves with no table access |
| `idx_entity_names_one_primary(entity_id) WHERE name_kind='primary'` | one primary name per entity |
| `entity_name_assertions` | append-only assert/retract log — names as versioned, provenance-bearing assertions |
| `entity_identity_backfill` | resumable cursor and counters |
| `entity_rehome_window` | the audited escape hatch for repartitioning |
| triggers | id immutability, `home_domain` immutability, directory synchronisation |

The UNIQUE index stays in place throughout phase A, so **no behaviour
changes and no new duplicate can appear while the backfill runs**.

Rollback drops every object and both meta keys. Verified byte-exact
against `sqlite_master` (`phase_a_is_reversible`). The one residue is
`entities.home_domain`, which SQLite cannot drop without a table rebuild;
an unread nullable column is inert and is re-used if phase A runs again.
`entity_name_assertions` deliberately avoids `AUTOINCREMENT`, because
that would create `sqlite_sequence`, which SQLite never drops — the log
is append-only, so plain rowids are already monotonic.

Directory synchronisation is by **trigger, not application code**. A
directory that application code can forget to update is a directory that
silently lies; the triggers make the invariant hold for a stale binary
and for `sqlite3` by hand. Verified: an insert from a writer that has
never heard of `entity_names` still lands in the directory with the right
home domain, a rename moves it, and the FK cascade reaps it
(`triggers_keep_the_directory_honest_for_writers_that_do_not_know_about_it`).

### Phase B — one-way, and gated

```sql
DROP INDEX idx_entities_name_type;
CREATE INDEX idx_entities_name_type ON entities(name, entity_type);
```

Same name, same column order, so every query plan that used the index
still uses it; only uniqueness disappears.

Phase B refuses to run unless **all** of:

- the backfill reports `complete = 1`;
- no entity has a NULL `home_domain`;
- every entity has a primary directory row agreeing on name, type and
  home domain.

This is the whole compatibility argument. The window in which a reader
could be surprised by a duplicate is closed by the gate, not by
scheduling. Verified that the gate actually refuses
(`phase_b_refuses_until_the_backfill_is_complete`), and that phase A
rollback is rejected afterwards, since restoring UNIQUE once homonyms
exist would either fail or silently destroy rows.

## 5. Backfill

Bounded pages, one transaction each, cursor persisted in the same
transaction as the page it describes.

- **Ordered by primary key.** `id > cursor ORDER BY id LIMIT n` is a range
  scan, not a sort, so cost is independent of table size.
- **Idempotent.** `INSERT OR IGNORE` for the directory row,
  `WHERE home_domain IS NULL` for the stamp. Crash recovery is "run it
  again".
- **Resumable.** Verified by dropping the connection mid-run at 137
  entities and resuming from the persisted cursor: 137 directory rows, no
  gaps, no entity with two primary rows.
- **Budgeted.** `run(page_size, max_pages)` returns after `max_pages` so
  a background job can hold a time budget and come back.

On the live store copy: 59 entities, page size 10, interrupted after 2
pages (connection dropped), resumed, 7 pages total, 59 covered, 59
directory rows, complete. Re-running afterwards wrote 0 rows.

## 6. `entity_rehome_window`

`home_domain` must be changeable exactly once per repartition, and never
otherwise. The immutability trigger aborts unless a matching row exists
in `entity_rehome_window(entity_id, to_domain, changeset_id, opened_at)`.
That makes every re-home an explicit, attributable, auditable act rather
than an ordinary `UPDATE`. Verified: the update is rejected without a
window, accepted with one, and the directory follows the move.

This does not reintroduce the `entity_rehome` staged job for renames.
Renames never touch `home_domain`; only changing the domain *count* does.

## 7. The lookup adapter

A name resolves to a **bounded candidate set**, never to one entity by
SQL order:

```rust
pub enum Resolution {
    NotFound,
    Unique(Box<EntityRow>),
    Ambiguous { name: String, candidates: Vec<EntityRow>, truncated: bool },
}
```

and the compatibility shim:

```rust
legacy_get_entity(conn, name, max_candidates)
    -> Result<Result<Option<EntityRow>, Ambiguity>>
```

- `Ok(None)` and `Ok(Some(row))` are byte-identical to today's answer
  whenever today's answer was not arbitrary.
- `Err(Ambiguity)` is returned **exactly** where today's answer *was*
  arbitrary.

So the adapter never silently changes a correct result and never
silently preserves an incorrect one.

Bounding is load-bearing. `01-contract-and-invariants.md` requires
candidate indexing to stay bounded at 100,000 homonyms; a resolver that
materialised every row sharing a hot name would turn a popular label into
an unbounded read. `max_candidates` caps the set, `max_candidates + 1`
rows are requested so truncation is detected without a counting query,
and `truncated` is reported so a caller can distinguish "these are all of
them" from "these are some of them". Verified at 500 rows sharing one
name with a bound of 64.

Verified on the live store copy: 58 distinct names, 57 unambiguous, **57
of 57 byte-identical** to the pre-migration answer across all seven
columns; the one ambiguous name (`ProjectAlpha`) returns a typed
two-candidate result instead of an arbitrary row.

## 8. What the change costs

See `LOGBOOK.md` 2026-07-28 for the grid and its stated limits. The
short version, and the parts that matter for design:

### 8.1 The bounded candidate set is only bounded through the directory

This is the finding that decides whether the directory table is
justified, and it was not visible in T4b's uniform population.

`01-contract-and-invariants.md` asks for a candidate set that is both
deterministic ("never an arbitrary row chosen by SQL order") and bounded
at 100,000 homonyms. On `entities` those two requirements are in
conflict. `idx_entities_name_type` is `(name, entity_type)` and does not
order by `id`, so

```sql
SELECT ... FROM entities WHERE name = ? ORDER BY id LIMIT 64
```

plans as `SEARCH entities USING INDEX idx_entities_name_type` **plus
`USE TEMP B-TREE FOR ORDER BY`**. Every row carrying the name is
materialised and sorted before 64 are returned. `LIMIT` bounds the
result; it does not bound the work. Against the directory,
`idx_entity_names_lookup` is `(name, entity_id, …)`, so the same query
shape is a covering range scan that stops after 64 entries and never
sorts.

Priced at 100,000 entities under a Zipf-1.2 name distribution whose
hottest name is carried by **20,438** entities, warm, one reader — and
both strategies return the **same ~54 rows per lookup**:

| strategy | µs/lookup | 95% CI |
|---|---:|---:|
| `directory_route` | 8.1 | ±0.03 |
| `relaxed_name` (`entities` + `ORDER BY id`) | **3,146** | ±229 |

**387× for an identical result set**, widening to 2,038× at 8 concurrent
readers (57,415 µs against 28 µs). It is not a constant factor; it is
O(fan-out) against O(bound).

Dropping `ORDER BY` bounds the work and returns an arbitrary subset —
which is the defect the candidate set exists to remove. The query plans
are asserted in `tests/ordering.rs`, so a future index change that
reintroduces the sort fails a test rather than a user.

**The directory is therefore load-bearing on a single-domain store too**,
not only for cross-domain routing.

### 8.2 T4b's headline does not reproduce

T4b reported 3.89 µs for the directory against 4.64 µs for a direct
lookup and concluded that "the identity fix does not add a hop". At
near-fan-out-1 (100k entities over 100k requested names; realised p50 1,
p99 4, max 8), warm, one reader, 7 reps:

| strategy | µs/lookup | 95% CI |
|---|---:|---:|
| `hash_route` (routing today) | 0.093 | ±0.001 |
| `unique_name` (`get_entity` today) | 3.067 | ±0.156 |
| `directory_route` (routing after) | 4.219 | ±0.106 |
| `relaxed_name` (candidate rows from `entities`) | 7.613 | ±0.073 |
| `directory_fetch` (route + fetch rows) | 7.843 | ±0.092 |

The directory is **slower**, by 1.15 µs (+37%), intervals not
overlapping — despite reading two columns from a covering index where
`unique_name` reads seven from the table.

The cause is the semantics, not the structure. A UNIQUE index lets SQLite
stop after the first match; a candidate set cannot, because "is there a
second one?" is precisely the question. **Ambiguity-awareness has a floor
cost that no index layout removes.** T4b hid it by comparing a
non-covering direct read against a covering directory read — a difference
in projection, not in identity model. Doc 01's 3.89/4.64 figures should be
treated as withdrawn.

### 8.3 The two cost findings point in opposite directions

Both are real:

- on a store with no homonyms, ambiguity-awareness costs **~1.2 µs**
  per lookup against today — a genuine regression, small in absolute
  terms;
- on a store with one very popular name, resolving through the directory
  instead of the entities table saves **~3.1 ms** per lookup (§8.1), and
  converts an unbounded path into a bounded one.

At an intermediate fan-out of 5 (100k entities, 20k names, uniform),
warm, one reader: `unique_name` 3.21 ±0.09, `directory_route` 4.98 ±0.17,
`relaxed_name` 14.72 ±1.40, `directory_fetch` 24.84 ±2.53. At Zipf 0.7
(fan-out p50 3, p99 43, max 1,540; 20 rows per lookup) `relaxed_name`
rises to 82.0 ±4.9 while `directory_route` rises only to 5.7 ±0.1 — the
covering scan barely notices, the sort does.

### 8.4 Routing today is free and cannot stay free

`PartitionedStore::domain_for(name)` is an in-memory hash: **0.07–0.10 µs**
and zero I/O. Routing by immutable id needs the id, which the caller does
not have, so it needs a read: **4.2–8.1 µs warm**. That is the structural
price of the change. It is small against a recall path measured in tens of
milliseconds — but "small" is a judgement about the recall path, not a
measurement of it.

### 8.5 Concurrency does not change the ranking

Under 4 and 8 concurrent readers on 12 logical cores the ranking is
unchanged at every skew, and per-reader latency degrades for *every*
strategy including today's baseline (`unique_name` 3.1 → 15.6 → 43.4 µs
at fan-out ≈1). That is saturation, not a property of the new design. All
concurrency numbers are **read-only**: no writer contends for the WAL.

### 8.6 The partitioned deployment pays an extra file read

The directory must stay **name-sharded** on a partitioned store: routing
consults it *before* it knows the home domain, so it cannot live in the
target domain's file. It is derived and rebuildable, so name-sharding it
is safe — but a partitioned deployment pays one extra file open/read on
the name-addressed write path. That is an architectural change, not a
microsecond, and it is not prototyped here.

## 9. Delivery order

1. **Landed.** The adapter and `EntityResolution` against today's schema,
   in `openmemory-graph::resolve`. A strict improvement immediately: it
   makes the existing cross-type `ProjectAlpha` ambiguity visible rather
   than silent, and makes the two destructive by-name paths
   (`forget_entity`, `retire_entity_observations`) fail closed instead of
   acting on an arbitrary row. See §11 for what shipped.
2. Land phase A plus the backfill as a background job. No behaviour
   change; reversible.
3. **Change the write path.** `remember` must take an explicit
   "create new entity" versus "append to `EntityId` X" decision, and the
   candidate set must be surfaced to the caller. Until this lands,
   phase B changes nothing a user can observe, because `ensure_entity`
   still coalesces.
4. Land phase B behind the readiness gate.
5. Route new entities by `domain_for(entity_id, n)`; leave legacy
   `home_domain` values alone.

Steps 1–2 and 4–5 are mechanical. Step 3 is the real work and is not
prototyped here.

## 9a. The resolver query the adapter should actually issue

§8.1 changes the adapter's implementation, not its interface. The
prototype's `resolve()` queries `entities` directly because it must work
before the directory exists. In production it should resolve candidate
**ids** through `entity_names` — bounded, ordered, no sort — and then
fetch the bounded set of rows by primary key. `tests/ordering.rs` asserts
both paths return the same 65 candidates, so the substitution is
behaviour-preserving.

## 10. What remains risky

- **The write path is unchanged and is where the defect actually lives.**
  Proven on the live copy: with two `Alex Chen` rows present, the verbatim
  `ensure_entity` query still returns exactly one. Relaxing the schema
  without step 3 fixes nothing observable.
- **The MCP and CLI surfaces are name-addressed end to end.**
  `openmemory_get_entity`, `openmemory_forget_entity`, `add_relation`,
  and the relation targets in `remember` all take names. Every one of
  them needs an ambiguity answer, and "return a candidate set" is not a
  drop-in for a tool that returns one entity. Not designed here.
- **Aliases can make an unambiguous name ambiguous.** The directory
  merges primary names and aliases into one candidate space. An alias
  asserted on entity B can turn a previously unique lookup of A's name
  into a two-candidate result. The prototype records `name_kind` so a
  caller can prefer primary matches, but no policy is specified.
- **Fan-out is unmeasured in the wild.** The cost curve is reported as a
  function of fan-out because nobody has measured the name-frequency
  distribution of a real store. The live store has one homonym in 59
  entities; that is not a distribution.
- **Concurrency was measured read-only.** No writer contends for the WAL
  in any of these numbers.
- **The prototype ran on a v2 store.** The live profile is at schema
  version 2 while the binary supports 7. The identity migration was
  applied on top of v2, not on top of v7. The objects it creates do not
  reference anything from v3–v7, but composition with those steps is
  argued, not tested.

## 11. What landed in step 1

Production, not `experiments/`. The schema is unchanged: this is purely
the read/act path learning to say "several".

**`openmemory-graph::resolve`** — `EntityResolution` (`NotFound` /
`Unique` / `Ambiguous`) plus `EntityCandidates`. `MemoryStore::resolve_entity`
and `resolve_entity_by_name_and_type` return it. Candidate sets are
bounded at `MAX_NAME_CANDIDATES` (64) and report truncation; ordering is
`(created_at, id)` so "the first candidate" is a stated rule rather than
SQLite row order.

**`MemoryStore::get_entity(name) -> Option<Entity>` is gone.** There is
no longer a silent single-row path for an unqualified name to inherit.
`get_entity_by_name_and_type` remains — it is unambiguous under the
current UNIQUE index — but now delegates to the resolver and returns
`None` rather than a guess if that constraint is ever relaxed.

**Destructive paths fail closed.** `forget_entity` and
`retire_entity_observations` return the new
`MemoryError::AmbiguousEntityName { name, candidates }`. Each gained an
id-keyed sibling (`forget_entity_by_id`,
`retire_entity_observations_by_id`) so a caller can act after
disambiguating.

**Surfaces, each decided rather than inherited:**

| surface | contract | decision |
|---|---|---|
| `openmemory_get_entity` (MCP) | one entity | Returns the oldest match and says so: `ambiguous`, `candidates[]`, `candidates_truncated`, `selected_by`. Additive fields; existing readers unaffected. |
| `openmemory_forget_entity` (MCP) | destructive | Refuses. JSON-RPC `-32005` with `data.candidates` so an agent can retry against an id. |
| `openmemory forget-entity` (CLI) | destructive | Refuses, prints the candidate ids, and gained `--id <ID>` so the user is not left at a dead end. |
| `DomainStore::forget_entity` | destructive | Propagates the refusal. Its cross-domain stub sweep now deletes **by id** and visits every candidate, so a domain holding two rows under one name no longer leaves a stub behind or removes the wrong one. |
| `migrate.rs` staging verification | presence check | Ambiguity is *not* a failure: the check asks "did this land in the right domain", and entities sharing a name hash to the same domain. Uses `candidates().is_empty()`. |
| `add_relation` endpoints (MCP) | one entity each | Unchanged — already `(name, entity_type)`-qualified, so unambiguous under the current index, and now fails closed via the delegation above. |
| `remember` / `ensure_entity` | write path | **Deliberately unchanged.** See §10. |

Tests: `openmemory-graph/tests/name_ambiguity.rs` (10) builds the exact
live-store shape — `ProjectAlpha` as `concept` and `project` — and pins
that a single-row answer cannot describe it, that both are reported, and
that both destructive paths refuse. Three MCP handler tests cover the
tool payloads; one CLI test covers refusal plus `--id` recovery.
Verified against a copy of the real store: `forget-entity ProjectAlpha`
lists both real ids and exits non-zero; `--id` succeeds.
