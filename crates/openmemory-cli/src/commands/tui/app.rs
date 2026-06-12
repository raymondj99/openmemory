//! Top-level App state, focus model, and event loop.
//!
//! [`App`] owns:
//!
//! * the resolved profile + data directory (the TUI never re-reads
//!   `OPENMEMORY_HOME` after launch — we want one stable view of the
//!   world for the duration of a session);
//! * the [`MemoryStore`] borrow we read from;
//! * panel-local state for every screen (a separate `StatsState`,
//!   `SearchState`, `GraphState`, `ModelsState`);
//! * the persisted [`super::state::TuiState`] preferences;
//! * the active [`Panel`] and any overlays (help, confirm dialogs).
//!
//! The loop is single-threaded: events come from [`EventSource::poll`],
//! get dispatched to the active panel's `handle_event`, then we
//! redraw. Anything CPU-bound (recall, future graph spreads) is
//! delegated to PR2+ worker threads.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use openmemory_graph::MemoryStore;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::events::{AppEvent, EventSource};
use super::screens::{graph::GraphState, models::ModelsState, search::SearchState, stats::StatsState};
use super::state::TuiState;
use super::theme::Theme;

/// Which panel is currently visible. Tabs in the chrome strip are
/// listed in the same order as the variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Stats,
    Search,
    Graph,
    Models,
}

impl Panel {
    /// Title shown in the tab strip.
    pub fn title(self) -> &'static str {
        match self {
            Self::Stats => "stats",
            Self::Search => "search",
            Self::Graph => "graph",
            Self::Models => "models",
        }
    }

    /// Stable string for [`super::state::TuiState`] persistence.
    /// Kept separate from [`Self::title`] so a future rename of a
    /// user-visible label doesn't invalidate everyone's saved state.
    pub fn persisted(self) -> &'static str {
        match self {
            Self::Stats => "stats",
            Self::Search => "search",
            Self::Graph => "graph",
            Self::Models => "models",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        Some(match s {
            "stats" => Self::Stats,
            "search" => Self::Search,
            "graph" => Self::Graph,
            "models" => Self::Models,
            _ => return None,
        })
    }

    /// All panels in tab-strip order.
    pub const ALL: [Panel; 4] = [Self::Stats, Self::Search, Self::Graph, Self::Models];

    /// Index in [`Self::ALL`]. Used by the chrome strip to highlight
    /// the active tab.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| p == &self).unwrap_or(0)
    }
}

/// Drop-safe wrapper around the ratatui terminal: enters raw mode +
/// alt screen on construction and unconditionally tears them down on
/// drop. Paired with [`super::install_panic_hook`] so a panic during
/// `Terminal::draw` still restores the user's shell.
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    pub fn new() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context("enabling raw mode")?;
        let mut stdout = io::stdout();
        crossterm::execute!(
            stdout,
            crossterm::terminal::EnterAlternateScreen,
            crossterm::cursor::Hide,
        )
        .context("entering alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("creating terminal")?;
        Ok(Self { terminal })
    }

    /// Direct mutable access to the underlying ratatui terminal. Kept
    /// available for PR2+ paths (e.g. clearing the screen between
    /// confirmation dialogs) but currently the event loop in
    /// [`App::run_event_loop`] owns its own handle, so this is unused.
    #[allow(dead_code)]
    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show,
        );
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}

/// All long-lived TUI state. One per launch.
///
/// We hold the store as an `Arc<MemoryStore>` rather than a borrow so
/// worker threads (Search panel's recall runner in PR2, future graph
/// expanders) can `clone()` the Arc and run queries off the UI thread
/// without lifetime gymnastics.
pub struct App {
    pub profile: String,
    pub data_dir: PathBuf,
    pub store: Arc<MemoryStore>,
    pub theme: Theme,
    pub panel: Panel,
    pub stats: StatsState,
    pub search: SearchState,
    pub graph: GraphState,
    pub models: ModelsState,
    pub show_help: bool,
    /// Pending exit request. Once `true` the next loop iteration
    /// flushes any state and returns.
    pub should_quit: bool,
    /// Persisted preferences. Saved on shutdown.
    pub persisted: TuiState,
    /// Entity detail overlay (Track 3). Set when the user presses `d`
    /// on a panel that has a selected entity; cleared on Esc or after
    /// the overlay's pending refocus is applied.
    pub entity_detail: Option<super::screens::entity_detail::EntityDetailState>,
}

impl App {
    pub fn new(profile: &str, store: Arc<MemoryStore>, data_dir: PathBuf) -> Self {
        let persisted = TuiState::load(&data_dir);
        let panel = persisted.panel();
        let mut stats = StatsState::default();
        // Eagerly populate so the first frame isn't a blank Stats
        // panel waiting for the first tick (2 s away).
        stats.refresh(&store);
        let models = ModelsState::new();
        let search = SearchState::new(Arc::clone(&store), &data_dir);
        let graph = GraphState::default();
        Self {
            profile: profile.to_string(),
            data_dir,
            store,
            theme: Theme,
            panel,
            stats,
            search,
            graph,
            models,
            show_help: false,
            should_quit: false,
            persisted,
            entity_detail: None,
        }
    }

    /// Drive the loop until the user quits or a panic unwinds us.
    pub fn run_event_loop(&mut self) -> Result<()> {
        // We have to construct the terminal here rather than via the
        // guard's borrow because the guard is held above us in
        // `tui::run`. Re-enter the same stdout we wrapped earlier —
        // crossterm's mode flags are global, so this is safe.
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
            .context("creating event-loop terminal handle")?;
        let mut events = EventSource::new();

        while !self.should_quit {
            // Drain any worker responses before redrawing so freshly
            // arrived recall results land on this frame, not the next.
            self.search.drain_worker_responses();

            terminal.draw(|frame| super::screens::draw(self, frame))?;

            if let Some(ev) = events.poll()? {
                self.handle_event(ev);
            }
            if let Some(tick) = events.take_due_tick() {
                self.handle_event(tick);
            }
        }

        self.persisted.set_panel(self.panel);
        self.persisted.save(&self.data_dir);
        Ok(())
    }

    /// Route an event through the global hotkey table first; if no
    /// global key matches, the active panel gets the event.
    pub fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Tick => {
                // Stats is the only screen with auto-refreshing data
                // in v1; everything else is event-driven.
                self.stats.refresh(&self.store);
            }
            AppEvent::Resize(_, _) => {}
            AppEvent::Key(k) => self.handle_key(k),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Ignore key-up events on terminals that report them
        // (Windows + Kitty protocol). We're a key-down only UI.
        if key.kind == KeyEventKind::Release {
            return;
        }

        // Overlay takes precedence: any key while help is shown closes
        // it, including the global hotkeys. This matches the plan's
        // "Esc closes overlay" rule without inventing per-overlay
        // tables that no one else maintains.
        if self.show_help {
            self.show_help = false;
            return;
        }

        // Entity detail overlay (Track 3). When open, it consumes
        // every keystroke until the user dismisses it or jumps to a
        // relation. Esc / `q` / `Ctrl-C` close; arrows + Tab move the
        // cursor inside; Enter selects.
        if self.entity_detail.is_some() {
            self.handle_entity_detail_key(key);
            return;
        }

        // Global hotkeys.
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                self.should_quit = true;
                return;
            }
            (_, KeyCode::Char('q')) => {
                self.should_quit = true;
                return;
            }
            (_, KeyCode::Char('?')) => {
                self.show_help = true;
                return;
            }
            (_, KeyCode::Char('d')) => {
                if let Some(name) = self.selected_entity_name() {
                    self.entity_detail = Some(
                        super::screens::entity_detail::EntityDetailState::open(&self.store, &name),
                    );
                }
                return;
            }
            (_, KeyCode::Char('1')) => {
                self.panel = Panel::Stats;
                return;
            }
            (_, KeyCode::Char('2')) => {
                self.panel = Panel::Search;
                return;
            }
            (_, KeyCode::Char('3')) => {
                self.panel = Panel::Graph;
                return;
            }
            (_, KeyCode::Char('4')) => {
                self.panel = Panel::Models;
                return;
            }
            (_, KeyCode::Tab) => {
                self.panel = next_panel(self.panel);
                return;
            }
            (KeyModifiers::SHIFT, KeyCode::BackTab) => {
                self.panel = prev_panel(self.panel);
                return;
            }
            (_, KeyCode::BackTab) => {
                self.panel = prev_panel(self.panel);
                return;
            }
            _ => {}
        }

        // Per-panel handling.
        match self.panel {
            Panel::Stats => {} // Stats has no per-panel keys in v1.
            Panel::Search => self.search.handle_key(key, &self.data_dir),
            Panel::Graph => self.graph.handle_key(key, &self.store),
            Panel::Models => self.models.handle_key(key, &self.store),
        }
    }

    /// Resolve the entity name the user is currently "pointing at"
    /// for the entity-detail overlay. Each panel exposes its own
    /// cursor concept; this is the only place that joins them.
    fn selected_entity_name(&self) -> Option<String> {
        match self.panel {
            Panel::Search => self.search.selected_entity_name(),
            Panel::Graph => self.graph.selected_entity_name(),
            Panel::Stats | Panel::Models => None,
        }
    }

    /// Route a key event through the entity-detail overlay. After
    /// dispatch we check for a pending refocus (the user pressed
    /// `Enter` on a relation) and, if present, jump the Graph panel
    /// onto that neighbour and close the overlay.
    fn handle_entity_detail_key(&mut self, key: KeyEvent) {
        let Some(detail) = self.entity_detail.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.entity_detail = None;
                return;
            }
            KeyCode::Char('q') => {
                // Inside the overlay `q` still quits to be consistent
                // with the global `q` -> quit binding.
                self.should_quit = true;
                self.entity_detail = None;
                return;
            }
            KeyCode::Tab => detail.cycle_focus(),
            KeyCode::Up | KeyCode::Char('k') => detail.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => detail.move_cursor(1),
            KeyCode::Enter => detail.confirm(),
            _ => {}
        }

        if let Some(target) = detail.pending_refocus.take() {
            self.entity_detail = None;
            self.panel = Panel::Graph;
            self.graph.jump_focus(target, &self.store);
        }
    }
}

fn next_panel(p: Panel) -> Panel {
    let idx = (p.index() + 1) % Panel::ALL.len();
    Panel::ALL[idx]
}

fn prev_panel(p: Panel) -> Panel {
    let idx = (p.index() + Panel::ALL.len() - 1) % Panel::ALL.len();
    Panel::ALL[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_cycles_through_every_panel() {
        let panels: Vec<Panel> = std::iter::successors(Some(Panel::Stats), |p| Some(next_panel(*p)))
            .take(Panel::ALL.len() + 1)
            .collect();
        // After ALL.len() transitions we're back to Stats.
        assert_eq!(panels.first(), panels.last());
        // Every panel is visited exactly once before the wraparound.
        let mut seen: Vec<Panel> = panels[..Panel::ALL.len()].to_vec();
        seen.sort_by_key(|p| p.index());
        let expected: Vec<Panel> = Panel::ALL.to_vec();
        assert_eq!(seen, expected);
    }

    #[test]
    fn shift_tab_walks_backwards() {
        assert_eq!(prev_panel(Panel::Stats), Panel::Models);
        assert_eq!(prev_panel(Panel::Search), Panel::Stats);
    }

    #[test]
    fn persisted_round_trip_through_panel() {
        let mut s = TuiState::default();
        s.set_panel(Panel::Models);
        assert_eq!(s.panel(), Panel::Models);
    }
}
