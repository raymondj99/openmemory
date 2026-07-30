# Phase 1 merge fixtures

These compact fixtures are sanitized, source-grounded derivatives of the
larger isolated prototype corpora under `experiments/memory-model/fixtures`.
They contain public repository facts only—no user data, credentials, local
paths, model output, or network dependency.

Each fixture records pinned upstream commits and repository licenses. The test
loader treats those records only as provenance. It constructs validated
production `openmemory-merge` values, runs generic candidate discovery,
deterministic packet analysis, explicit human receipts, the streaming planner,
structural verification, and an independent bounded materialized oracle.
Fixture names and descriptions are never inspected by production code.

- `codex-homebrew-tools.json`: `openai/codex` and
  `openai/homebrew-tools`, Apache-2.0.
- `axum-actix-web.json`: `tokio-rs/axum` and `actix/actix-web`, MIT.
- `mathlib-lean4.json`: `leanprover-community/mathlib4` and
  `leanprover/lean4`, Apache-2.0.

The fixture expectations are review oracles, not implementation branches.
