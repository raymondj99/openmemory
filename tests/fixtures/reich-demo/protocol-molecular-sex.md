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
