# MultiParse experimental results

Pilot evaluation on 11 low-resource treebanks, test split, LAS.

| condition | mean LAS |
|---|---|
| monolingual baseline | 61.4 |
| shared encoder, full fine-tune | 66.8 |
| shared encoder + language adapters | 68.9 |

Largest gain on Kazakh (+11.2 LAS). Regression on Maltese (-0.8),
traced to tokenizer merges that split definite articles.
