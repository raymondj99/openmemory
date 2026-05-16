#!/usr/bin/env bash
# count_reads_per_pos.sh - canonicalised samtools wrapper.
# Usage: count_reads_per_pos.sh <bed> <bam>
set -euo pipefail

BED="$1"
BAM="$2"
samtools view -c -q 30 -L "$BED" "$BAM"
