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

pub mod coding_mem;
pub mod dataset;
pub mod io;
pub mod longmem_s;
pub mod metrics;
pub mod runner;

pub use dataset::{Dataset, Document, Judgment, Query};
pub use metrics::{Metrics, RetrievalReport};
pub use runner::{EvalRunner, RunnerConfig};
