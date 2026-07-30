"""Generate the two non-code scenario corpora.

Both are fictional but structurally grounded in real examples:
the researcher scenario mirrors the Stanford NLP group's actual
project mix (a multilingual pipeline like Stanza, an LLM-programming
framework like DSPy, and a benchmark/evaluation effort); the student
scenario mirrors the Stanford CS229 / CS230 / CS224N course trio and
their real syllabus topics. Content is written, not scraped: the test
needs realistic *structure* (related folders, shared vocabulary,
homonym filenames, cross-references), not copyrighted text.
"""

import json
import os

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "scenarios"))

R = {}  # researcher corpus: path -> content
S = {}  # student corpus: path -> content

# --------------------------------------------------------------------------
# Scenario 1: a researcher with three related project folders
# --------------------------------------------------------------------------

R["multiparse/proposal.md"] = """# MultiParse: robust multilingual parsing for low-resource languages

## Aim
Extend our neural pipeline to 30 additional low-resource languages,
with tokenization, lemmatization, part-of-speech tagging, and
dependency parsing trained jointly. Current per-language models
degrade badly when training treebanks have fewer than 2,000 sentences.

## Approach
Cross-lingual transfer from a shared multilingual encoder, with
language-specific adapters. Character-level fallback for scripts with
no pretrained coverage.

## Deliverables
Year 1: adapter architecture and treebank harmonization.
Year 2: release of models for all 30 languages with evaluation
against the evalbench multilingual suite (see evalbench/tasks/multilingual-qa.md).
"""

R["multiparse/meeting-notes.md"] = """# MultiParse weekly meeting notes

## 2026-06-02
Kim reported the Uyghur tagger stalls at 71 F1; suspect tokenizer
over-segmentation on Arabic-script loanwords. Action: swap in the
shared sentencepiece vocabulary.

## 2026-06-16
Adapter fusion beats full fine-tuning on 9 of 11 pilot languages.
Decision: adapters are the default going forward.

## 2026-07-07
Treebank licensing blocks redistribution for two languages; we will
ship weights but not data. Priya to draft the data statement.
"""

R["multiparse/results.md"] = """# MultiParse experimental results

Pilot evaluation on 11 low-resource treebanks, test split, LAS.

| condition | mean LAS |
|---|---|
| monolingual baseline | 61.4 |
| shared encoder, full fine-tune | 66.8 |
| shared encoder + language adapters | 68.9 |

Largest gain on Kazakh (+11.2 LAS). Regression on Maltese (-0.8),
traced to tokenizer merges that split definite articles.
"""

R["multiparse/data/treebanks.md"] = """# Treebank inventory and harmonization notes

We use Universal Dependencies release 2.14. Twelve treebanks needed
relabeling: their auxiliary chains used the pre-2.8 convention.
Harmonization scripts live in the repo under tools/harmonize.

Licensing: two treebanks are research-only; see meeting notes 2026-07-07
for the redistribution decision.
"""

R["multiparse/models/tagger-architecture.md"] = """# Tagger and parser architecture

A shared 12-layer multilingual encoder feeds four task heads:
tokenizer, lemmatizer, POS tagger, and biaffine dependency parser.
Language adapters (bottleneck dim 64) insert after layers 4, 8, 12.
The biaffine head follows Dozat and Manning; arc and label scores
are decoded with the Chu-Liu-Edmonds maximum spanning tree algorithm.
Character-level fallback embeddings activate when a wordpiece maps
to the unknown token more than twice per sentence.
"""

R["multiparse/error-analysis.md"] = """# Error analysis: where the parser still fails

Three dominant error classes across the pilot languages:

1. Copula constructions in zero-copula languages get spurious root
   attachments (34% of Kazakh errors).
2. Clitic pronouns fused into verb forms confuse the lemmatizer.
3. Code-switched segments: the language-id gate routes the whole
   sentence to one adapter, so mixed sentences lose per-token accuracy.

Item 3 motivates the token-level adapter routing experiment proposed
for promptlib's optimizer to tune (see promptlib/design/optimizers.md).
"""

R["promptlib/proposal.md"] = """# PromptLib: programming language-model pipelines instead of prompting them

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
"""

R["promptlib/meeting-notes.md"] = """# PromptLib weekly meeting notes

## 2026-05-20
The bootstrapped few-shot optimizer overfits on tasks with under 50
training examples; add a held-out gate before accepting a candidate.

## 2026-06-10
Decision: module signatures are frozen for the 0.3 release. The
compiler may rewrite instructions and demonstrations, never types.

## 2026-07-01
Cost tracking landed. A full compile of the QA pipeline costs $4.10
against the small hosted model, $61 against the large one.
"""

R["promptlib/results.md"] = """# PromptLib results

Compiled vs hand-written prompts, three tasks, same base model.

| task | hand-written | compiled |
|---|---|---|
| multilingual QA | 54.2 | 61.7 |
| citation verification | 71.0 | 78.3 |
| table-to-text | 44.9 | 47.2 |

The multilingual QA gain concentrates in low-resource languages,
which matches the MultiParse observation that preprocessing quality
dominates there.
"""

R["promptlib/design/compiler.md"] = """# Compiler design

The compiler treats a pipeline as a dataflow graph of modules. Each
module exposes a signature (typed inputs and outputs) and free
parameters (instruction text, demonstrations). Optimization proceeds
in rounds: propose candidates, score on the metric via the evalbench
runner, keep the Pareto set. Search strategies are pluggable; the
default is bootstrapped few-shot with a random-search fallback.
"""

R["promptlib/design/optimizers.md"] = """# Optimizer catalogue

- BootstrapFewShot: sample demonstrations from successful traces.
- InstructionSearch: mutate instruction text with an LLM proposer.
- Ensemble: compile k pipelines, vote at inference.

Open question from the MultiParse error analysis: can InstructionSearch
tune a token-level routing policy for code-switched input? Tracked as
experiment PL-17.
"""

R["promptlib/tutorials/rag-pipeline.md"] = """# Tutorial: a retrieval-augmented QA pipeline in eleven lines

The tutorial builds a two-stage pipeline: a retriever module over a
document index, then a generator module constrained to cite retrieved
passages. Compiling with twenty labeled examples lifts exact-match by
seven points over the zero-shot pipeline. The full script is in the
repository examples directory.
"""

R["evalbench/proposal.md"] = """# EvalBench: living evaluation for the lab's models and pipelines

## Aim
One harness, one leaderboard, every project. Tasks are versioned,
metrics are frozen per version, and every run records model, revision,
task version, and cost, so results stay comparable months later.

## Scope
Starts with multilingual QA, citation verification, and calibration
measurement. MultiParse taggers provide preprocessing for the
multilingual tasks (multiparse/models/tagger-architecture.md).
"""

R["evalbench/meeting-notes.md"] = """# EvalBench weekly meeting notes

## 2026-05-27
Leaderboard schema settled: run receipts are append-only JSONL,
one row per (model, task, version).

## 2026-06-24
Two submissions differed only in prompt template and moved the QA
score nine points; decision: templates are part of the submission
hash, not free variables.

## 2026-07-15
Calibration task added after the reliability review; see
metrics/calibration.md for the binning choice.
"""

R["evalbench/results.md"] = """# EvalBench quarterly results snapshot

Multilingual QA (task v3), exact match:

| system | EM |
|---|---|
| zero-shot large model | 48.1 |
| PromptLib compiled pipeline | 61.7 |
| compiled + MultiParse preprocessing | 64.0 |

Preprocessing with harmonized tokenization is worth +2.3 EM on the
low-resource slice, consistent with the MultiParse pilot.
"""

R["evalbench/metrics/calibration.md"] = """# Calibration metric

We report expected calibration error with fifteen equal-mass bins,
plus the reliability diagram. Equal-width binning was rejected: with
confident models most predictions land in the top bin and the metric
collapses. Confidence is read from token logprobs when the API
exposes them, otherwise from a verbalized 0-100 self-estimate, and
the two sources are never compared on one leaderboard.
"""

R["evalbench/tasks/multilingual-qa.md"] = """# Task: multilingual question answering (v3)

Questions in 14 languages over a shared document pool. Answers are
short spans; scoring is exact match after Unicode normalization.
Preprocessing uses the MultiParse tokenizers so that span boundaries
are consistent across scripts. Task v3 froze the document pool after
we found v2 documents drifting when the source wiki updated.
"""

R["evalbench/infra/run-harness.md"] = """# Running the harness

One command per submission: the runner pulls the task version, runs
the system adapter, writes a receipt row with scores, cost, latency,
and git revision, and refuses to overwrite an existing receipt.
Adapters exist for hosted APIs, local checkpoints, and PromptLib
pipelines (the promptlib adapter imports the compiled program
directly, see promptlib/design/compiler.md).
"""

# --------------------------------------------------------------------------
# Scenario 2: a student taking three closely related classes
# --------------------------------------------------------------------------

S["cs229/syllabus.md"] = """# CS229 Machine Learning: course syllabus

Weekly topics: supervised learning and linear regression; logistic
regression, Newton's method and generalized linear models; Gaussian
discriminant analysis and naive Bayes; support vector machines and
kernels; bias-variance tradeoff and regularization; tree ensembles;
neural network basics and training; k-means, mixtures of Gaussians
and expectation maximization; PCA and ICA; MDPs, value iteration and
Q-learning. Two midterms, weekly problem sets, final project.
"""

S["cs229/notes03-logistic-regression.md"] = """# Notes 3: logistic regression and GLMs

The sigmoid maps scores to probabilities; the log-likelihood is
concave so Newton's method converges in a handful of steps. Deriving
the gradient gives the same update shape as linear regression, which
the professor kept emphasizing: it is a property of the exponential
family, not a coincidence. Softmax regression generalizes to
multiclass and its derivative is the one reused later for neural
network output layers.
"""

S["cs229/notes07-svm-kernels.md"] = """# Notes 7: support vector machines and kernels

Maximum margin as a convex quadratic program; the dual form depends
on inner products only, which is what makes the kernel trick work.
Gaussian kernel corresponds to an infinite-dimensional feature map.
Slack variables trade margin violations against C. Remember for the
exam: the representer theorem argument for why the solution is a
combination of support vectors.
"""

S["cs229/notes10-neural-networks.md"] = """# Notes 10: neural networks and backpropagation

Backpropagation is just the chain rule organized so each local
gradient is computed once. We derived the two-layer case by hand;
the vectorized form uses the same softmax gradient from notes 3.
Initialization matters: symmetric weights never break symmetry.
CS230 goes much deeper on optimization tricks; these notes only
cover vanilla gradient descent.
"""

S["cs229/notes13-em-kmeans.md"] = """# Notes 13: k-means, mixtures of Gaussians, EM

K-means is coordinate descent on the distortion objective. Mixtures
of Gaussians add soft assignments; expectation maximization
alternates responsibilities (E-step) and parameter updates (M-step),
monotonically improving the evidence lower bound. The lower-bound
derivation via Jensen's inequality is a favorite exam question.
"""

S["cs229/pset2-writeup.md"] = """# Problem set 2 writeup

Implemented logistic regression with Newton's method: converged in
six iterations on the provided dataset. The GDA comparison shows the
generative model winning when its Gaussian assumption holds and
losing under the log-transformed features. Proof question: showed
the exponential family form of the Poisson and derived its canonical
link.
"""

S["cs229/project-proposal.md"] = """# CS229 project proposal: predicting bike-share demand

Regression on hourly rental counts with weather and calendar
features. Plan: linear baseline, then gradient boosted trees, then a
small feedforward network, with learning curves to diagnose bias vs
variance. Success metric: RMSE under 60 rentals per hour on held-out
weeks. I intend to reuse the neural net for the CS230 project if the
tabular results justify it.
"""

S["cs230/syllabus.md"] = """# CS230 Deep Learning: course syllabus

Modules: neural network foundations and vectorization; optimization
(momentum, RMSprop, Adam, learning-rate schedules); regularization
(L2, dropout, batch normalization, early stopping); convolutional
networks and computer vision; sequence models (RNNs, LSTMs, GRUs);
attention and an introduction to transformers; practical project
methodology (error analysis, train/dev/test splits). Graded on
programming assignments, a midterm, and a team project with
milestone and final report.
"""

S["cs230/notes02-optimization.md"] = """# Notes 2: optimization beyond vanilla gradient descent

Mini-batch gradient descent with momentum smooths the update
direction; RMSprop rescales per-parameter step sizes; Adam combines
both with bias correction. Learning-rate warmup then decay is the
default recipe. The professor's rule of thumb: tune the learning
rate first, everything else second. CS229 derived plain gradient
descent; this lecture is about making it actually converge fast.
"""

S["cs230/notes04-regularization.md"] = """# Notes 4: regularization in deep networks

L2 weight decay, dropout as an ensemble approximation, batch
normalization's regularizing side effect, early stopping against the
dev-set curve, and data augmentation. Dropout at test time is
replaced by weight scaling. Batch norm changes the loss landscape
enough that it interacts badly with small batch sizes; use layer
norm there, which is also what transformers use (cs224n/notes08-attention-transformers.md).
"""

S["cs230/notes06-cnn.md"] = """# Notes 6: convolutional networks

Convolutions share weights across positions; parameter count depends
on kernel size and channels, not image size. Padding and stride
arithmetic. Pooling for translation tolerance. Classic
architectures: LeNet to ResNet, where skip connections fix vanishing
gradients in very deep stacks. Assignment 2 implements the forward
and backward pass for a conv layer by hand.
"""

S["cs230/notes08-rnn-lstm.md"] = """# Notes 8: recurrent networks and LSTMs

Recurrent networks share weights across time; backpropagation
through time unrolls the graph. Vanishing gradients motivate gates:
the LSTM cell state carries information with additive updates, the
GRU merges gates for a cheaper cell. For long sequences attention
now outperforms pure recurrence; CS224N covers that shift in detail.
"""

S["cs230/pset1-writeup.md"] = """# Programming assignment 1 writeup

Built a two-layer network in NumPy: forward pass, cross-entropy
loss, backward pass, gradient check at 1e-7 relative error. Compared
optimizers on the moons dataset: Adam reached 97% dev accuracy in a
third of the epochs momentum needed. Dropout at 0.5 cost accuracy on
this small model; 0.2 was the sweet spot.
"""

S["cs230/project-milestone.md"] = """# CS230 project milestone: bike-share demand, deep version

Continuing the CS229 project data. The feedforward baseline matches
gradient boosting only after batch norm and Adam with warmup.
Added an LSTM over the previous 24 hours of rentals: RMSE 54.1 vs
58.9 for the tabular network. Error analysis says holidays dominate
the residual; next step is a holiday embedding.
"""

S["cs224n/syllabus.md"] = """# CS224N NLP with Deep Learning: course syllabus

Topics: word vectors (word2vec, GloVe); neural classifiers;
dependency parsing; recurrent networks for language modeling;
sequence-to-sequence and machine translation; attention and
transformers; pretraining (BERT, GPT) and finetuning; question
answering; natural language generation; final project on a task of
your choice. Five assignments then an open-ended project.
"""

S["cs224n/notes01-word-vectors.md"] = """# Notes 1: word vectors

Distributional hypothesis: meaning from context. word2vec skip-gram
trains a word to predict its neighbors with negative sampling; GloVe
factorizes the log co-occurrence matrix. The softmax over the
vocabulary is the same function from the CS229 GLM lecture, just
enormous, hence the sampling tricks. Evaluation: analogy tasks and
similarity correlations, both flawed but standard.
"""

S["cs224n/notes05-dependency-parsing.md"] = """# Notes 5: dependency parsing

Transition-based parsing with a stack and buffer; the neural parser
scores shift, left-arc, right-arc actions from embedding features.
Universal Dependencies gives consistent annotation across languages.
Evaluation is unlabeled and labeled attachment score. Assignment 3
implements the arc-standard oracle and trains the action classifier.
"""

S["cs224n/notes08-attention-transformers.md"] = """# Notes 8: attention and transformers

Attention replaces recurrence with direct weighted access to all
positions: queries, keys, values, scaled dot products, softmax over
scores. Multi-head attention looks at different subspaces. The
transformer block stacks attention with feedforward layers, residual
connections, and layer normalization. Positional encodings restore
order information. This is the architecture every pretrained model
in the rest of the course builds on.
"""

S["cs224n/notes09-pretraining.md"] = """# Notes 9: pretraining and finetuning

Masked language modeling (BERT) versus autoregressive pretraining
(GPT). Finetuning adapts the whole network; prompting and lightweight
adapters adapt without touching most weights. Scaling laws: loss
falls predictably with compute, data, and parameters. The assignment
finetunes a small pretrained transformer for question answering.
"""

S["cs224n/a4-writeup.md"] = """# Assignment 4 writeup: neural machine translation

Sequence-to-sequence LSTM with attention, Cherokee to English.
Implemented the attention mechanism from the CS230 sequence-model
notes but with bilinear scoring. BLEU 22.4 after beam search width 5.
Ablation: removing attention drops BLEU to 13.1, and the attention
maps visibly align determiners and nouns.
"""

S["cs224n/final-project.md"] = """# CS224N final project: calibration of finetuned QA models

Question: does finetuning a pretrained transformer for extractive QA
preserve calibration? Method: expected calibration error with
equal-mass bins on SQuAD dev, before and after finetuning, plus
temperature scaling as the fix. Uses the softmax confidence directly,
connecting back to the CS229 logistic regression view of probability
outputs.
"""

SCENARIOS = {
    "researcher": {
        "corpus": R,
        "projects": {
            "multiparse": "Multilingual parsing pipeline project (Stanza-like): tokenization, tagging, dependency parsing for low-resource languages.",
            "promptlib": "LLM pipeline programming framework project (DSPy-like): typed modules compiled against task metrics.",
            "evalbench": "Living evaluation harness project (HELM-like): versioned tasks, frozen metrics, append-only leaderboard.",
        },
        "project_relations": [
            ("promptlib", "uses", "evalbench"),
            ("evalbench", "uses", "multiparse"),
            ("multiparse", "related_to", "promptlib"),
        ],
        "representatives": ["proposal.md"],
    },
    "student": {
        "corpus": S,
        "projects": {
            "cs229": "CS229 Machine Learning: classical ML, GLMs, SVMs, EM, intro neural networks and RL.",
            "cs230": "CS230 Deep Learning: optimization, regularization, CNNs, sequence models, project methodology.",
            "cs224n": "CS224N NLP with Deep Learning: word vectors, parsing, attention, transformers, pretraining.",
        },
        "project_relations": [
            ("cs229", "prerequisite_of", "cs230"),
            ("cs230", "related_to", "cs224n"),
            ("cs229", "related_to", "cs224n"),
        ],
        "representatives": ["syllabus.md"],
    },
}


def main():
    for name, sc in SCENARIOS.items():
        base = os.path.join(ROOT, name, "corpus")
        for path, content in sc["corpus"].items():
            full = os.path.join(base, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w") as f:
                f.write(content)
        meta = {k: v for k, v in sc.items() if k != "corpus"}
        with open(os.path.join(ROOT, name, "scenario.json"), "w") as f:
            json.dump(meta, f, indent=2)
        print(f"{name}: {len(sc['corpus'])} files")


if __name__ == "__main__":
    main()
