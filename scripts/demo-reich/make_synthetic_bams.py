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
            chrom, start, _end = line.strip().split("\t")
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
    ("LOW01", 0.40, 80, "low_coverage"),
    ("AMB01", 0.20, 500, "XY?"),
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
            read = pysam.AlignedSegment(bam.header)
            read.query_name = qname
            read.query_sequence = "A" * 50
            read.flag = 0
            read.reference_id = tid_for[chrom]
            read.reference_start = pos
            read.mapping_quality = 60
            read.cigar = [(0, 50)]  # 50M
            read.query_qualities = pysam.qualitystring_to_array("I" * 50)
            bam.write(read)
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
