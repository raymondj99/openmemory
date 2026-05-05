//! Persistent knowledge-graph memory for AI agents.
//!
//! `open-memory-graph` is the heart of the project: SQLite plus the
//! `open-memory-index` hybrid search engine, kept in lockstep by a
//! single [`MemoryStore`]. Entities have stable names; observations
//! are temporal facts about entities (`valid_from`, `valid_until`);
//! relations are directed edges. Recall is hybrid search over
//! observation text, filtered by temporal validity, scored with
//! Ebbinghaus-style decay and access-frequency boosts.
//!
//! Implementation lands in subsequent commits — see the project
//! roadmap under `docs/03-roadmap.md`.

#![forbid(unsafe_code)]
