#!/usr/bin/env bash
# Build the synthetic labshare tree for the Reich molecular-sex demo.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEMO_ROOT="${DEMO_ROOT:-/tmp/openmemory-reich-demo}"
FIXTURES="$REPO_ROOT/tests/fixtures/reich-demo"

mkdir -p \
    "$DEMO_ROOT/labshare/bin" \
    "$DEMO_ROOT/labshare/protocols" \
    "$DEMO_ROOT/labshare/notebooks" \
    "$DEMO_ROOT/labshare/data/aadr/v54.1" \
    "$DEMO_ROOT/labshare/data/panels" \
    "$DEMO_ROOT/labshare/bams/2026-05" \
    "$DEMO_ROOT/labshare/results/sex" \
    "$DEMO_ROOT/fixtures" \
    "$DEMO_ROOT/artifacts"

cp "$FIXTURES/protocol-molecular-sex.md" \
    "$DEMO_ROOT/labshare/protocols/molecular-sex.md"
cp "$FIXTURES/protocol-1240K-panel-layout.md" \
    "$DEMO_ROOT/labshare/protocols/1240K-panel-layout.md"
cp "$FIXTURES/protocol-bam-readcount.md" \
    "$DEMO_ROOT/labshare/protocols/bam-readcount.md"
cp "$FIXTURES/notebook-2024-11-18.md" \
    "$DEMO_ROOT/labshare/notebooks/2024-11-18.md"
sed "s#__DEMO_ROOT__#$DEMO_ROOT#g" \
    "$FIXTURES/notebook-2026-05-02.md.in" \
    > "$DEMO_ROOT/labshare/notebooks/2026-05-02.md"
cp "$FIXTURES/aadr-v54.1-readme.md" \
    "$DEMO_ROOT/labshare/data/aadr/v54.1/README.md"

cp "$SCRIPT_DIR/labshare-bin/count_reads_per_pos.sh" \
    "$DEMO_ROOT/labshare/bin/count_reads_per_pos.sh"
cp "$SCRIPT_DIR/labshare-bin/classify_sex.py" \
    "$DEMO_ROOT/labshare/bin/classify_sex.py"
chmod +x \
    "$DEMO_ROOT/labshare/bin/count_reads_per_pos.sh" \
    "$DEMO_ROOT/labshare/bin/classify_sex.py"

cp "$FIXTURES"/paper-*.txt "$DEMO_ROOT/fixtures/"
cp "$FIXTURES"/decision-*.md "$DEMO_ROOT/fixtures/"

DEMO_ROOT="$DEMO_ROOT" python3 - <<'PY'
import os
import random

root = os.environ["DEMO_ROOT"]
random.seed(42)

x_positions = sorted(random.sample(range(1000, 999000), 200))
y_positions = sorted(random.sample(range(1000, 999000), 200))

panel_dir = f"{root}/labshare/data/panels"
with open(f"{panel_dir}/1240K.snp", "w") as f:
    for i, p in enumerate(x_positions):
        f.write(f"rsX{i}\t23\t0.0\t{p}\tA\tG\n")
    for i, p in enumerate(y_positions):
        f.write(f"rsY{i}\t24\t0.0\t{p}\tA\tG\n")

with open(f"{panel_dir}/1240K_chrX.bed", "w") as f:
    for p in x_positions:
        f.write(f"chrX\t{p - 1}\t{p}\n")

with open(f"{panel_dir}/1240K_chrY.bed", "w") as f:
    for p in y_positions:
        f.write(f"chrY\t{p - 1}\t{p}\n")
PY

printf 'built Reich demo labshare at %s/labshare\n' "$DEMO_ROOT"
