//! Bridges the one-shot CLI palette (`crate::ui::style`) onto ratatui's
//! [`ratatui::style::Style`] so the TUI looks like a natural extension
//! of `status`, `model list`, and the other banner-driven commands.
//!
//! Two design rules carry over from the static UI:
//!
//! * **Single warm pop.** [`Theme::accent`] (yellow + bold) is reserved
//!   for the application title in the top chrome strip and nothing else.
//! * **Cool counterpoint.** [`Theme::section`] (cyan) marks every
//!   section heading and the highlighted tab. Lists, headings inside a
//!   panel, and the help overlay's headings all use it.
//!
//! ratatui's color model does not distinguish "dim" via a foreground
//! color: it has a `DIM` modifier. Mapping our `MUTED` and `BORDER`
//! roles to `Modifier::DIM` keeps frame/chrome receding without us
//! having to pick an arbitrary gray.

use ratatui::style::{Color, Modifier, Style};

/// Resolved TUI palette. Constructed once on launch (in [`App::new`])
/// and reused for every redraw. Currently field-less because the
/// palette is static; keeping it as a struct rather than a free
/// function lets us swap in a `--theme dark|light` knob later without
/// touching every widget.
#[derive(Debug, Clone, Copy, Default)]
pub struct Theme;

impl Theme {
    /// Top-level title text (`openmemory`, panel name). The single
    /// warm accent in the palette.
    pub fn accent(self) -> Style {
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    }

    /// Section heading inside a panel; selected tab; entity names
    /// referenced in body text. Cool counterpoint to [`Self::accent`].
    pub fn section(self) -> Style {
        Style::new().fg(Color::Cyan)
    }

    /// Subdued chrome: window frame, tab divider, status bar. Recedes
    /// so the content reads as the figure.
    pub fn border(self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    /// Secondary body text: paths, timestamps, counts. Same role as
    /// `style::MUTED` in the one-shot UI.
    pub fn muted(self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    /// Primary body text. Explicit so a future light-theme swap can
    /// override the terminal's default without touching call sites.
    pub fn fg(self) -> Style {
        Style::new()
    }

    /// Success / active state. Matches `style::SUCCESS` so the active
    /// embedding model row in the Models panel uses the same green as
    /// the `openmemory model list` card. Used by PR3 (Models picker).
    #[allow(dead_code)]
    pub fn success(self) -> Style {
        Style::new().fg(Color::Green)
    }

    /// Degraded-but-non-fatal state (e.g. a model entry whose weights
    /// aren't downloaded yet). Used by PR3.
    #[allow(dead_code)]
    pub fn warn(self) -> Style {
        Style::new().fg(Color::Yellow)
    }

    /// Failure copy.
    pub fn danger(self) -> Style {
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
    }

    /// Bar chart fill color. Cyan to match section, dimmed slightly
    /// via reverse-video on the bar value label so it's readable on
    /// any terminal background.
    pub fn bar(self) -> Style {
        Style::new().fg(Color::Cyan)
    }

    /// Highlighted row in a list (cursor row in Search, Models, Graph).
    /// Reverse video so the choice of accent color doesn't conflict
    /// with the row's own coloring. Used by PR2 (Search) onwards.
    #[allow(dead_code)]
    pub fn highlight(self) -> Style {
        Style::new().add_modifier(Modifier::REVERSED)
    }
}
