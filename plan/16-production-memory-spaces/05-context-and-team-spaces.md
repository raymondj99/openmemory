# Context and Team Spaces

## Catalog and roots

`SpaceService` alone creates, opens, closes, and deletes catalog entries:

```text
creating -> manifest/store fsynced and verified -> active -> closed
closed -> deleting -> removed
```

Every transition has an expected state/version and recoverable artifacts.
Never expose `active` before manifest and store verification. Destruction
closes leases and requires a verified backup or explicit hash-bound no-backup
confirmation. Personal-global cannot be deleted while its profile exists.
Team creation provisions its team-global space before the team becomes active.

`SpaceId` remains canonical UUIDv7. Managed roots are derived internally and
checked under the rules in `03-persistence-and-migrations.md`; caller paths and
display labels never become roots.

## Projects and workspaces

A workspace is activation metadata, not project identity:

1. require an existing directory and canonicalize it once;
2. use an existing indexed mapping's stable `ProjectId`;
3. for a new mapping, a normalized VCS remote may suggest—but never
   automatically join—an existing project;
4. moving a workspace changes mapping metadata, not `ProjectId`;
5. multiple worktrees may map to one project; one canonical workspace maps to
   one project per profile.

Creating/activating a project mapping is a recoverable service operation that
first provisions and verifies its personal-project space. The mapping is not
active until that space is active. Team-project spaces remain explicit; if one
is absent, its read layer is omitted and `team` write selection uses the
authorized team-global space or returns a typed missing-target error. Context
resolution itself never creates files/catalog rows.

Recall never scans or canonicalizes the workspace catalog. Context resolution
performs one indexed product lookup when minting/refreshing a context.
The workspace boundary uses the lossless `WorkspacePathKey` contract in
`01-contract-and-invariants.md`; display rendering is never reused as a
catalog, authorization, or cache key.

## Context capability

Ingress first decodes and bounds a `RawContextSelection`, then resolves it once
to an immutable `ResolvedContextRequest`. The resolved request records
`SelectionSource` for each explicit, workspace-derived, or default choice,
along with the normalized profile, actor, workspace/project, team, overlays,
read policy, and requested write target. An invalid or unauthorized explicit
selection returns its typed error; it never falls through to workspace
inference or a product default.

Resolution:

1. authenticate principal/actor/profile and canonicalize the optional
   workspace;
2. load exact personal owner/context keys;
3. if a team is active, load its current unexpired membership and exact spaces
   owned by that team;
4. build the requested contextual/project/global/explicit-overlay read set;
5. resolve one write target and role;
6. capture the versioned authority snapshot;
7. generate at least 256 random bits, persist only its BLAKE3 token hash plus a
   bounded versioned concrete context, and return plaintext once.

Default expiry is 15 minutes. On use, bind the capability to bearer credential,
principal, actor kind, and profile; acquire an `AuthorizationLease`; and
revalidate catalog/team/membership generations. Rotation, revocation, expiry,
team archive, space close, profile switch, or promotion invalidates it before
another operation can publish.

After the request and capability are resolved, recall/write hot paths consume
only that immutable policy and captured authority. They do not reread current
directory, environment, mutable configuration, platform probes, or workspace
heuristics. Any context cache key includes every normalized selection and
authority/catalog generation that can change the concrete read set, write
target, readiness, or failure.

## Space registry

The daemon's bounded `SpaceRegistry` owns lazy `SpaceRuntime`s and the one
shared `ExecutionRuntime`.

- Default 8/hard 64 open non-legacy spaces; personal-global is pinned.
- Opening reserves domain-family, writer/reader connection, index-handle, and
  optional context-flusher resources from the process budget before touching
  the filesystem. Evict or reject atomically if the reservation cannot fit.
- Concurrent opens for one ID share one result.
- RAII leases pin a runtime. Idle eviction occurs only when unleased and after
  flush/checkpoint; no high-frequency polling.
- Open verifies catalog/manifest binding, root identity, pinned domains,
  identical graph `SpaceId`, schemas, recovery intents, and derived readiness.
- `close_for_promotion` blocks new leases, waits existing leases, pauses and
  quiesces every writer, checkpoints, and drops file handles.
- Cache/runtime keys include catalog/manifest generation.
- Ten thousand closed catalog rows open no stores, connections, index files, or
  executor workers.

## Flattened layered recall

### Bounds and failure policy

- read set: 1–4 unique authorized spaces;
- query: at most 64 KiB;
- `top_k`: 1–256;
- candidates per component: `min(256, top_k * 2)` by default;
- total fusion input: at most 1,024;
- no missing provenance, duplicate task key, invalid origin, or non-finite
  score.

Default behavior is fail-closed if a required component fails. A typed partial
response is allowed only behind explicit product policy and identifies every
missing/degraded space.

### Facade cache protocol

Scheduling lives beneath the existing `DomainStore` facade:

1. resolve/authorize the read set and hold its authorization lease;
2. for each space in read priority, call a private `probe_recall` that computes
   the existing normalized cache key, captures the pre-search write version
   with Acquire ordering, and returns a valid TTL hit or a miss ticket;
3. single space delegates directly to its facade; a single domain remains a
   direct `MemoryStore` call after acquiring one shared domain-I/O permit;
4. flatten all miss tickets into numeric `(space_priority, domain_index)` cold
   tasks and admit them as one foreground operation;
5. run the exact current per-domain recall mode, filters, spreading behavior,
   and access-recording contract;
6. after the complete barrier, reduce domains per space in numeric order;
7. publish a space cache entry only if its captured write version is still
   current; a racing write makes the result non-cacheable;
8. fuse space results in read-set order.

No outer per-space worker may call `DomainStore::recall`, because that would
nested-fan-out. Expose only engine-private probe/cold-reduce/conditional-publish
operations; remove public `stores()` reliance from production integration.

Pin exact parity before replacement for:

- cold and warm cache output;
- cache key normalization and every filter/mode;
- TTL and capacity;
- write racing cache population;
- every write-kind invalidation;
- hybrid/vector, spreading activation, and non-finite storage output;
- access-count updates on miss/hit and `record_access` true/false.

The flattened path must preserve the existing facade access-count semantics;
layered fusion never applies a second increment. A separate fused-result cache
is out of scope until it proves complete authority/generation invalidation and
does not change telemetry.

**Access-count feedback is scheduled for removal from default ranking.**
`compute_score` multiplies by `1 + 0.15 * ln(1 + access_count)` and
`recall` increments that count for every result it returns, so retrieval
feeds back into ranking. Measured on a frozen copy of a real store with
organic access counts, removing the term changed the top-1 result for
**67% of queries** and perturbed the ordering of every one
(rank-biased overlap 0.639). No evidence exists that it improves
selection, and it costs a write lock on a read path, makes recall
irreproducible for a given query history, and structurally
disadvantages rarely-accessed memories.

Until it is removed, evaluation and any read-only surface must pass
`record_access = false`; an evaluation that records access mutates the
term it is measuring. Retention of the parity surface above should
assume this term is going away rather than hardening it.

### Fusion

Cross-space fusion is **rank-based**. Per-space scores order results
within their own space and are not compared across spaces.

1. Rank each space's results locally, preserving its graph-local score
   for provenance and display only.
2. Assign each result a cross-space value of
   `layer_prior / (rrf_k + rank + 1)`, with `rrf_k = 60` to match the
   within-space constant and every `layer_prior` initially `1.0`.
3. A result surfaced by several spaces keeps its best value and its
   highest-priority origin; presentation-deduplicate only equal semantic
   revision hashes and retain all ordered origins.
4. Sort by cross-space value descending, then read priority, then
   validity/recency, then `SpaceId`, then logical ID.
5. Truncate to `top_k`.

Completion timing never affects output. Equal text/name alone never
deduplicates or creates identity.

**This is rank interleaving, not fusion.** With space-qualified document
keys the candidate lists are disjoint, so no candidate ever accumulates
reciprocal-rank contributions from more than one space. Equal priors tie
equal ranks and read priority breaks the tie. The measured origin share
is exactly 50.0% / 50.0% across two spaces, which is the signature of
deterministic interleaving. The mechanism is named here rather than
described as fusion because its behaviour — a guaranteed share of the
top-k per space — is quota-like and has its own failure mode (below).

If a canonical cross-space evidence identity is later defined, so that
the same underlying memory in two spaces is recognisably one document,
then true evidence-accumulating fusion becomes possible and must be
re-evaluated on its own terms.

### Status: invariant, provisional policy, and promotion gate

**Invariant (established).** Never compare uncalibrated scores across
spaces. Measured on two real spaces, sorting by raw per-space score gave
one space **0.0% of fused positions and returned nothing from it on all
839 queries**, with no signal to the caller that a layer had dropped out.

**Provisional policy.** Deterministic rank interleaving, as specified
above. It is robust to per-space score offsets by construction and needs
no calibration. It is a safe default, not a proven optimum.

**Not established, and required before this becomes fixed:**

- controlled separation of corpus age, corpus size, and score scale on
  further corpus pairs (the first experiment confounded age with size);
- human-reviewed cross-space judgments, including genuine ambiguity
  where different spaces hold different correct answers;
- paired uncertainty (bootstrap confidence intervals) on rule deltas —
  the current gap between interleaving and max-normalisation is well
  inside noise;
- behaviour with 3–4 spaces, unequal trust weights, per-space caps, and
  at least one stale or adversarial space.

**Known weakness of the provisional policy.** Interleaving guarantees
every authorized space a share of the top-k even when that space holds
nothing relevant. That prevents starvation but permits the opposite
failure: a weak, stale, or attacker-controlled space crowding out a
strong one. Bounded layer priors exist to bias against this and are
untested. This trade must be resolved before the policy is frozen.

**What the original failure actually was.** A full 2×2 over verified
experiment arms, crossing corpus age with corpus size. The figure is the
starved space's share of fused positions under raw-score composition,
and how often it contributed nothing across 839 queries:

| larger space | smaller space | its share | contributed nothing |
|---|---|---:|---:|
| full, fresh | original age | **0.0%** | **839 / 839** |
| full, fresh | age-matched | 53.9% | 15 |

Corpus age is a **sufficient** cause: removing the age gap, or setting
λ = 0, each eliminates the starvation entirely. Re-established on
corpora built entirely through the authoritative write path (1811
observations each, identical code, differing only in event time):
the older corpus contributed **0.0%** of fused positions across all 884
queries; shifted forward in time, **51.8%**. Corpus size has **not**
been tested — a size-matched arm was attempted and found invalid,
because deleting canonical rows left the retrieval indexes untouched, so
the arm searched its parent's full corpus while reporting a reduced
size.

The generalisation does not depend on which asymmetry is responsible:
**uncalibrated cross-space score comparison is sensitive to any
systematic per-space difference in the score distribution.** Age is one
demonstrated example; corpus statistics, index mode, embedding model
version, and confidence conventions would each produce the same silent
zeroing.

That is why the invariant is stated in terms of calibration rather than
in terms of decay, and why the remedy is composition that does not
depend on cross-space score comparability at all.

Experiment arms are built by re-running the authoritative ingest with a
transformed clock, never by rewriting stored rows. Rewriting in place
was tried three times and produced invalid evidence every time — a
post-hoc timestamp update left every revision describing different
state, a row deletion left the retrieval indexes holding the parent's
full corpus, and a timestamp shift left every semantic hash describing
the original time. Derived state has exactly one correct writer, and a
verifier only checks the invariants someone thought to encode.

A layer prior remains available as a versioned multiplier on the rank
contribution. It is applied to the rank term, never to a raw score.

## Write/review policy

All writes resolve through `ContextService::authorize_write` and receive one
space-bound write handle plus current authorization lease.

| Owner/actor | Default |
|---|---|
| Personal human | immediate unless explicitly proposed |
| Personal agent | ordinary remember immediate; legacy forget performs reversible retire; other lifecycle actions proposed |
| Team contributor human | proposal |
| Team agent | proposal; never review |
| Team reviewer human | propose and review another actor |
| Team maintainer human | configured low-risk direct write; membership/destruction/merge authority |

Team self-approval is forbidden by default. An optional emergency Maintainer
path requires explicit configuration, reason, separate audit event, and tests.

## Agent, MCP, watch, and ingest context

- Daemon/HTTP MCP requires bearer authentication and, when provided, an opaque
  context capability. Tool arguments cannot broaden it.
- Stdio `openmemory mcp` resolves its startup working directory once and asks a
  daemon for agent context. Without a daemon it remains fixed
  personal-global.
- Explicit overrides: `--workspace`, `--team`, `--project-only`,
  `--global-only`, `--no-project-context`.
- MCP initialization/context output states the concrete safe read layers,
  default target, readiness, and team proposal policy.
- Recall returns provenance. Remember accepts only
  `default|personal|team`; a team proposal is durable but not applied.
- Agents have no approval, identity-decision, membership, promotion, or hard
  destruction tool.
- Watch/ingest resolves one target at startup and retains it for the run.
  Current-directory/team changes never reroute events. Non-atomic bulk ingest
  returns ordered resumable child receipts.

## Required isolation proofs

- Identical names/text in Projects A/B remain physically isolated; only an
  explicit overlay returns both origins.
- Personal/team `cerpheus` concepts never gain shared IDs/edges from recall.
- 1, 1,000, and 10,000 closed catalog entries produce identical results and no
  statistically significant single-context trend.
- Revocation waits old leases, invalidates contexts/caches, and rejects the next
  read/write.
- Workspace move preserves project identity; an unregistered sibling does not
  inherit it.
- Agent team write proposes; agent review fails; human review applies once.
- Concurrent churn never exceeds open-handle, coordinator, queued-task, byte,
  or worker bounds.
- Every 1/4-space × 1/4-domain cold/warm shape has result and cache parity and
  shares one global worker limit across concurrent callers.
