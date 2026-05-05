//! Optional ONNX Runtime text embeddings for `open-memory`.
//!
//! Loads `nomic-embed-text-v1.5` (default) or
//! `snowflake-arctic-embed-l-v2.0`, caches results in SQLite by
//! BLAKE3 content hash. When this crate is excluded from the build,
//! the rest of the system runs keyword-only — every API still works,
//! recall just has no vector contribution to RRF.
//!
//! Implementation lands in subsequent commits — see the project
//! roadmap under `docs/03-roadmap.md`.

#![forbid(unsafe_code)]
