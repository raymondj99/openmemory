//! MCP server for `openmemory` — exposes the knowledge-graph and free-text
//! index as JSON-RPC 2.0 tools over stdio (always) and HTTP (feature
//! `mcp-http`).
//!
//! The crate ships eleven `openmemory_*` tools — seven memory tools, three
//! index tools, one consolidation tool. See [`tools`] for the full list and
//! [`docs/mcp.md`] for the wire-level contract.
//!
//! [`docs/mcp.md`]: https://github.com/raymondj99/openmemory/blob/main/docs/mcp.md
//!
//! # Quick start
//!
//! ```no_run
//! # async fn run() -> anyhow::Result<()> {
//! use openmemory_core::config::Config;
//! use openmemory_mcp::{OpenMemoryMcpServer, run_stdio_server};
//!
//! let server = OpenMemoryMcpServer::open(Config::default(), "default")?;
//! run_stdio_server(server).await?;
//! # Ok(()) }
//! ```
//!
//! # Why no `rmcp` dependency?
//!
//! Every published `rmcp` (0.13+ and all 1.x) uses if-let chains that need
//! rustc 1.88+. openmemory pins MSRV to 1.85. This crate therefore
//! implements the minimal slice of MCP we need by hand: JSON-RPC 2.0
//! request/response framing over a `tokio::io::AsyncRead + AsyncWrite` pair,
//! plus the four MCP methods we serve — `initialize`, `tools/list`,
//! `tools/call`, and `notifications/initialized`.
//!
//! The shape mirrors rmcp closely so swapping in the upstream SDK later is a
//! mechanical change: a [`Tool`] trait per tool, a single
//! [`ToolRouter`] keeping handlers + descriptors colocated, and a
//! `ServerHandler` shape the HTTP transport implements against.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use openmemory_core::config::Config;
use openmemory_engine::partition::DomainStore;
use openmemory_engine::{ContextEngine, EngineOptions};
use openmemory_graph::MemoryStore;

pub mod params;
pub mod protocol;
pub mod stdio;
pub mod tools;

#[cfg(feature = "mcp-http")]
pub mod http;

#[cfg(feature = "mcp-http")]
pub use http::{BearerToken, BEARER_TOKEN_ENV};

pub use params::{EntityTypeParam, MemoryTierParam, SearchModeParam};
pub use protocol::{
    Content, JsonRpcError, JsonRpcRequest, JsonRpcResponse, RequestId, ServerCapabilities,
    ServerInfo, ToolDescriptor,
};
pub use tools::{Tool, ToolGroup, ToolRouter};

/// Protocol version this server speaks. Reported in the `initialize`
/// response. Bump when upstream MCP releases a new version we adopt.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// The openmemory MCP server. Holds shared state — the memory store, the
/// active config, the tool router — and dispatches incoming requests
/// through the registry in [`tools`].
pub struct OpenMemoryMcpServer {
    router: ToolRouter,
    pub(crate) config: Config,
    /// Domain-partitioned memory facade. One domain (a zero-cost
    /// passthrough) unless `[engine] domains > 1` partitioned the
    /// profile.
    pub(crate) memory: Arc<DomainStore>,
    /// Write-behind context engine, present when `[engine] enabled` is
    /// set in the config. `openmemory_remember` routes through it.
    pub(crate) engine: Option<Arc<ContextEngine>>,
}

impl Clone for OpenMemoryMcpServer {
    fn clone(&self) -> Self {
        Self {
            router: self.router.clone(),
            config: self.config.clone(),
            memory: self.memory.clone(),
            engine: self.engine.clone(),
        }
    }
}

impl std::fmt::Debug for OpenMemoryMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenMemoryMcpServer").finish()
    }
}

/// Server-info block returned by `initialize`. Lives inside the
/// `serverInfo` field. Field names are camelCase to match the MCP wire
/// format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
    pub instructions: String,
}

impl OpenMemoryMcpServer {
    /// Build a server from an already-opened [`MemoryStore`]. Lets the CLI
    /// own the open ceremony and any embedder attachment, then pass the
    /// configured store through. The write-behind engine is NOT started
    /// on this path; use [`Self::from_domain_store`] to honour
    /// `[engine] enabled`.
    pub fn from_memory(config: Config, memory: Arc<MemoryStore>) -> Self {
        Self {
            router: tools::build_router(),
            config,
            memory: Arc::new(DomainStore::from_single(memory)),
            engine: None,
        }
    }

    /// Like [`Self::from_memory`], but starts the write-behind
    /// [`ContextEngine`] when `[engine] enabled` is set, replaying any
    /// crash-recovery journal under `<data_dir>/engine-journal/` first.
    pub fn from_memory_with_engine(
        config: Config,
        memory: Arc<MemoryStore>,
    ) -> anyhow::Result<Self> {
        Self::from_domain_store(config, Arc::new(DomainStore::from_single(memory)))
    }

    /// Build a server over a domain-partitioned store, starting the
    /// write-behind [`ContextEngine`] when `[engine] enabled` is set
    /// (replaying any crash-recovery journal under
    /// `<data_dir>/engine-journal/` first).
    pub fn from_domain_store(config: Config, memory: Arc<DomainStore>) -> anyhow::Result<Self> {
        let engine = if config.engine.enabled {
            let opts = EngineOptions {
                shards: config.engine.shards,
                flush_interval: Duration::from_millis(config.engine.flush_interval_ms),
                shard_capacity: config.engine.shard_capacity,
                flush_threads: config.engine.flush_threads,
                normalize: config.engine.normalize,
                journal_dir: config
                    .engine
                    .journal
                    .then(|| memory.data_dir().join("engine-journal")),
                checkpoint_interval: Duration::from_millis(config.engine.checkpoint_interval_ms),
            };
            Some(Arc::new(ContextEngine::start_partitioned(
                Arc::clone(&memory),
                opts,
            )?))
        } else {
            None
        };
        Ok(Self {
            router: tools::build_router(),
            config,
            memory,
            engine,
        })
    }

    /// Open the configured profile (partitioned per `[engine] domains`)
    /// and wrap it. Honours `[engine] enabled`.
    pub fn open(config: Config, profile: &str) -> anyhow::Result<Self> {
        let data_dir = Config::data_dir(profile)?;
        let memory = DomainStore::open(&config, &data_dir, config.engine.domains)?;
        Self::from_domain_store(config, Arc::new(memory))
    }

    /// Borrow the active config.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Borrow the active memory facade.
    pub fn memory(&self) -> &Arc<DomainStore> {
        &self.memory
    }

    /// Borrow the write-behind context engine, if enabled.
    pub fn engine(&self) -> Option<&Arc<ContextEngine>> {
        self.engine.as_ref()
    }

    /// Borrow the tool router. Tests use this to verify the registered set.
    pub fn router(&self) -> &ToolRouter {
        &self.router
    }

    /// Render the `initialize` response.
    pub fn initialize_result(&self) -> InitializeResult {
        InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            capabilities: ServerCapabilities::with_tools(),
            server_info: ServerInfo {
                name: "openmemory".into(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: tools::server_instructions(),
        }
    }

    /// Handle one JSON-RPC request and return a (possibly empty) response.
    /// Notifications return `None`; requests return `Some(response)`.
    pub fn handle(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let id = req.id.clone();
        match req.method.as_str() {
            "initialize" => Some(JsonRpcResponse::success(
                id,
                serde_json::to_value(self.initialize_result()).unwrap_or_default(),
            )),
            "notifications/initialized" => None,
            "tools/list" => Some(JsonRpcResponse::success(
                id,
                serde_json::json!({
                    "tools": self.router.list_descriptors(),
                }),
            )),
            "tools/call" => Some(match self.router.call(self, req.params) {
                Ok(result) => JsonRpcResponse::success(id, result),
                Err(e) => JsonRpcResponse::error(id, e),
            }),
            other => Some(JsonRpcResponse::error(
                id,
                JsonRpcError::method_not_found(other),
            )),
        }
    }
}

/// Run the MCP server on stdio (stdin / stdout). Logs go to stderr so the
/// JSON-RPC stream stays clean.
pub async fn run_stdio_server(server: OpenMemoryMcpServer) -> anyhow::Result<()> {
    stdio::run(server, tokio::io::stdin(), tokio::io::stdout()).await
}
