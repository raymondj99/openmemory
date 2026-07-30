# MultiParse: robust multilingual parsing for low-resource languages

## Aim
Extend our neural pipeline to 30 additional low-resource languages,
with tokenization, lemmatization, part-of-speech tagging, and
dependency parsing trained jointly. Current per-language models
degrade badly when training treebanks have fewer than 2,000 sentences.

## Approach
Cross-lingual transfer from a shared multilingual encoder, with
language-specific adapters. Character-level fallback for scripts with
no pretrained coverage.

## Deliverables
Year 1: adapter architecture and treebank harmonization.
Year 2: release of models for all 30 languages with evaluation
against the evalbench multilingual suite (see evalbench/tasks/multilingual-qa.md).
