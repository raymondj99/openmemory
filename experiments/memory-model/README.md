# Memory Model Validation Harness

This standalone Rust workspace validates the memory-space, audit, manual-edit,
and merge design before those abstractions enter production crates.

It intentionally has its own `Cargo.toml` and lockfile. Production graph and
domain stores are path dependencies for isolation/recall experiments. The
audit and merge database is disposable proof-of-concept code, not a schema to
copy directly into production.

Run the correctness, property, concurrency, and subprocess crash suite:

```text
cargo test --offline
```

Run the Rust quality gate:

```text
cargo fmt --all -- --check
cargo clippy --offline --all-targets -- -D warnings
```

Build and run release measurements:

```text
cargo run --release --offline --bin measure
cargo run --release --offline --bin identity-eval
cargo run --release --offline --bin wikipedia-identity-eval
cargo run --release --offline --bin additional-real-world-eval
cargo run --release --offline --bin github-merge-eval
cargo run --release --offline --bin github-merge-measure
cargo run --release --offline --bin framework-merge-eval
cargo run --release --offline --bin framework-merge-measure
cargo run --release --offline --bin math-code-merge-eval
cargo run --release --offline --bin math-code-merge-measure
cargo run --release --offline --bin redesign-eval
cargo run --release --offline --bin semantic-merge-scale
```

The runner emits JSON containing p50/p95/p99/max latency, relative audit
overhead, durable database bytes, layered recall, candidate fusion, 10k-object
planning, and 1k-object materialization. Run it repeatedly; the audit controls
are interleaved to reduce filesystem phase bias. Results and interpretations
belong in the repository-level `LOGBOOK.md`.

`identity-eval` compares an intentionally unsafe name-only control with the
proof-gated identity policy, exercises a reproducible contextual control at
the untrusted agent boundary, and measures bounded indexed lookup plus the
durable proposal/review path. Its 80,000-entity scale fixture includes two
aliases per entity, source-verified identifiers, incompatible ontology kinds,
and directional relation evidence. The synthetic corpus validates routing and
safety invariants; it does not claim to measure a real model's semantic
accuracy.

`wikipedia-identity-eval` loads two source-grounded project spaces from
`fixtures/wikipedia_two_projects.json`: a Solar System history exhibit and a
Roman mythology/planetary-naming exhibit. It compares the legacy name/string-
kind behavior with versioned ontology rules, alias discovery, source-verified
identifier assertions, and reviewed `named_after` suggestions. The fixture is
curated and paraphrased; it validates the architecture against real entities,
not model accuracy or human-review usability.

`additional-real-world-eval` adds two independent collision corpora. Java
tests one programming language against its renamed representation, the island,
and Java coffee. Python tests the programming language against its renamed
representation, the snake genus, and Monty Python. The Python fixture requires
a source-verified `named_after` assertion to discover a non-homonymous target.
Only an assertion whose source is bound to the subject record may nominate a
target, and both normalized target label and controlled kind must match;
claimed or wrong-kind assertions cannot expand the candidate set.

`github-merge-eval` loads pinned, separately scoped snapshots of
`openai/codex` and `openai/homebrew-tools`, validates every provenance URL and
graph endpoint, resolves six deliberately ambiguous cross-space candidates,
and prints the complete semantic changeset for importing the latter into a
graph based on the former. It explicitly proves that the tap's OpenAI CLI and
`openai` cask are not Codex or its `codex` cask. `github-merge-measure` runs
10,000 complete parse/validate/resolve/render iterations for a reproducible
end-to-end timing sample.

`framework-merge-eval` applies the same reusable evaluator to pinned snapshots
of `tokio-rs/axum` and `actix/actix-web`. It merges shared package identities
and framework-independent concepts only through team review while proving that
similarly named routers, handlers, extractors, request/response types,
middleware contracts, WebSocket sessions, and license policies remain
distinct. In particular, merging the crates.io `http` package does not imply
type compatibility between the 1.x and 0.2 API lines.
`framework-merge-measure` runs 10,000 complete evaluations of this larger
59-record, 74-relation, 20-candidate corpus.

`math-code-merge-eval` tests a direct mathematics/implementation dependency:
`leanprover-community/mathlib4` and `leanprover/lean4`. It coalesces the two
reciprocal repository references, Lake, Std, the core `Nat` declaration,
shared namespaces, licensing, and abstract concepts while keeping Mathlib's
project, modules, tactics, metaprogramming extensions, number theory, CI, and
fixed release policy distinct from Lean's implementation records. The diff
also demonstrates relation rewiring in both directions: Mathlib's toolchain
dependency resolves to the imported Lean repository graph, and Lean's
downstream compatibility workflow resolves back to the existing Mathlib
repository. `math-code-merge-measure` runs 10,000 complete evaluations of the
64-record, 93-relation, 22-candidate corpus.

All three repository evaluators now delegate changeset construction to
`semantic_merge`, the redesigned generic planner. Fixture expectations are
test oracles only. The planner consumes immutable snapshots, the complete
candidate set, and revision/packet-bound resolution receipts; it independently
enforces human review for `same`, one-to-one coalescence, collision-safe
source-qualified additions, immutable origin contributions, provenance-bearing
relation rewiring, complete source accounting, stale-input rejection, predicted
result hashing, and plan-hash verification. `redesign-eval` runs the three
permanent repository examples and emits one machine-readable summary.

`semantic-merge-scale` measures repeated full plan plus in-memory materialize
cycles at 100/99, 1,000/999, and 10,000/9,999 entity/relation sizes. Planning
already simulates and verifies the predicted result; the measured materialize
step deliberately repeats validation as the apply boundary would. This is a
deterministic algorithmic envelope, not a persistence or end-user benchmark.

`poc-crash-worker` is invoked by integration tests. It deliberately aborts and
must not be run against valuable data.
