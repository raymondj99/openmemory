//! Per-panel screens.
//!
//! Each panel exposes a `*State` struct plus a
//! `render(frame, inner, state, theme)` free function. The top-level
//! [`draw`] dispatcher wraps each panel in a single outer rounded
//! frame, paints the wordmark + profile header into the first inner
//! row, and lets the panel fill the rest with section-tickmark
//! separated content. The status bar (panel chips + global hints)
//! lives outside the frame, on the bottom row of the terminal.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Frame;

use super::app::{App, Panel};
use super::widgets::{chrome, help_overlay};

pub mod entity_detail;
pub mod graph;
pub mod graph_layout;
pub mod models;
pub mod search;
pub mod stats;

pub fn draw(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let [body, status] = chrome::split_screen(area);

    // Outer rounded frame around the active panel. The wordmark +
    // version ride on the top-left of the border (Claude-Code style)
    // and the current panel name on the top-right, so the entire
    // inner area is available for content; we only reserve a single
    // blank row at the top for breathing room.
    let outer = chrome::panel_frame(app.theme, app.panel);
    let inner = outer.inner(body);
    frame.render_widget(outer, body);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // blank
            Constraint::Min(1),    // panel body
        ])
        .split(inner);

    match app.panel {
        Panel::Stats => stats::render(frame, rows[1], &app.stats, app.theme),
        Panel::Search => search::render(frame, rows[1], &app.search, app.theme),
        Panel::Graph => graph::render(frame, rows[1], &app.graph, app.theme),
        Panel::Models => models::render(frame, rows[1], &app.models, app.theme),
    }

    chrome::render_status_bar(frame, status, app.theme, app.panel);

    if let Some(detail) = &app.entity_detail {
        entity_detail::render(frame, area, detail, app.theme);
    }

    if app.show_help {
        help_overlay::render(frame, area, app.theme);
    }
}
