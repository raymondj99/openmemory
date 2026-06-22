//! `openmemory migrate-domains` — re-home a profile to a different
//! storage-domain count.
//!
//! Thin adapter over [`openmemory_engine::migrate::migrate_domains`],
//! which owns the staging build, raw-count verification, and the
//! sentinel-guarded two-phase swap. The CLI's job is the confirmation
//! gate (the profile must be offline) and rendering the report.

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_engine::migrate::migrate_domains;
use openmemory_engine::partition::DomainStore;

use crate::cli::MigrateDomainsArgs;
use crate::ui::banner::Banner;
use crate::ui::stdout_stream;
use crate::ui::table::KvTable;

pub fn run(profile: &str, args: &MigrateDomainsArgs) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        anyhow::bail!(
            "openmemory profile {profile:?} not initialised (no data directory at {}). \
             Run `openmemory init` first.",
            data_dir.display()
        );
    }
    let from = DomainStore::manifest_domains(&data_dir).context("reading domain manifest")?;
    if !args.yes {
        anyhow::bail!(
            "this migrates profile {profile:?} from {from} to {} domain(s) and requires \
             exclusive access (stop any MCP server, watcher, or TUI first). \
             Re-run with --yes to proceed; the old layout is kept as a backup.",
            args.domains.max(1),
        );
    }

    let report = migrate_domains(&config, &data_dir, args.domains).context("migrating domains")?;

    let mut stream = stdout_stream();
    Banner::new("migrate-domains")
        .subtitle(format!("profile: {profile}"))
        .render_header(&mut stream);
    let _ = std::io::Write::write_all(&mut stream, b"\n");
    KvTable::new()
        .row(
            "domains",
            format!("{} -> {}", report.from_domains, report.to_domains),
        )
        .row("entities", report.entities.to_string())
        .row("observations", report.observations.to_string())
        .row("relations", report.relations.to_string())
        .row("index entries", report.index_entries.to_string())
        .row("source records", report.source_records.to_string())
        .blank()
        .heading("partition bookkeeping")
        .row(
            "stubs",
            format!(
                "{} dropped, {} created",
                report.stubs_dropped, report.stubs_created
            ),
        )
        .row(
            "mirrors",
            format!(
                "{} dropped, {} created",
                report.mirrors_dropped, report.mirrors_created
            ),
        )
        .row(
            "orphaned index rows",
            report.orphaned_index_entries_dropped.to_string(),
        )
        .blank()
        .row("backup", report.backup_dir.display().to_string())
        .render(&mut stream);
    println!("\nverify the profile, then remove the backup directory to reclaim space");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;

    #[test]
    fn migrate_requires_yes() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            crate::commands::init::run("default", crate::cli::InitArgs { force: false }).unwrap();
            let err = run(
                "default",
                &MigrateDomainsArgs {
                    domains: 4,
                    yes: false,
                },
            )
            .unwrap_err();
            assert!(err.to_string().contains("--yes"), "got: {err}");
        });
    }

    #[test]
    fn migrate_round_trips_a_profile_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            crate::commands::init::run("default", crate::cli::InitArgs { force: false }).unwrap();
            // Seed through the scriptable path.
            crate::commands::scriptable::remember(
                "default",
                crate::cli::RememberArgs {
                    entity: "alpha".into(),
                    entity_type: "project".into(),
                    observations: vec!["survives migration".into()],
                    relation: vec![],
                    source: None,
                    json: true,
                },
            )
            .unwrap();

            run(
                "default",
                &MigrateDomainsArgs {
                    domains: 4,
                    yes: true,
                },
            )
            .unwrap();

            let config = Config::load().unwrap_or_default();
            let data_dir = Config::data_dir("default").unwrap();
            let store = DomainStore::open_existing(&config, &data_dir).unwrap();
            assert_eq!(store.domains(), 4);
            assert!(store.get_entity("alpha").unwrap().is_some());
        });
    }
}
