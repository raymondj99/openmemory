# Reich Molecular-Sex Determination Demo

This demo exercises openmemory as a working-memory layer for an agent
writing a small bioinformatics pipeline. The prompt asks for a TSV of
molecular-sex calls for a synthetic Rhine-Meuse BAM batch. The agent is
not given constants directly; it must retrieve the formula, thresholds,
panel paths, read-counting command, minimum coverage floor, ambiguity
labels, input directory, and output schema from openmemory.

## What It Builds

`scripts/demo-reich/run-demo.sh` creates all runtime state under:

```text
/tmp/openmemory-reich-demo
```

The sandbox contains:

- `labshare/protocols/` with the current molecular-sex protocol.
- `labshare/notebooks/` with one current note and one stale threshold note.
- `labshare/data/panels/` with synthetic 1240K X/Y BED files.
- `labshare/bams/2026-05/` with seven sorted, indexed synthetic BAMs.
- `fixtures/` with paper excerpts and decision notes seeded through MCP.
- `artifacts/` with logs, transcript, rubric, and generated TSV.

The script starts openmemory MCP over HTTP at `http://127.0.0.1:7801/mcp`
and starts the filesystem watcher on the labshare tree with
`--exts md,sh,py`.

## Grading

The rubric in `scripts/demo-reich/grade.sh` awards ten points for the
agent script:

- Correct Ry formula.
- Current female and male thresholds, not the stale Skoglund cutoff.
- Correct 1240K chrX/chrY BED paths.
- `samtools view -c -q 30 -L`.
- 200-read floor.
- Exact TSV columns.
- Correct results and BAM directories.
- Ambiguous gap handling.

After the rubric, the script runs the generated pipeline and diffs
`labshare/results/sex/rhine_meuse_2026-05.tsv` against the held-out
manifest. A green run prints `End-to-end: 7/7 match`.

## Running

```bash
scripts/demo-reich/run-demo.sh
```

Run the agent with `PROMPT.md`, save its answer to
`/tmp/openmemory-reich-demo/artifacts/transcript.md`, then grade:

```bash
scripts/demo-reich/run-demo.sh \
  --skip-build \
  --transcript /tmp/openmemory-reich-demo/artifacts/transcript.md
```

Use the reference script only to verify the harness itself:

```bash
scripts/demo-reich/run-demo.sh --reference
```

## Last Smoke Check

On 2026-05-16, `scripts/demo-reich/run-demo.sh --skip-build --reference`
completed with:

```text
RUBRIC SCORE: 10 / 10
End-to-end: 7/7 match
```
