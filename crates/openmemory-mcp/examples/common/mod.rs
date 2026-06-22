//! Shared scaffolding for the MCP integration-test examples (`swarm`,
//! `swmr`): an in-process HTTP server over a partitioned engine
//! profile, and a minimal JSON-RPC `tools/call` client.
//!
//! Each example compiles this module independently, so items used by
//! only one of them would warn in the other.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use openmemory_core::config::Config;
use openmemory_engine::partition::DomainStore;
use openmemory_engine::ContextEngine;
use openmemory_mcp::OpenMemoryMcpServer;
use serde_json::{json, Value};

/// An in-process `openmemory mcp --http` instance over a fresh
/// tempdir profile with the write-behind engine enabled.
pub struct TestServer {
    /// `http://127.0.0.1:<port>/mcp`
    pub base: String,
    pub domains: Arc<DomainStore>,
    pub engine: Arc<ContextEngine>,
    pub config: Config,
    pub data_dir: std::path::PathBuf,
    runtime: tokio::runtime::Runtime,
    server: OpenMemoryMcpServer,
    dir: tempfile::TempDir,
}

impl TestServer {
    /// Boot a server on an ephemeral port and wait for readiness.
    pub fn start(domains_count: usize, shards: usize) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        let mut config = Config::default();
        config.engine.enabled = true;
        config.engine.domains = domains_count;
        config.engine.shards = shards;
        let domains =
            Arc::new(DomainStore::open(&config, &data_dir, domains_count).expect("open domains"));
        let server = OpenMemoryMcpServer::from_domain_store(config.clone(), Arc::clone(&domains))
            .expect("start server");
        let engine = server.engine().cloned().expect("engine enabled");

        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
            probe.local_addr().unwrap().port()
        };
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("tokio runtime");
        {
            let server = server.clone();
            runtime.spawn(async move {
                if let Err(e) = openmemory_mcp::http::serve(server, addr).await {
                    eprintln!("mcp http server exited: {e}");
                }
            });
        }
        let base = format!("http://{addr}/mcp");
        let probe = McpClient::new(base.clone());
        for _ in 0..100 {
            if probe.call("openmemory_status", json!({})).is_ok() {
                break;
            }
            std::thread::park_timeout(Duration::from_millis(50));
        }
        Self {
            base,
            domains,
            engine,
            config,
            data_dir,
            runtime,
            server,
            dir,
        }
    }

    /// Stop the HTTP tasks and release every store handle, draining the
    /// engine so journals are empty. Returns a handle that KEEPS THE
    /// PROFILE ALIVE (the tempdir is deleted when the handle drops) for
    /// offline follow-up phases such as migration.
    pub fn shutdown(self) -> OfflineProfile {
        self.runtime.shutdown_timeout(Duration::from_secs(5));
        drop(self.server);
        drop(self.domains);
        Arc::try_unwrap(self.engine)
            .map_err(|_| ())
            .expect("sole engine handle")
            .shutdown();
        OfflineProfile {
            config: self.config,
            data_dir: self.data_dir,
            _dir: self.dir,
        }
    }
}

/// A shut-down [`TestServer`]'s profile, kept on disk for offline
/// phases. Dropping this deletes the tempdir.
pub struct OfflineProfile {
    pub config: Config,
    pub data_dir: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

/// Minimal JSON-RPC 2.0 client for `tools/call`. Returns the tool's
/// text payload parsed as JSON.
pub struct McpClient {
    agent: ureq::Agent,
    url: String,
    next_id: AtomicU64,
}

impl McpClient {
    pub fn new(url: String) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(30))
                .build(),
            url,
            next_id: AtomicU64::new(1),
        }
    }

    pub fn call(&self, tool: &str, arguments: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        });
        let response: Value = self
            .agent
            .post(&self.url)
            .send_json(request)
            .map_err(|e| format!("{tool}: transport: {e}"))?
            .into_json()
            .map_err(|e| format!("{tool}: bad json: {e}"))?;
        if let Some(error) = response.get("error") {
            return Err(format!("{tool}: rpc error: {error}"));
        }
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .ok_or_else(|| format!("{tool}: missing content"))?;
        serde_json::from_str(text).map_err(|e| format!("{tool}: payload: {e}"))
    }
}

/// Percentile over a sorted nanosecond vector.
pub fn pctl(sorted: &[u64], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    Duration::from_nanos(sorted[idx])
}
