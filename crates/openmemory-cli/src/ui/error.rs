//! Top-level error renderer used by `main.rs`.
//!
//! Replaces `error: {e}` + `caused by: {cause}` chain dump with a
//! danger-styled glyph plus a muted, indented cause chain capped at
//! a small depth (deeper chains are summarised). Output goes to
//! stderr.

use std::io::Write;

use super::glyph::Glyph;
use super::style::{DANGER, MUTED};
use super::{paint, writeln_swallow};

const MAX_CHAIN_DEPTH: usize = 4;

/// Render a top-level error. Format:
///
/// ```text
///   ✗ short description
///       caused by: …
///       caused by: …
/// ```
pub fn render<W: Write>(w: &mut W, err: &anyhow::Error) {
    let head = err.to_string();
    let first_line = head.lines().next().unwrap_or(&head);
    writeln_swallow(
        w,
        &format!("  {} {}", paint(DANGER, Glyph::Fail.as_str()), first_line),
    );
    // Continuation of the head error, if any.
    for line in head.lines().skip(1) {
        writeln_swallow(w, &format!("      {}", paint(MUTED, line)));
    }
    let total_causes = err.chain().skip(1).count();
    for (shown, cause) in err.chain().skip(1).take(MAX_CHAIN_DEPTH).enumerate() {
        let text = cause.to_string();
        let mut iter = text.lines();
        if let Some(first) = iter.next() {
            writeln_swallow(
                w,
                &format!("      {}", paint(MUTED, &format!("caused by: {first}"))),
            );
        }
        for tail in iter {
            writeln_swallow(w, &format!("                 {}", paint(MUTED, tail)));
        }
        let _ = shown;
    }
    if total_causes > MAX_CHAIN_DEPTH {
        let remaining = total_causes - MAX_CHAIN_DEPTH;
        writeln_swallow(
            w,
            &format!("      {}", paint(MUTED, &format!("… {remaining} more"))),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(buf: Vec<u8>) -> String {
        anstream::adapter::strip_str(&String::from_utf8(buf).unwrap()).to_string()
    }

    #[test]
    fn renders_head_and_first_cause() {
        let _g = crate::ui::pin_unicode_glyphs();
        let err = anyhow::anyhow!("top").context("middle").context("outer");
        let mut buf = Vec::new();
        render(&mut buf, &err);
        let out = strip(buf);
        assert!(out.contains("outer"));
        assert!(out.contains("caused by: middle"));
        assert!(out.contains("caused by: top"));
        // ✗ glyph present
        assert!(out.contains('✗'));
    }

    #[test]
    fn caps_chain_depth() {
        let _g = crate::ui::pin_unicode_glyphs();
        let mut err = anyhow::anyhow!("level 0");
        for i in 1..=10 {
            err = err.context(format!("level {i}"));
        }
        let mut buf = Vec::new();
        render(&mut buf, &err);
        let out = strip(buf);
        // The renderer keeps a head + up to MAX_CHAIN_DEPTH causes
        // plus an "… N more" line.
        let cause_lines = out.lines().filter(|l| l.contains("caused by:")).count();
        assert!(cause_lines <= MAX_CHAIN_DEPTH);
        assert!(out.contains("more"));
    }
}
