# Error analysis: where the parser still fails

Three dominant error classes across the pilot languages:

1. Copula constructions in zero-copula languages get spurious root
   attachments (34% of Kazakh errors).
2. Clitic pronouns fused into verb forms confuse the lemmatizer.
3. Code-switched segments: the language-id gate routes the whole
   sentence to one adapter, so mixed sentences lose per-token accuracy.

Item 3 motivates the token-level adapter routing experiment proposed
for promptlib's optimizer to tune (see promptlib/design/optimizers.md).
