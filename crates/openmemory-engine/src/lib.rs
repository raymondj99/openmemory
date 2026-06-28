//! The openmemory **context engine**: a concurrency bus between agents
//! and the memory stores.
//!
//! Agents produce organizational context far faster than a single
//! SQLite writer can absorb it one transaction at a time, and they
//! read while they write. This crate sits between the two and makes
//! both directions cheap, in five stages:
//!
//! ```text
//!  agents ──▶ accept ──▶ journal ──▶ route ──▶ commit ──▶ publish ──▶ agents
//!            (ticket)   (crash-     (entity   (epoch     (durability  (reads:
//!                        durable     hash →    drain =    watermark)   cache +
//!                        JSONL)      shard →   ONE tx)                 fan-out)
//!                                    domain)
//! ```
//!
//! - **Accept** ([`ContextEngine::submit`], [`engine`]): append to a
//!   per-shard in-memory queue under a short lock, return a [`Ticket`]
//!   in sub-microsecond time. Backpressure (a full shard blocks),
//!   never data loss.
//! - **Journal** ([`journal`]): when enabled, the request is on disk
//!   before the ticket returns and fsynced before its commit; replay
//!   after a crash is exactly-once via a checkpoint written inside the
//!   commit transaction.
//! - **Route** ([`partition`]): entities hash by name onto shards, and
//!   whole shards onto storage *domains* — independent store families
//!   committed by parallel writers. Cross-domain relations are
//!   double-written (TAO-style mirrors) so every domain traverses its
//!   edges locally.
//! - **Commit** ([`engine`]): each epoch (default 20 ms) drains whole
//!   shards into single batched `remember_batch` transactions —
//!   embeddings precomputed in one batched call, vector-index
//!   persistence deferred to the maintenance tick, WAL checkpoints
//!   moved out of the commit path entirely.
//! - **Publish**: a cacheline-aligned per-shard watermark (the flux-rs
//!   epoch-counter idea) advances after each commit;
//!   [`ContextEngine::wait_durable`] gives lock-free read-your-writes,
//!   and the MCP `remember` tool waits on it by default.
//!
//! Reads run against the same [`partition::DomainStore`]: recall fans
//! out to every domain in parallel, merges by score, and memoises
//! merged results in a write-version-invalidated cache, so repeated
//! queries skip the domains (and their index-visibility locks)
//! entirely.
//!
//! [`migrate`] re-homes a profile between domain counts offline with a
//! verified staging build and a crash-safe two-phase swap; [`adapter`]
//! turns external sources (meeting notes, chat exports) into request
//! streams for `openmemory ingest`.
//!
//! Measured on an M-series laptop (see `docs/context-engine.md` for
//! the full numbers): ~65k observations/s durable at 4 domains with
//! journaling under 4000 concurrent writers, microsecond acks,
//! ~1 us cached recalls, and sub-100 ms reader worst case under
//! write storms.

pub mod adapter;
pub mod engine;
pub mod evidence;
pub mod journal;
pub mod migrate;
pub mod partition;

pub use engine::{ContextEngine, EngineOptions, EngineStats, Ticket};
