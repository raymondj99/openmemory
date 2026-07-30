# Notes 1: word vectors

Distributional hypothesis: meaning from context. word2vec skip-gram
trains a word to predict its neighbors with negative sampling; GloVe
factorizes the log co-occurrence matrix. The softmax over the
vocabulary is the same function from the CS229 GLM lecture, just
enormous, hence the sampling tricks. Evaluation: analogy tasks and
similarity correlations, both flawed but standard.
