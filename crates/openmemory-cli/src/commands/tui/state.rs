//! Persistent TUI state: which panel was active last, what the Graph
//! panel was focused on. Lives at `<data_dir>/tui/state.toml`.
//!
//! Best-effort: a missing or malformed file falls back to defaults
//! silently. We intentionally don't surface a hard error here — losing
//! a UI preference is never worth blocking the launch.
//!
//! History (Search panel queries) is a separate concern; it's
//! append-only and lives in `history.jsonl`. See [`super::history`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::app::Panel;

/// Persisted UI preferences. Reads + writes go through [`Self::load`]
/// and [`Self::save`]; both are infallible from the caller's
/// perspective (errors are swallowed so a corrupted file never blocks
/// startup or shutdown).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TuiState {
    #[serde(default)]
    pub last_panel: Option<String>,
    #[serde(default)]
    pub graph_focus: Option<String>,
}

impl TuiState {
    /// Load from `<data_dir>/tui/state.toml` if it exists. Anything
    /// other than a successful parse returns [`Self::default`].
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&raw).unwrap_or_default()
    }

    /// Save to `<data_dir>/tui/state.toml`. Creates the parent
    /// directory on demand; silently ignores any IO error.
    pub fn save(&self, data_dir: &Path) {
        let path = Self::path(data_dir);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(raw) = toml::to_string_pretty(self) {
            let _ = std::fs::write(&path, raw);
        }
    }

    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("tui").join("state.toml")
    }

    /// Resolve `last_panel` (a string for forward-compat in the file)
    /// onto a [`Panel`]. Unknown values map to [`Panel::Stats`].
    pub fn panel(&self) -> Panel {
        self.last_panel
            .as_deref()
            .and_then(Panel::from_persisted)
            .unwrap_or(Panel::Stats)
    }

    pub fn set_panel(&mut self, panel: Panel) {
        self.last_panel = Some(panel.persisted().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_default() {
        let dir = tempfile::tempdir().unwrap();
        let s = TuiState::load(dir.path());
        assert!(s.last_panel.is_none());
        assert!(s.graph_focus.is_none());
    }

    #[test]
    fn round_trips_panel_preference() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = TuiState::default();
        s.set_panel(Panel::Search);
        s.graph_focus = Some("openmemory".into());
        s.save(dir.path());

        let loaded = TuiState::load(dir.path());
        assert_eq!(loaded.panel(), Panel::Search);
        assert_eq!(loaded.graph_focus.as_deref(), Some("openmemory"));
    }

    #[test]
    fn malformed_file_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let tui_dir = dir.path().join("tui");
        std::fs::create_dir_all(&tui_dir).unwrap();
        std::fs::write(tui_dir.join("state.toml"), "this is = not toml [").unwrap();
        let s = TuiState::load(dir.path());
        // Default Panel::Stats after the bad parse.
        assert_eq!(s.panel(), Panel::Stats);
    }
}
