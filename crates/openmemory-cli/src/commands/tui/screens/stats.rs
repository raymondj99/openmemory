//! Stats panel — watch-face layout.
//!
//! Three sections inside a single outer frame (drawn by the dispatcher
//! in `screens/mod.rs`):
//!
//! 1. **Summary** — KV rows mirroring `openmemory status` (data dir,
//!    schema version, counts, oldest/newest observation timestamps).
//!    No section heading: the summary is the implicit top of the
//!    panel, immediately under the wordmark.
//! 2. **`ACTIVITY ────`** — 14-day sparkline of observations written
//!    per day (block-character heights proportional to the peak day),
//!    today's count, and a one-line summary with the rolling average
//!    and total.
//! 3. **`RELEVANCE ────`** — Robinhood-style ticker tape over entity
//!    relevance. Each row pairs an entity name with its current
//!    composite score, an arrow + signed delta (today vs the recent
//!    average), a 14-day per-entity sparkline, and a one-word trend
//!    label. The score and ordering come from
//!    [`MemoryStore::entity_index`]; the row formatting here is
//!    purely presentational.
//!
//! Refresh: [`StatsState::refresh`] is called on launch and on every
//! [`crate::commands::tui::events::AppEvent::Tick`].

use std::path::PathBuf;

use openmemory_graph::{EntityIndexRow, MemoryStatus, MemoryStore};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::commands::tui::theme::Theme;
use crate::commands::tui::widgets::chrome::{kv_row, render_section_break};

/// Number of RELEVANCE rows we surface. Capped to keep the section
/// readable at 80 columns and the per-tick query cheap.
const RELEVANCE_LIMIT: usize = 6;

/// Trailing window for both the panel-wide ACTIVITY sparkline and the
/// per-row RELEVANCE sparklines. Two weeks is enough to see weekday
/// rhythms without making the row too long to render at 80 columns.
const SPARKLINE_DAYS: u32 = 14;

#[derive(Default)]
pub struct StatsState {
    pub data_dir: PathBuf,
    pub status: Option<MemoryStatus>,
    /// Top movers for the RELEVANCE section, highest score first. Length
    /// is bounded by [`RELEVANCE_LIMIT`].
    pub index: Vec<EntityIndexRow>,
    /// Observations created per day for the last [`SPARKLINE_DAYS`].
    /// Oldest-first; the last entry is today's running count.
    pub daily_writes: Vec<u64>,
    pub error: Option<String>,
}

impl StatsState {
    pub fn refresh(&mut self, store: &MemoryStore) {
        self.data_dir = store.data_dir().to_path_buf();
        match store.status() {
            Ok(s) => {
                self.status = Some(s);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("status: {e}")),
        }
        match store.entity_index(RELEVANCE_LIMIT, SPARKLINE_DAYS) {
            Ok(rows) => self.index = rows,
            Err(e) => {
                self.error
                    .get_or_insert_with(|| format!("entity_index: {e}"));
            }
        }
        match store.observations_per_day(SPARKLINE_DAYS) {
            Ok(buckets) => self.daily_writes = buckets,
            Err(e) => {
                self.error
                    .get_or_insert_with(|| format!("observations_per_day: {e}"));
            }
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, state: &StatsState, theme: Theme) {
    // Layout: three stacked regions, each section preceded by a
    // visible blank row + the section-break rule + a blank row so
    // adjacent sections never touch. The RELEVANCE rows fill whatever
    // height remains.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(17), // summary KV (9 rows + 8 inter-row blanks)
            Constraint::Length(1),  // blank
            Constraint::Length(1),  // ACTIVITY rule
            Constraint::Length(1),  // blank
            Constraint::Length(3),  // sparkline + meta
            Constraint::Length(1),  // blank
            Constraint::Length(1),  // RELEVANCE rule
            Constraint::Length(1),  // blank
            Constraint::Min(1),     // index rows
        ])
        .split(area);

    render_summary(frame, rows[0], state, theme);
    render_section_break(frame, rows[2], theme, "ACTIVITY");
    render_activity(frame, rows[4], state, theme);
    render_section_break(frame, rows[6], theme, "RELEVANCE");
    render_index(frame, rows[8], state, theme);
}

fn render_summary(frame: &mut Frame, area: Rect, state: &StatsState, theme: Theme) {
    let Some(s) = &state.status else {
        let body = vec![Line::from(vec![
            Span::raw("  "),
            Span::styled("loading status…", theme.muted()),
        ])];
        frame.render_widget(Paragraph::new(body), area);
        return;
    };

    // Build the KV rows first, then interleave blank lines between
    // them so the summary block reads as a list, not a packed table —
    // matching the breathing room we added to the RELEVANCE rows.
    let label_w = 14;
    let mut rows: Vec<Line> = Vec::with_capacity(10);
    rows.push(kv_row(theme, "data dir", &state.data_dir.display().to_string(), label_w));
    rows.push(kv_row(theme, "schema", &s.schema_version.to_string(), label_w));
    rows.push(kv_row(theme, "entities", &format_count(s.total_entities), label_w));
    rows.push(kv_row(theme, "observations", &format_count(s.total_observations), label_w));
    rows.push(kv_row(theme, "relations", &format_count(s.total_relations), label_w));
    rows.push(kv_row(theme, "tombstoned", &format_count(s.tombstoned_observations), label_w));
    rows.push(kv_row(theme, "vector index", &format_count(s.vector_count), label_w));
    if let Some(oldest) = s.oldest_observation {
        rows.push(kv_row(theme, "oldest obs", &format_timestamp(oldest), label_w));
    }
    if let Some(newest) = s.newest_observation {
        rows.push(kv_row(theme, "newest obs", &format_timestamp(newest), label_w));
    }
    if let Some(err) = &state.error {
        rows.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("error: {err}"), theme.danger()),
        ]));
    }

    let mut lines: Vec<Line> = Vec::with_capacity(rows.len() * 2);
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(row);
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_activity(frame: &mut Frame, area: Rect, state: &StatsState, theme: Theme) {
    if state.daily_writes.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled("no observations yet.", theme.muted()),
            ])),
            area,
        );
        return;
    }

    let sparkline = sparkline_string(&state.daily_writes);
    let today = state.daily_writes.last().copied().unwrap_or(0);
    let total: u64 = state.daily_writes.iter().sum();
    let avg = total as f64 / state.daily_writes.len() as f64;

    let label_w = 16;
    let line1 = Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{:<label_w$}", "last 14 days"), theme.muted()),
        Span::raw("  "),
        Span::styled(sparkline, theme.section()),
        Span::raw("    "),
        Span::styled("today", theme.muted()),
        Span::raw("  "),
        Span::styled(format!("{today}"), theme.fg()),
    ]);
    let line2 = Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{:<label_w$}", "per day"), theme.muted()),
        Span::raw("  "),
        Span::styled(format!("avg {avg:.1}"), theme.fg()),
        Span::styled("  ·  ", theme.border()),
        Span::styled(format!("total {total}"), theme.muted()),
    ]);
    frame.render_widget(Paragraph::new(vec![line1, Line::raw(""), line2]), area);
}

/// Build a unicode sparkline (▁▂▃▄▅▆▇█) from a series of daily counts.
/// Zero days render as a thin baseline (▁) so the row stays the same
/// visual height across the window. Quantises into 8 levels.
fn sparkline_string(buckets: &[u64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = buckets.iter().copied().max().unwrap_or(0);
    buckets
        .iter()
        .map(|v| {
            if max == 0 {
                BARS[0]
            } else {
                // Map non-zero values into bars[1..=7]; zero stays at
                // bars[0] so a quiet day reads as a thin baseline.
                if *v == 0 {
                    BARS[0]
                } else {
                    let scaled = (*v as f64 / max as f64) * (BARS.len() - 1) as f64;
                    BARS[scaled.round() as usize]
                }
            }
        })
        .collect()
}

/// Render the RELEVANCE section: a stack of "ticker" rows over the top-K
/// entities by current relevance. Each row contains:
///
/// ```text
///   {name : padded}  {score}   {arrow} {delta : signed}  {sparkline}  {label}
/// ```
///
/// The arrow + delta are computed against the rolling 7-day mean
/// (excluding today) so a quiet day on a previously-active entity
/// shows a meaningful negative delta. The label is a one-word read on
/// the trend (`hot` / `rising` / `stable` / `cooling` / `quiet`).
fn render_index(frame: &mut Frame, area: Rect, state: &StatsState, theme: Theme) {
    if state.index.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "no activity yet; remember something to populate the index.",
                    theme.muted(),
                ),
            ])),
            area,
        );
        return;
    }

    let name_w = state
        .index
        .iter()
        .map(|r| r.entity_name.chars().count())
        .max()
        .unwrap_or(8)
        .min(28);

    // Two lines per ticker (the row + a trailing blank) for the
    // breathing room a Robinhood-style ticker tape needs. The
    // available height bounds how many tickers we surface so a short
    // terminal still produces a clean layout.
    let row_capacity = (area.height as usize + 1) / 2;
    let mut lines: Vec<Line> = Vec::with_capacity(row_capacity * 2);
    for (i, row) in state.index.iter().take(row_capacity).enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(ticker_row(row, name_w, theme));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Build one RELEVANCE line. Style of the arrow + delta segment depends
/// on the sign of the delta so the visual matches Robinhood-style
/// "green up / yellow down" semantics without us reaching for the
/// success accent on rows that aren't actively rising.
fn ticker_row(row: &EntityIndexRow, name_w: usize, theme: Theme) -> Line<'static> {
    let name = truncate(&row.entity_name, name_w);
    let trend = compute_trend(row);
    let (arrow, arrow_style) = match trend.direction {
        TrendDirection::Up => ("↗", theme.success()),
        TrendDirection::Down => ("↘", theme.warn()),
        TrendDirection::Flat => ("→", theme.muted()),
    };
    let delta_text = format_delta(trend.delta);

    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{name:<name_w$}"), theme.muted()),
        Span::raw("   "),
        Span::styled(format!("{:>5.1}", row.relevance_score), theme.fg()),
        Span::raw("   "),
        Span::styled(arrow.to_string(), arrow_style),
        Span::raw(" "),
        Span::styled(format!("{delta_text:>3}"), arrow_style),
        Span::raw("    "),
        Span::styled(sparkline_string(&row.daily_writes), theme.section()),
        Span::raw("    "),
        Span::styled(trend.label.to_string(), theme.muted()),
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrendDirection {
    Up,
    Down,
    Flat,
}

#[derive(Debug, Clone, Copy)]
struct Trend {
    direction: TrendDirection,
    /// Today minus the rolling 7-day mean (excluding today), rounded.
    /// May be zero for both Up (rising history) and Flat states; the
    /// renderer formats `0` for flat and shows the signed value
    /// otherwise.
    delta: i64,
    label: &'static str,
}

fn compute_trend(row: &EntityIndexRow) -> Trend {
    let days = row.daily_writes.len();
    if days < 2 {
        return Trend {
            direction: TrendDirection::Flat,
            delta: 0,
            label: "quiet",
        };
    }
    let today = *row.daily_writes.last().unwrap() as i64;
    // Rolling average over the prior 7 days (or whatever's available),
    // excluding today's bucket so the comparison reads "today vs the
    // recent normal."
    let window = 7.min(days - 1);
    let start = days - 1 - window;
    let prior_sum: u64 = row.daily_writes[start..days - 1].iter().sum();
    let prior_mean = prior_sum as f64 / window as f64;
    let delta = (today as f64 - prior_mean).round() as i64;

    // Direction thresholds: ignore a single-write difference so a row
    // doesn't flicker between Up/Down on noisy days.
    let direction = if delta >= 1 {
        TrendDirection::Up
    } else if delta <= -1 {
        TrendDirection::Down
    } else {
        TrendDirection::Flat
    };

    let label = if today >= 3 {
        "hot"
    } else if delta >= 1 {
        "rising"
    } else if delta <= -1 {
        "cooling"
    } else if prior_sum == 0 && today == 0 {
        "quiet"
    } else {
        "stable"
    };

    Trend {
        direction,
        delta,
        label,
    }
}

fn format_delta(delta: i64) -> String {
    match delta.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{delta}"),
        std::cmp::Ordering::Less => delta.to_string(),
        std::cmp::Ordering::Equal => "0".to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Thousand-separated count rendering, matching `status` output.
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

/// Render an epoch-seconds timestamp as `YYYY-MM-DD HH:MM UTC`.
pub fn format_timestamp(secs: i64) -> String {
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

    #[test]
    fn format_count_thousands() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(1_204), "1,204");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn format_timestamp_matches_known_epoch() {
        assert_eq!(format_timestamp(1_700_000_000), "2023-11-14 22:13 UTC");
        assert_eq!(format_timestamp(0), "1970-01-01 00:00 UTC");
    }

    #[test]
    fn truncate_caps_long_strings_with_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("longer than max", 6), "longe…");
    }
}
