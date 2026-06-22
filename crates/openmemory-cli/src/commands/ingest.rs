//! `openmemory ingest` — bulk-load a data source through the
//! write-behind context engine.
//!
//! The adapters live in `openmemory-engine::adapter`; this command picks
//! one from the path/format, opens the profile's store, starts a
//! temporary engine sized from `[engine]` config (enabled or not — an
//! explicit ingest is always routed through the engine), pumps the
//! adapter until exhaustion, and quiesces so every observation is
//! committed before the process exits.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_engine::adapter::{
    ingest_all, ChatJsonlAdapter, MarkdownNotesAdapter, SourceAdapter,
};
use openmemory_engine::partition::DomainStore;
use openmemory_engine::{ContextEngine, EngineOptions};

use crate::cli::{IngestArgs, IngestFormat};

pub fn run(profile: &str, args: IngestArgs) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        anyhow::bail!(
            "openmemory profile {profile:?} not initialised (no data directory at {}). \
             Run `openmemory init` first.",
            data_dir.display()
        );
    }
    // Partitioning materialises here when `[engine] domains > 1`.
    let memory = Arc::new(
        DomainStore::open(&config, &data_dir, config.engine.domains)
            .with_context(|| format!("opening memory store at {}", data_dir.display()))?,
    );

    let format = match args.format {
        IngestFormat::Auto => {
            if args.path.is_dir() {
                IngestFormat::Markdown
            } else if args.path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                IngestFormat::Chat
            } else {
                anyhow::bail!(
                    "cannot auto-detect format for {}; pass --format markdown|chat",
                    args.path.display()
                );
            }
        }
        other => other,
    };

    let mut adapter: Box<dyn SourceAdapter> = match format {
        IngestFormat::Markdown => Box::new(
            MarkdownNotesAdapter::open(&args.path)
                .with_context(|| format!("scanning {}", args.path.display()))?,
        ),
        IngestFormat::Chat => Box::new(
            ChatJsonlAdapter::open(&args.path)
                .with_context(|| format!("opening {}", args.path.display()))?,
        ),
        IngestFormat::Auto => unreachable!("resolved above"),
    };

    let engine = ContextEngine::start_partitioned(
        Arc::clone(&memory),
        EngineOptions {
            shards: config.engine.shards,
            flush_interval: Duration::from_millis(config.engine.flush_interval_ms),
            shard_capacity: config.engine.shard_capacity,
            flush_threads: config.engine.flush_threads,
            normalize: config.engine.normalize && !args.no_normalize,
            journal_dir: config
                .engine
                .journal
                .then(|| memory.data_dir().join("engine-journal")),
            checkpoint_interval: Duration::from_millis(config.engine.checkpoint_interval_ms),
        },
    )
    .context("starting context engine")?;

    let t0 = Instant::now();
    let report = ingest_all(&engine, adapter.as_mut()).context("ingesting source")?;
    let elapsed = t0.elapsed();
    engine.shutdown();

    let status = memory.status().context("reading store status")?;
    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "source": args.path.display().to_string(),
                "format": format!("{format:?}").to_lowercase(),
                "requests": report.requests,
                "batches": report.batches,
                "elapsed_ms": elapsed.as_millis(),
                "total_entities": status.total_entities,
                "total_observations": status.total_observations,
            })
        );
    } else {
        println!(
            "ingested {} requests in {:.2?} ({:.0} req/s); store now holds {} entities / {} observations",
            report.requests,
            elapsed,
            report.requests as f64 / elapsed.as_secs_f64().max(f64::EPSILON),
            status.total_entities,
            status.total_observations,
        );
    }
    Ok(())
}
