# Product Principles

These principles govern product, UX, architecture, and implementation
decisions. When a feature conflicts with them, the feature should shrink
or wait.

## 1. Local First Is The Product

The product must work with no cloud account, no hosted dependency, and
no network access after install, except explicit model downloads or
opt-in sync.

Implications:

- First run does not require sign-in.
- The app can run offline after local assets are installed.
- Every network-capable feature is visible in settings.
- Cloud backup/sync is additive, encrypted, and opt-in.

## 2. Keep The Engine Separable

The desktop app is not the engine. CLI, MCP, and Rust crates must keep
working without the app.

Implications:

- No desktop-only dependency enters core recall, graph, index, or MCP
  paths.
- Desktop code goes through stable local APIs instead of reaching into
  SQLite files directly.
- The CLI remains scriptable and preserves JSON output behavior.
- Product crates are additive around existing workspace boundaries.

## 3. Make Memory Inspectable

Users must be able to understand and correct what agents remember.
Every persisted fact needs provenance, timestamp, scope, and a clear
edit/delete path.

Implications:

- Memory rows show source, tier, confidence, entity, profile, and
  workspace when available.
- Recall explanations expose scoring factors in human terms.
- Deletion and forgetting are reachable from entity, observation,
  search, and recall-explanation surfaces.
- Raw internal IDs are available for debugging but are not the primary
  UI language.

## 4. Review Before Automatic Capture

Automatic capture must not silently persist inferred facts by default.
Candidate memories go to an inbox where users can approve, reject, edit,
or downgrade them.

Implications:

- Rule-based extraction starts conservative.
- Optional LLM extraction is explicit and locally scoped where possible.
- Auto-approve, if added, is category-specific and off by default.
- Rejections feed local suppressions to reduce repeated noise.

## 5. Scope Is Mandatory

Global user preferences and repository-specific decisions must not
pollute each other.

Implications:

- The product has an explicit workspace/profile mapping model.
- Every capture session records client, workspace, and profile.
- UI filters expose scope clearly.
- Cross-scope recall is a deliberate setting, not a hidden default.

## 6. Diagnostics Are Product Surface

MCP setup failures are common enough that `doctor`, setup verification,
and health state are first-class features.

Implications:

- Setup is previewed before writing client config.
- Verification is idempotent and re-runnable.
- Errors have stable typed codes and suggested fixes.
- The dashboard shows health, model, profile, and client state.

## 7. Simplicity Beats Completeness

The app should do fewer things well. It is a control plane for agent
memory, not a notes app, document manager, project tracker, or hosted
developer platform.

Implications:

- V1 screens are operational: setup, dashboard, memory, search,
  inbox, integrations, settings.
- No decorative data views unless they help diagnosis or correction.
- Graph visualization is optional and post-core-flow.
- Advanced team features wait for individual retention.

## 8. Performance Is Part Of Trust

Local-first products earn trust by feeling instant, quiet, and
predictable.

Implications:

- Idle CPU is effectively zero.
- Startup uses cached health where safe and refreshes incrementally.
- Large memory stores paginate from the API boundary.
- Capture work is queued and isolated from recall/search hot paths.

## 9. Boring Failure Modes

Local user data is hard to recover if product code is careless.
Failures should be explicit, typed, and reversible where possible.

Implications:

- No panic or `unwrap()` on request, setup, capture, backup, restore,
  or migration paths.
- Store migrations are forward-only and refuse schema-too-new data.
- Destructive operations preview impact and use soft-delete where the
  engine supports it.
- Backup and restore have dry-run validation and CI fixtures.

