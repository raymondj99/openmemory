# PROMPT.md — Agent prompt for the Reich molecular-sex demo

Paste the block below verbatim into the agent session that has the
`openmemory-demo` MCP server attached (see IMPLEMENTATION.md §8).

Do not edit the prompt between runs; the rubric in PLAN.md §5 grades
against this exact wording so that recall behaviour is comparable
across runs.

---

You have an MCP server named `openmemory-demo` available, exposing
the openmemory tools:

- `openmemory_recall(query, ...)`
- `openmemory_search(query, uri_prefix?, ...)`
- `openmemory_get_entity(entity)`
- `openmemory_list_entities(entity_type?, ...)`
- `openmemory_remember(...)` (do not use; this task is read-only)
- `openmemory_consolidate(...)` (optional; safe to call once if you
  suspect duplicates are polluting recall)

This is the lab's working memory. It already contains everything you
need: protocol notes, decision notes, paper excerpts, dataset paths,
prior shell scripts, and notebook entries. Do not ask me for any of
those facts. Recall them.

**Task.** For every BAM in our Rhine-Meuse batch, compute the
molecular-sex ratio the way our lab does it, and produce a TSV with
`sample_id`, `n_reads_X`, `n_reads_Y`, `Ry`, `sex_call`. Match the
protocol we used in the Olalde Rhine-Meuse paper.

**Deliverable.** Emit a single, self-contained bash script (one
fenced ```bash code block, nothing else after it) that I can run
unmodified. The script must:

1. Iterate over every `*.bam` in the lab's current Rhine-Meuse BAM
   directory.
2. Count reads against the lab's 1240K chrX and chrY BED panels using
   the lab's canonical samtools recipe and mapping-quality filter.
3. Compute `Ry = n_reads_Y / (n_reads_X + n_reads_Y)`.
4. Apply the lab's minimum-coverage floor before classifying. Below
   the floor, emit the literal token `low_coverage` in the `sex_call`
   column and `NA` in the `Ry` column.
5. Classify the rest using the lab's thresholds, including its
   convention for samples that fall in the ambiguous gap. (The paper
   is silent on the gap; the convention lives in the protocol notes.)
6. Write a TSV at the lab's results directory with the column order
   above.

Every numeric constant, every path, and every threshold in the script
must trace back to a specific entity or indexed note in
`openmemory-demo`. If you find conflicting values (for example, an
older threshold pinned in a 2024 note), prefer the lab's current
standing protocol and the most recent decision note.

Start with `openmemory_recall` for the high-level concept ("molecular
sex determination ratio", or the lab's protocol name), then drill in
with `openmemory_get_entity` and `openmemory_search` with URI-prefix
filters (`protocol://`, `paper://`, `decision://`, `file://`) to
resolve each constant. Do not invent values. Do not ask me clarifying
questions; everything is in memory.

When you have the script, emit it and stop.
