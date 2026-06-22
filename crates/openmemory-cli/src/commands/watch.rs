//! `openmemory watch <PATH>` — start a foreground filesystem
//! watcher.
//!
//! Opens the same `MemoryStore` the MCP server uses, walks the watch
//! root once, then tails create / modify / delete events through
//! `notify-debouncer-full` and re-indexes incrementally.
//!
//! The watcher is killed by the OS sending SIGINT / SIGTERM; we
//! deliberately don't install a custom signal handler in v0.2. SQLite
//! is in WAL mode and every write happens inside a transaction, so a
//! hard kill mid-batch is recovered cleanly when the database is
//! reopened. v0.3 can layer in a graceful-shutdown signal hook on top
//! of the existing `Watcher::run_with_shutdown` API.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_graph::MemoryStore;
use openmemory_watch::{WatchOptions, Watcher, DEFAULT_EXTENSIONS};

use crate::cli::WatchArgs;

pub fn run(profile: &str, args: WatchArgs) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir).context("creating data directory")?;
    }
    if openmemory_engine::partition::DomainStore::manifest_domains(&data_dir)? > 1 {
        anyhow::bail!(
            "`openmemory watch` does not support domain-partitioned profiles yet; \
             use `openmemory ingest` for bulk loading"
        );
    }
    let memory = MemoryStore::open(&config, &data_dir)
        .with_context(|| format!("opening memory store at {}", data_dir.display()))?;
    let memory = Arc::new(memory);

    let options = build_options(&config, &args);
    let watcher = Watcher::new(memory, args.path.clone(), options).map_err(anyhow::Error::from)?;

    eprintln!("openmemory watch: watching {}", watcher.root().display());
    eprintln!(
        "openmemory watch: send SIGINT (Ctrl-C) or SIGTERM to stop; SQLite is in WAL mode \
         so a kill mid-batch is recovered on next open."
    );
    let shutdown = Arc::new(AtomicBool::new(false));
    let report = watcher
        .run_with_shutdown(shutdown)
        .map_err(anyhow::Error::from)?;
    eprintln!(
        "openmemory watch: stopped — inserted={}, updated={}, removed={}, unchanged={}, skipped={}",
        report.inserted, report.updated, report.removed, report.unchanged, report.skipped
    );
    Ok(())
}

fn build_options(config: &Config, args: &WatchArgs) -> WatchOptions {
    let mut options = WatchOptions::from_config(config);
    if let Some(ms) = args.debounce_ms {
        options.debounce = Duration::from_millis(ms);
    }
    if let Some(raw) = args.exts.as_deref() {
        let exts: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_start_matches('.').to_lowercase())
            .collect();
        if !exts.is_empty() {
            options.extensions = exts;
        }
    }
    if let Some(max) = args.max_size {
        options.max_size = max;
    }
    if args.no_initial_scan {
        options.initial_scan = false;
    }
    if options.extensions.is_empty() {
        options.extensions = DEFAULT_EXTENSIONS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn args() -> WatchArgs {
        WatchArgs {
            path: PathBuf::from("."),
            debounce_ms: None,
            exts: None,
            max_size: None,
            no_initial_scan: false,
        }
    }

    #[test]
    fn build_options_defaults_to_curated_extensions() {
        let opts = build_options(&Config::default(), &args());
        assert!(opts.extensions.iter().any(|e| e == "md"));
        assert!(opts.initial_scan);
    }

    #[test]
    fn build_options_overrides_debounce_and_max_size() {
        let mut a = args();
        a.debounce_ms = Some(500);
        a.max_size = Some(2048);
        let opts = build_options(&Config::default(), &a);
        assert_eq!(opts.debounce, Duration::from_millis(500));
        assert_eq!(opts.max_size, 2048);
    }

    #[test]
    fn build_options_parses_extension_csv() {
        let mut a = args();
        a.exts = Some(".MD, txt , .rs ,, ".to_string());
        let opts = build_options(&Config::default(), &a);
        assert_eq!(
            opts.extensions,
            vec!["md".to_string(), "txt".to_string(), "rs".to_string()]
        );
    }

    #[test]
    fn build_options_no_initial_scan_flag_propagates() {
        let mut a = args();
        a.no_initial_scan = true;
        let opts = build_options(&Config::default(), &a);
        assert!(!opts.initial_scan);
    }
}
