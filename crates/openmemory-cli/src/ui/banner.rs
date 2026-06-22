//! Rounded banner box.
//!
//! ```text
//! ╭─ title ───────────────────── subtitle ─╮
//! │                                        │
//! │   body line one                        │
//! │   body line two                        │
//! │                                        │
//! ╰────────────────────────────────────────╯
//! ```
//!
//! Width is clamped to `min(terminal_width, 78) - 0` and bounded below
//! by 40. If the resolved width can't host both the title and
//! subtitle, the subtitle is dropped (then truncated; we never wrap).
//! For terminals narrower than 40 we fall back to a bare title
//! underline with no border.

use std::io::Write;

use super::glyph::Glyph;
use super::style::{ACCENT, ACCENT_DIM, BORDER, MUTED};
use super::{paint, term_width, writeln_swallow};

/// Blank rows of padding above the first body line and below the last,
/// inside the box. Two rows gives the Braun "museum label" breathing
/// room without looking like a forgotten draft.
const BODY_PADDING_ROWS: usize = 2;

/// Internal default. Banner is meant to feel intimate; we don't grow
/// it to the full width of a 200-column terminal.
const PREFERRED_WIDTH: u16 = 72;
/// Below this width we drop the box and render a flat heading.
const MIN_BOX_WIDTH: u16 = 40;

/// Body line classifications. Each variant maps to a specific style and
/// layout; callers describe intent, not ANSI.
#[derive(Debug, Clone)]
pub enum Line {
    /// Blank padding line.
    Blank,
    /// Plain body text, default fg.
    Body(String),
    /// Dim text, used for paths, hints, "try this" snippets.
    Muted(String),
    /// Two-column row (`label`, muted `value`). Used for key/value
    /// pairs like `home  ~/.openmemory`.
    Pair { label: String, value: String },
}

/// Builder for a single banner.
#[derive(Debug, Clone)]
pub struct Banner {
    title: String,
    subtitle: Option<String>,
    lines: Vec<Line>,
    /// Explicit width override. `None` means "ask the terminal".
    width: Option<u16>,
}

impl Banner {
    /// Create a banner with the given title text.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            lines: Vec::new(),
            width: None,
        }
    }

    /// Set a right-aligned subtitle (e.g. version, profile name).
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    /// Pin the width. Tests set this so the output is independent of
    /// the terminal the harness happens to run in.
    #[cfg(test)]
    pub fn width(mut self, w: u16) -> Self {
        self.width = Some(w);
        self
    }

    /// Append a body line.
    pub fn line(mut self, line: Line) -> Self {
        self.lines.push(line);
        self
    }

    /// Render to the writer. Errors are swallowed (broken-pipe safety).
    pub fn render<W: Write>(&self, w: &mut W) {
        let width = self
            .width
            .unwrap_or_else(|| term_width().min(PREFERRED_WIDTH));
        if width < MIN_BOX_WIDTH {
            self.render_flat(w, width);
        } else {
            self.render_boxed(w, width);
        }
    }

    /// Render only the top edge as a lightweight section header, with
    /// no body and no closing edge. Use this when the section content
    /// is a [`KvTable`](super::table::KvTable) that does its own
    /// padding; a full box would feel boxed-in.
    pub fn render_header<W: Write>(&self, w: &mut W) {
        let width = self
            .width
            .unwrap_or_else(|| term_width().min(PREFERRED_WIDTH));
        if width < MIN_BOX_WIDTH {
            let edge = Glyph::EdgeHorizontal.as_str().repeat(2);
            let painted_edge = paint(BORDER, &edge);
            writeln_swallow(
                w,
                &format!(
                    "{painted_edge} {} {painted_edge}",
                    paint(ACCENT, &self.title)
                ),
            );
            return;
        }
        let inner = (width as usize).saturating_sub(2);
        writeln_swallow(w, &self.top_edge(inner));
    }

    fn render_flat<W: Write>(&self, w: &mut W, _width: u16) {
        // Tiny terminal: render a flat heading. Better than a broken box.
        let edge = Glyph::EdgeHorizontal.as_str().repeat(2);
        let painted_edge = paint(BORDER, &edge);
        writeln_swallow(
            w,
            &format!(
                "{painted_edge} {} {painted_edge}",
                paint(ACCENT, &self.title)
            ),
        );
        for line in &self.lines {
            match line {
                Line::Blank => writeln_swallow(w, ""),
                Line::Body(s) => writeln_swallow(w, &format!("  {s}")),
                Line::Muted(s) => writeln_swallow(w, &format!("  {}", paint(MUTED, s))),
                Line::Pair { label, value } => {
                    writeln_swallow(w, &format!("  {label}  {}", paint(MUTED, value)));
                }
            }
        }
    }

    fn render_boxed<W: Write>(&self, w: &mut W, width: u16) {
        // Minus two corners.
        let inner = (width as usize).saturating_sub(2);
        // ── Top edge ──
        writeln_swallow(w, &self.top_edge(inner));

        // ── Body ──
        // Pad with `BODY_PADDING_ROWS` blank lines above the first body
        // line and below the last so the box reads as a calm, Braun-style
        // enclosure rather than a tight bezel.
        let has_body = !self.lines.is_empty();
        if has_body {
            for _ in 0..BODY_PADDING_ROWS {
                writeln_swallow(w, &Self::blank_row(inner));
            }
        }
        for line in &self.lines {
            writeln_swallow(w, &Self::body_row(line, inner));
        }
        if has_body {
            for _ in 0..BODY_PADDING_ROWS {
                writeln_swallow(w, &Self::blank_row(inner));
            }
        }

        // ── Bottom edge ──
        writeln_swallow(w, &Self::bottom_edge(inner));
    }

    fn top_edge(&self, inner: usize) -> String {
        let edge = Glyph::EdgeHorizontal.as_str();
        let tl = Glyph::CornerTopLeft.as_str();
        let tr = Glyph::CornerTopRight.as_str();

        // `inner` is the cell count between the two corners. Reserve a
        // leading `─ ` run, the title, a trailing space, then the fill;
        // an optional ` subtitle ─` segment sits flush against the
        // right corner. Long titles/subtitles are truncated so the top
        // row never exceeds the requested width.
        let title_budget = inner.saturating_sub(3).max(1);
        let (title, title_w) = truncate_plain(&self.title, title_budget);
        // Visible cells consumed by the entire left segment:
        // `─ ` (2) + title + ` ` (1).
        let title_visible = title_w + 3;

        let (subtitle, sub_text_w, sub_visible) = match self.subtitle.as_deref() {
            Some(s) if !s.is_empty() => {
                let remaining = inner.saturating_sub(title_visible);
                if remaining >= 4 {
                    let (subtitle, w) = truncate_plain(s, remaining - 3);
                    // ` subtitle ─` consumes w + 3 cells.
                    (subtitle, w, w + 3)
                } else {
                    (String::new(), 0, 0)
                }
            }
            _ => (String::new(), 0, 0),
        };

        let fill_cols = inner.saturating_sub(title_visible + sub_visible);
        let fill: String = edge.repeat(fill_cols);

        // Chrome (corners, edges) recedes via BORDER; the title carries
        // the single warm accent; the subtitle is the dim variant of
        // that same accent so it reads as a discreet watermark.
        let painted_subtitle_tail = if sub_text_w == 0 {
            String::new()
        } else {
            format!(" {} {}", paint(ACCENT_DIM, &subtitle), paint(BORDER, edge))
        };
        format!(
            "{}{}{}{}{}{}{}",
            paint(BORDER, tl),
            paint(BORDER, &format!("{edge} ")),
            paint(ACCENT, &title),
            paint(BORDER, " "),
            paint(BORDER, &fill),
            painted_subtitle_tail,
            paint(BORDER, tr),
        )
    }

    fn bottom_edge(inner: usize) -> String {
        let edge = Glyph::EdgeHorizontal.as_str();
        let bl = Glyph::CornerBottomLeft.as_str();
        let br = Glyph::CornerBottomRight.as_str();
        let fill: String = edge.repeat(inner);
        format!(
            "{}{}{}",
            paint(BORDER, bl),
            paint(BORDER, &fill),
            paint(BORDER, br)
        )
    }

    fn blank_row(inner: usize) -> String {
        let v = Glyph::EdgeVertical.as_str();
        format!(
            "{}{}{}",
            paint(BORDER, v),
            " ".repeat(inner),
            paint(BORDER, v)
        )
    }

    /// Build the body for a single banner row: paint the styled pieces
    /// to fit into `usable` cells. Returns `(painted, visible_cells)`;
    /// `visible_cells` may differ from the painted string's byte length
    /// because the painted form embeds ANSI escapes.
    fn paint_body(line: &Line, usable: usize) -> (String, usize) {
        match line {
            Line::Blank => (String::new(), 0),
            Line::Body(s) => fit(s, usable, |t| t.to_string()),
            Line::Muted(s) => fit(s, usable, |t| paint(MUTED, t)),
            Line::Pair { label, value } => {
                let combined = format!("{label}  {value}");
                // Try to keep the pair structure; fall back to a single
                // truncated muted run if it doesn't fit.
                if visible_width(&combined) <= usable {
                    (
                        format!("{label}  {}", paint(MUTED, value)),
                        visible_width(&combined),
                    )
                } else {
                    fit(&combined, usable, |t| paint(MUTED, t))
                }
            }
        }
    }

    fn body_row(line: &Line, inner: usize) -> String {
        let v = Glyph::EdgeVertical.as_str();
        let usable = inner.saturating_sub(4); // two-space gutter on each side
        let (painted, visible) = Self::paint_body(line, usable);
        let pad = usable.saturating_sub(visible);
        format!(
            "{}  {painted}{}  {}",
            paint(BORDER, v),
            " ".repeat(pad),
            paint(BORDER, v),
        )
    }
}

/// Display-cell width of a string, assuming no ANSI escapes and no
/// wide characters. Our content is constrained to ASCII paths +
/// box-drawing glyphs (which we measure as 1 cell each), so this
/// simple `.chars().count()` is correct for our inputs and keeps us
/// off `unicode-width` as a dependency.
pub(crate) fn visible_width(s: &str) -> usize {
    s.chars().count()
}

/// Truncate `text` to fit `max` display cells (appending `…` when it
/// has to cut), then run `paint_fn` over the resulting plain string to
/// add styling. Returns `(painted, visible_cells_used)`.
fn fit<F: FnOnce(&str) -> String>(text: &str, max: usize, paint_fn: F) -> (String, usize) {
    let width = visible_width(text);
    if width <= max {
        return (paint_fn(text), width);
    }
    let take = max.saturating_sub(1);
    let mut truncated: String = text.chars().take(take).collect();
    truncated.push_str(Glyph::Ellipsis.as_str());
    (paint_fn(&truncated), take + 1)
}

fn truncate_plain(text: &str, max: usize) -> (String, usize) {
    let width = visible_width(text);
    if width <= max {
        return (text.to_string(), width);
    }
    if max == 0 {
        return (String::new(), 0);
    }
    let take = max.saturating_sub(1);
    let mut truncated: String = text.chars().take(take).collect();
    truncated.push_str(Glyph::Ellipsis.as_str());
    (truncated, take + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_plain(b: &Banner) -> String {
        // Pin unicode glyphs so the snapshot is stable across TTY /
        // non-TTY harnesses. ANSI is stripped via anstream.
        let _g = crate::ui::pin_unicode_glyphs();
        let mut buf = Vec::new();
        b.render(&mut buf);
        let raw = String::from_utf8(buf).unwrap();
        anstream::adapter::strip_str(&raw).to_string()
    }

    #[test]
    fn boxed_render_includes_title_and_borders() {
        let banner = Banner::new("openmemory")
            .subtitle("v0.4.3")
            .width(50)
            .line(Line::Body("hello world".into()));
        let out = render_plain(&banner);
        // Title and subtitle present
        assert!(out.contains("openmemory"));
        assert!(out.contains("v0.4.3"));
        // Top and bottom corners
        assert!(out.contains('╭') && out.contains('╮'));
        assert!(out.contains('╰') && out.contains('╯'));
        // Vertical edges
        assert!(out.contains('│'));
        // Body line
        assert!(out.contains("hello world"));
    }

    #[test]
    fn boxed_rows_have_uniform_width() {
        let banner = Banner::new("title")
            .subtitle("sub")
            .width(50)
            .line(Line::Body("a".into()))
            .line(Line::Body("a much longer body line here".into()));
        let out = render_plain(&banner);
        let lines: Vec<&str> = out.lines().collect();
        let widths: Vec<usize> = lines.iter().map(|l| l.chars().count()).collect();
        // All rendered rows are exactly the configured width.
        for (i, w) in widths.iter().enumerate() {
            assert_eq!(*w, 50, "row {i} {:?} has width {w}", lines[i]);
        }
    }

    #[test]
    fn flat_render_below_min_width() {
        let banner = Banner::new("openmemory")
            .width(30)
            .line(Line::Body("hi".into()));
        let out = render_plain(&banner);
        // No box characters
        assert!(!out.contains('╭'));
        assert!(out.contains("── openmemory ──"));
        assert!(out.contains("hi"));
    }

    #[test]
    fn long_body_line_is_truncated_to_fit() {
        let banner = Banner::new("t").width(40).line(Line::Body(
            "this body is far too long for a forty column box to host".into(),
        ));
        let out = render_plain(&banner);
        // Every row still exactly 40 columns wide.
        for l in out.lines() {
            assert_eq!(l.chars().count(), 40, "row {l:?}");
        }
        assert!(out.contains('…'));
    }

    #[test]
    fn pair_line_renders_both_parts() {
        let banner = Banner::new("t").width(60).line(Line::Pair {
            label: "data".into(),
            value: "~/.openmemory".into(),
        });
        let out = render_plain(&banner);
        assert!(out.contains("data"));
        assert!(out.contains("~/.openmemory"));
    }

    #[test]
    fn long_title_and_subtitle_do_not_overflow_top_edge() {
        let banner = Banner::new("a very long title that cannot fit in the header")
            .subtitle("a very long subtitle that also cannot fit")
            .width(40)
            .line(Line::Body("body".into()));
        let out = render_plain(&banner);
        for l in out.lines() {
            assert_eq!(l.chars().count(), 40, "row {l:?}");
        }
    }
}
