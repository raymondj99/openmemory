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

use std::ops::Deref;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use openmemory_core::config::Config;
use openmemory_core::space::MemoryContext;
use openmemory_engine::partition::DomainStore;
use openmemory_engine::space::{
    layered_recall, LayeredRecallRequest, LayeredRecallResponse, SpaceReadHandle,
};
use openmemory_engine::{ContextEngine, EngineOptions};
use openmemory_graph::{MemoryError, MemoryResult, MemoryStore};

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
    runtime: McpRuntimeController,
    request_context: Option<Arc<McpResolvedContext>>,
}

impl Clone for OpenMemoryMcpServer {
    fn clone(&self) -> Self {
        Self {
            router: self.router.clone(),
            config: self.config.clone(),
            runtime: self.runtime.clone(),
            request_context: self.request_context.clone(),
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

#[derive(Debug)]
struct RuntimeResources {
    memory: Arc<DomainStore>,
    engine: Option<Arc<ContextEngine>>,
}

#[derive(Debug)]
struct RuntimeState {
    resources: Option<Arc<RuntimeResources>>,
    paused: bool,
}

#[derive(Debug)]
struct RuntimeInner {
    state: Mutex<RuntimeState>,
    changed: Condvar,
}

/// Admission controller for the MCP store and write-behind engine.
///
/// Normal calls obtain a short runtime lease. Material promotion first pauses
/// admissions, waits for all leases to drain, and closes the engine and store
/// before renaming any live artifact.
#[derive(Debug, Clone)]
pub struct McpRuntimeController {
    inner: Arc<RuntimeInner>,
}

/// Lease over the currently active domain store.
#[derive(Debug)]
pub struct McpMemoryLease {
    resources: Option<Arc<RuntimeResources>>,
    scoped: Option<Arc<DomainStore>>,
    runtime: Arc<RuntimeInner>,
}

impl Deref for McpMemoryLease {
    type Target = DomainStore;

    fn deref(&self) -> &Self::Target {
        if let Some(scoped) = self.scoped.as_ref() {
            scoped
        } else {
            &self
                .resources
                .as_ref()
                .expect("memory lease resources live until drop")
                .memory
        }
    }
}

impl Drop for McpMemoryLease {
    fn drop(&mut self) {
        drop(self.resources.take());
        let _state = self
            .runtime
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.runtime.changed.notify_all();
    }
}

/// Lease over the currently active write-behind engine.
#[derive(Debug)]
pub struct McpEngineLease {
    resources: Option<Arc<RuntimeResources>>,
    runtime: Arc<RuntimeInner>,
}

impl Deref for McpEngineLease {
    type Target = ContextEngine;

    fn deref(&self) -> &Self::Target {
        self.resources
            .as_ref()
            .expect("engine lease resources live until drop")
            .engine
            .as_deref()
            .expect("engine lease is only constructed when an engine exists")
    }
}

impl McpEngineLease {
    /// Pause queue admission while retaining this runtime lease.
    pub fn pause_admissions(&self) -> openmemory_engine::EnginePause {
        self.resources
            .as_ref()
            .expect("engine lease resources live until drop")
            .engine
            .as_ref()
            .expect("engine lease has an engine")
            .pause_admissions()
    }
}

impl Drop for McpEngineLease {
    fn drop(&mut self) {
        drop(self.resources.take());
        let _state = self
            .runtime
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.runtime.changed.notify_all();
    }
}

/// Exclusive paused state used by promotion. Dropping it without reopening
/// keeps the runtime closed, so callers cannot accidentally serve a renamed
/// or partially recovered root.
#[derive(Debug)]
pub struct PausedMcpRuntime {
    controller: McpRuntimeController,
}

#[derive(Debug, thiserror::Error)]
pub enum McpRuntimeError {
    #[error("timed out waiting for active MCP requests to drain")]
    DrainTimeout,
    #[error("MCP runtime is not paused")]
    NotPaused,
    #[error("MCP runtime is already active")]
    AlreadyActive,
    #[error("context engine could not start: {0}")]
    Start(String),
}

/// A request-scoped, already-authorized semantic context supplied by the
/// daemon. The opaque guards keep registry leases alive for the entire MCP
/// request without coupling this crate to the daemon registry type.
pub struct McpResolvedContext {
    pub context: MemoryContext,
    pub read_handles: Vec<SpaceReadHandle>,
    pub write_store: Arc<DomainStore>,
    pub proposal_required: bool,
    guards: Vec<Arc<dyn Send + Sync>>,
}

impl std::fmt::Debug for McpResolvedContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResolvedContext")
            .field("context", &self.context)
            .field("read_handles", &self.read_handles)
            .field("proposal_required", &self.proposal_required)
            .field("guard_count", &self.guards.len())
            .finish()
    }
}

impl McpResolvedContext {
    pub fn new(
        context: MemoryContext,
        read_handles: Vec<SpaceReadHandle>,
        write_store: Arc<DomainStore>,
        proposal_required: bool,
        guards: Vec<Arc<dyn Send + Sync>>,
    ) -> MemoryResult<Self> {
        context
            .validate(false)
            .map_err(|error| MemoryError::Authorization(error.to_string()))?;
        if read_handles.len() != context.read_set.len()
            || read_handles
                .iter()
                .zip(context.read_set.iter())
                .any(|(handle, grant)| handle.space != grant.space)
            || write_store.space_id() != context.default_write
        {
            return Err(MemoryError::Authorization(
                "resolved MCP context does not match its physical handles".to_string(),
            ));
        }
        Ok(Self {
            context,
            read_handles,
            write_store,
            proposal_required,
            guards,
        })
    }
}

impl McpRuntimeController {
    fn new(resources: RuntimeResources) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                state: Mutex::new(RuntimeState {
                    resources: Some(Arc::new(resources)),
                    paused: false,
                }),
                changed: Condvar::new(),
            }),
        }
    }

    fn lease(&self) -> Arc<RuntimeResources> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while state.paused || state.resources.is_none() {
            state = self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        Arc::clone(
            state
                .resources
                .as_ref()
                .expect("active runtime has resources"),
        )
    }

    /// Lease the active write-behind engine, when configured.
    pub fn engine(&self) -> Option<McpEngineLease> {
        let resources = self.lease();
        if resources.engine.is_some() {
            Some(McpEngineLease {
                resources: Some(resources),
                runtime: Arc::clone(&self.inner),
            })
        } else {
            None
        }
    }

    /// Stop new MCP calls, drain existing runtime leases, quiesce the engine,
    /// and release every store handle before returning.
    pub fn pause_and_close(&self, timeout: Duration) -> Result<PausedMcpRuntime, McpRuntimeError> {
        let deadline = Instant::now() + timeout;
        let resources = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.paused = true;
            loop {
                let Some(resources) = state.resources.as_ref() else {
                    return Err(McpRuntimeError::NotPaused);
                };
                if Arc::strong_count(resources) == 1 {
                    break state
                        .resources
                        .take()
                        .expect("resources were present while locked");
                }
                let now = Instant::now();
                if now >= deadline {
                    state.paused = false;
                    self.inner.changed.notify_all();
                    return Err(McpRuntimeError::DrainTimeout);
                }
                let wait = deadline.saturating_duration_since(now);
                let (next, _) = self
                    .inner
                    .changed
                    .wait_timeout(state, wait)
                    .unwrap_or_else(|error| error.into_inner());
                state = next;
            }
        };

        let mut resources =
            Arc::try_unwrap(resources).expect("all MCP runtime leases drained before close");
        if let Some(engine) = resources.engine.take() {
            match Arc::try_unwrap(engine) {
                Ok(engine) => engine.shutdown(),
                Err(engine) => {
                    engine.quiesce();
                    drop(engine);
                }
            }
        }
        drop(resources);
        Ok(PausedMcpRuntime {
            controller: self.clone(),
        })
    }
}

impl PausedMcpRuntime {
    /// Reopen request admission over the freshly verified active store.
    pub fn resume(self, config: &Config, memory: Arc<DomainStore>) -> Result<(), McpRuntimeError> {
        let resources = build_runtime_resources(config, memory)
            .map_err(|error| McpRuntimeError::Start(error.to_string()))?;
        let mut state = self
            .controller
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !state.paused {
            return Err(McpRuntimeError::NotPaused);
        }
        if state.resources.is_some() {
            return Err(McpRuntimeError::AlreadyActive);
        }
        state.resources = Some(Arc::new(resources));
        state.paused = false;
        self.controller.inner.changed.notify_all();
        Ok(())
    }
}

fn build_runtime_resources(
    config: &Config,
    memory: Arc<DomainStore>,
) -> anyhow::Result<RuntimeResources> {
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
    Ok(RuntimeResources { memory, engine })
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
            runtime: McpRuntimeController::new(RuntimeResources {
                memory: Arc::new(DomainStore::from_single(memory)),
                engine: None,
            }),
            request_context: None,
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
        let resources = build_runtime_resources(&config, memory)?;
        Ok(Self {
            router: tools::build_router(),
            config,
            runtime: McpRuntimeController::new(resources),
            request_context: None,
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
    pub fn memory(&self) -> McpMemoryLease {
        if let Some(context) = &self.request_context {
            return McpMemoryLease {
                resources: None,
                scoped: Some(Arc::clone(&context.write_store)),
                runtime: Arc::clone(&self.runtime.inner),
            };
        }
        McpMemoryLease {
            resources: Some(self.runtime.lease()),
            scoped: None,
            runtime: Arc::clone(&self.runtime.inner),
        }
    }

    /// Borrow the write-behind context engine, if enabled.
    pub fn engine(&self) -> Option<McpEngineLease> {
        if let Some(context) = &self.request_context {
            let resources = self.runtime.lease();
            if resources.memory.space_id() != context.write_store.space_id() {
                return None;
            }
            drop(resources);
        }
        self.runtime.engine()
    }

    /// Return the request's authorized context, if this is a daemon-scoped
    /// call. Direct stdio compatibility calls have no explicit context.
    pub fn resolved_context(&self) -> Option<&McpResolvedContext> {
        self.request_context.as_deref()
    }

    /// Run deterministic layered recall when a daemon supplied a context.
    pub fn contextual_recall(
        &self,
        request: &LayeredRecallRequest,
    ) -> MemoryResult<Option<LayeredRecallResponse>> {
        self.request_context
            .as_ref()
            .map(|context| layered_recall(&context.read_handles, request))
            .transpose()
    }

    /// Clone the admission controller used for safe material promotion.
    pub fn runtime_controller(&self) -> McpRuntimeController {
        self.runtime.clone()
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

    /// Dispatch one request against an immutable, pre-authorized context.
    pub fn handle_with_context(
        &self,
        req: JsonRpcRequest,
        context: Arc<McpResolvedContext>,
    ) -> Option<JsonRpcResponse> {
        let mut scoped = self.clone();
        scoped.request_context = Some(context);
        scoped.handle(req)
    }
}

/// Run the MCP server on stdio (stdin / stdout). Logs go to stderr so the
/// JSON-RPC stream stays clean.
pub async fn run_stdio_server(server: OpenMemoryMcpServer) -> anyhow::Result<()> {
    stdio::run(server, tokio::io::stdin(), tokio::io::stdout()).await
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn promotion_pause_drains_leases_and_reopens_without_polling() {
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        let config = Config::default();
        let first = Arc::new(
            DomainStore::open(&config, first_root.path(), 1).expect("open first runtime"),
        );
        let server =
            OpenMemoryMcpServer::from_domain_store(config.clone(), first).expect("build server");
        let lease = server.memory();
        let controller = server.runtime_controller();
        let worker_controller = controller.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let pause = worker_controller
                .pause_and_close(Duration::from_secs(5))
                .expect("pause after lease drains");
            closed_tx.send(pause).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(closed_rx.try_recv().is_err());
        drop(lease);
        let pause = closed_rx.recv().unwrap();
        let second = Arc::new(
            DomainStore::open(&config, second_root.path(), 1).expect("open second runtime"),
        );
        pause.resume(&config, second).expect("resume runtime");
        assert_eq!(server.memory().domains(), 1);
        worker.join().unwrap();
    }
}
