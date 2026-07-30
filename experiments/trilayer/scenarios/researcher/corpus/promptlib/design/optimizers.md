# Optimizer catalogue

- BootstrapFewShot: sample demonstrations from successful traces.
- InstructionSearch: mutate instruction text with an LLM proposer.
- Ensemble: compile k pipelines, vote at inference.

Open question from the MultiParse error analysis: can InstructionSearch
tune a token-level routing policy for code-switched input? Tracked as
experiment PL-17.
