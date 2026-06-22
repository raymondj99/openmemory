//! `openmemory status` — open every store read-only and print a summary.

use std::io::Write;

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_engine::partition::DomainStore;
use openmemory_graph::MemoryStatus;

use crate::ui::banner::{Banner, Line};
use crate::ui::stdout_stream;
use crate::ui::table::KvTable;

pub fn run(profile: &str) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    let mut stream = stdout_stream();

    if !data_dir.exists() {
        Banner::new("status")
            .subtitle(format!("profile: {profile}"))
            .line(Line::Body("profile not initialised on disk.".into()))
            .line(Line::Muted(format!("expected at {}", data_dir.display())))
            .line(Line::Blank)
            .line(Line::Body("run `openmemory init` to bootstrap.".into()))
            .render(&mut stream);
        return Ok(());
    }
    let store = DomainStore::open_existing(&config, &data_dir)
        .with_context(|| format!("opening memory store at {}", data_dir.display()))?;
    let status = store.status().context("reading status")?;
    render_status(&mut stream, profile, &data_dir, &status, store.domains());
    Ok(())
}

fn render_status<W: Write>(
    w: &mut W,
    profile: &str,
    data_dir: &std::path::Path,
    s: &MemoryStatus,
    domains: usize,
) {
    Banner::new("status")
        .subtitle(format!("profile: {profile}"))
        .render_header(w);
    let _ = writeln!(w);

    let mut table = KvTable::new()
        .row("data dir", data_dir.display().to_string())
        .row("schema vers.", s.schema_version.to_string());
    if domains > 1 {
        table = table.row("domains", domains.to_string());
    }
    table = table
        .row("entities", format_count(s.total_entities))
        .row("observations", format_count(s.total_observations))
        .row("relations", format_count(s.total_relations))
        .row("tombstoned", format_count(s.tombstoned_observations))
        .row("vector index", format_count(s.vector_count));

    if let (Some(oldest), Some(newest)) = (s.oldest_observation, s.newest_observation) {
        table = table
            .row("oldest obs", format_timestamp(oldest))
            .row("newest obs", format_timestamp(newest));
    }
    table.render(w);

    if !s.entity_type_counts.is_empty() {
        let mut rows: Vec<(&String, &u64)> = s.entity_type_counts.iter().collect();
        rows.sort_by_key(|(k, _)| (*k).clone());
        let mut t = KvTable::new();
        t = t.blank().heading("entity types");
        for (k, v) in rows {
            t = t.row(k.clone(), format_count(*v));
        }
        t.render(w);
    }

    if !s.tier_counts.is_empty() {
        let mut rows: Vec<(&String, &u64)> = s.tier_counts.iter().collect();
        rows.sort_by_key(|(k, _)| (*k).clone());
        let mut t = KvTable::new();
        t = t.blank().heading("observation tiers");
        for (k, v) in rows {
            t = t.row(k.clone(), format_count(*v));
        }
        t.render(w);
    }
}

/// Render a count with thousands separators (e.g. `1,204`).
fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

/// Format an epoch-seconds timestamp as `YYYY-MM-DD HH:MM UTC`. We
/// hand-roll the calendar arithmetic to avoid pulling `chrono`/`time`
/// into the CLI for one cosmetic feature. The `UTC` suffix is
/// explicit so users don't read a timestamp 8 hours off from their
/// wall clock and assume it's their local time.
fn format_timestamp(secs: i64) -> String {
    if secs < 0 {
        return secs.to_string();
    }
    let secs = secs as u64;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02} UTC")
}

/// Convert a count of days since the Unix epoch to a (year, month,
/// day) tuple in the proleptic Gregorian calendar. Adapted from
/// Howard Hinnant's "date" algorithm.
fn civil_from_days(z: u64) -> (i32, u32, u32) {
    let z = z as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64 + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
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

    #[test]
    fn format_count_inserts_thousands_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(1_204), "1,204");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn format_timestamp_matches_known_epoch() {
        // 1700000000 = 2023-11-14 22:13:20 UTC. We render to minute
        // precision and drop seconds.
        assert_eq!(format_timestamp(1_700_000_000), "2023-11-14 22:13 UTC");
        assert_eq!(format_timestamp(0), "1970-01-01 00:00 UTC");
    }
}
