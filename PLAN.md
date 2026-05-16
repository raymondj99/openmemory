# Demo: Molecular Sex Determination from Sequencing Reads

A reproducible, end-to-end demo that stresses every part of openmemory's
surface (knowledge graph, free-text index, filesystem watcher, hybrid
recall, decay, consolidate) by asking an MCP agent to write a working
bioinformatics script using only knowledge stored in openmemory.

The contrived experiment is drawn from Olalde et al. 2026, "Genome-wide
ancient DNA from the Lower Rhine-Meuse area shows late persistence of
forager ancestry shaped Bell Beaker expansion", *Nature*
(doi:10.1038/s41586-026-10111-8), section *Molecular sex determination*:

> Genetic sex was determined by calculating the ratio of reads mapped to
> Y-chromosome SNP positions to the total reads mapped to sex-chromosome
> SNP positions. Individuals with a ratio of <0.03 were classified as
> female, while those with a ratio >0.32 were classified as male.

Three sentences. Fully specified mathematically. But to actually *run*
this on a directory of BAM files, an agent has to pull together at
least ten facts that are not in the paper: where the SNP panel lives on
disk, how the EIGENSTRAT `.snp` file encodes chromosomes, which
read-counting flags the lab uses, the minimum-read floor the lab
applies, how to label ambiguous samples whose ratio falls in the gap
between 0.03 and 0.32, the BAM directory naming convention, and the
output TSV schema. Those facts live in protocols, notebooks, prior
shell scripts, and decision notes. That is exactly the surface
openmemory is meant to consolidate, and that is what this demo tests.

## 1. Goal

The user prompt to the agent (verbatim, reproducible across runs):

> "For every BAM in our Rhine-Meuse batch, compute the molecular-sex
> ratio the way our lab does it, and produce a TSV with `sample_id`,
> `n_reads_X`, `n_reads_Y`, `Ry`, `sex_call`. Match the protocol we used
> in the Olalde Rhine-Meuse paper."

Nothing else is specified. Everything the agent needs has to come out
of openmemory. Success is mechanically graded against the rubric in §5
and against ground truth in §7.

## 2. Why this method is a good stress test

| openmemory feature                | Stressed by                                                                                          |
| --------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Knowledge graph + relations       | Sample, library, BAM, SNP panel, reference genome, tool form a real graph                            |
| Hybrid search (vector + keyword)  | Some queries are conceptual ("molecular sex ratio"), others exact ("samtools -L", "1240K_chrY.bed")  |
| Free-text index under custom URIs | Paper excerpts (`paper://`), protocols (`protocol://`), decision notes (`decision://`), notebooks    |
| Filesystem watcher                | `~/labshare/` carries scripts, protocols, notebooks, READMEs; incrementally indexed                  |
| Ebbinghaus decay                  | Old 2024 note pinning the Skoglund 0.075 threshold must lose to the fresh 2026-05-01 decision        |
| Spreading activation              | Recall on "Rhine-Meuse batch" should activate the BAM directory and the 1240K panel through relations |
| Consolidate / dedup               | Multiple notes describing the same protocol in different words must collapse                         |
| Entity normalization              | "1240K capture", "1240K", "1,240K SNP panel" must resolve to one entity                              |

The method is also cheap to actually run. `samtools view -c -L sites.bed`
plus an awk one-liner is the entire pipeline. We can drive the whole
thing end-to-end in CI on synthetic BAMs, with no ADMIXTOOLS, no R, and
no real ancient DNA.

## 3. Knowledge-base design

Three storage layers, each carrying the kind of fact it is best at.

### 3.1 Knowledge graph (`openmemory_remember`)

**Datasets and panels**

- `AADR_v54_1` (type `dataset`)
  - "Path: /labshare/data/aadr/v54.1/v54.1_1240K_public"
  - "Format: EIGENSTRAT (.geno, .snp, .ind)"
  - "Published: 2023-04, supersedes v52.2"
  - relation `succeeds` -> `AADR_v52_2`
- `SNP_panel_1240K` (type `dataset`)
  - "1,233,013 targeted SNPs"
  - "Chromosome encoding in .snp column 2: X=23, Y=24, autosomes 1-22"
  - "Capture reagent described in Mathieson et al. 2015"
- `SNP_panel_1240K_X` (type `dataset`)
  - "Path: /labshare/data/panels/1240K_chrX.bed"
  - "Derived from 1240K .snp by `awk '$2==23'` then BED conversion"
  - relation `derived_from` -> `SNP_panel_1240K`
- `SNP_panel_1240K_Y` (type `dataset`)
  - "Path: /labshare/data/panels/1240K_chrY.bed"
  - relation `derived_from` -> `SNP_panel_1240K`

**Reference and tools**

- `hg19` (type `fact`)
  - "Reference genome used for all Lower Rhine-Meuse BAMs"
  - "Olalde 2026 methods, §Bioinformatics"
- `bwa_0_7_15` (type `tool`)
  - "Aligner used by the lab pipeline (bwa samse)"
- `samtools` (type `tool`)
  - "Used for read counting"
  - "Lab default: `samtools view -c -q 30 -L <bed> <bam>` to count reads overlapping a position set"
- `SeqPrep_v1_1` (type `tool`)
  - "Adapter merging, 15-bp overlap rule"

**Protocols and decisions**

- `MolecularSexProtocol` (type `preference`)
  - "Ry = n_reads_on_Y_SNPs / (n_reads_on_X_SNPs + n_reads_on_Y_SNPs)"
  - "Threshold female: Ry < 0.03"
  - "Threshold male:   Ry > 0.32"
  - "Min reads on sex chromosomes: 200. Below that, do not call; output `low_coverage`."
  - "Ambiguous (0.03 <= Ry <= 0.32) label: `XX?` if Ry < 0.15 else `XY?`. Lab convention; not in Olalde 2026."
  - "Counting strategy: any read overlapping a panel position (samtools -L), not strict base coverage."
  - relation `cites` -> `Olalde2026_RhineMeuse`
  - relation `supersedes` -> `Skoglund2013_SexProtocol`
- `Olalde2026_RhineMeuse` (type `fact`)
  - "Nature 2026, doi:10.1038/s41586-026-10111-8"
  - "Source of the 0.03 / 0.32 thresholds"
- `Skoglund2013_SexProtocol` (type `fact`)
  - "Original Ry method, used a 0.075 cutoff for female"
  - "Deprecated by our lab on 2026-05-01"
- `LabPipelineDefaults` (type `preference`)
  - "min_reads_sex: 200"
  - "output_tsv_columns: sample_id, n_reads_X, n_reads_Y, Ry, sex_call"
  - "results_dir: /labshare/results/sex"
  - "input_bam_dir for current batch: /labshare/bams/2026-05"

**Samples (subset, used for grading)**

- `I12091`, `I12902`, `I15651`, `I33738`, `I38442` (type `fact`, role `sample`)
  - observations: site, period, expected sex (held-out ground truth)
  - relation `aligned_to` -> `hg19`
  - relation `enriched_with` -> `SNP_panel_1240K`
  - relation `reported_in` -> `Olalde2026_RhineMeuse`

**People**

- `Raymond` (type `person`)
  - relation `maintains` -> `MolecularSexProtocol`
  - relation `maintains` -> `openmemory`

### 3.2 Free-text index (`openmemory_index_text`)

| URI                                                | Contents                                                                            |
| -------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `paper://olalde-2026-rhine-meuse#methods-sex`      | The verbatim three-sentence Methods paragraph on molecular sex determination        |
| `paper://olalde-2026-rhine-meuse#methods-bioinfo`  | Bioinformatics paragraph (hg19, bwa 0.7.15, SeqPrep, duplicate removal)             |
| `paper://skoglund-2013-sex-determination`          | Original Ry method abstract and the 0.075 cutoff (kept on purpose as a stale source) |
| `protocol://molecular-sex`                         | The lab's standing protocol, including the ambiguity rule that is NOT in the paper  |
| `protocol://1240K-panel-layout`                    | "Column 2 of the .snp file is chromosome; X=23, Y=24."                              |
| `protocol://bam-readcount`                         | The `samtools view -c -q 30 -L` recipe and why we prefer it over bedtools           |
| `dataset-readme://1240K`                           | AADR README excerpt describing the panel files                                      |
| `decision://sex-thresholds-olalde-2026`            | "Adopted 0.03 / 0.32 thresholds on 2026-05-01, replacing Skoglund 0.075"            |
| `decision://min-reads-sex-200`                     | "200-read floor chosen after the 2024 Hazendonk batch produced unstable Ry < 100 reads" |

Each chunk is 300 to 800 characters so vector recall can return just the
relevant bit instead of an entire document.

### 3.3 Watched directory (`openmemory watch ~/labshare`)

```
~/labshare/
  bin/
    count_reads_per_pos.sh        # prior wrapper; canonicalizes the samtools flags
    classify_sex.py               # tiny helper, takes nX, nY and emits a call
  protocols/
    molecular-sex.md              # mirrors the protocol:// entry, on disk
    1240K-panel-layout.md
    bam-readcount.md
  notebooks/
    2026-05-02.md                 # "running sex calls on Rhine-Meuse batch; bams in /labshare/bams/2026-05/"
    2024-11-18.md                 # "old Skoglund 0.075 threshold deprecated, see decision note"
  data/
    aadr/v54.1/README.md
    panels/
      1240K.snp                   # full EIGENSTRAT .snp panel
      1240K_chrX.bed              # derived (X positions only)
      1240K_chrY.bed              # derived (Y positions only)
    bams/2026-05/
      I12091.bam, I12091.bam.bai
      I12902.bam, I12902.bam.bai
      I15651.bam, I15651.bam.bai
      I33738.bam, I33738.bam.bai
      I38442.bam, I38442.bam.bai
      LOW01.bam,  LOW01.bam.bai
      AMB01.bam,  AMB01.bam.bai
    sample-manifest.tsv           # sample_id, site, period, expected_sex (ground truth, held out)
```

The watcher's default extension list already covers `.md` and `.sh`. We
add `.py` via `--exts md,sh,py`. The `.bam`, `.bai`, `.bed`, and `.snp`
files are deliberately not indexed; they are referenced by path through
the knowledge graph entries.

## 4. Expected agent retrieval flow

A correct run produces an MCP trace that looks roughly like this:

1. `openmemory_recall("molecular sex determination ratio")`
   surfaces `MolecularSexProtocol` from the graph plus
   `paper://olalde-2026-rhine-meuse#methods-sex` and
   `protocol://molecular-sex` from the index. All three corroborate the
   formula.
2. `openmemory_get_entity("MolecularSexProtocol")`
   pulls thresholds, the 200-read floor, and the ambiguity convention.
3. `openmemory_search("1240K snp file chromosome column", uri_prefix="protocol://")`
   returns `protocol://1240K-panel-layout` ("col 2 is chrom; X=23, Y=24").
4. `openmemory_get_entity("SNP_panel_1240K_X")` and
   `openmemory_get_entity("SNP_panel_1240K_Y")`
   resolve the bed file paths.
5. `openmemory_search("count reads bed", uri_prefix="file:///labshare/bin/")`
   discovers `count_reads_per_pos.sh` is `samtools view -c -q 30 -L $BED $BAM`.
6. `openmemory_recall("input bams Rhine-Meuse batch location")`
   pulls the notebook `2026-05-02.md` which names `/labshare/bams/2026-05/`.
7. `openmemory_get_entity("LabPipelineDefaults")`
   returns the output column order and results directory.
8. The agent emits the script.

### Expected output script (rubric reference)

```bash
#!/usr/bin/env bash
# molecular_sex.sh — implements Olalde 2026 §Molecular sex determination
set -euo pipefail

BAM_DIR=/labshare/bams/2026-05
BED_X=/labshare/data/panels/1240K_chrX.bed
BED_Y=/labshare/data/panels/1240K_chrY.bed
OUT=/labshare/results/sex/rhine_meuse_2026-05.tsv
MIN_READS=200      # lab floor; below this, output low_coverage

mkdir -p "$(dirname "$OUT")"
printf "sample_id\tn_reads_X\tn_reads_Y\tRy\tsex_call\n" > "$OUT"

for bam in "$BAM_DIR"/*.bam; do
    sid=$(basename "$bam" .bam)
    nX=$(samtools view -c -q 30 -L "$BED_X" "$bam")
    nY=$(samtools view -c -q 30 -L "$BED_Y" "$bam")
    total=$((nX + nY))
    if (( total < MIN_READS )); then
        call=low_coverage
        ry="NA"
    else
        ry=$(awk -v y="$nY" -v t="$total" 'BEGIN{printf "%.4f", y/t}')
        call=$(awk -v r="$ry" 'BEGIN{
            if      (r < 0.03) print "XX";
            else if (r > 0.32) print "XY";
            else if (r < 0.15) print "XX?";
            else               print "XY?";
        }')
    fi
    printf "%s\t%d\t%d\t%s\t%s\n" "$sid" "$nX" "$nY" "$ry" "$call" >> "$OUT"
done
```

Every constant here must trace back to a specific record in openmemory.
That is what makes the rubric mechanical.

## 5. Evaluation rubric

Ten binary checks against the agent's emitted script:

1. Uses `Ry = nY / (nX + nY)`, not `nY / nX`.
2. Female threshold is `< 0.03`, not Skoglund's 0.075.
3. Male threshold is `> 0.32`.
4. Reads are counted against the 1240K chrX and chrY bed files at the
   correct paths.
5. Uses `samtools` (or `pysam` equivalent) and honours a mapping-quality
   filter (`-q 30`), not bare `bedtools coverage`.
6. Applies the 200-read minimum-coverage floor before classifying.
7. Output TSV columns are `sample_id, n_reads_X, n_reads_Y, Ry, sex_call`
   in that exact order.
8. Writes to `/labshare/results/sex/`.
9. Iterates over `/labshare/bams/2026-05/`.
10. Has *some* convention for ambiguous Ry in `[0.03, 0.32]`. The lab's
    note is in the KB; missing it means the agent did not consult the
    protocol thoroughly.

Score 7 or more out of 10: pass. Below 5: recall failure; we replay the
MCP transcript to debug.

**Bonus end-to-end check.** Run the agent's emitted script on the
synthetic BAMs (§7). Diff the produced TSV against
`sample-manifest.tsv`. Expect a 100% match on the non-ambiguous,
non-low-coverage samples.

## 6. Failure modes seeded on purpose

To prove the scoring layer earns its keep:

- **Stale threshold.** A 2024 observation pins the Skoglund 0.075 cutoff.
  The 2026-05-01 decision note plus `MolecularSexProtocol` observations
  should outscore it via Ebbinghaus decay. If the agent uses 0.075, that
  is a decay-scoring bug, not an agent bug.
- **Wrong panel.** A `decision://wrong-panel-2023` note explains why the
  Human Origins array is unsuitable for sex calls (too few Y SNPs). The
  agent should not even propose HO.
- **Near-duplicate protocols.** Two protocol notes restate the same
  formula in different words. `openmemory_consolidate` should collapse
  them; the agent should still get a single coherent answer.
- **Lexical red herring.** A paper note that mentions "sex chromosomes"
  only in a discussion of mtDNA inheritance. Vector recall should
  outrank pure keyword overlap here.
- **Entity-normalization stress.** Seed `1240K capture`, `1240K`,
  `1,240K SNP panel`, and `1240K_capture` as four separate
  `openmemory_remember` calls. The fuzzy-resolver should merge them
  into one entity at write time.

## 7. Synthetic BAMs for end-to-end grading

We do not need real ancient DNA. The demo generates synthetic but valid
BAMs aligned to a minimal `hg19` chrX/chrY header, using `pysam` so the
files pass `samtools view` cleanly.

```python
# scripts/demo-reich/make_synthetic_bams.py
# For each sample, pick a target Ry. Sample 1240K panel positions
# weighted by chromosome so n_reads_Y / (n_reads_X + n_reads_Y) ~= Ry.
# Emit a sorted, indexed BAM.
```

Manifest written alongside (ground truth, held out from the index):

```
sample_id  target_ry  expected_sex   total_reads
I12091     0.50       XY             500
I12902     0.48       XY             500
I15651     0.01       XX             500
I33738     0.49       XY             500
I38442     0.00       XX             500
LOW01      0.40       low_coverage    80    # below 200-read floor
AMB01      0.20       XY?            500    # ambiguous on purpose
```

The script the agent emits, run on these BAMs, must reproduce
`expected_sex` exactly. This converts the demo from "looks plausible"
into a hard pass / fail.

## 8. Deliverables

In recommended build order:

1. `scripts/demo-reich/build-labshare.sh`
   Generates `~/labshare/...` with fake but realistic contents.
   Idempotent.
2. `scripts/demo-reich/make_synthetic_bams.py`
   Synthetic BAMs and the held-out ground-truth manifest.
3. `tests/fixtures/reich-demo/`
   - `paper-olalde-2026-methods-sex.txt`
   - `paper-olalde-2026-methods-bioinfo.txt`
   - `paper-skoglund-2013-sex.txt`
   - `protocol-molecular-sex.md`
   - `protocol-1240K-panel-layout.md`
   - `protocol-bam-readcount.md`
   - `decision-sex-thresholds-olalde-2026.md`
   - `decision-min-reads-sex-200.md`
   - `decision-wrong-panel-2023.md`
   - `notebook-2026-05-02.md`
   - `notebook-2024-11-18.md` (stale; carries the deprecated 0.075 cutoff)
4. `scripts/demo-reich/seed.sh`
   Idempotent. Issues `openmemory remember` for §3.1, `openmemory_index_text`
   for §3.2, and starts `openmemory watch ~/labshare --exts md,sh,py` for §3.3.
5. `scripts/demo-reich/run.md`
   The canonical agent prompt plus the rubric, copy-pastable into any
   MCP client.
6. `tests/demo_reich_sex.rs`
   Integration test that drives `OpenMemoryMcpServer::handle` directly,
   replays the prompt, asserts the rubric items, runs the emitted
   script against the synthetic BAMs, and diffs against the ground truth.
   This turns the demo into regression coverage instead of a one-off.
7. `docs/demos/reich-sex-determination.md`
   Writeup with a passing MCP transcript and the rubric scoreboard.

## 9. Decisions implicit in this plan

Flagged so we can confirm or revise before building:

- **Counting strategy.** `samtools view -c -L bed` counts any read
  overlapping a position set. The paper says "reads mapped to SNP
  positions", which is slightly ambiguous between any-overlap and
  strict-base-coverage. The lab protocol pins it to any-overlap for
  tractability; we record this explicitly in `protocol://bam-readcount`.
- **Min-read floor of 200.** The paper uses 20,000 SNPs as a population-
  analysis inclusion threshold, which is a different metric. For sex
  calls Olalde does not publish a floor. We invent 200 and put it in
  the lab notes so the agent has to find it there, not in the paper.
  This is the demo's main point: knowledge that lives in the lab, not
  the literature.
- **Ambiguity convention** (`XX?` and `XY?`). Also lab-invented for the
  same reason. The paper is silent on samples that fall in the gap.
- **Emit-only or execute.** The agent emits the script. We run it
  ourselves in the integration test against the synthetic BAMs. Letting
  the agent shell out would conflate "did it remember correctly" with
  "can it run a shell command", which we do not want to grade together.

## 10. Out of scope

- Real ancient DNA, real BAMs, or any external network calls.
- Anything beyond `samtools`/awk in the emitted script. No GLIMPSE, no
  ADMIXTOOLS, no R.
- The qpAdm or admixture-dating workflows from the same Olalde paper.
  Those would be good follow-up demos but they multiply the runtime
  dependencies and make rubric grading much harder.
- Authentication, multi-user, or remote-watcher setups. This is a
  single-machine demo.

## 11. Open questions

1. Do we want to land the demo as a CI-running integration test, or as
   an optional `cargo run --example demo_reich_sex` that the developer
   triggers manually? CI is cleaner but adds `samtools` as a CI
   dependency.
2. Should we publish the demo alongside an `openmemory demo` subcommand
   that wires steps 1, 2, 4 together for users, or keep it as scripts
   under `scripts/demo-reich/`?
3. Do we want a second demo prompt that asks the agent for a *report*
   (markdown summary of which samples came out male/female and why)
   rather than a script, to test recall in a different shape?
