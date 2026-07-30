# MultiParse weekly meeting notes

## 2026-06-02
Kim reported the Uyghur tagger stalls at 71 F1; suspect tokenizer
over-segmentation on Arabic-script loanwords. Action: swap in the
shared sentencepiece vocabulary.

## 2026-06-16
Adapter fusion beats full fine-tuning on 9 of 11 pilot languages.
Decision: adapters are the default going forward.

## 2026-07-07
Treebank licensing blocks redistribution for two languages; we will
ship weights but not data. Priya to draft the data statement.
