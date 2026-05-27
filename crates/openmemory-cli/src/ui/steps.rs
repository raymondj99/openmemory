//! Stepped task list.
//!
//! ```text
//!   ✓ initialising data root        ~/.openmemory
//!   ● detecting MCP clients
//! ```
//!
//! The renderer is intentionally append-only: every state change emits
//! a new line. Live in-place rewrites would need cursor control, which
//! breaks non-TTY captures (CI, `| tee`, snapshot tests). Each step
//! gets one line when it starts (`●`) and a second line when it
//! finishes (`✓`/`✗`/`○`). On a fast terminal the eye merges the two;
//! on a log they read as a clean sequence.

use std::io::Write;

use super::glyph::Glyph;
use super::style::{DANGER, MUTED, SUCCESS, WARN};
use super::{paint, writeln_swallow};

/// A `Steps` borrows the writer and emits step transitions to it.
pub struct Steps<'w, W: Write> {
    writer: &'w mut W,
    indent: &'static str,
    /// When true (default), we emit the `●` opener line. Set to false
    /// for fast one-shot commands where the opener would be noise.
    show_opener: bool,
}

impl<'w, W: Write> Steps<'w, W> {
    pub fn new(writer: &'w mut W) -> Self {
        Self {
            writer,
            indent: "  ",
            show_opener: true,
        }
    }

    /// Suppress the in-progress opener; only emit the terminal line.
    pub fn opener(mut self, show: bool) -> Self {
        self.show_opener = show;
        self
    }

    /// Emit a free-form detail line nested under the most recent
    /// step. Renders with extra indent and a leading glyph so the eye
    /// reads it as a child of the previous bullet.
    pub fn detail_ok(&mut self, label: &str, suffix: &str) {
        let glyph = paint(SUCCESS, Glyph::Ok.as_str());
        let line = if suffix.is_empty() {
            format!("{}    {} {}", self.indent, glyph, label)
        } else {
            format!(
                "{}    {} {}  {}",
                self.indent,
                glyph,
                label,
                paint(MUTED, suffix)
            )
        };
        writeln_swallow(self.writer, &line);
    }

    /// Detail variant for a failed sub-task.
    pub fn detail_fail(&mut self, label: &str, error: &dyn std::fmt::Display) {
        let glyph = paint(DANGER, Glyph::Fail.as_str());
        let line = format!(
            "{}    {} {}  {}",
            self.indent,
            glyph,
            label,
            paint(MUTED, &error.to_string())
        );
        writeln_swallow(self.writer, &line);
    }

    /// Begin a step. The returned [`StepHandle`] consumes itself on
    /// `finish_*`. Drop without finishing emits a `✗` line so a
    /// panicking step doesn't leave the user wondering what state we
    /// got to.
    pub fn step<'s>(&'s mut self, label: impl Into<String>) -> StepHandle<'s, 'w, W> {
        let label = label.into();
        if self.show_opener {
            let line = format!(
                "{}{} {}",
                self.indent,
                paint(MUTED, Glyph::InProgress.as_str()),
                label
            );
            writeln_swallow(self.writer, &line);
        }
        StepHandle {
            parent: self,
            label,
            finished: false,
        }
    }
}

/// Per-step finisher. Created by [`Steps::step`].
pub struct StepHandle<'s, 'w, W: Write> {
    parent: &'s mut Steps<'w, W>,
    label: String,
    finished: bool,
}

impl<W: Write> StepHandle<'_, '_, W> {
    /// Mark the step successful. `suffix` is rendered dim on the same
    /// line; pass an empty string to omit.
    pub fn finish_ok(mut self, suffix: impl AsRef<str>) {
        self.emit(Glyph::Ok, SUCCESS, suffix.as_ref(), None);
        self.finished = true;
    }

    /// Mark the step skipped (not applicable / not detected).
    pub fn finish_skip(mut self, suffix: impl AsRef<str>) {
        self.emit(Glyph::Skip, WARN, suffix.as_ref(), None);
        self.finished = true;
    }

    /// Mark the step failed. The error chain is rendered indented
    /// underneath.
    pub fn finish_fail(mut self, error: &dyn std::fmt::Display) {
        self.emit(Glyph::Fail, DANGER, "", Some(error.to_string()));
        self.finished = true;
    }

    fn emit(
        &mut self,
        glyph: Glyph,
        glyph_style: anstyle::Style,
        suffix: &str,
        error: Option<String>,
    ) {
        let mut line = format!(
            "{}{} {}",
            self.parent.indent,
            paint(glyph_style, glyph.as_str()),
            self.label
        );
        if !suffix.is_empty() {
            line.push_str("  ");
            line.push_str(&paint(MUTED, suffix));
        }
        writeln_swallow(self.parent.writer, &line);
        if let Some(err) = error {
            for chunk in err.lines() {
                let row = format!("{}    {}", self.parent.indent, paint(MUTED, chunk));
                writeln_swallow(self.parent.writer, &row);
            }
        }
    }
}

impl<W: Write> Drop for StepHandle<'_, '_, W> {
    fn drop(&mut self) {
        if !self.finished {
            // Panicked or early-returned mid-step. Surface that.
            let line = format!(
                "{}{} {}",
                self.parent.indent,
                paint(DANGER, Glyph::Fail.as_str()),
                self.label
            );
            writeln_swallow(self.parent.writer, &line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(f: impl FnOnce(&mut Vec<u8>)) -> String {
        let _g = crate::ui::pin_unicode_glyphs();
        let mut buf = Vec::new();
        f(&mut buf);
        let raw = String::from_utf8(buf).unwrap();
        anstream::adapter::strip_str(&raw).to_string()
    }

    #[test]
    fn ok_step_emits_opener_then_finish() {
        let out = render(|buf| {
            let mut s = Steps::new(buf);
            s.step("doing the thing").finish_ok("12 ms");
        });
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains('●'));
        assert!(lines[0].contains("doing the thing"));
        assert!(lines[1].contains('✓'));
        assert!(lines[1].contains("12 ms"));
    }

    #[test]
    fn fail_step_renders_error_indented() {
        let out = render(|buf| {
            let mut s = Steps::new(buf);
            s.step("doing the thing").finish_fail(&"boom");
        });
        assert!(out.contains('✗'));
        // Error indented further than the bullet.
        assert!(out.contains("    boom"));
    }

    #[test]
    fn skip_step_uses_open_circle() {
        let out = render(|buf| {
            let mut s = Steps::new(buf);
            s.step("optional thing").finish_skip("not detected");
        });
        assert!(out.contains('○'));
        assert!(out.contains("not detected"));
    }

    #[test]
    fn unfinished_handle_emits_fail_on_drop() {
        let out = render(|buf| {
            let mut s = Steps::new(buf);
            drop(s.step("interrupted"));
        });
        assert!(out.contains('✗'), "got {out:?}");
        assert!(out.contains("interrupted"));
    }

    #[test]
    fn opener_can_be_suppressed() {
        let out = render(|buf| {
            let mut s = Steps::new(buf).opener(false);
            s.step("quick thing").finish_ok("");
        });
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "got {lines:?}");
        assert!(lines[0].contains('✓'));
    }
}
