//! `openmemory mcp` — start the MCP server.
//!
//! Default transport is stdio (matching what OpenClaw runs). With
//! `--http <addr>` and the `mcp-http` feature, the server binds an HTTP
//! listener and serves the same router over `POST /mcp`.

use std::sync::Arc;

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_graph::MemoryStore;
use openmemory_mcp::OpenMemoryMcpServer;

use crate::cli::McpArgs;

pub fn run(profile: &str, args: McpArgs) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir).context("creating data directory")?;
    }
    let memory = MemoryStore::open(&config, &data_dir)
        .with_context(|| format!("opening memory store at {}", data_dir.display()))?;
    let server = OpenMemoryMcpServer::from_memory(config, Arc::new(memory));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    runtime.block_on(serve(server, args))
}

async fn serve(server: OpenMemoryMcpServer, args: McpArgs) -> Result<()> {
    if let Some(addr) = args.http {
        run_http(server, addr).await
    } else {
        eprintln!("openmemory mcp: serving on stdio");
        openmemory_mcp::run_stdio_server(server).await
    }
}

#[cfg(feature = "mcp-http")]
async fn run_http(server: OpenMemoryMcpServer, addr: std::net::SocketAddr) -> Result<()> {
    eprintln!("openmemory mcp: serving on http://{addr}");
    openmemory_mcp::http::serve(server, addr).await
}

// Async signature is required so the call site at `serve` matches both
// feature variants. With `mcp-http` off there is nothing to await, so
// allow `clippy::unused_async` rather than fork the call site.
#[cfg(not(feature = "mcp-http"))]
#[allow(clippy::unused_async)]
async fn run_http(_server: OpenMemoryMcpServer, _addr: std::net::SocketAddr) -> Result<()> {
    anyhow::bail!(
        "this build does not include the mcp-http feature; rebuild openmemory with \
         `--features mcp-http` to use --http"
    )
}
