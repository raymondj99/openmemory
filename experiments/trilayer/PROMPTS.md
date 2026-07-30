# Demo prompts: building and correcting the shared knowledge graph

These are the agent-facing prompts the ingest harness automates. Each
prompt is something a user could type to an MCP-connected agent; the
tool calls underneath are the same ones `scripts/ingest.py` issues.
They double as a manual walkthrough for demoing the tri-layer design.

## 1. Register the projects (L1 concepts)

> "Remember three projects we work with: codex is OpenAI's coding
> agent CLI, clap is the Rust command-line argument parser, and anyhow
> is dtolnay's error-handling library. Codex depends on both clap and
> anyhow."

Tool calls: `openmemory_remember` x3 (`entity_type: project`), then
`openmemory_add_relation` codex-depends_on->clap and
codex-depends_on->anyhow (explicit entity types on both ends; the
tools default to `concept` and will not find a `project` otherwise).

## 2. Register structure (L1 crates and files)

> "clap is a workspace: clap_builder holds the runtime, clap_derive
> the proc macros, clap_lex the token lexer, clap_complete shell
> completions. Each is part_of clap."

> "Remember the file clap/clap_builder/src/parser/features/suggestions.rs:
> it implements the 'did you mean' suggestions when a user mistypes a
> flag. It is part of clap_builder."

Tool calls: `openmemory_remember` with `relations: [{to_entity:
"clap_builder", relation_type: "part_of"}]` and a fielded observation
(`title`, `concepts`, `source_files`) carrying the gloss. The gloss is
the L2 detail; the entity and edges are L1.

## 3. Point at ground truth (L3 index)

> "Index the content of that file so I can search inside it later."

Tool calls: `openmemory_index_text` with
`uri: omem://clap/clap_builder/src/parser/features/suggestions.rs#0`.
The graph never copies file content; the URI is the pointer.

## 4. Query through the layers

> "Where does clap suggest a close alternative for a mistyped flag?"

Route: `openmemory_recall` localizes the concept (file entity gloss),
`openmemory_search` scoped by `uri_prefix: omem://clap/` retrieves the
chunk, and the entity's `part_of` chain frames the answer
(clap_builder, clap).

## 5. Correct the graph (malleability demo)

> "Actually, codex no longer uses its own arg parser; it uses clap for
> all subcommands. Update your knowledge."

Tool calls today: `openmemory_add_relation` (codex uses->clap) plus a
new observation on `codex` with `source: "correction"`. Under the
target design (plan/17) this would be a supersession event: new
assertion, `supersedes` edge, `valid_until` stamp on the old one, user
authority pinning the new fact. The gap between those two paragraphs
is the roadmap.

## 6. Interrogate identity (the homonym demo)

> "There is an error.rs in anyhow, an error module in clap_builder,
> and an error.rs in codex's protocol crate. Show me anyhow's."

This is the query class where flat lexical search collapses (every
sense matches "error") and concept scoping or vector disambiguation
must carry it; it is measured by the `homonym` category in
`queries/queries.jsonl`.
