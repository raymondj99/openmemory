//! Shared visual primitives for `openmemory`'s human-facing output.
//!
//! The module aims for a calm, single-accent aesthetic on the same shelf
//! as Claude Code. Three rules hold across every primitive:
//!
//! * **Honour the user's terminal.** [`color_choice`] folds together
//!   `NO_COLOR`, `CLICOLOR_FORCE`, an explicit `--no-color` flag, and
//!   the destination's TTY-ness so callers never have to.
//! * **Render to any writer.** Every primitive takes a generic
//!   `io::Write`. Production callers wrap stdout/stderr in
//!   `anstream::AutoStream` (see [`stdout_stream`] / [`stderr_stream`])
//!   so ANSI escapes are auto-stripped when color is off. Tests write
//!   to a `Vec<u8>` and run `anstream::adapter::strip_str` over the
//!   captured bytes to keep snapshots stable without ANSI noise.
//! * **Degrade gracefully.** When the terminal is too narrow to host a
//!   banner, or color is off, the same primitives still emit clean,
//!   aligned plain text.

pub mod banner;
pub mod card;
pub mod error;
pub mod glyph;
pub mod steps;
pub mod style;
pub mod table;

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicU8, Ordering};

use anstream::AutoStream;
use anstyle::Style;

/// Resolved color policy for the process. Computed once on first use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Emit ANSI escapes unconditionally.
    Always,
    /// Emit ANSI escapes only if the target writer is a TTY.
    Auto,
    /// Never emit ANSI escapes.
    Never,
}

/// Resolved glyph policy. Mirrors [`ColorMode`] but a degree finer: a
/// piped stdout still wants ASCII bullets even when the user forced
/// color on. We cache this alongside the color mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphMode {
    Unicode,
    Ascii,
}

const POLICY_UNSET: u8 = 0;
const POLICY_AUTO: u8 = 1;
const POLICY_ALWAYS: u8 = 2;
const POLICY_NEVER: u8 = 3;

const GLYPH_UNSET: u8 = 0;
const GLYPH_FORCE_UNICODE: u8 = 1;

/// Module-global override set by `--no-color` (or `--color=always`). Env
/// vars are read directly each time; the override exists so a single
/// `cli::run` call propagates without thread-locals.
static COLOR_OVERRIDE: AtomicU8 = AtomicU8::new(POLICY_UNSET);
/// Companion override for glyph mode. Decoupled from color so a piped
/// run that forces color on (`--color=always`) still gets the safer
/// ASCII bullets. Tests pin this explicitly.
static GLYPH_OVERRIDE: AtomicU8 = AtomicU8::new(GLYPH_UNSET);

/// Install an explicit override. Call exactly once during CLI dispatch.
pub fn set_color_override(mode: ColorMode) {
    let val = match mode {
        ColorMode::Auto => POLICY_AUTO,
        ColorMode::Always => POLICY_ALWAYS,
        ColorMode::Never => POLICY_NEVER,
    };
    COLOR_OVERRIDE.store(val, Ordering::Relaxed);
}

/// Resolved color policy, blending the override with `NO_COLOR` and
/// `CLICOLOR_FORCE`. Caller still needs to apply TTY detection at the
/// point of write; see [`stdout_stream`] for the typical wiring.
#[must_use]
pub fn color_choice() -> ColorMode {
    // Hardest constraint wins: NO_COLOR shuts off color even if the
    // user passed `--color=always`, matching the NO_COLOR convention.
    if std::env::var_os("NO_COLOR").is_some() {
        return ColorMode::Never;
    }
    match COLOR_OVERRIDE.load(Ordering::Relaxed) {
        POLICY_ALWAYS => return ColorMode::Always,
        POLICY_NEVER => return ColorMode::Never,
        _ => {}
    }
    if std::env::var_os("CLICOLOR_FORCE")
        .filter(|v| v != "0")
        .is_some()
    {
        return ColorMode::Always;
    }
    ColorMode::Auto
}

/// Glyph policy. `--no-color`/`NO_COLOR` force ASCII; otherwise we use
/// unicode whenever stdout is a TTY (best proxy for "a real terminal
/// that can render box drawing"). Piped output gets ASCII so logs and
/// CI artifacts stay readable, even if color is explicitly forced on.
#[must_use]
pub fn glyph_mode() -> GlyphMode {
    if GLYPH_OVERRIDE.load(Ordering::Relaxed) == GLYPH_FORCE_UNICODE {
        return GlyphMode::Unicode;
    }
    match color_choice() {
        ColorMode::Never => GlyphMode::Ascii,
        ColorMode::Always | ColorMode::Auto => {
            if io::stdout().is_terminal() {
                GlyphMode::Unicode
            } else {
                GlyphMode::Ascii
            }
        }
    }
}

/// Wrap `stdout` in an [`AutoStream`] that strips ANSI when color is
/// off. Callers can write styled text unconditionally; the stream
/// erases the SGR codes if they aren't wanted.
#[must_use]
pub fn stdout_stream() -> AutoStream<io::Stdout> {
    AutoStream::new(io::stdout(), color_choice().into())
}

/// Same as [`stdout_stream`] but for stderr.
#[must_use]
pub fn stderr_stream() -> AutoStream<io::Stderr> {
    AutoStream::new(io::stderr(), color_choice().into())
}

impl From<ColorMode> for anstream::ColorChoice {
    fn from(value: ColorMode) -> Self {
        match value {
            ColorMode::Always => Self::Always,
            ColorMode::Auto => Self::Auto,
            ColorMode::Never => Self::Never,
        }
    }
}

/// Detect the terminal width, clamped into a sensible band. Falls back
/// to 80 when stdout is not a TTY (CI logs, piped output).
#[must_use]
pub fn term_width() -> u16 {
    terminal_size::terminal_size()
        .map_or(80, |(w, _)| w.0)
        .clamp(40, 200)
}

/// Wrap a styled fragment for direct embedding in `write!`. Equivalent
/// to `format!("{style}{text}{style:#}")` but doesn't require the
/// caller to know anstyle's reset convention.
pub(crate) fn paint(style: Style, text: &str) -> String {
    format!("{style}{text}{style:#}")
}

/// Write a single line followed by `\n`, ignoring write errors. Output
/// helpers should never panic the process; broken pipes are a routine
/// outcome (`openmemory status | head -n 5`).
pub(crate) fn writeln_swallow<W: Write>(w: &mut W, line: &str) {
    let _ = writeln!(w, "{line}");
}

/// Pin glyph mode for the duration of a test. The shared [`TEST_LOCK`]
/// serialises access so concurrent tests don't race on the override.
#[cfg(test)]
pub(crate) fn pin_unicode_glyphs() -> impl Drop {
    let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = GLYPH_OVERRIDE.swap(GLYPH_FORCE_UNICODE, Ordering::Relaxed);
    GlyphGuard {
        _guard: guard,
        prev,
    }
}

#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
struct GlyphGuard<'a> {
    _guard: std::sync::MutexGuard<'a, ()>,
    prev: u8,
}

#[cfg(test)]
impl Drop for GlyphGuard<'_> {
    fn drop(&mut self) {
        GLYPH_OVERRIDE.store(self.prev, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests run in arbitrary order and may collide on the env var; we
    /// guard the env-sensitive ones with this mutex.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset_override() {
        COLOR_OVERRIDE.store(POLICY_UNSET, Ordering::Relaxed);
    }

    #[test]
    fn no_color_env_forces_never() {
        let _g = ENV_LOCK.lock().unwrap();
        reset_override();
        let prev = std::env::var_os("NO_COLOR");
        std::env::set_var("NO_COLOR", "1");
        set_color_override(ColorMode::Always);
        assert_eq!(color_choice(), ColorMode::Never);
        match prev {
            Some(v) => std::env::set_var("NO_COLOR", v),
            None => std::env::remove_var("NO_COLOR"),
        }
        reset_override();
    }

    #[test]
    fn explicit_override_beats_clicolor_force() {
        let _g = ENV_LOCK.lock().unwrap();
        reset_override();
        let prev_no_color = std::env::var_os("NO_COLOR");
        let prev_force = std::env::var_os("CLICOLOR_FORCE");
        std::env::remove_var("NO_COLOR");
        std::env::set_var("CLICOLOR_FORCE", "1");

        set_color_override(ColorMode::Never);
        assert_eq!(color_choice(), ColorMode::Never);

        if let Some(v) = prev_no_color {
            std::env::set_var("NO_COLOR", v);
        }
        match prev_force {
            Some(v) => std::env::set_var("CLICOLOR_FORCE", v),
            None => std::env::remove_var("CLICOLOR_FORCE"),
        }
        reset_override();
    }

    #[test]
    fn auto_is_default() {
        let _g = ENV_LOCK.lock().unwrap();
        reset_override();
        let prev_no_color = std::env::var_os("NO_COLOR");
        let prev_force = std::env::var_os("CLICOLOR_FORCE");
        std::env::remove_var("NO_COLOR");
        std::env::remove_var("CLICOLOR_FORCE");
        assert_eq!(color_choice(), ColorMode::Auto);
        if let Some(v) = prev_no_color {
            std::env::set_var("NO_COLOR", v);
        }
        if let Some(v) = prev_force {
            std::env::set_var("CLICOLOR_FORCE", v);
        }
    }

    #[test]
    fn term_width_is_within_band() {
        let w = term_width();
        assert!((40..=200).contains(&w));
    }
}
