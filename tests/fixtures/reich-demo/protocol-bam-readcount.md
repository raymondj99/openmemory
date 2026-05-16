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
