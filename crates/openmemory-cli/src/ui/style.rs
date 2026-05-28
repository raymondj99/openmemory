//! Palette: one warm accent, one cool whisper, semantic colors.
//!
//! Inspired by Braun industrial design: a near-neutral hull with a
//! single warm pop (the ET66 "=" key) and a cool counterpoint (the
//! LCD). Border chrome is dimmed so it recedes; only the title carries
//! the warm accent; section headings inside tables get the cool one.
//!
//! Every public color is an [`anstyle::Style`] so it composes with
//! `write!(w, "{style}…{style:#}")` and round-trips through
//! [`anstream`]'s ANSI stripper when color is off.

use anstyle::{AnsiColor, Color, Style};

/// Title text. The single warm accent in the palette.
pub const ACCENT: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
    .bold();

/// Subtitle. Same hue, dropped to non-bold so it recedes behind the title.
pub const ACCENT_DIM: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));

/// Section heading inside tables. Cool counterpoint to the warm accent.
pub const SECTION: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)));

/// Border chrome (box edges, corners). Dimmed so the frame recedes and
/// the content reads as the figure.
pub const BORDER: Style = Style::new().dimmed();

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
