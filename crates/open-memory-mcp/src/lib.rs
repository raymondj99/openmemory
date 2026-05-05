//! MCP server for `open-memory`.
//!
//! Exposes graph and index operations as MCP tools over stdio
//! (always) and Streamable HTTP (optional, behind the `http`
//! feature). All tools are prefixed `open_memory_*`; see
//! `docs/02-openclaw-integration.md` for the full surface.
//!
//! Implementation lands in subsequent commits — see the project
//! roadmap under `docs/03-roadmap.md`.

#![forbid(unsafe_code)]
