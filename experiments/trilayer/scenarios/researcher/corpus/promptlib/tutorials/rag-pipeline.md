# Tutorial: a retrieval-augmented QA pipeline in eleven lines

The tutorial builds a two-stage pipeline: a retriever module over a
document index, then a generator module constrained to cite retrieved
passages. Compiling with twenty labeled examples lifts exact-match by
seven points over the zero-shot pipeline. The full script is in the
repository examples directory.
