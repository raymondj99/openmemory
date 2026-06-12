//! TestBackend snapshots for the TUI screens.
//!
//! These tests render each screen into a fixed-size buffer using
//! [`ratatui::backend::TestBackend`] and assert on layout invariants:
//! every row is the right width, chrome appears at the expected rows,
//! and the panel under test has its title visible.
//!
//! The tests deliberately avoid byte-for-byte snapshot comparison
//! (`insta`-style golden files): the goal is to catch layout
//! regressions, not to forbid every minor wording change.
//!
//! Coverage in PR1: Stats. PR2/3/4 add their respective panels here.

#![cfg(feature = "tui")]

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::Terminal;

/// Pin every test to these three terminal sizes, mirroring the plan.
const SIZES: [(u16, u16); 3] = [(80, 24), (100, 30), (160, 48)];

fn render_at<F>(width: u16, height: u16, draw: F) -> Buffer
where
    F: FnOnce(&mut ratatui::Frame),
{
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| draw(frame))
        .expect("draw should succeed");
    terminal.backend().buffer().clone()
}

/// Plain-text extraction so we can grep the rendered cells.
fn to_string(buf: &Buffer) -> String {
    let area = buf.area();
    let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Find the first row index containing `needle`. Used by row-position
/// invariants so we don't pin the exact y coordinate (which would
/// brittle the test against benign chrome tweaks).
fn row_containing(s: &str, needle: &str) -> Option<usize> {
    s.lines().position(|l| l.contains(needle))
}

mod chrome {
    use super::*;
    use openmemory_cli::commands::tui::app::Panel;
    use openmemory_cli::commands::tui::theme::Theme;
    use openmemory_cli::commands::tui::widgets::chrome;

    /// Mini fixture: render the watch-face frame, in-frame header, a
    /// left-aligned section break, and the bottom status bar. Mirrors
    /// what the top-level `screens::draw` dispatcher does in
    /// production.
    fn render_chrome(width: u16, height: u16) -> String {
        let buf = super::render_at(width, height, |frame| {
            let area = frame.area();
            let theme = Theme;
            let [body, status] = chrome::split_screen(area);
            // Titled frame (wordmark + profile ride on the border)
            // plus a section break a few rows down so the test
            // exercises every chrome primitive in one frame.
            let outer = chrome::panel_frame(theme, Panel::Stats);
            let inner = outer.inner(body);
            frame.render_widget(outer, body);

            let rows = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Length(1),
                    ratatui::layout::Constraint::Length(1),
                    ratatui::layout::Constraint::Min(1),
                ])
                .split(inner);
            chrome::render_section_break(frame, rows[1], theme, "TYPES");
            chrome::render_status_bar(frame, status, theme, Panel::Stats);
        });
        super::to_string(&buf)
    }

    #[test]
    fn chrome_renders_title_tabs_and_status_at_every_size() {
        for (w, h) in super::SIZES {
            let out = render_chrome(w, h);
            // Wordmark + crate version ride on the top border now.
            assert!(out.contains("OPENMEMORY"), "[{w}x{h}] missing title: {out}");
            assert!(
                out.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))),
                "[{w}x{h}] missing version chip: {out}"
            );
            // All four panel chips in the status bar.
            for label in ["stats", "search", "graph", "models"] {
                assert!(
                    out.contains(label),
                    "[{w}x{h}] missing tab {label:?}: {out}"
                );
            }
            // Status hints.
            assert!(out.contains("quit"), "[{w}x{h}] missing quit hint: {out}");
            assert!(out.contains("help"), "[{w}x{h}] missing help hint: {out}");
            // Left-aligned section break with the uppercase label.
            assert!(
                out.contains("TYPES"),
                "[{w}x{h}] missing tickmark label: {out}"
            );

            // The wordmark now rides on the top border (Claude-Code
            // style), so OPENMEMORY shares row 0 with the rounded
            // top-left corner `╭` rather than living below it.
            assert_eq!(
                row_containing(&out, "OPENMEMORY"),
                Some(0),
                "[{w}x{h}] title should be on row 0 (in the top border)"
            );
            let lines: Vec<&str> = out.lines().collect();
            assert!(
                lines[lines.len() - 1].contains("quit"),
                "[{w}x{h}] status bar should be on last row"
            );
        }
    }
}

mod graph {
    use super::*;
    use openmemory_cli::commands::tui::screens::graph::{self, GraphState};
    use openmemory_cli::commands::tui::theme::Theme;

    fn render_graph(width: u16, height: u16) -> String {
        let buf = super::render_at(width, height, |frame| {
            let theme = Theme;
            let state = GraphState::default();
            graph::render(frame, Rect::new(0, 0, width, height), &state, theme);
        });
        super::to_string(&buf)
    }

    #[test]
    fn graph_panel_renders_empty_state_at_every_size() {
        // Graph no longer wears its own inner box (the outer frame in
        // the dispatcher carries the panel name in the header). The
        // empty-state hint is the panel body for an unseeded graph.
        for (w, h) in super::SIZES {
            let out = render_graph(w, h);
            assert!(
                out.contains("no entities yet"),
                "[{w}x{h}] missing empty-state hint: {out}"
            );
        }
    }
}

mod models {
    use super::*;
    use openmemory_cli::commands::tui::screens::models::{self, ModelsState};
    use openmemory_cli::commands::tui::theme::Theme;

    fn render_models(width: u16, height: u16) -> String {
        let buf = super::render_at(width, height, |frame| {
            let theme = Theme;
            let state = ModelsState::new();
            models::render(frame, Rect::new(0, 0, width, height), &state, theme);
        });
        super::to_string(&buf)
    }

    #[test]
    fn models_panel_renders_registry_at_every_size() {
        // Watch-face Models panel has no inner box: the outer frame
        // (drawn by the dispatcher) carries the chrome. The panel
        // body renders the registry rows directly.
        for (w, h) in super::SIZES {
            let out = render_models(w, h);
            // Default registry includes nomic-embed-text-v1.5.
            assert!(
                out.contains("nomic-embed-text-v1.5"),
                "[{w}x{h}] missing default model row: {out}"
            );
            // Active/peer marker text is visible.
            assert!(
                out.contains("active") || out.contains("downloaded") || out.contains("not downloaded"),
                "[{w}x{h}] missing model status suffix: {out}"
            );
        }
    }
}

mod search {
    use super::*;
    use openmemory_cli::commands::tui::screens::search::{self, SearchState};
    use openmemory_cli::commands::tui::theme::Theme;

    fn render_search(width: u16, height: u16) -> String {
        let buf = super::render_at(width, height, |frame| {
            let theme = Theme;
            let state = SearchState::default();
            search::render(frame, Rect::new(0, 0, width, height), &state, theme);
        });
        super::to_string(&buf)
    }

    #[test]
    fn search_panel_shows_two_section_tickmarks_at_every_size() {
        // Watch-face Search: input + status is the implicit top of
        // the panel (no tickmark), then `── RESULTS` and
        // `── HISTORY` mark the two lower sections.
        for (w, h) in super::SIZES {
            let out = render_search(w, h);
            for spaced in ["RESULTS", "HISTORY"] {
                assert!(
                    out.contains(spaced),
                    "[{w}x{h}] missing tickmark {spaced:?}: {out}"
                );
            }
            // Empty input + live-search prompt copy.
            assert!(
                out.contains("start typing"),
                "[{w}x{h}] missing live-search hint: {out}"
            );
            assert!(
                out.contains("type at least"),
                "[{w}x{h}] missing min-chars hint in empty hits: {out}"
            );
            assert!(
                out.contains("no history yet"),
                "[{w}x{h}] missing history hint: {out}"
            );
        }
    }

    #[test]
    fn search_panel_filter_strip_renders_at_top() {
        let out = render_search(100, 30);
        let filter_row = row_containing(&out, "filter")
            .expect("filter strip row");
        let prompt = row_containing(&out, "start typing").expect("input prompt row");
        let r = row_containing(&out, "RESULTS").expect("results tickmark row");
        assert!(
            filter_row < prompt,
            "filter strip should be above input prompt: filter={filter_row}, prompt={prompt}"
        );
        assert!(
            prompt < r,
            "input prompt should be above RESULTS tickmark: prompt={prompt}, r={r}"
        );
    }
}

mod stats {
    use super::*;
    use openmemory_cli::commands::tui::screens::stats::{self, StatsState};
    use openmemory_cli::commands::tui::theme::Theme;

    /// Stats with a `None` status renders the "reading status..."
    /// placeholder. We use this fixture for layout invariants since
    /// constructing a real `MemoryStore` from inside a unit test would
    /// pull in tempdir + the full graph crate; the integration test
    /// in `cli_output.rs` exercises the live data path.
    fn render_stats(width: u16, height: u16) -> String {
        let buf = super::render_at(width, height, |frame| {
            let theme = Theme;
            let state = StatsState::default();
            // Use the full frame area; we're only checking the panel
            // body in this sub-module.
            stats::render(frame, Rect::new(0, 0, width, height), &state, theme);
        });
        super::to_string(&buf)
    }

    #[test]
    fn stats_panel_renders_two_section_breaks_at_every_size() {
        // Watch-face layout: the summary is the implicit top of the
        // panel (no section break), then `ACTIVITY` (sparkline) and
        // `RELEVANCE` (ticker tape) mark the two lower sections.
        for (w, h) in super::SIZES {
            let out = render_stats(w, h);
            for label in ["ACTIVITY", "RELEVANCE"] {
                assert!(
                    out.contains(label),
                    "[{w}x{h}] missing section break {label:?}: {out}"
                );
            }
        }
    }

    #[test]
    fn stats_panel_summary_appears_above_activity_break() {
        let out = render_stats(100, 30);
        // "loading status…" placeholder is at the top of the summary
        // region; the ACTIVITY section break lives below it.
        let summary = row_containing(&out, "loading status").expect("summary row");
        let activity = row_containing(&out, "ACTIVITY").expect("ACTIVITY break row");
        assert!(
            summary < activity,
            "summary should be above ACTIVITY break: summary={summary}, activity={activity}"
        );
    }

    #[test]
    fn empty_state_messages_are_present_when_no_data() {
        // No status loaded -> "loading status…" placeholder in the
        // summary section; the tickmarks still render below.
        let out = render_stats(80, 24);
        assert!(out.contains("loading status"), "{out}");
    }

    /// Render with a synthetic `MemoryStatus`, synthetic daily-writes,
    /// and synthetic RELEVANCE rows so the test exercises the populated
    /// path through every section. Guards against regressions where
    /// an empty-state placeholder accidentally runs for valid data,
    /// and verifies the RELEVANCE ticker columns render.
    #[test]
    fn populated_status_renders_counts_sparkline_and_index() {
        use openmemory_graph::{EntityIndexRow, EntityType, MemoryStatus};
        use std::collections::HashMap;
        let status = MemoryStatus {
            total_entities: 22,
            total_observations: 154,
            total_relations: 9,
            tombstoned_observations: 1,
            schema_version: 2,
            oldest_observation: Some(1_700_000_000),
            newest_observation: Some(1_748_000_000),
            entity_type_counts: HashMap::new(),
            tier_counts: HashMap::new(),
            vector_count: 154,
            reader_pool_size: 4,
        };
        let index = vec![
            EntityIndexRow {
                entity_id: "id-a".into(),
                entity_name: "Raymond".into(),
                entity_type: EntityType::Person,
                daily_writes: vec![0, 0, 1, 0, 1, 2, 1, 0, 1, 2, 3, 2, 4, 5],
                today_observations: 5,
                total_in_window: 22,
                relevance_score: 12.4,
            },
            EntityIndexRow {
                entity_id: "id-b".into(),
                entity_name: "openmemory".into(),
                entity_type: EntityType::Project,
                // Strong recent activity then a quiet today -> delta
                // pushes negative, label becomes `cooling`.
                daily_writes: vec![0, 0, 0, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 0],
                today_observations: 0,
                total_in_window: 22,
                relevance_score: 2.7,
            },
        ];
        let state = StatsState {
            status: Some(status),
            data_dir: std::path::PathBuf::from("/Users/test/.openmemory/data/default"),
            index,
            daily_writes: vec![1, 2, 1, 5, 3, 8, 5, 3, 1, 5, 7, 8, 5, 8],
            error: None,
        };
        let buf = super::render_at(100, 30, |frame| {
            stats::render(frame, Rect::new(0, 0, 100, 30), &state, Theme);
        });
        let out = super::to_string(&buf);
        // KV summary
        assert!(out.contains("22"), "missing entities count: {out}");
        assert!(out.contains("154"), "missing observations count: {out}");
        // Sparkline copy
        assert!(out.contains("last 14 days"), "missing sparkline label: {out}");
        assert!(out.contains("today"), "missing today label: {out}");
        // Per-day summary
        assert!(out.contains("per day"), "missing per-day row: {out}");
        assert!(out.contains("total 62"), "missing total: {out}");
        // RELEVANCE ticker — entity name, score, and a trend label all visible.
        assert!(out.contains("Raymond"), "missing index entity Raymond: {out}");
        assert!(out.contains("12.4"), "missing relevance score 12.4: {out}");
        // First row should be tagged hot (5 obs today >= 3 threshold)
        // and the second should read cooling (today=0 vs prior > 0).
        assert!(
            out.contains("hot") && out.contains("cooling"),
            "missing trend labels: {out}"
        );
    }
}
