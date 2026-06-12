//! `?` help overlay: lists every keyboard shortcut grouped by scope.
//!
//! Rendered as a centered modal on top of the current frame. Any key
//! while it is open closes it (this is the rule enforced in
//! [`crate::commands::tui::app::App::handle_key`]).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::commands::tui::theme::Theme;

/// Render the overlay centered on `area`. Caller is responsible for
/// deciding whether to call this at all (only when `app.show_help`).
pub fn render(frame: &mut Frame, area: Rect, theme: Theme) {
    let popup = centered_rect(area, 64, 18);
    // Clear the cells under the popup so the underlying panel doesn't
    // bleed through. ratatui's `Clear` is the standard way.
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(vec![Span::styled("global", theme.section())]),
        kv(theme, "1-4", "jump to panel"),
        kv(theme, "Tab / Shift-Tab", "cycle panels"),
        kv(theme, "?", "toggle this overlay"),
        kv(theme, "q / Ctrl-C", "quit"),
        kv(theme, "Esc", "pop focus / close overlay"),
        Line::raw(""),
        Line::from(vec![Span::styled("search", theme.section())]),
        kv(theme, "Enter", "submit query"),
        kv(theme, "↑ / ↓", "navigate results"),
        kv(theme, "e", "expand selected hit"),
        kv(theme, "Ctrl-R", "filter history"),
        Line::raw(""),
        Line::from(vec![Span::styled("graph", theme.section())]),
        kv(theme, "Enter", "refocus on selected neighbor"),
        kv(theme, "/", "jump-to entity"),
        kv(theme, "+ / -", "increase / decrease depth"),
        Line::raw(""),
        Line::from(vec![Span::styled("models", theme.section())]),
        kv(theme, "Enter", "switch active model (with confirm)"),
        kv(theme, "i", "show details"),
        kv(theme, "w", "show weights path"),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title(Span::styled(" help ", theme.accent()));
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn kv<'a>(theme: Theme, key: &'a str, action: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{key:<18}"), theme.fg()),
        Span::styled(action, theme.muted()),
    ])
}

/// Centered rectangle of fixed character dimensions (clamped to the
/// available area so a too-small terminal still renders something
/// sensible).
fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(w)) / 2),
            Constraint::Length(w),
            Constraint::Min(0),
        ])
        .split(area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(h)) / 2),
            Constraint::Length(h),
            Constraint::Min(0),
        ])
        .split(horizontal[1]);
    vertical[1]
}
