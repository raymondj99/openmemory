#!/usr/bin/env bash
# Grade an agent-emitted molecular-sex script and run it end to end.
set -euo pipefail

DEMO_ROOT="${DEMO_ROOT:-/tmp/openmemory-reich-demo}"
SCRIPT="${1:-$DEMO_ROOT/artifacts/molecular_sex.sh}"
ARTIFACTS="$DEMO_ROOT/artifacts"
export DEMO_ROOT

if [[ ! -s "$SCRIPT" ]]; then
    printf 'error: script is missing or empty: %s\n' "$SCRIPT" >&2
    exit 1
fi

score=0
check() {
    local name="$1" cond="$2"
    if eval "$cond"; then
        printf 'PASS %s\n' "$name"
        score=$((score + 1))
    else
        printf 'FAIL %s\n' "$name"
    fi
}

check "01 Ry = nY/(nX+nY)" \
    "grep -Eq 'nY[[:space:]]*/[[:space:]]*\\([[:space:]]*nX[[:space:]]*\\+[[:space:]]*nY|/\\(.*total' \"\$SCRIPT\""
check "02 female threshold 0.03" \
    "grep -Eq '0\\.03' \"\$SCRIPT\" && ! grep -Eq '0\\.075' \"\$SCRIPT\""
check "03 male threshold 0.32" \
    "grep -Eq '0\\.32' \"\$SCRIPT\""
check "04 uses 1240K chrX/chrY bed" \
    "grep -q '1240K_chrX.bed' \"\$SCRIPT\" && grep -q '1240K_chrY.bed' \"\$SCRIPT\""
check "05 samtools with -q 30" \
    "grep -Eq 'samtools view .*-q[[:space:]]*30' \"\$SCRIPT\""
check "06 200-read floor" \
    "grep -Eq '\\b200\\b' \"\$SCRIPT\""
check "07 TSV columns" \
    "grep -Eq 'sample_id.*n_reads_X.*n_reads_Y.*Ry.*sex_call' \"\$SCRIPT\""
check "08 results dir" \
    "grep -q 'labshare/results/sex' \"\$SCRIPT\""
check "09 bam dir" \
    "grep -q 'labshare/bams/2026-05' \"\$SCRIPT\""
check "10 ambiguous bucket" \
    "grep -Eq 'XX\\?|XY\\?|ambiguous' \"\$SCRIPT\""

mkdir -p "$ARTIFACTS"
printf 'RUBRIC SCORE: %s / 10\n' "$score" | tee "$ARTIFACTS/rubric.txt"

bash "$SCRIPT"

OUT="$DEMO_ROOT/labshare/results/sex/rhine_meuse_2026-05.tsv"
if [[ ! -f "$OUT" ]]; then
    printf 'FAIL: expected TSV at %s\n' "$OUT" >&2
    exit 1
fi

python3 - <<'PY'
import csv
import os
import sys

root = os.environ["DEMO_ROOT"]
truth_path = f"{root}/labshare/data/sample-manifest.tsv"
out_path = f"{root}/labshare/results/sex/rhine_meuse_2026-05.tsv"

with open(truth_path) as f:
    truth = {
        row["sample_id"]: row["expected_sex"]
        for row in csv.DictReader(f, delimiter="\t")
    }

with open(out_path) as f:
    got = {
        row["sample_id"]: row["sex_call"]
        for row in csv.DictReader(f, delimiter="\t")
    }

passes = 0
fails = 0
for sid, expected in truth.items():
    if sid == "AMB01":
        ok = got.get(sid) in {"XY?", "XX?"}
    else:
        ok = got.get(sid) == expected
    print(
        f"{sid}: expected={expected!r:14} got={got.get(sid)!r:14} "
        f"{'OK' if ok else 'MISMATCH'}"
    )
    passes += int(ok)
    fails += int(not ok)

print(f"\nEnd-to-end: {passes}/{len(truth)} match")
if fails:
    sys.exit(1)
PY
