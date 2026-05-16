#!/usr/bin/env bash
# Reference answer for local smoke tests. The demo agent should derive
# these constants from openmemory, not from this file.
set -euo pipefail

DEMO_ROOT="${DEMO_ROOT:-/tmp/openmemory-reich-demo}"
BAM_DIR="$DEMO_ROOT/labshare/bams/2026-05"
BED_X="$DEMO_ROOT/labshare/data/panels/1240K_chrX.bed"
BED_Y="$DEMO_ROOT/labshare/data/panels/1240K_chrY.bed"
OUT="$DEMO_ROOT/labshare/results/sex/rhine_meuse_2026-05.tsv"
MIN_READS=200

mkdir -p "$(dirname "$OUT")"
printf "sample_id\tn_reads_X\tn_reads_Y\tRy\tsex_call\n" > "$OUT"

for bam in "$BAM_DIR"/*.bam; do
    sid="$(basename "$bam" .bam)"
    nX="$(samtools view -c -q 30 -L "$BED_X" "$bam")"
    nY="$(samtools view -c -q 30 -L "$BED_Y" "$bam")"
    total=$((nX + nY))
    if (( total < MIN_READS )); then
        ry="NA"
        call="low_coverage"
    else
        ry="$(awk -v nY="$nY" -v total="$total" 'BEGIN{printf "%.4f", nY/(total)}')"
        call="$(awk -v r="$ry" 'BEGIN{
            if      (r < 0.03) print "XX";
            else if (r > 0.32) print "XY";
            else if (r < 0.15) print "XX?";
            else               print "XY?";
        }')"
    fi
    printf "%s\t%d\t%d\t%s\t%s\n" "$sid" "$nX" "$nY" "$ry" "$call" >> "$OUT"
done
