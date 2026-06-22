//! `openmemory mcp` — start the MCP server.
//!
//! Default transport is stdio (matching what OpenClaw runs). With
//! `--http <addr>` and the `mcp-http` feature, the server binds an HTTP
//! listener and serves the same router over `POST /mcp`.

use std::sync::Arc;

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_engine::partition::DomainStore;
use openmemory_mcp::OpenMemoryMcpServer;

use crate::cli::McpArgs;

pub fn run(profile: &str, args: McpArgs) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir).context("creating data directory")?;
    }
    // Partitioning materialises here: `[engine] domains` from the config
    // creates (or reopens) the domain layout. A mismatch with an existing
    // manifest is a hard error from DomainStore::open.
    #[cfg(feature = "embeddings")]
    let memory = {
        let models_dir = Config::models_dir().context("resolving models directory")?;
        if let Some(embedder) = openmemory_embed::load_embedder(&models_dir) {
            eprintln!(
                "openmemory mcp: embeddings active ({})",
                embedder.model_name()
            );
            DomainStore::open_with_embedder(
                &config,
                &data_dir,
                config.engine.domains,
                Arc::new(embedder),
            )
        } else {
            eprintln!("openmemory mcp: running in keyword-only mode (no embedder)");
            DomainStore::open(&config, &data_dir, config.engine.domains)
        }
    }
    .with_context(|| format!("opening memory store at {}", data_dir.display()))?;
    #[cfg(not(feature = "embeddings"))]
    let memory = DomainStore::open(&config, &data_dir, config.engine.domains)
        .with_context(|| format!("opening memory store at {}", data_dir.display()))?;

    if memory.domains() > 1 {
        eprintln!(
            "openmemory mcp: domain-partitioned profile ({} domains)",
            memory.domains()
        );
    }
    let server = OpenMemoryMcpServer::from_domain_store(config, Arc::new(memory))
        .context("starting context engine")?;
    let engine = server.engine().cloned();
    if engine.is_some() {
        eprintln!("openmemory mcp: write-behind context engine active");
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    let result = runtime.block_on(serve(server, args));

    // Graceful exit: drain the engine so acknowledged writes commit.
    // After a crash the per-shard journal replays them instead.
    if let Some(engine) = engine {
        engine.quiesce();
    }
    result
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
