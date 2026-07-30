# Running the harness

One command per submission: the runner pulls the task version, runs
the system adapter, writes a receipt row with scores, cost, latency,
and git revision, and refuses to overwrite an existing receipt.
Adapters exist for hosted APIs, local checkpoints, and PromptLib
pipelines (the promptlib adapter imports the compiled program
directly, see promptlib/design/compiler.md).
