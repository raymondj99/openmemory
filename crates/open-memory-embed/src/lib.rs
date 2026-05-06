//! Optional ONNX Runtime text embeddings for `open-memory`.
//!
//! Loads `nomic-embed-text-v1.5` (default) or
//! `snowflake-arctic-embed-l-v2.0`, caches results in SQLite by
//! BLAKE3 content hash. When this crate is excluded from the build,
//! the rest of the system runs keyword-only — every API still works,
//! recall just has no vector contribution to RRF.

#![forbid(unsafe_code)]

pub mod error;
pub mod integrity;
pub mod models;
pub mod onnx;
pub mod traits;

#[cfg(feature = "sqlite")]
pub mod cache;
pub mod json_cache;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use error::{EmbedError, EmbedResult};
pub use integrity::{verify_sha256, VerificationOutcome};
pub use models::{Model, ModelRegistry, NOMIC_EMBED_TEXT_V1_5, SNOWFLAKE_ARCTIC_EMBED_L_V2};
pub use onnx::{OnnxEmbedder, OnnxOptions, PoolingStrategy};
pub use traits::Embedder;

#[cfg(feature = "sqlite")]
pub use cache::EmbeddingCache;
#[cfg(not(feature = "sqlite"))]
pub use json_cache::EmbeddingCache;

#[cfg(any(test, feature = "testing"))]
pub use testing::StubEmbedder;
