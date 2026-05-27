//! Glyph vocabulary with unicode + ASCII fallbacks.
//!
//! All glyphs are written through [`Glyph::as_str`], which consults
//! [`super::glyph_mode`] once per call. Callers never branch on the
//! mode directly; that keeps tests deterministic and the call sites
//! short.

use super::{glyph_mode, GlyphMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// In-progress bullet (●).
    InProgress,
    /// Completed bullet (✓).
    Ok,
    /// Skipped bullet (○).
    Skip,
    /// Failed bullet (✗).
    Fail,
    /// Forward arrow used for "next step" hints (›).
    Arrow,
    /// Inline separator used between compact metadata fields (·).
    Separator,
    /// Single-cell truncation marker (…).
    Ellipsis,
    /// Corner glyphs used by the rounded banner border.
    CornerTopLeft,
    CornerTopRight,
    CornerBottomLeft,
    CornerBottomRight,
    /// Edge glyphs.
    EdgeHorizontal,
    EdgeVertical,
}

impl Glyph {
    /// Render the glyph using the current process-wide policy.
    pub fn as_str(self) -> &'static str {
        self.for_mode(glyph_mode())
    }

    /// Render under a specific policy. Exposed for snapshot tests so
    /// they can pin the choice without reaching into the env.
    pub fn for_mode(self, mode: GlyphMode) -> &'static str {
        match (self, mode) {
            (Self::InProgress, GlyphMode::Unicode) => "●",
            (Self::InProgress, GlyphMode::Ascii) => "*",
            (Self::Ok, GlyphMode::Unicode) => "✓",
            (Self::Ok, GlyphMode::Ascii) => "v",
            (Self::Skip, GlyphMode::Unicode) => "○",
            (Self::Skip, GlyphMode::Ascii) => "-",
            (Self::Fail, GlyphMode::Unicode) => "✗",
            (Self::Fail, GlyphMode::Ascii) => "x",
            (Self::Arrow, GlyphMode::Unicode) => "›",
            (Self::Arrow, GlyphMode::Ascii) => ">",
            (Self::Separator, GlyphMode::Unicode) => "·",
            (Self::Separator, GlyphMode::Ascii) => "-",
            (Self::Ellipsis, GlyphMode::Unicode) => "…",
            (Self::Ellipsis, GlyphMode::Ascii) => "~",
            (Self::CornerTopLeft, GlyphMode::Unicode) => "╭",
            (Self::CornerTopLeft, GlyphMode::Ascii) => "+",
            (Self::CornerTopRight, GlyphMode::Unicode) => "╮",
            (Self::CornerTopRight, GlyphMode::Ascii) => "+",
            (Self::CornerBottomLeft, GlyphMode::Unicode) => "╰",
            (Self::CornerBottomLeft, GlyphMode::Ascii) => "+",
            (Self::CornerBottomRight, GlyphMode::Unicode) => "╯",
            (Self::CornerBottomRight, GlyphMode::Ascii) => "+",
            (Self::EdgeHorizontal, GlyphMode::Unicode) => "─",
            (Self::EdgeHorizontal, GlyphMode::Ascii) => "-",
            (Self::EdgeVertical, GlyphMode::Unicode) => "│",
            (Self::EdgeVertical, GlyphMode::Ascii) => "|",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_glyphs_are_single_grapheme() {
        // We assume each rendered glyph is one display column. That's
        // not technically guaranteed by Unicode but holds for the box
        // drawing + bullet set we picked. Lock it in.
        for g in [
            Glyph::InProgress,
            Glyph::Ok,
            Glyph::Skip,
            Glyph::Fail,
            Glyph::Arrow,
            Glyph::Separator,
            Glyph::Ellipsis,
            Glyph::CornerTopLeft,
            Glyph::CornerTopRight,
            Glyph::CornerBottomLeft,
            Glyph::CornerBottomRight,
            Glyph::EdgeHorizontal,
            Glyph::EdgeVertical,
        ] {
            let s = g.for_mode(GlyphMode::Unicode);
            assert!(!s.is_empty(), "{g:?} renders to non-empty");
            assert!(s.chars().count() == 1, "{g:?} = {s:?} is one char");
        }
    }

    #[test]
    fn ascii_fallbacks_are_pure_ascii() {
        for g in [
            Glyph::InProgress,
            Glyph::Ok,
            Glyph::Skip,
            Glyph::Fail,
            Glyph::Arrow,
            Glyph::Separator,
            Glyph::Ellipsis,
            Glyph::CornerTopLeft,
            Glyph::CornerTopRight,
            Glyph::CornerBottomLeft,
            Glyph::CornerBottomRight,
            Glyph::EdgeHorizontal,
            Glyph::EdgeVertical,
        ] {
            let s = g.for_mode(GlyphMode::Ascii);
            assert!(s.is_ascii(), "{g:?} = {s:?} should be ascii");
        }
    }
}
