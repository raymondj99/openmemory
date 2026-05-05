//! MCP server for `open-memory` — exposes the knowledge-graph and free-text
//! index as JSON-RPC 2.0 tools over stdio (always) and HTTP (feature
//! `mcp-http`).
//!
//! The crate ships eleven `open_memory_*` tools — seven memory tools, three
//! index tools, one consolidation tool. See [`tools`] for the full list and
//! [`docs/02-openclaw-integration.md`] for the wire-level contract.
//!
//! [`docs/02-openclaw-integration.md`]: https://github.com/raymondj99/open-memory/blob/main/docs/02-openclaw-integration.md
//!
//! # Quick start
//!
//! ```no_run
//! # async fn run() -> anyhow::Result<()> {
//! use open_memory_core::config::Config;
//! use open_memory_mcp::{OpenMemoryMcpServer, run_stdio_server};
//!
//! let server = OpenMemoryMcpServer::open(Config::default(), "default")?;
//! run_stdio_server(server).await?;
//! # Ok(()) }
//! ```
//!
//! # Why no `rmcp` dependency?
//!
//! Every published `rmcp` (0.13+ and all 1.x) uses if-let chains that need
//! rustc 1.88+. open-memory pins MSRV to 1.85. This crate therefore
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

use serde::{Deserialize, Serialize};

use open_memory_core::config::Config;
use open_memory_graph::MemoryStore;

pub mod params;
pub mod protocol;
pub mod stdio;
pub mod tools;

#[cfg(feature = "mcp-http")]
pub mod http;

pub use params::{EntityTypeParam, MemoryTierParam, SearchModeParam};
pub use protocol::{
    Content, JsonRpcError, JsonRpcRequest, JsonRpcResponse, RequestId, ServerCapabilities,
    ServerInfo, ToolDescriptor,
};
pub use tools::{Tool, ToolGroup, ToolRouter};

/// Protocol version this server speaks. Reported in the `initialize`
/// response. Bump when upstream MCP releases a new version we adopt.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// The open-memory MCP server. Holds shared state — the memory store, the
/// active config, the tool router — and dispatches incoming requests
/// through the registry in [`tools`].
pub struct OpenMemoryMcpServer {
    router: ToolRouter,
    pub(crate) config: Config,
    pub(crate) memory: Arc<MemoryStore>,
}

impl Clone for OpenMemoryMcpServer {
    fn clone(&self) -> Self {
        Self {
            router: self.router.clone(),
            config: self.config.clone(),
            memory: self.memory.clone(),
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
    /// configured store through.
    pub fn from_memory(config: Config, memory: Arc<MemoryStore>) -> Self {
        Self {
            router: tools::build_router(),
            config,
            memory,
        }
    }

    /// Open a [`MemoryStore`] under the configured profile and wrap it.
    pub fn open(config: Config, profile: &str) -> anyhow::Result<Self> {
        let data_dir = Config::data_dir(profile)?;
        let memory = MemoryStore::open(&config, &data_dir)?;
        Ok(Self::from_memory(config, Arc::new(memory)))
    }

    /// Borrow the active config.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Borrow the active memory store.
    pub fn memory(&self) -> &Arc<MemoryStore> {
        &self.memory
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
                name: "open-memory".into(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: tools::server_instructions(),
        }
    }

    /// Handle one JSON-RPC request and return a (possibly empty) response.
    /// Notifications return `None`; requests return `Some(response)`.
    pub fn handle(
        &self,
        req: JsonRpcRequest,
    ) -> Option<JsonRpcResponse> {
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
