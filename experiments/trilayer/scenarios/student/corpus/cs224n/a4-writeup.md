# Assignment 4 writeup: neural machine translation

Sequence-to-sequence LSTM with attention, Cherokee to English.
Implemented the attention mechanism from the CS230 sequence-model
notes but with bilinear scoring. BLEU 22.4 after beam search width 5.
Ablation: removing attention drops BLEU to 13.1, and the attention
maps visibly align determiners and nouns.
