# PromptLib weekly meeting notes

## 2026-05-20
The bootstrapped few-shot optimizer overfits on tasks with under 50
training examples; add a held-out gate before accepting a candidate.

## 2026-06-10
Decision: module signatures are frozen for the 0.3 release. The
compiler may rewrite instructions and demonstrations, never types.

## 2026-07-01
Cost tracking landed. A full compile of the QA pipeline costs $4.10
against the small hosted model, $61 against the large one.
