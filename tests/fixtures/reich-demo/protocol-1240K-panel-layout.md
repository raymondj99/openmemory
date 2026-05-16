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
