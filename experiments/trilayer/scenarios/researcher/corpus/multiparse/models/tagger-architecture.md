# Tagger and parser architecture

A shared 12-layer multilingual encoder feeds four task heads:
tokenizer, lemmatizer, POS tagger, and biaffine dependency parser.
Language adapters (bottleneck dim 64) insert after layers 4, 8, 12.
The biaffine head follows Dozat and Manning; arc and label scores
are decoded with the Chu-Liu-Edmonds maximum spanning tree algorithm.
Character-level fallback embeddings activate when a wordpiece maps
to the unknown token more than twice per sentence.
