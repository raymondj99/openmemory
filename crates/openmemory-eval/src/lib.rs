//! Retrieval-quality evaluation harness for openmemory.
//!
//! Loads a benchmark dataset, ingests it into a fresh `MemoryStore`,
//! runs the configured queries through the hybrid search engine, and
//! reports recall-at-K, mean reciprocal rank, and normalised discounted
//! cumulative gain alongside per-query latency.
//!
//! The crate is split along the same axes the runbook calls out:
//!
//! - [`dataset`] defines the corpus + query + judgment shape every
//!   adapter has to implement.
//! - [`metrics`] is a pure-function module computing R@K, MRR, and
//!   NDCG against the ranked URI list returned by the engine.
//! - [`runner`] wires the dataset adapter to a fresh `MemoryStore`
//!   and produces a [`RetrievalReport`].
//! - [`longmem_s`] and [`coding_mem`] are loader stubs for the two
//!   public retrieval benchmarks. Each reads a JSONL fixture tree
//!   (`corpus.jsonl`, `queries.jsonl`, `judgments.jsonl`) and is
//!   deliberately thin so the harness compiles without the actual
//!   dataset files present; CI gates that need them mount the
//!   fixtures under `tests/fixtures/<dataset>/` at runtime.

//! ## Two evaluation paths
//!
//! [`runner`] drives a dataset through the **index** (`engine.search`)
//! against a fresh in-memory store. [`recall_runner`] drives a query set
//! through **`MemoryStore::recall`** against a store that already
//! exists. The distinction is not cosmetic: decay, the access-count
//! boost, the correction boost, importance, temporal validity, and
//! spreading activation all live in `recall()` and are invisible to the
//! index path.
//!
//! - [`recall_set`] defines categorised queries with graded judgments,
//!   abstention cases, and judgment provenance.
//! - [`resolver`] bridges stable document keys to per-ingest observation
//!   ids.
//! - [`recall_report`] adds latency percentiles, per-category rollups,
//!   and abstention scoring, reusing [`metrics`] for the metric maths.

pub mod calibration;
pub mod coding_mem;
pub mod contract;
pub mod dataset;
pub mod gates;
pub mod io;
pub mod longmem_s;
pub mod metrics;
pub mod recall_report;
pub mod recall_runner;
pub mod recall_set;
pub mod resolver;
pub mod runner;

pub use calibration::{ClusteredMean, ClusteredRate, Confidence, ScoreShape, SeparationReport};
pub use contract::{EvaluationContract, METRIC_VERSION};
pub use dataset::{Dataset, Document, Judgment, Query};
pub use gates::{evaluate as evaluate_gates, GateOutcome, GateReport, QualityGates};
pub use metrics::{Metrics, RetrievalReport};
pub use recall_report::{
    AbstentionMetrics, CategoryCeiling, LatencyStats, QualityMetrics, RecallCeiling, RecallReport,
    WaivedQuery, GATED_K,
};
pub use recall_runner::{unknown_judgment_keys, RecallRunner, RecallRunnerConfig};
pub use recall_set::{CapacityWaiver, GradedKey, JudgmentSource, RecallQuery, RecallQuerySet};
pub use resolver::{KeyResolver, MapResolver, ObservationIdResolver};
pub use runner::{EvalRunner, RunnerConfig};
