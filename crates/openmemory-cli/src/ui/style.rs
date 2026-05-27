//! Palette: one accent, three semantic colors, two text weights.
//!
//! Every public color is an [`anstyle::Style`] so it composes with
//! `write!(w, "{style}…{style:#}")` and round-trips through
//! [`anstream`]'s ANSI stripper when color is off.

use anstyle::{AnsiColor, Color, Style};

/// Brand accent. Used for box borders, top-level headings, and the
/// "next" arrow. Magenta deliberately, so we don't collide with Claude
/// Code's orange.
pub const ACCENT: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Magenta)))
    .bold();

/// Subdued accent for sub-headings inside a banner; same hue, no bold.
pub const ACCENT_DIM: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Magenta)));

/// `✓` glyph and success copy.
pub const SUCCESS: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));

/// `○` glyph and degraded-but-non-fatal copy.
pub const WARN: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));

/// `✗` glyph and failure copy.
pub const DANGER: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .bold();

/// Secondary text: paths, timestamps, suffixes after a `✓`.
pub const MUTED: Style = Style::new().dimmed();

/// Primary text: explicit so callers can opt out of the terminal's
/// default fg without us re-deriving it.
pub const FG: Style = Style::new();
