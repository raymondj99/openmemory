//! Optional ONNX Runtime text embeddings for `openmemory`.
//!
//! Loads `nomic-embed-text-v1.5` (default) or
//! `snowflake-arctic-embed-l-v2.0`, caches results in SQLite by
//! BLAKE3 content hash. When this crate is excluded from the build,
//! the rest of the system runs keyword-only — every API still works,
//! recall just has no vector contribution to RRF.

#![deny(unsafe_code)]

pub mod bootstrap;
pub mod cached;
pub mod download;
pub mod error;
pub mod integrity;
pub mod models;
pub mod onnx;
pub mod runtime;
pub mod traits;

#[cfg(feature = "sqlite")]
pub mod cache;
pub mod json_cache;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use bootstrap::{ensure_model, load_default_embedder, load_embedder};
pub use cached::CachedEmbedder;
pub use download::ModelManager;
pub use error::{EmbedError, EmbedResult};
pub use integrity::{verify_sha256, VerificationOutcome};
pub use models::{Model, ModelRegistry, NOMIC_EMBED_TEXT_V1_5, SNOWFLAKE_ARCTIC_EMBED_L_V2};
pub use onnx::{OnnxEmbedder, OnnxOptions, PoolingStrategy};
pub use runtime::{RuntimeManager, ONNX_RUNTIME_VERSION};
pub use traits::Embedder;

#[cfg(feature = "sqlite")]
pub use cache::EmbeddingCache;
#[cfg(not(feature = "sqlite"))]
pub use json_cache::EmbeddingCache;

#[cfg(any(test, feature = "testing"))]
pub use testing::StubEmbedder;
