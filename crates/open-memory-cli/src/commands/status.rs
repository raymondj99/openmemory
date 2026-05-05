//! `open-memory status` — open every store read-only and print a summary.

use anyhow::{Context, Result};
use open_memory_core::config::Config;
use open_memory_graph::{MemoryStatus, MemoryStore};

pub fn run(profile: &str) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        println!(
            "open-memory profile {profile:?} not initialised (no data directory at {}). \
             Run `open-memory init` first.",
            data_dir.display()
        );
        return Ok(());
    }
    let store = MemoryStore::open(&config, &data_dir)
        .with_context(|| format!("opening memory store at {}", data_dir.display()))?;
    let status = store.status().context("reading status")?;
    print_status(profile, &data_dir, &status);
    Ok(())
}

fn print_status(profile: &str, data_dir: &std::path::Path, s: &MemoryStatus) {
    println!("open-memory status");
    println!("  profile         : {profile}");
    println!("  data dir        : {}", data_dir.display());
    println!("  schema version  : {}", s.schema_version);
    println!("  entities        : {}", s.total_entities);
    println!("  observations    : {}", s.total_observations);
    println!("  relations       : {}", s.total_relations);
    println!("  tombstoned obs  : {}", s.tombstoned_observations);
    println!("  vector entries  : {}", s.vector_count);
    if let (Some(oldest), Some(newest)) = (s.oldest_observation, s.newest_observation) {
        println!("  oldest obs (ts) : {oldest}");
        println!("  newest obs (ts) : {newest}");
    }
    if !s.entity_type_counts.is_empty() {
        let mut rows: Vec<(&String, &u64)> = s.entity_type_counts.iter().collect();
        rows.sort_by_key(|(k, _)| (*k).clone());
        println!("  entity types:");
        for (k, v) in rows {
            println!("    {k:>14} : {v}");
        }
    }
    if !s.tier_counts.is_empty() {
        let mut rows: Vec<(&String, &u64)> = s.tier_counts.iter().collect();
        rows.sort_by_key(|(k, _)| (*k).clone());
        println!("  observation tiers:");
        for (k, v) in rows {
            println!("    {k:>14} : {v}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;

    #[test]
    fn status_runs_after_init() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            crate::commands::init::run("default", crate::cli::InitArgs { force: false }).unwrap();
            run("default").unwrap();
        });
    }

    #[test]
    fn status_returns_ok_on_uninitialised_profile() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            run("never-initialised").unwrap();
        });
    }
}
