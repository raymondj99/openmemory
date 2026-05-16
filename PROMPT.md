# Task: Implement Entity Normalization for openmemory

Implement fuzzy entity normalization on the `remember` write path, exactly as specified in `PLAN.md` at the repo root. Read that file first. This prompt fills in the implementation details and codebase patterns you must follow.

## Context

openmemory is a Rust workspace (MSRV 1.85) with 8 crates under `crates/`. The relevant ones are:

- `openmemory-core` — config, clock, migrations, error types
- `openmemory-graph` — knowledge graph (entities, observations, relations, MemoryStore)
- `openmemory-mcp` — MCP JSON-RPC server wrapping the graph

The change adds fuzzy matching to `ensure_entity` (the function that resolves entity names to IDs on every `remember` call). Currently it does exact `(name, entity_type)` lookup. After this change, near-misses like "ProjectAlpha" vs "Project Alpha" auto-merge instead of creating duplicates.

## Implementation order

Do these in order. Run `cargo check -p <crate>` after each step to catch errors early. Run the full test suite (`cargo test --workspace`) at the end.

### Step 1: Add `strsim` dependency

In the root `Cargo.toml`, add to `[workspace.dependencies]`:

```toml
strsim = "0.11"
```

In `crates/openmemory-graph/Cargo.toml`, add to `[dependencies]`:

```toml
strsim = { workspace = true }
```

### Step 2: Add `NormalizationSection` to config

File: `crates/openmemory-core/src/config.rs`

Follow the exact pattern of `SearchSection` (lines 28-36) and its `impl` (lines 163-182):

1. Add a `NormalizationSection` struct with `#[derive(Debug, Clone, Serialize, Deserialize)]` and four fields, each with `#[serde(default = "NormalizationSection::default_*")]`:
   - `enabled: bool` (default `true`)
   - `auto_merge_threshold: f64` (default `0.95`)
   - `flag_threshold: f64` (default `0.85`)
   - `max_candidates: usize` (default `100`)

2. Add the `default_*` factory methods in `impl NormalizationSection`, matching the style of `SearchSection::default_hybrid_alpha` etc.

3. Add `impl Default for NormalizationSection` that calls the factory methods, matching the pattern of `impl Default for SearchSection` (lines 175-182).

4. Add a `#[serde(default)] pub normalization: NormalizationSection` field to `Config` (lines 6-18).

5. Add validation in `Config::validate()` (lines 129-151):
   - `auto_merge_threshold` must be in `[0.0, 1.0]`
   - `flag_threshold` must be in `[0.0, 1.0]`
   - `flag_threshold` must be strictly less than `auto_merge_threshold`

6. Add tests following the existing patterns (lines 248-354):
   - `default_config_validates` already exists and will cover the new defaults
   - Add `validate_rejects_bad_normalization_thresholds` (flag >= auto_merge)
   - Add a TOML round-trip test with `[normalization]` section

### Step 3: Create `normalize.rs`

New file: `crates/openmemory-graph/src/normalize.rs`

The code is specified exactly in PLAN.md. Key points:

- Module doc comment (the `//!` block) is required. It explains the dual-path scoring strategy.
- `NormalizeMatch` enum has exactly two variants: `AutoMerge { entity_id: String, score: f64 }` and `Flag { entity_id: String, score: f64 }`. Derive `Debug, Clone, PartialEq`.
- `similarity(a, b) -> f64` is the public scoring function. Dual-path: `max(token_sorted_jaro_winkler, alnum_stripped_jaro_winkler)`. Both paths lowercase first.
- `find_best_match(name, candidates, auto_merge_threshold, flag_threshold) -> Option<NormalizeMatch>` takes thresholds as plain `f64` args, not a config struct.
- `token_sort` and `alnum_only` are private helper functions.

Add unit tests in a `#[cfg(test)] mod tests` block. These scores are empirically verified against `strsim 0.11`:

```
similarity("ProjectAlpha", "projectalpha")   == 1.0    -> AutoMerge
similarity("ProjectAlpha", "Project Alpha")  == 1.0    -> AutoMerge
similarity("Project Alpha", "Alpha Project") == 1.0    -> AutoMerge
similarity("open-memory", "OpenMemory")      == 1.0    -> AutoMerge
similarity("React.js", "reactjs")            == 1.0    -> AutoMerge
similarity("PostgreSQL", "Postgres")         ~= 0.96   -> AutoMerge
similarity("sift", "swift")                  ~= 0.94   -> Flag
similarity("ML Pipeline", "Machine Learning Pipeline") ~= 0.57 -> None
similarity("Raymond", "Kubernetes")          ~= 0.50   -> None
```

Test both `similarity` directly (assert score ranges) and `find_best_match` (assert correct variant or None). Test the disabled case: empty candidates list returns None.

### Step 4: Store normalization config on `MemoryStore`

File: `crates/openmemory-graph/src/store.rs`

1. Add four fields to `MemoryStore` (line 83-97), matching the `decay_rate: f64` pattern:

```rust
normalization_enabled: bool,
auto_merge_threshold: f64,
flag_threshold: f64,
max_candidates: usize,
```

2. Initialize them in `open()` (line 118-129) from `config.normalization.*`:

```rust
normalization_enabled: config.normalization.enabled,
auto_merge_threshold: config.normalization.auto_merge_threshold,
flag_threshold: config.normalization.flag_threshold,
max_candidates: config.normalization.max_candidates,
```

3. Same in `open_in_memory()` (line 158-169).

### Step 5: Modify `ensure_entity` in `remember.rs`

File: `crates/openmemory-graph/src/remember.rs`

This is the core change. The function is at line 318. It is private (no `pub`).

1. Add `use crate::normalize::{find_best_match, NormalizeMatch};` at the top.

2. Replace the `(String, bool)` return with a private struct:

```rust
struct EnsureResult {
    entity_id: String,
    existed: bool,
    normalized: Option<NormalizeMatch>,
}
```

3. Change `ensure_entity` signature. It needs access to the normalization config, so pass the store's fields. The current signature is:

```rust
fn ensure_entity(
    tx: &Transaction<'_>,
    name: &str,
    entity_type: EntityType,
    source: &str,
    now: i64,
) -> MemoryResult<(String, bool)>
```

Change to:

```rust
fn ensure_entity(
    tx: &Transaction<'_>,
    name: &str,
    entity_type: EntityType,
    source: &str,
    now: i64,
    normalization_enabled: bool,
    auto_merge_threshold: f64,
    flag_threshold: f64,
    max_candidates: usize,
) -> MemoryResult<EnsureResult>
```

4. Implementation flow (keep the existing exact-match block at the top unchanged, then add the fuzzy path after it):

```
a. Exact match (existing code, lines 325-338):
   SELECT id FROM entities WHERE name = ?1 AND entity_type = ?2
   If found: bump updated_at, return EnsureResult { id, true, None }

b. If !normalization_enabled: create new entity (existing code, lines 341-363),
   return EnsureResult { id, false, None }

c. Load candidates:
   SELECT id, name FROM entities WHERE entity_type = ?1
   ORDER BY updated_at DESC LIMIT ?2
   Collect into Vec<(String, String)>.

d. Call find_best_match(name, &candidates, auto_merge_threshold, flag_threshold).

e. Match result:
   - Some(AutoMerge { entity_id, score }):
     Bump updated_at on the matched entity (same UPDATE as the exact match path).
     Return EnsureResult { entity_id, true, Some(AutoMerge { entity_id, score }) }

   - Some(Flag { entity_id: matched_id, score }):
     Create new entity (existing code).
     Insert SAME_AS relation:
       INSERT INTO relations (id, from_entity, to_entity, relation_type, weight, created_at, source)
       VALUES (?1, ?2, ?3, 'SAME_AS', ?4, ?5, 'normalization')
     where from_entity = new entity id, to_entity = matched_id, weight = score.
     Return EnsureResult { new_id, false, Some(Flag { entity_id: matched_id, score }) }

   - None:
     Create new entity (existing code).
     Return EnsureResult { new_id, false, None }
```

5. Add `normalized: Option<NormalizeMatch>` to `RememberOutcome` (line 102-110).

6. Update the call site in `remember()` (around line 165-175 where `ensure_entity` is called). The current code destructures `let (entity_id, entity_existed) = ensure_entity(...)`. Change to destructure `EnsureResult`. Pass the normalization fields from `&self`. Propagate `result.normalized` into `RememberOutcome`.

7. Also update the call to `ensure_entity` inside the relations loop (around line 215-225 where relation targets are resolved). Relation targets should also go through normalization. Pass the same config fields.

### Step 6: Update `lib.rs` re-exports

File: `crates/openmemory-graph/src/lib.rs`

1. Add `pub mod normalize;` to the module list (line 79-87).
2. Add `pub use normalize::NormalizeMatch;` to the re-exports (lines 89-101).

### Step 7: Update MCP response

File: `crates/openmemory-mcp/src/tools/memory.rs`

In `OpenMemoryRememberTool::call` (line 114-171), update the response JSON (lines 164-169):

```rust
let mut response = json!({
    "entity_id": outcome.entity_id,
    "entity_existed": outcome.entity_existed,
    "observation_ids": outcome.observation_ids,
    "relation_ids": outcome.relation_ids,
});
if let Some(ref norm) = outcome.normalized {
    let (action, matched_id, score) = match norm {
        NormalizeMatch::AutoMerge { entity_id, score } => ("auto_merged", entity_id, score),
        NormalizeMatch::Flag { entity_id, score } => ("flagged", entity_id, score),
    };
    response.as_object_mut().unwrap().insert("normalized".into(), json!({
        "action": action,
        "matched_entity_id": matched_id,
        "score": score,
    }));
}
```

Add `NormalizeMatch` to the import from `openmemory_graph`.

### Step 8: Integration tests in `remember.rs`

Add tests in the existing `#[cfg(test)] mod tests` block (starts at line 366). Follow the existing test patterns: use `open_with_clock()` helper for deterministic timestamps, use `open_temp()` for on-disk tests.

Required tests:

1. **Auto-merge on casing**: `remember("Project Alpha", Project, ...)` then `remember("project alpha", Project, ...)`. Assert: same `entity_id`, second call returns `entity_existed = true`, `normalized = Some(AutoMerge { .. })`. Assert only one entity exists in the database.

2. **Auto-merge on spacing**: `remember("ProjectAlpha", Project, ...)` then `remember("Project Alpha", Project, ...)`. Assert: same `entity_id`.

3. **Flag on near-miss**: `remember("sift", Tool, ...)` then `remember("swift", Tool, ...)`. Assert: different `entity_id`s, second returns `normalized = Some(Flag { .. })`. Query relations table: a `SAME_AS` relation exists between the two entities.

4. **No match on unrelated**: `remember("Raymond", Person, ...)` then `remember("Kubernetes", Person, ...)`. Assert: different `entity_id`s, `normalized = None`.

5. **Type constraint**: `remember("Raymond", Person, ...)` then `remember("Raymond", Project, ...)`. Assert: different `entity_id`s (normalization only compares within the same type).

6. **Observations land correctly after auto-merge**: After auto-merging, query observations for the entity and assert both sets of observations are present.

7. **Normalization disabled**: Construct a store with a config where `normalization.enabled = false`. Remember "Project Alpha" then "project alpha". Assert: different `entity_id`s (no normalization).

### Step 9: Config tests

File: `crates/openmemory-core/src/config.rs`

Add in the existing test module:

1. `validate_rejects_bad_normalization_thresholds`: set `flag_threshold = 0.96, auto_merge_threshold = 0.95`, assert validate fails.
2. `validate_rejects_normalization_threshold_out_of_range`: set `auto_merge_threshold = 1.5`, assert validate fails.
3. TOML round-trip: create config with custom normalization values, serialize to TOML, deserialize back, assert values match.

### Step 10: Final verification

Run:
```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

All tests must pass. No clippy warnings.

## Patterns to follow exactly

**Do NOT:**
- Add features, abstractions, or tools beyond what's specified (no merge tool, no batch dedup, no feature flags for normalization)
- Use `String` where the codebase uses typed enums
- Add multi-line doc comments on functions (keep them to one line or omit)
- Add comments explaining what code does (only why, if non-obvious)
- Create any new error variants (the existing `Sqlite` variant via `?` is sufficient)
- Use `pub` on `EnsureResult` (it's private to `remember.rs`)

**DO:**
- Follow the `#[serde(default = "...")]` factory method pattern for config
- Use `impl Default for NormalizationSection` that calls the factory methods
- Store config values as individual fields on `MemoryStore` (like `decay_rate`), not as a struct
- Use `rusqlite::params![]` for SQL parameters
- Use `.optional()?` for queries that may return no rows
- Use `new_id()` from `types.rs` for generating UUIDs
- Keep `ensure_entity` as a free function (not a method on `MemoryStore`), matching its current shape; pass the normalization fields as arguments
- Match the existing test helper style: `open_with_clock()`, `open_temp()`, `cfg()`
- Keep the module doc comment on `normalize.rs`
