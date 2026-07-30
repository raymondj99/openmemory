# EvalBench quarterly results snapshot

Multilingual QA (task v3), exact match:

| system | EM |
|---|---|
| zero-shot large model | 48.1 |
| PromptLib compiled pipeline | 61.7 |
| compiled + MultiParse preprocessing | 64.0 |

Preprocessing with harmonized tokenization is worth +2.3 EM on the
low-resource slice, consistent with the MultiParse pilot.
