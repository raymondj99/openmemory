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

use super::style::{ACCENT_DIM, MUTED};
use super::{paint, term_width, writeln_swallow};

/// Cap on the first card line including the suffix. Calls to
/// [`render`] clamp this to the actual terminal width minus the two-
/// space gutter so a narrow window doesn't push the suffix off-screen.
const CARD_HEADER_TARGET: usize = 64;

/// Smallest header column we ever render to, regardless of terminal
/// width. Below this we'd just be writing into the gutter.
const CARD_HEADER_MIN: usize = 24;

/// Print a single card. `header` is the entity/type label; `suffix`
/// (typically a score) is rendered dim and right-aligned within the
/// per-call header budget. `body` is rendered on the second line,
/// indented four spaces.
pub fn render<W: Write>(w: &mut W, header: &str, suffix: &str, body: &str) {
    let header_budget = header_budget();
    let header_width = header.chars().count();
    let suffix_width = suffix.chars().count();
    let used = header_width + suffix_width;
    let pad = header_budget.saturating_sub(used).max(2);
    let line1 = if suffix.is_empty() {
        format!("  {}", paint(ACCENT_DIM, header))
    } else {
        format!(
            "  {}{}{}",
            paint(ACCENT_DIM, header),
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
}
