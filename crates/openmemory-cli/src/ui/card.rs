//! Result cards.
//!
//! Used by `openmemory recall` to render one hit per card and by
//! `openmemory model list` to render one model per card. The layout
//! is two indented lines per card with a right-aligned suffix on the
//! first line:
//!
//! ```text
//!   Raymond · person                                  0.852
//!     prefers Rust over Python
//! ```

use std::io::Write;

use anstyle::Style;

use super::style::{MUTED, SECTION, SUCCESS};
use super::{paint, term_width, writeln_swallow};

/// Cap on the first card line including the suffix. Calls to
/// [`render`] clamp this to the actual terminal width minus the two-
/// space gutter so a narrow window doesn't push the suffix off-screen.
const CARD_HEADER_TARGET: usize = 64;

/// Smallest header column we ever render to, regardless of terminal
/// width. Below this we'd just be writing into the gutter.
const CARD_HEADER_MIN: usize = 24;

/// Header emphasis. `Default` is the cool section accent used for
/// neutral list rows; `Active` swaps in the green success color so
/// the eye lands on the currently-selected item in a list of peers
/// (e.g. the active embedding model in `model list`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderEmphasis {
    Default,
    // The active marker is only rendered by `model list` (behind the
    // `embeddings` feature); the variant is still exercised by this
    // module's tests, so suppress the dead-code lint on minimal builds
    // rather than gate the variant out and fragment the `match`.
    #[cfg_attr(not(feature = "embeddings"), allow(dead_code))]
    Active,
}

impl HeaderEmphasis {
    fn style(self) -> Style {
        match self {
            Self::Default => SECTION,
            Self::Active => SUCCESS,
        }
    }
}

/// Print a single card. `header` is the entity/type label; `suffix`
/// (typically a score or status) is rendered dim and right-aligned
/// within the per-call header budget. `body` is rendered on the second
/// line, indented four spaces.
pub fn render<W: Write>(w: &mut W, header: &str, suffix: &str, body: &str) {
    render_with(w, header, suffix, body, HeaderEmphasis::Default);
}

/// Print a single card whose header reads as the active/selected item
/// in a peer list. Identical layout to [`render`], but the header is
/// painted with the success accent so it pops in a stack of muted peers.
///
/// Consumed by `model list` (the `embeddings` feature); always reachable
/// from this module's tests.
#[cfg_attr(not(feature = "embeddings"), allow(dead_code))]
pub fn render_active<W: Write>(w: &mut W, header: &str, suffix: &str, body: &str) {
    render_with(w, header, suffix, body, HeaderEmphasis::Active);
}

fn render_with<W: Write>(
    w: &mut W,
    header: &str,
    suffix: &str,
    body: &str,
    emphasis: HeaderEmphasis,
) {
    let header_budget = header_budget();
    let header_width = header.chars().count();
    let suffix_width = suffix.chars().count();
    let used = header_width + suffix_width;
    let pad = header_budget.saturating_sub(used).max(2);
    let header_style = emphasis.style();
    let line1 = if suffix.is_empty() {
        format!("  {}", paint(header_style, header))
    } else {
        format!(
            "  {}{}{}",
            paint(header_style, header),
            " ".repeat(pad),
            paint(MUTED, suffix)
        )
    };
    writeln_swallow(w, &line1);
    writeln_swallow(w, &format!("    {body}"));
}

/// Print a blank padding line between cards.
pub fn separator<W: Write>(w: &mut W) {
    writeln_swallow(w, "");
}

/// Compute the header budget for one card. We aim for
/// [`CARD_HEADER_TARGET`] but never exceed the terminal width minus
/// the two-space left gutter, and never drop below [`CARD_HEADER_MIN`].
fn header_budget() -> usize {
    let term = term_width() as usize;
    let max = term.saturating_sub(2);
    CARD_HEADER_TARGET.min(max).max(CARD_HEADER_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(buf: Vec<u8>) -> String {
        anstream::adapter::strip_str(&String::from_utf8(buf).unwrap()).to_string()
    }

    #[test]
    fn renders_header_body_and_suffix() {
        let mut buf = Vec::new();
        render(&mut buf, "Raymond · person", "0.852", "prefers Rust");
        let out = strip(buf);
        assert!(out.contains("Raymond"));
        assert!(out.contains("0.852"));
        assert!(out.contains("prefers Rust"));
    }

    #[test]
    fn omits_suffix_padding_when_empty() {
        let mut buf = Vec::new();
        render(&mut buf, "openmemory · project", "", "hybrid search");
        let out = strip(buf);
        let first = out.lines().next().unwrap();
        // First line is just the header, no trailing whitespace block.
        assert_eq!(first.trim_end(), first);
    }

    #[test]
    fn header_budget_clamps_to_min_and_max() {
        let b = header_budget();
        assert!(b >= CARD_HEADER_MIN);
        assert!(b <= CARD_HEADER_TARGET);
    }

    /// `render` and `render_active` must produce the same layout when
    /// ANSI is stripped: same header text, suffix, body, padding. Only
    /// the color of the header byte sequence may differ, which is
    /// invisible after `strip_str`.
    #[test]
    fn active_variant_matches_default_layout_when_ansi_stripped() {
        let mut a = Vec::new();
        render(&mut a, "model-x", "downloaded", "768 dim");
        let plain = strip(a);

        let mut b = Vec::new();
        render_active(&mut b, "model-x", "active - downloaded", "768 dim");
        let active = strip(b);

        // Active suffix is longer, so we don't compare byte-equal — but
        // both must place the header at the same column and end with
        // the same body line.
        let plain_lines: Vec<&str> = plain.lines().collect();
        let active_lines: Vec<&str> = active.lines().collect();
        assert_eq!(plain_lines.len(), 2);
        assert_eq!(active_lines.len(), 2);
        assert!(plain_lines[0].starts_with("  model-x"));
        assert!(active_lines[0].starts_with("  model-x"));
        assert!(active_lines[0].contains("active - downloaded"));
        assert_eq!(plain_lines[1], active_lines[1]);
    }

    /// Active header carries the SUCCESS (green) escape; default
    /// header carries the SECTION (cyan) escape. We pin the bytes so
    /// a regression in emphasis routing doesn't silently demote the
    /// active row back to neutral cyan.
    #[test]
    fn active_variant_emits_success_escape_in_header() {
        let mut buf = Vec::new();
        render_active(&mut buf, "model-x", "", "768 dim");
        let raw = String::from_utf8(buf).unwrap();
        // ANSI 32 = green foreground; SUCCESS uses AnsiColor::Green.
        assert!(
            raw.contains("\x1b[32m"),
            "expected green SGR in active header: {raw:?}"
        );
    }

    #[test]
    fn default_variant_emits_section_escape_in_header() {
        let mut buf = Vec::new();
        render(&mut buf, "model-x", "", "768 dim");
        let raw = String::from_utf8(buf).unwrap();
        // ANSI 36 = cyan foreground; SECTION uses AnsiColor::Cyan.
        assert!(
            raw.contains("\x1b[36m"),
            "expected cyan SGR in default header: {raw:?}"
        );
    }
}
