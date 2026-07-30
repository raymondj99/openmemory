# PromptLib results

Compiled vs hand-written prompts, three tasks, same base model.

| task | hand-written | compiled |
|---|---|---|
| multilingual QA | 54.2 | 61.7 |
| citation verification | 71.0 | 78.3 |
| table-to-text | 44.9 | 47.2 |

The multilingual QA gain concentrates in low-resource languages,
which matches the MultiParse observation that preprocessing quality
dominates there.
