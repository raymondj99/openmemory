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
