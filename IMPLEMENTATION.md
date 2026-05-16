# IMPLEMENTATION.md — Reich Molecular-Sex Demo

A copy-paste runbook for the end-to-end demo specified in [PLAN.md](PLAN.md).
Every step is mechanical; no judgement calls. An agent executing this
file should end with a graded rubric score and a TSV that matches the
held-out ground truth.

The demo runs entirely inside a sandbox at:

```
/tmp/openmemory-reich-demo/
```

Nothing under `$HOME/labshare` or `$HOME/.openmemory` is touched. All
state lives under that single tmp root and can be wiped with `rm -rf`.

## 0. Conventions used in this file

* All commands assume the working directory is the openmemory repo
  root: `/Users/rjow/open-memory`.
* Where a script must be created, the exact contents are inlined.
  Write the file verbatim; do not paraphrase.
* Wherever the runbook says "run", invoke the command exactly. Do not
  add flags, retries, or sleeps not listed.
* The MCP JSON-RPC calls in §6 are issued over the HTTP transport at
  `http://127.0.0.1:7801/mcp`. No bearer token is set.
* Tests run on macOS (Darwin) with Homebrew. Linux paths are noted
  where they differ.

## 1. Prerequisites

Run each check. If a check fails, run the install command listed.

| Check                                  | Install if missing                          |
| -------------------------------------- | ------------------------------------------- |
| `command -v cargo`                     | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh -s -- -y` |
| `command -v jq`                        | `brew install jq`                           |
| `command -v curl`                      | preinstalled on macOS                       |
| `command -v samtools`                  | `brew install samtools`                     |
| `python3 -c "import pysam"`            | `python3 -m pip install --user pysam`       |
| `python3 -c "import numpy"`            | `python3 -m pip install --user numpy`       |

If `pip install --user pysam` fails on macOS due to PEP 668, create a
venv at `/tmp/openmemory-reich-demo/.venv` first (covered in §3 below)
and run `pip install pysam numpy` inside it.

## 2. Build a fresh `openmemory` binary

The currently-installed binary at `~/.cargo/bin/openmemory` is `0.2.0`
and does not expose the `watch` subcommand the demo requires. Build
from the workspace.

```bash
cargo build --release --bin openmemory
```

This produces `/Users/rjow/open-memory/target/release/openmemory`.
Verify it has `watch`:

```bash
./target/release/openmemory watch --help | head -1
```

Expected output starts with `Start a foreground filesystem watcher` or
similar. If you see `error: unrecognized subcommand`, the build did
not include the `watch` feature. Rebuild with:

```bash
cargo build --release --bin openmemory --features watch
```

Export a shorthand for the rest of this runbook:

```bash
export OM=/Users/rjow/open-memory/target/release/openmemory
```

## 3. Create the demo workspace

```bash
export DEMO_ROOT=/tmp/openmemory-reich-demo
rm -rf "$DEMO_ROOT"
mkdir -p "$DEMO_ROOT"/{labshare/bin,labshare/protocols,labshare/notebooks,labshare/data/aadr/v54.1,labshare/data/panels,labshare/bams/2026-05,labshare/results/sex,om-home,fixtures,artifacts}
```

Create the isolated openmemory home and the python venv that holds
`pysam`:

```bash
export OPENMEMORY_HOME="$DEMO_ROOT/om-home"
python3 -m venv "$DEMO_ROOT/.venv"
"$DEMO_ROOT/.venv/bin/pip" install --quiet pysam numpy
export PYBIN="$DEMO_ROOT/.venv/bin/python"
```

Initialize the openmemory data dir:

```bash
"$OM" --home "$OPENMEMORY_HOME" init
```

This writes `$OPENMEMORY_HOME/default/config.toml` and an empty SQLite
DB. From now on every `openmemory` invocation must include
`--home "$OPENMEMORY_HOME"`.

## 4. Generate the lab filesystem (`~/labshare`)

The lab "shares" we are simulating are a flat directory tree. We write
each file with exact contents. Do not paraphrase observations; the
rubric in PLAN.md §5 grades against the literal strings the agent
recalls.

### 4.1 `labshare/protocols/molecular-sex.md`

```bash
cat > "$DEMO_ROOT/labshare/protocols/molecular-sex.md" <<'EOF'
# Molecular sex determination protocol (lab standing protocol)

Adopted 2026-05-01. Supersedes the Skoglund 2013 0.075 cutoff.

## Formula

    Ry = n_reads_on_Y_SNPs / (n_reads_on_X_SNPs + n_reads_on_Y_SNPs)

## Thresholds (from Olalde et al. 2026, Rhine-Meuse)

- Female (`XX`): Ry < 0.03
- Male   (`XY`): Ry > 0.32

## Minimum coverage floor

- Require n_reads_on_X_SNPs + n_reads_on_Y_SNPs >= 200.
- Below that, do not emit a call. Output the literal token
  `low_coverage` in the `sex_call` column and `NA` in the `Ry` column.

## Ambiguity convention (lab-invented; not in Olalde 2026)

Samples with 0.03 <= Ry <= 0.32 are reported as:

- `XX?` if Ry < 0.15
- `XY?` otherwise

## Read-counting strategy

Count any read overlapping a panel position. The canonical recipe is:

    samtools view -c -q 30 -L <bed> <bam>

We do NOT use strict base coverage; see protocols/bam-readcount.md for
the rationale.
EOF
```

### 4.2 `labshare/protocols/1240K-panel-layout.md`

```bash
cat > "$DEMO_ROOT/labshare/protocols/1240K-panel-layout.md" <<'EOF'
# 1240K panel layout

The EIGENSTRAT `.snp` file at `data/panels/1240K.snp` has one row per
SNP. Column 2 is the chromosome, encoded numerically:

- Autosomes: 1 through 22
- X: 23
- Y: 24
- mtDNA: 90

To derive a BED for chr-X positions only:

    awk '$2==23 {print "chrX\t"$4-1"\t"$4}' data/panels/1240K.snp \
        > data/panels/1240K_chrX.bed

Same recipe with `$2==24` and `chrY` for the Y panel.

The committed bed files are:

- data/panels/1240K_chrX.bed
- data/panels/1240K_chrY.bed
EOF
```

### 4.3 `labshare/protocols/bam-readcount.md`

```bash
cat > "$DEMO_ROOT/labshare/protocols/bam-readcount.md" <<'EOF'
# Read counting against a BED panel

Canonical command:

    samtools view -c -q 30 -L $BED $BAM

Flags:

- `-c` returns a count, not the reads themselves.
- `-q 30` enforces minimum mapping quality 30. Below that, we trust
  neither the alignment nor the position.
- `-L $BED` restricts to reads overlapping any interval in $BED. Note
  this is "any overlap", not strict base coverage. We picked any-
  overlap for tractability; switching to bedtools coverage gave the
  same calls on the 2024 Hazendonk batch within rounding error and
  was 4x slower.

Do not use `bedtools coverage -counts` here; it counts per-interval
and we want per-read.
EOF
```

### 4.4 `labshare/notebooks/2026-05-02.md`

```bash
cat > "$DEMO_ROOT/labshare/notebooks/2026-05-02.md" <<EOF
# 2026-05-02

Running sex calls on the Rhine-Meuse batch. BAMs are in
\`$DEMO_ROOT/labshare/bams/2026-05/\`. Output TSV goes to
\`$DEMO_ROOT/labshare/results/sex/\`.

Reuse \`bin/count_reads_per_pos.sh\` for the samtools wrapper so the
flags stay consistent with last month's batch.

Thresholds are the Olalde 2026 numbers (0.03 / 0.32), as recorded in
the 2026-05-01 decision note. The Skoglund 0.075 cutoff is deprecated;
see notebook 2024-11-18 for the deprecation rationale.
EOF
```

### 4.5 `labshare/notebooks/2024-11-18.md` (stale on purpose)

```bash
cat > "$DEMO_ROOT/labshare/notebooks/2024-11-18.md" <<'EOF'
# 2024-11-18

Sex-call sanity check on the Hazendonk batch.

Currently using the Skoglund 2013 cutoff: female if Ry < 0.075. Three
samples land in the gap between 0.075 and 0.30; flagging for review.

NOTE 2026-05-01: this threshold has been retired. The Olalde 2026
Rhine-Meuse paper uses 0.03 / 0.32 and we have adopted those as the
lab standard. Keep this note for historical context only.
EOF
```

### 4.6 `labshare/bin/count_reads_per_pos.sh`

```bash
cat > "$DEMO_ROOT/labshare/bin/count_reads_per_pos.sh" <<'EOF'
#!/usr/bin/env bash
# count_reads_per_pos.sh — canonicalised samtools wrapper.
# Usage: count_reads_per_pos.sh <bed> <bam>
set -euo pipefail
BED="$1"
BAM="$2"
samtools view -c -q 30 -L "$BED" "$BAM"
EOF
chmod +x "$DEMO_ROOT/labshare/bin/count_reads_per_pos.sh"
```

### 4.7 `labshare/bin/classify_sex.py`

```bash
cat > "$DEMO_ROOT/labshare/bin/classify_sex.py" <<'EOF'
#!/usr/bin/env python3
"""classify_sex.py — tiny helper. Takes nX and nY on stdin or argv
and emits the lab's sex call. Implements the lab protocol exactly:
female < 0.03, male > 0.32, low_coverage if nX+nY < 200, with
ambiguity bucket XX? / XY? around 0.15."""
import sys


def call(nx: int, ny: int) -> tuple[str, str]:
    total = nx + ny
    if total < 200:
        return ("NA", "low_coverage")
    ry = ny / total
    if ry < 0.03:
        return (f"{ry:.4f}", "XX")
    if ry > 0.32:
        return (f"{ry:.4f}", "XY")
    if ry < 0.15:
        return (f"{ry:.4f}", "XX?")
    return (f"{ry:.4f}", "XY?")


if __name__ == "__main__":
    nx, ny = int(sys.argv[1]), int(sys.argv[2])
    ry, label = call(nx, ny)
    print(f"{ry}\t{label}")
EOF
chmod +x "$DEMO_ROOT/labshare/bin/classify_sex.py"
```

### 4.8 `labshare/data/aadr/v54.1/README.md`

```bash
cat > "$DEMO_ROOT/labshare/data/aadr/v54.1/README.md" <<'EOF'
# AADR v54.1 (public release, 2023-04)

EIGENSTRAT files: `v54.1_1240K_public.{geno,snp,ind}`.

The `.snp` file uses numerical chromosome codes in column 2:
X=23, Y=24, mtDNA=90, autosomes 1-22.

The 1240K panel was published in Mathieson et al. 2015 and captures
1,233,013 targeted SNPs. This release supersedes v52.2 (2022).
EOF
```

### 4.9 Synthetic SNP panel + BED files

The .snp file only needs enough rows for `samtools view -L` to find
reads. We synthesise 200 X positions and 200 Y positions on a tiny
toy reference (`chrX` and `chrY`, each 1 Mb).

Write the BED files directly:

```bash
python3 - <<'EOF'
import os, random
root = os.environ["DEMO_ROOT"]
random.seed(42)

x_positions = sorted(random.sample(range(1000, 999000), 200))
y_positions = sorted(random.sample(range(1000, 999000), 200))

with open(f"{root}/labshare/data/panels/1240K.snp", "w") as f:
    for i, p in enumerate(x_positions):
        f.write(f"rsX{i}\t23\t0.0\t{p}\tA\tG\n")
    for i, p in enumerate(y_positions):
        f.write(f"rsY{i}\t24\t0.0\t{p}\tA\tG\n")

with open(f"{root}/labshare/data/panels/1240K_chrX.bed", "w") as f:
    for p in x_positions:
        f.write(f"chrX\t{p-1}\t{p}\n")

with open(f"{root}/labshare/data/panels/1240K_chrY.bed", "w") as f:
    for p in y_positions:
        f.write(f"chrY\t{p-1}\t{p}\n")
EOF
```

### 4.10 `paper-*.txt` fixtures (used in §6 to seed the free-text index)

```bash
cat > "$DEMO_ROOT/fixtures/paper-olalde-2026-methods-sex.txt" <<'EOF'
Olalde et al. 2026, "Genome-wide ancient DNA from the Lower Rhine-Meuse
area shows late persistence of forager ancestry shaped Bell Beaker
expansion", Nature, doi:10.1038/s41586-026-10111-8, Methods, Molecular
sex determination:

Genetic sex was determined by calculating the ratio of reads mapped to
Y-chromosome SNP positions to the total reads mapped to sex-chromosome
SNP positions. Individuals with a ratio of <0.03 were classified as
female, while those with a ratio >0.32 were classified as male.
EOF

cat > "$DEMO_ROOT/fixtures/paper-olalde-2026-methods-bioinfo.txt" <<'EOF'
Olalde et al. 2026, Methods, Bioinformatics:

Reads were adapter-trimmed and merged with SeqPrep (15 bp overlap
rule), aligned to the GRCh37/hg19 reference with bwa 0.7.15 (aln/
samse mode), filtered to mapping quality >= 30, and deduplicated with
samtools rmdup. Capture data targeted the 1240K SNP panel of
Mathieson et al. 2015.
EOF

cat > "$DEMO_ROOT/fixtures/paper-skoglund-2013-sex.txt" <<'EOF'
Skoglund et al. 2013, "Accurate sex identification of ancient human
remains using DNA shotgun sequencing", J. Archaeol. Sci. The original
Ry method. Skoglund proposed a cutoff of Ry < 0.075 for female calls.
Modern protocols (Olalde 2026 et seq.) tighten this to Ry < 0.03 to
reduce false-female calls on contaminated low-coverage libraries.
EOF

cat > "$DEMO_ROOT/fixtures/decision-sex-thresholds-olalde-2026.md" <<'EOF'
Decision: adopt the Olalde 2026 Rhine-Meuse sex-call thresholds.

Date: 2026-05-01
Owner: Raymond
Supersedes: the Skoglund 2013 0.075 cutoff used through 2024-11.

Going forward, our standing protocol uses Ry < 0.03 for female and
Ry > 0.32 for male. Samples in the gap are reported with the lab's
XX? / XY? ambiguity convention. See protocols/molecular-sex.md.
EOF

cat > "$DEMO_ROOT/fixtures/decision-min-reads-sex-200.md" <<'EOF'
Decision: minimum 200 reads on sex-chromosome SNP positions before
emitting a sex call. Below the floor, output the literal token
`low_coverage`.

Date: 2024-11-30
Rationale: the 2024 Hazendonk batch produced unstable Ry estimates
when fewer than ~100 reads landed on the panel. We picked 200 as a
conservative floor with comfortable margin.

Note: Olalde 2026 does not publish a floor for sex calls; this is a
lab-only decision and must be discovered through the protocol notes,
not the paper.
EOF

cat > "$DEMO_ROOT/fixtures/decision-wrong-panel-2023.md" <<'EOF'
Decision: do NOT use the Human Origins (HO) array for sex calls.

Date: 2023-08-12
The HO array targets ~600K SNPs but carries fewer than 2000 Y-chrom
positions, which yields unstable Ry below typical lab coverage. Sex
calls must use the 1240K capture panel.
EOF
```

These four `decision-*.md` and `paper-*.txt` files are not yet under
`labshare/`; the watcher does not see them. They are seeded via the
MCP `openmemory_index_text` tool in §6, which gives them custom
`paper://` and `decision://` URIs as PLAN.md §3.2 specifies.

## 5. Synthesise the BAM corpus

The pipeline must run real `samtools view -c -L`. Generate seven
sorted, indexed BAMs aligned to a tiny chrX/chrY reference. Each BAM
hits panel positions in a controlled ratio. The ground-truth manifest
is held out from the index.

### 5.1 `scripts/demo-reich/make_synthetic_bams.py`

```bash
mkdir -p scripts/demo-reich
cat > scripts/demo-reich/make_synthetic_bams.py <<'EOF'
#!/usr/bin/env python3
"""Emit synthetic BAMs at controlled Ry ratios for the Reich demo.

For each sample, sample `total_reads` positions from the 1240K X and Y
bed files, weighted so that
    n_y / (n_x + n_y) approximates the target Ry.
Each read is 50 bp, mapping-quality 60, on a toy hg19 header with
chrX and chrY of length 1 Mb each.

Reads are written sorted by coordinate and indexed via pysam.
"""
import os
import random
import sys
import pysam

ROOT = os.environ["DEMO_ROOT"]
BAM_DIR = f"{ROOT}/labshare/bams/2026-05"
BED_X = f"{ROOT}/labshare/data/panels/1240K_chrX.bed"
BED_Y = f"{ROOT}/labshare/data/panels/1240K_chrY.bed"

random.seed(20260501)

def load_positions(path):
    out = []
    with open(path) as f:
        for line in f:
            chrom, start, end = line.strip().split("\t")
            out.append((chrom, int(start)))
    return out

X_POS = load_positions(BED_X)
Y_POS = load_positions(BED_Y)

SAMPLES = [
    # (sample_id, target_ry, total_reads, expected_sex)
    ("I12091", 0.50, 500, "XY"),
    ("I12902", 0.48, 500, "XY"),
    ("I15651", 0.01, 500, "XX"),
    ("I33738", 0.49, 500, "XY"),
    ("I38442", 0.00, 500, "XX"),
    ("LOW01",  0.40, 80,  "low_coverage"),
    ("AMB01",  0.20, 500, "XY?"),
]

HEADER = {
    "HD": {"VN": "1.6", "SO": "coordinate"},
    "SQ": [
        {"SN": "chrX", "LN": 1_000_000},
        {"SN": "chrY", "LN": 1_000_000},
    ],
    "PG": [{"ID": "synthetic", "PN": "make_synthetic_bams.py", "VN": "1.0"}],
}

def emit_bam(sample_id, target_ry, total_reads):
    n_y = int(round(target_ry * total_reads))
    n_x = total_reads - n_y
    reads = []
    for i in range(n_x):
        chrom, pos = random.choice(X_POS)
        reads.append((chrom, pos, f"{sample_id}_X_{i}"))
    for i in range(n_y):
        chrom, pos = random.choice(Y_POS)
        reads.append((chrom, pos, f"{sample_id}_Y_{i}"))
    reads.sort(key=lambda r: (r[0], r[1]))

    tid_for = {"chrX": 0, "chrY": 1}
    out_path = f"{BAM_DIR}/{sample_id}.bam"
    with pysam.AlignmentFile(out_path, "wb", header=HEADER) as bam:
        for chrom, pos, qname in reads:
            a = pysam.AlignedSegment(bam.header)
            a.query_name = qname
            a.query_sequence = "A" * 50
            a.flag = 0
            a.reference_id = tid_for[chrom]
            a.reference_start = pos
            a.mapping_quality = 60
            a.cigar = [(0, 50)]  # 50M
            a.query_qualities = pysam.qualitystring_to_array("I" * 50)
            bam.write(a)
    pysam.index(out_path)

def write_manifest():
    path = f"{ROOT}/labshare/data/sample-manifest.tsv"
    with open(path, "w") as f:
        f.write("sample_id\ttarget_ry\texpected_sex\ttotal_reads\n")
        for sid, ry, n, sex in SAMPLES:
            f.write(f"{sid}\t{ry}\t{sex}\t{n}\n")
    print(f"wrote {path}")

if __name__ == "__main__":
    os.makedirs(BAM_DIR, exist_ok=True)
    for sid, ry, n, _sex in SAMPLES:
        emit_bam(sid, ry, n)
        print(f"emitted {sid}.bam", file=sys.stderr)
    write_manifest()
EOF
```

Run it:

```bash
DEMO_ROOT="$DEMO_ROOT" "$PYBIN" scripts/demo-reich/make_synthetic_bams.py
```

The manifest at `$DEMO_ROOT/labshare/data/sample-manifest.tsv` is
ground truth. It is intentionally placed in `data/`, not under a
watched protocol or notebook path, but `.tsv` IS in the default
watcher extension list, so we explicitly exclude `data/` from the
watch via the `--exts` whitelist in §7. (The watcher's `--exts` flag
is an inclusive whitelist; `tsv` is not in it.)

Sanity-check the BAMs:

```bash
samtools view -c -q 30 -L "$DEMO_ROOT/labshare/data/panels/1240K_chrX.bed" \
    "$DEMO_ROOT/labshare/bams/2026-05/I12091.bam"
samtools view -c -q 30 -L "$DEMO_ROOT/labshare/data/panels/1240K_chrY.bed" \
    "$DEMO_ROOT/labshare/bams/2026-05/I12091.bam"
```

Expected: both counts non-zero and within a few percent of 250/250
(target Ry = 0.50, total = 500).

## 6. Seed the knowledge graph and the free-text index

There are two stores to populate:

* The graph (entities + observations + relations) via `openmemory remember`.
* The free-text index under custom `paper://`, `protocol://`,
  `decision://`, `dataset-readme://` URIs via the MCP
  `openmemory_index_text` tool.

Watched files in `labshare/` are picked up automatically by the
watcher in §7 under `file://` URIs; do not re-seed them here.

### 6.1 Start the MCP HTTP server in the background

```bash
"$OM" --home "$OPENMEMORY_HOME" mcp --http 127.0.0.1:7801 \
    > "$DEMO_ROOT/artifacts/mcp.log" 2>&1 &
echo $! > "$DEMO_ROOT/artifacts/mcp.pid"
# wait until /healthz answers
until curl -sf http://127.0.0.1:7801/healthz > /dev/null; do sleep 0.2; done
```

### 6.2 Seed the graph (§3.1 of PLAN.md)

These are CLI `remember` calls; the URIs in `--source` are just audit
tags, not index URIs.

```bash
"$OM" --home "$OPENMEMORY_HOME" remember "AADR_v54_1" \
    --entity-type fact --source "demo-seed" \
    --observation "Path: $DEMO_ROOT/labshare/data/aadr/v54.1" \
    --observation "Format: EIGENSTRAT (.geno, .snp, .ind)" \
    --observation "Published 2023-04; supersedes v52.2"

"$OM" --home "$OPENMEMORY_HOME" remember "SNP_panel_1240K" \
    --entity-type fact --source "demo-seed" \
    --observation "1,233,013 targeted SNPs" \
    --observation "Chromosome encoding in .snp column 2: X=23, Y=24, autosomes 1-22" \
    --observation "Capture reagent described in Mathieson et al. 2015"

"$OM" --home "$OPENMEMORY_HOME" remember "SNP_panel_1240K_X" \
    --entity-type fact --source "demo-seed" \
    --observation "Path: $DEMO_ROOT/labshare/data/panels/1240K_chrX.bed" \
    --observation "Derived from 1240K .snp by awk '\$2==23' then BED conversion" \
    --relation "derived_from=SNP_panel_1240K:fact"

"$OM" --home "$OPENMEMORY_HOME" remember "SNP_panel_1240K_Y" \
    --entity-type fact --source "demo-seed" \
    --observation "Path: $DEMO_ROOT/labshare/data/panels/1240K_chrY.bed" \
    --relation "derived_from=SNP_panel_1240K:fact"

"$OM" --home "$OPENMEMORY_HOME" remember "hg19" \
    --entity-type fact --source "demo-seed" \
    --observation "Reference genome used for all Lower Rhine-Meuse BAMs" \
    --observation "Olalde 2026 Methods, Bioinformatics section"

"$OM" --home "$OPENMEMORY_HOME" remember "samtools" \
    --entity-type tool --source "demo-seed" \
    --observation "Used for read counting" \
    --observation "Lab default: samtools view -c -q 30 -L <bed> <bam> to count reads overlapping a position set"

"$OM" --home "$OPENMEMORY_HOME" remember "bwa_0_7_15" \
    --entity-type tool --source "demo-seed" \
    --observation "Aligner used by the lab pipeline (bwa samse mode)"

"$OM" --home "$OPENMEMORY_HOME" remember "SeqPrep_v1_1" \
    --entity-type tool --source "demo-seed" \
    --observation "Adapter merging, 15-bp overlap rule"

"$OM" --home "$OPENMEMORY_HOME" remember "MolecularSexProtocol" \
    --entity-type preference --source "demo-seed" \
    --observation "Ry = n_reads_on_Y_SNPs / (n_reads_on_X_SNPs + n_reads_on_Y_SNPs)" \
    --observation "Threshold female: Ry < 0.03" \
    --observation "Threshold male: Ry > 0.32" \
    --observation "Min reads on sex chromosomes: 200. Below that, do not call; output low_coverage." \
    --observation "Ambiguous (0.03 <= Ry <= 0.32): XX? if Ry < 0.15 else XY?. Lab convention; not in Olalde 2026." \
    --observation "Counting strategy: any read overlapping a panel position (samtools -L), not strict base coverage." \
    --relation "cites=Olalde2026_RhineMeuse:fact" \
    --relation "supersedes=Skoglund2013_SexProtocol:fact"

"$OM" --home "$OPENMEMORY_HOME" remember "Olalde2026_RhineMeuse" \
    --entity-type fact --source "demo-seed" \
    --observation "Nature 2026, doi:10.1038/s41586-026-10111-8" \
    --observation "Source of the 0.03 / 0.32 thresholds for sex calls"

"$OM" --home "$OPENMEMORY_HOME" remember "Skoglund2013_SexProtocol" \
    --entity-type fact --source "demo-seed" \
    --observation "Original Ry method; used a 0.075 cutoff for female calls" \
    --observation "Deprecated by our lab on 2026-05-01 in favour of Olalde 2026 thresholds"

"$OM" --home "$OPENMEMORY_HOME" remember "LabPipelineDefaults" \
    --entity-type preference --source "demo-seed" \
    --observation "min_reads_sex: 200" \
    --observation "output_tsv_columns: sample_id, n_reads_X, n_reads_Y, Ry, sex_call" \
    --observation "results_dir: $DEMO_ROOT/labshare/results/sex" \
    --observation "input_bam_dir for current batch: $DEMO_ROOT/labshare/bams/2026-05"

for sid in I12091 I12902 I15651 I33738 I38442; do
    "$OM" --home "$OPENMEMORY_HOME" remember "$sid" \
        --entity-type fact --source "demo-seed" \
        --observation "Sample in the Lower Rhine-Meuse batch reported in Olalde 2026" \
        --relation "aligned_to=hg19:fact" \
        --relation "enriched_with=SNP_panel_1240K:fact" \
        --relation "reported_in=Olalde2026_RhineMeuse:fact"
done

"$OM" --home "$OPENMEMORY_HOME" remember "Raymond" \
    --entity-type person --source "demo-seed" \
    --observation "Maintainer of the molecular-sex protocol and the openmemory project" \
    --relation "maintains=MolecularSexProtocol:preference" \
    --relation "maintains=openmemory:project"
```

### 6.3 Entity-normalization stress (PLAN.md §6)

Four near-identical names that the fuzzy resolver should merge:

```bash
for name in "1240K capture" "1240K" "1,240K SNP panel" "1240K_capture"; do
    "$OM" --home "$OPENMEMORY_HOME" remember "$name" \
        --entity-type fact --source "demo-seed" \
        --observation "Alias variant for the 1240K capture panel" \
        --json
done
```

Verify the alias merge fired at least once: inspect the JSON output
of the calls above for a `"normalized"` field. (PLAN.md §6 calls this
out as a feature being stressed; non-merge is not fatal but logs as
a warning in §9 below.)

### 6.4 Seed the free-text index (§3.2 of PLAN.md)

Define a helper for JSON-RPC calls:

```bash
mcp_call() {
    local method="$1" params="$2"
    curl -s -X POST http://127.0.0.1:7801/mcp \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg m "$method" --argjson p "$params" \
              '{jsonrpc:"2.0",id:1,method:$m,params:$p}')"
}

index_text() {
    local uri="$1" path="$2"
    local text
    text=$(cat "$path")
    mcp_call tools/call "$(jq -n --arg u "$uri" --arg t "$text" \
        '{name:"openmemory_index_text",arguments:{uri:$u,text:$t}}')" \
        > /dev/null
    echo "indexed $uri"
}

index_text "paper://olalde-2026-rhine-meuse#methods-sex" \
    "$DEMO_ROOT/fixtures/paper-olalde-2026-methods-sex.txt"
index_text "paper://olalde-2026-rhine-meuse#methods-bioinfo" \
    "$DEMO_ROOT/fixtures/paper-olalde-2026-methods-bioinfo.txt"
index_text "paper://skoglund-2013-sex-determination" \
    "$DEMO_ROOT/fixtures/paper-skoglund-2013-sex.txt"
index_text "decision://sex-thresholds-olalde-2026" \
    "$DEMO_ROOT/fixtures/decision-sex-thresholds-olalde-2026.md"
index_text "decision://min-reads-sex-200" \
    "$DEMO_ROOT/fixtures/decision-min-reads-sex-200.md"
index_text "decision://wrong-panel-2023" \
    "$DEMO_ROOT/fixtures/decision-wrong-panel-2023.md"
index_text "protocol://molecular-sex" \
    "$DEMO_ROOT/labshare/protocols/molecular-sex.md"
index_text "protocol://1240K-panel-layout" \
    "$DEMO_ROOT/labshare/protocols/1240K-panel-layout.md"
index_text "protocol://bam-readcount" \
    "$DEMO_ROOT/labshare/protocols/bam-readcount.md"
index_text "dataset-readme://1240K" \
    "$DEMO_ROOT/labshare/data/aadr/v54.1/README.md"
```

### 6.5 Lexical-red-herring seed (PLAN.md §6)

A note that mentions "sex chromosomes" only in an unrelated mtDNA
context. Vector recall must out-rank pure keyword overlap on this one:

```bash
cat > "$DEMO_ROOT/fixtures/paper-mtdna-redherring.txt" <<'EOF'
Mitochondrial DNA is inherited strictly maternally and is unaffected
by sex chromosomes; nevertheless many ancient-DNA pipelines emit
mtDNA QC reports alongside sex-chromosome coverage statistics. This
note is about mtDNA contamination thresholds, not sex determination.
EOF
index_text "paper://mtdna-contamination" \
    "$DEMO_ROOT/fixtures/paper-mtdna-redherring.txt"
```

### 6.6 Stale-threshold seed (PLAN.md §6)

A 2024-dated observation pinning the deprecated Skoglund 0.075 cutoff.
The fresh 2026-05-01 decision must out-score it under Ebbinghaus decay.

```bash
"$OM" --home "$OPENMEMORY_HOME" remember "MolecularSexProtocol" \
    --entity-type preference --source "stale-2024-note" \
    --observation "Historical: through 2024-11 the lab used the Skoglund 0.075 cutoff for female calls." \
    --confidence 0.6
```

## 7. Start the filesystem watcher

The watcher must run in the background for the duration of the demo so
that any agent-visible changes to `labshare/protocols/`, `notebooks/`,
or `bin/` are reflected. Extensions are restricted to `md,sh,py` per
PLAN.md §3.3.

```bash
"$OM" --home "$OPENMEMORY_HOME" watch "$DEMO_ROOT/labshare" \
    --exts md,sh,py \
    > "$DEMO_ROOT/artifacts/watch.log" 2>&1 &
echo $! > "$DEMO_ROOT/artifacts/watch.pid"
# wait until initial scan completes (look for "watching" log line)
until grep -q "openmemory watch: watching" "$DEMO_ROOT/artifacts/watch.log" 2>/dev/null; do
    sleep 0.2
done
sleep 2  # let the initial scan drain
```

Sanity-check the index has the watched markdown:

```bash
curl -s -X POST http://127.0.0.1:7801/mcp \
    -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/call",
         "params":{"name":"openmemory_search",
                   "arguments":{"query":"samtools view -c -L",
                                "uri_prefix":"file://"}}}' \
    | jq -r '.result.content[0].text' | head -20
```

Expected: at least one hit on `bam-readcount.md` or
`count_reads_per_pos.sh`.

## 8. Run the agent

Two equivalent paths. Pick one.

### 8.1 Claude Code (preferred for the demo writeup)

Register the running MCP server with Claude Code:

```bash
claude mcp add --transport http openmemory-demo http://127.0.0.1:7801/mcp
```

Open Claude Code (`claude`), confirm the `openmemory-demo` server is
listed under `/mcp`, then paste the contents of `PROMPT.md`. Capture
the agent's full transcript to `$DEMO_ROOT/artifacts/transcript.md`.
The agent must emit a single bash script (the molecular-sex pipeline)
inside a fenced code block. Extract it:

```bash
awk '/^```bash$/{flag=1;next}/^```$/{flag=0}flag' \
    "$DEMO_ROOT/artifacts/transcript.md" \
    > "$DEMO_ROOT/artifacts/molecular_sex.sh"
chmod +x "$DEMO_ROOT/artifacts/molecular_sex.sh"
```

### 8.2 Headless (CI-style replay)

If no interactive agent is available, drive the same prompt through
the `claude` CLI in non-interactive mode:

```bash
claude --print --allowed-tools "mcp__openmemory-demo__*" \
    < PROMPT.md > "$DEMO_ROOT/artifacts/transcript.md"
```

Then extract the script exactly as in §8.1.

When the agent finishes, leave the MCP server and the watcher running
for the grading step.

## 9. Grade the rubric

Score the ten checks in PLAN.md §5 against the extracted script.

```bash
SCRIPT="$DEMO_ROOT/artifacts/molecular_sex.sh"
score=0
check() {
    local name="$1" cond="$2"
    if eval "$cond"; then
        echo "PASS $name"; score=$((score+1))
    else
        echo "FAIL $name"
    fi
}

check "01 Ry = nY/(nX+nY)"          "grep -Eq 'nY[[:space:]]*/[[:space:]]*\([[:space:]]*nX[[:space:]]*\+[[:space:]]*nY|/\(.*total' \"\$SCRIPT\""
check "02 female threshold 0.03"     "grep -Eq '0\\.03' \"\$SCRIPT\" && ! grep -Eq '0\\.075' \"\$SCRIPT\""
check "03 male threshold 0.32"       "grep -Eq '0\\.32' \"\$SCRIPT\""
check "04 uses 1240K chrX/chrY bed"  "grep -q '1240K_chrX.bed' \"\$SCRIPT\" && grep -q '1240K_chrY.bed' \"\$SCRIPT\""
check "05 samtools with -q 30"       "grep -Eq 'samtools view .*-q[[:space:]]*30' \"\$SCRIPT\""
check "06 200-read floor"            "grep -Eq '\\b200\\b' \"\$SCRIPT\""
check "07 TSV columns"               "grep -Eq 'sample_id.*n_reads_X.*n_reads_Y.*Ry.*sex_call' \"\$SCRIPT\""
check "08 results dir"               "grep -q 'labshare/results/sex' \"\$SCRIPT\""
check "09 bam dir"                   "grep -q 'labshare/bams/2026-05' \"\$SCRIPT\""
check "10 ambiguous bucket"          "grep -Eq 'XX\\?|XY\\?|ambiguous' \"\$SCRIPT\""

echo "RUBRIC SCORE: $score / 10" | tee "$DEMO_ROOT/artifacts/rubric.txt"
```

Pass threshold: `>= 7`. Below `5` means the recall layer failed and
the MCP transcript at `$DEMO_ROOT/artifacts/transcript.md` should be
inspected.

## 10. Bonus end-to-end check

Run the agent's script and diff against the ground-truth manifest:

```bash
bash "$DEMO_ROOT/artifacts/molecular_sex.sh"

OUT="$DEMO_ROOT/labshare/results/sex/rhine_meuse_2026-05.tsv"
test -f "$OUT" || { echo "FAIL: expected TSV at $OUT"; exit 1; }

python3 - <<EOF
import csv, os
root = os.environ["DEMO_ROOT"]
truth = {row["sample_id"]: row["expected_sex"]
         for row in csv.DictReader(
             open(f"{root}/labshare/data/sample-manifest.tsv"), delimiter="\t")}
got = {row["sample_id"]: row["sex_call"]
       for row in csv.DictReader(
           open(f"{root}/labshare/results/sex/rhine_meuse_2026-05.tsv"),
           delimiter="\t")}

passes = fails = 0
for sid, expected in truth.items():
    # AMB01 (ambiguous on purpose) accepts either XY? or XX?.
    if sid == "AMB01":
        ok = got.get(sid) in {"XY?", "XX?"}
    else:
        ok = got.get(sid) == expected
    print(f"{sid}: expected={expected!r:14} got={got.get(sid)!r:14} {'OK' if ok else 'MISMATCH'}")
    passes += ok
    fails += not ok
print(f"\nEnd-to-end: {passes}/{len(truth)} match")
EOF
```

Pass criterion: all 5 non-ambiguous, non-low-coverage samples match;
`LOW01` reports `low_coverage`; `AMB01` reports one of `XX?` / `XY?`.

## 11. Teardown

```bash
kill "$(cat $DEMO_ROOT/artifacts/watch.pid)" 2>/dev/null || true
kill "$(cat $DEMO_ROOT/artifacts/mcp.pid)"   2>/dev/null || true
# optional, to reset between runs:
# rm -rf "$DEMO_ROOT"
claude mcp remove openmemory-demo 2>/dev/null || true
```

## 12. Reproducibility checklist

Before declaring the demo green, verify each of these holds:

- [ ] `cargo build --release --bin openmemory` succeeded with the
      `watch` feature on (default).
- [ ] `$DEMO_ROOT/labshare/bams/2026-05/` contains 7 sorted, indexed
      BAMs (`*.bam` and `*.bam.bai`).
- [ ] `$DEMO_ROOT/labshare/data/sample-manifest.tsv` exists and has 7
      rows.
- [ ] `openmemory --home "$OPENMEMORY_HOME" list-entities` reports
      at least 15 entities and one of them is `MolecularSexProtocol`
      with at least 6 observations.
- [ ] The MCP HTTP server responds 200 on `/healthz`.
- [ ] `tools/call openmemory_search` with
      `{query:"sex determination Ry", uri_prefix:"protocol://"}`
      returns `protocol://molecular-sex` in the top 3 results.
- [ ] The watcher's stderr log shows non-zero `inserted` count after
      the initial scan.
- [ ] `$DEMO_ROOT/artifacts/molecular_sex.sh` exists, is non-empty,
      and is executable.
- [ ] Rubric score in `$DEMO_ROOT/artifacts/rubric.txt` is `>= 7 / 10`.
- [ ] End-to-end TSV diff in §10 prints `End-to-end: 7/7 match`
      (allowing AMB01's ambiguity).

When all boxes are checked, the demo is complete. Archive
`$DEMO_ROOT/artifacts/` for the writeup at
`docs/demos/reich-sex-determination.md` (PLAN.md §8 deliverable 7).
