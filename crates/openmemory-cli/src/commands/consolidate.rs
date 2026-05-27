//! `openmemory consolidate` — run the dedup + decay-prune pipeline once.

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_graph::{ConsolidateConfig, MemoryStore};

use crate::cli::ConsolidateArgs;
use crate::ui::banner::Banner;
use crate::ui::stdout_stream;
use crate::ui::table::KvTable;

pub fn run(profile: &str, args: ConsolidateArgs) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        anyhow::bail!(
            "openmemory profile {profile:?} not initialised (no data directory at {}). \
             Run `openmemory init` first.",
            data_dir.display()
        );
    }
    let store = MemoryStore::open(&config, &data_dir)
        .with_context(|| format!("opening memory store at {}", data_dir.display()))?;
    let mut cfg = ConsolidateConfig::from_config(&config);
    if let Some(t) = args.dedup_threshold {
        cfg.dedup_text_threshold = t.clamp(0.0, 1.0);
    }
    if let Some(p) = args.prune_floor {
        cfg.prune_floor = p;
    }
    if let Some(a) = args.min_age_secs {
        cfg.min_age_secs = a.max(0);
    }

    let report = store.consolidate(&cfg).context("consolidate run failed")?;

    let mut stream = stdout_stream();
    Banner::new("consolidate")
        .subtitle(format!("profile: {profile}"))
        .render_header(&mut stream);
    let _ = std::io::Write::write_all(&mut stream, b"\n");

    KvTable::new()
        .row("duplicates merged", report.duplicates_merged.to_string())
        .row(
            "observations pruned",
            report.observations_pruned.to_string(),
        )
        .row("entities pruned", report.entities_pruned.to_string())
        .blank()
        .heading("config")
        .row(
            "dedup threshold",
            format!("{:.2}", cfg.dedup_text_threshold),
        )
        .row("prune floor", format!("{:.2}", cfg.prune_floor))
        .row("min age secs", cfg.min_age_secs.to_string())
        .render(&mut stream);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;
    use openmemory_graph::ObservationInput;

    #[test]
    fn consolidate_runs_on_empty_store_after_init() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            crate::commands::init::run("default", crate::cli::InitArgs { force: false }).unwrap();
            run(
                "default",
                ConsolidateArgs {
                    dedup_threshold: None,
                    prune_floor: None,
                    min_age_secs: None,
                },
            )
            .unwrap();
        });
    }

    #[test]
    fn consolidate_dedups_real_data() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            crate::commands::init::run("default", crate::cli::InitArgs { force: false }).unwrap();
            // Insert duplicate observations directly via the store.
            let cfg =
                openmemory_core::config::Config::load_from(dir.path().join("config.toml")).unwrap();
            let data_dir = dir.path().join("data").join("default");
            let store = MemoryStore::open(&cfg, &data_dir).unwrap();
            store
                .remember(
                    "X",
                    openmemory_graph::EntityType::Fact,
                    &[
                        ObservationInput::new("hello world"),
                        ObservationInput::new("hello world"),
                    ],
                    &[],
                    "t",
                )
                .unwrap();
            drop(store);
            run(
                "default",
                ConsolidateArgs {
                    dedup_threshold: None,
                    prune_floor: None,
                    min_age_secs: None,
                },
            )
            .unwrap();
        });
    }

    #[test]
    fn consolidate_errors_on_uninitialised_profile() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            let err = run(
                "never-initialised",
                ConsolidateArgs {
                    dedup_threshold: None,
                    prune_floor: None,
                    min_age_secs: None,
                },
            )
            .unwrap_err();
            assert!(err.to_string().contains("not initialised"));
        });
    }
}
