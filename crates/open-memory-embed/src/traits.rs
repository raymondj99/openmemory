//! Re-export of the [`Embedder`] trait from `open-memory-core`.
//!
//! The trait lives in core so that downstream crates can swap real
//! ONNX embedders for in-memory test doubles without taking a
//! dependency on the ONNX Runtime.

pub use open_memory_core::testing::Embedder;
