//! Outer chrome for the "watch face" layout.
//!
//! Each panel renders its own outer rounded frame; the wordmark and
//! profile note live as the first body row inside that frame, and
//! section breaks within the panel are right-aligned tickmarks rather
//! than nested boxes.
//!
//! Layout:
//!
//! ```text
//! ╭─ OPENMEMORY v0.4.4 ──────────────────────────────── stats ─╮
//! │                                                            │
//! │   …panel body…                                             │
//! │                                                            │
//! │  ACTIVITY  ──────────────────────────────────────────────  │   ← section break
//! │                                                            │
//! │   …more body…                                              │
//! ╰────────────────────────────────────────────────────────────╯
//!   1 stats · 2 search · 3 graph · 4 models    q quit · ? help    ← status bar
//! ```
//!
//! Panel renderers call [`split_screen`] once to get `(body, status)`
//! sub-rects, build the titled frame via [`panel_frame`], render their
//! content into the block's inner area, then call
//! [`render_status_bar`] for the bottom row. The wordmark + version
//! ride on the top-left of the border (Claude-Code style); the
//! current panel name rides on the top-right.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::commands::tui::app::Panel;
use crate::commands::tui::theme::Theme;

/// Split the full frame area into (body, status). Body gets all rows
/// except the last; status is the bottom row.
pub fn split_screen(area: Rect) -> [Rect; 2] {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    [chunks[0], chunks[1]]
}

/// Build the outer rounded frame each panel uses as its container,
/// with the wordmark + crate version riding on the top-left of the
/// border (Claude-Code style) and the current panel name on the
/// top-right. Caller is responsible for placing it
/// (`frame.render_widget(block, area)`) and for asking the block for
/// its inner area (`block.inner(area)`).
pub fn panel_frame(theme: Theme, panel: Panel) -> Block<'static> {
    let left_title = Line::from(vec![
        Span::styled(" OPENMEMORY", theme.accent()),
        Span::raw(" "),
        Span::styled(format!("v{} ", env!("CARGO_PKG_VERSION")), theme.muted()),
    ]);
    let right_title =
        Line::from(vec![Span::styled(format!(" {} ", panel.title()), theme.section())])
            .right_aligned();

    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title_top(left_title)
        .title_top(right_title)
}

/// Render a left-aligned section break: an uppercase label followed
/// by a hairline that fills the rest of the row. Sits on its own row
/// inside the panel frame; callers reserve one row above and one row
/// below for breathing room.
pub fn render_section_break(frame: &mut Frame, area: Rect, theme: Theme, label: &str) {
    let upper = label.to_uppercase();
    // Two-char gutter left + label + two-char gap before the rule +
    // rule + two-char gutter right.
    let used = 2 + upper.chars().count() + 2;
    let rule_len = (area.width as usize)
        .saturating_sub(used)
        .saturating_sub(2);
    let spans = vec![
        Span::raw("  "),
        Span::styled(upper, theme.section()),
        Span::raw("  "),
        Span::styled("─".repeat(rule_len.max(4)), theme.border()),
        Span::raw("  "),
    ];
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        area,
    );
}

/// Render the bottom status bar: panel chips on the left, global hints
/// on the right. Active panel is painted in section accent (cyan); the
/// rest dim.
pub fn render_status_bar(frame: &mut Frame, area: Rect, theme: Theme, panel: Panel) {
    let mut left: Vec<Span> = Vec::with_capacity(20);
    left.push(Span::raw("  "));
    for (i, p) in Panel::ALL.iter().enumerate() {
        let is_active = *p == panel;
        let style = if is_active {
            theme.section()
        } else {
            theme.muted()
        };
        if i > 0 {
            left.push(Span::styled("  ·  ", theme.border()));
        }
        left.push(Span::styled(format!("{} ", i + 1), theme.muted()));
        left.push(Span::styled(p.title().to_string(), style));
    }

    let right: Vec<Span> = vec![
        Span::styled("? ", theme.muted()),
        Span::styled("help", theme.section()),
        Span::styled("  ·  ", theme.border()),
        Span::styled("q ", theme.muted()),
        Span::styled("quit", theme.section()),
        Span::raw("  "),
    ];

    let left_w: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let pad = (area.width as usize)
        .saturating_sub(left_w)
        .saturating_sub(right_w)
        .max(1);
    let mut line = left;
    line.push(Span::raw(" ".repeat(pad)));
    line.extend(right);

    frame.render_widget(Paragraph::new(Line::from(line)), area);
}

/// Render a left-aligned kv pair on a single row inside a panel.
/// Used by Stats and Models for their KV summary blocks. Label is
/// left-aligned to `label_width`; value follows two spaces later in
/// default fg (numerals lean prominent in the watch-face aesthetic).
pub fn kv_row(theme: Theme, label: &str, value: &str, label_width: usize) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{label:<label_width$}"), theme.muted()),
        Span::raw("  "),
        Span::styled(value.to_string(), theme.fg()),
    ])
}


#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn section_break_places_label_left_then_rule() {
        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_section_break(frame, Rect::new(0, 0, 40, 1), Theme, "types");
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut row = String::new();
        for x in 0..40 {
            row.push_str(buf[(x, 0)].symbol());
        }
        // Label sits left after the 2-space gutter, uppercase, no
        // intra-letter spaces.
        assert!(row.starts_with("  TYPES"), "row: {row:?}");
        // Rule extends to the right edge.
        assert!(row.contains("──"), "row: {row:?}");
    }
}
