# EvalBench weekly meeting notes

## 2026-05-27
Leaderboard schema settled: run receipts are append-only JSONL,
one row per (model, task, version).

## 2026-06-24
Two submissions differed only in prompt template and moved the QA
score nine points; decision: templates are part of the submission
hash, not free variables.

## 2026-07-15
Calibration task added after the reliability review; see
metrics/calibration.md for the binning choice.
