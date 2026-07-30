# PromptLib: programming language-model pipelines instead of prompting them

## Aim
A Python framework where a pipeline is declared as typed modules
(retrieve, transform, generate, verify) and a compiler searches for
the instructions and demonstrations that maximize a task metric,
instead of hand-written prompt strings.

## Why now
Our lab maintains dozens of hand-tuned prompt chains; every model
upgrade breaks them. Compilation against a metric makes pipelines
portable across models.

## Evaluation
All compiled pipelines are scored on the evalbench harness
(evalbench/infra/run-harness.md) so numbers are comparable across projects.
