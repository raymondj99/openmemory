# Compiler design

The compiler treats a pipeline as a dataflow graph of modules. Each
module exposes a signature (typed inputs and outputs) and free
parameters (instruction text, demonstrations). Optimization proceeds
in rounds: propose candidates, score on the metric via the evalbench
runner, keep the Pareto set. Search strategies are pluggable; the
default is bootstrapped few-shot with a random-search fallback.
