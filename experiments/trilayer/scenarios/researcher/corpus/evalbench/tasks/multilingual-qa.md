# Task: multilingual question answering (v3)

Questions in 14 languages over a shared document pool. Answers are
short spans; scoring is exact match after Unicode normalization.
Preprocessing uses the MultiParse tokenizers so that span boundaries
are consistent across scripts. Task v3 froze the document pool after
we found v2 documents drifting when the source wiki updated.
