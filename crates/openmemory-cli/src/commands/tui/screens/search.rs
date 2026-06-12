//! Search panel.
//!
//! Layout (top to bottom):
//!
//! 1. **Filter strip** — entity-type chip (and room for more filters).
//!    `Tab` cycles focus into / out of the strip.
//! 2. **Input row** — single-line editor. Queries fire automatically
//!    as you type (debounced [`LIVE_SEARCH_DEBOUNCE`]); `Enter`
//!    force-submits immediately.
//! 3. **Status row** — last query result count / latency, or
//!    "in flight..." while a recall is running.
//! 4. **Hits list** — one row per scored result with query terms
//!    highlighted inline; `↑`/`↓` navigate; `e` toggles a full-
//!    observation expansion under the selected row.
//! 5. **History pane** — the most recent recorded queries; `↑` from
//!    the input jumps focus here; `Enter` populates the input with
//!    the selected query; `Ctrl-R` toggles a substring filter overlay.
//!
//! Recall runs on a background worker so the UI stays responsive even
//! for slow queries (vector mode on a cold cache). We discard stale
//! responses via a monotonic `query_id` so the panel never renders a
//! result from an aborted earlier query.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use openmemory_graph::{EntityType, MemoryStore, RecallFilters, RecallResult};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::commands::tui::history::{History, HistoryEntry};
use crate::commands::tui::screens::stats::format_timestamp;
use crate::commands::tui::theme::Theme;

/// Max hits the panel renders. The plan defers visual paging and we
/// don't want recall to fetch unbounded results.
const RESULT_LIMIT: usize = 25;

/// Quiet-time after the last keystroke before the live debounce
/// auto-fires a query. Short enough to feel instantaneous, long
/// enough to avoid a worker round-trip on every keystroke.
const LIVE_SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// Don't fire live queries shorter than this. Short fragments
/// (`a`, `ru`) match too much to be useful and burn worker cycles.
const LIVE_SEARCH_MIN_CHARS: usize = 2;

/// Where keyboard input is currently being directed within the panel.
/// Global hotkeys (panel switch, help) always take precedence — see
/// [`crate::commands::tui::app::App::handle_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Input,
    Filters,
    Hits,
    History,
}

/// User-facing filter chip state. Maps onto [`RecallFilters`] at
/// submit time. Each chip cycles through a small fixed list of
/// values; `None` means "don't filter on this dimension."
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub entity_type: Option<EntityType>,
}

impl SearchFilters {
    /// Cycle the entity-type chip through `(None, Person, Project,
    /// Concept, Tool, Preference, Fact, Event, Location,
    /// Organization)`. Wraps.
    fn cycle_entity_type(&mut self) {
        let next = match self.entity_type {
            None => Some(EntityType::Person),
            Some(EntityType::Person) => Some(EntityType::Project),
            Some(EntityType::Project) => Some(EntityType::Concept),
            Some(EntityType::Concept) => Some(EntityType::Tool),
            Some(EntityType::Tool) => Some(EntityType::Preference),
            Some(EntityType::Preference) => Some(EntityType::Fact),
            Some(EntityType::Fact) => Some(EntityType::Event),
            Some(EntityType::Event) => Some(EntityType::Location),
            Some(EntityType::Location) => Some(EntityType::Organization),
            Some(EntityType::Organization) => None,
        };
        self.entity_type = next;
    }

    fn entity_type_label(&self) -> &'static str {
        match self.entity_type {
            None => "any",
            Some(et) => et.as_str(),
        }
    }

    /// Build the [`RecallFilters`] handed to the worker.
    fn to_recall_filters(&self) -> RecallFilters {
        let mut f = RecallFilters::new();
        if let Some(et) = self.entity_type {
            f.entity_type = Some(et);
        }
        f
    }
}

/// Work item handed to the recall worker thread.
struct WorkerRequest {
    query: String,
    filters: RecallFilters,
    query_id: u64,
    started_at: Instant,
}

/// Result returned by the worker.
enum WorkerResponse {
    Hits {
        query_id: u64,
        hits: Vec<RecallResult>,
        took_ms: u32,
    },
    Error {
        query_id: u64,
        message: String,
    },
}

/// Channel handle to the worker thread. Dropping this sends the
/// worker an `Err(RecvError)` on its next recv and the thread exits.
struct Worker {
    tx: Sender<WorkerRequest>,
    rx: Receiver<WorkerResponse>,
    _handle: JoinHandle<()>,
}

pub struct SearchState {
    // Editor: a `Vec<char>` keeps the cursor index in graphemes so we
    // don't trip on multi-byte UTF-8 sequences without pulling in
    // `unicode-segmentation`. Queries are short so the cost is moot.
    input: Vec<char>,
    cursor: usize,

    // Live-search debounce. Each input edit stamps `last_input_change`;
    // the loop's `tick_debounce()` fires `submit()` once the stamp is
    // older than `LIVE_SEARCH_DEBOUNCE` and the input differs from
    // the last query we actually sent to the worker.
    last_input_change: Option<Instant>,
    last_submitted_query: String,

    // Query lifecycle. `query_id` is monotonically increasing and
    // stamped on every submission so stale worker responses are
    // discarded.
    query_id: u64,
    in_flight_id: Option<u64>,
    status: Option<String>,

    // Filters
    filters: SearchFilters,

    // Results
    hits: Vec<RecallResult>,
    selected: usize,
    expanded: bool,

    // History
    history: History,
    history_filter: String,
    history_filter_active: bool,
    history_selected: usize,

    // Focus
    focus: Focus,

    // Worker channel. Optional so SearchState can be `Default`-able
    // for tests; production always constructs via `new`.
    worker: Option<Worker>,
    data_dir: PathBuf,
}

impl SearchState {
    /// Construct a Search panel for the given store. Spawns the recall
    /// worker thread eagerly; the thread idles until the first query
    /// is submitted.
    pub fn new(store: Arc<MemoryStore>, data_dir: &Path) -> Self {
        let history = History::load(data_dir);
        let worker = spawn_worker(store);
        Self {
            input: Vec::new(),
            cursor: 0,
            last_input_change: None,
            last_submitted_query: String::new(),
            query_id: 0,
            in_flight_id: None,
            status: None,
            filters: SearchFilters::default(),
            hits: Vec::new(),
            selected: 0,
            expanded: false,
            history,
            history_filter: String::new(),
            history_filter_active: false,
            history_selected: 0,
            focus: Focus::Input,
            worker: Some(worker),
            data_dir: data_dir.to_path_buf(),
        }
    }

    /// Entity name the user is currently pointing at, for the
    /// entity-detail overlay. We return Some when the Hits focus
    /// holds a selection; from Input/History the cursor is text-
    /// editing, not entity-pointing, so the `d` key is a no-op.
    pub fn selected_entity_name(&self) -> Option<String> {
        if self.focus != Focus::Hits {
            return None;
        }
        self.hits
            .get(self.selected)
            .map(|h| h.entity_name.clone())
    }

    /// Drain any responses delivered by the worker since the last
    /// frame, then check whether the debounced live-search timer has
    /// fired. Called from the main loop before each redraw.
    pub fn drain_worker_responses(&mut self) {
        self.drain_results();
        self.tick_debounce();
    }

    /// Fire `submit()` if the user has paused typing past the debounce
    /// window and the current input differs from whatever was last
    /// sent to the worker. Skipped when a query is already in flight
    /// so we never queue overlapping requests.
    fn tick_debounce(&mut self) {
        if self.in_flight_id.is_some() {
            return;
        }
        let Some(stamp) = self.last_input_change else {
            return;
        };
        if stamp.elapsed() < LIVE_SEARCH_DEBOUNCE {
            return;
        }
        let current = self.input_string();
        if current.chars().count() < LIVE_SEARCH_MIN_CHARS {
            // Below the minimum; clear stamp so we don't re-check
            // every loop, and stop signalling stale results.
            self.last_input_change = None;
            return;
        }
        if current == self.last_submitted_query {
            self.last_input_change = None;
            return;
        }
        self.submit();
    }

    fn drain_results(&mut self) {
        let Some(worker) = &self.worker else { return };
        loop {
            match worker.rx.try_recv() {
                Ok(WorkerResponse::Hits {
                    query_id,
                    hits,
                    took_ms,
                }) => {
                    // Discard stale responses (user submitted again
                    // before the worker finished the previous query).
                    if Some(query_id) != self.in_flight_id {
                        continue;
                    }
                    let hit_count = hits.len();
                    self.hits = hits;
                    self.selected = 0;
                    self.expanded = false;
                    self.in_flight_id = None;
                    self.status = Some(format!("{hit_count} hits in {took_ms} ms"));

                    // Record in history.
                    self.history.push(
                        &self.data_dir,
                        HistoryEntry {
                            ts: now_secs(),
                            panel: "search".into(),
                            query: self.input_string(),
                            hits: hit_count as u32,
                            took_ms,
                        },
                    );
                }
                Ok(WorkerResponse::Error { query_id, message }) => {
                    if Some(query_id) != self.in_flight_id {
                        continue;
                    }
                    self.in_flight_id = None;
                    self.status = Some(format!("error: {message}"));
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _data_dir: &Path) {
        match self.focus {
            Focus::Input => self.handle_input_key(key),
            Focus::Filters => self.handle_filters_key(key),
            Focus::Hits => self.handle_hits_key(key),
            Focus::History => self.handle_history_key(key),
        }
    }

    /// Handle keys while focus is on the filter chip strip. v1 hosts
    /// a single chip (entity type); the strip is structured so adding
    /// `source`, `memory_tier`, etc. is purely additive.
    fn handle_filters_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('\t') => {
                self.focus = Focus::Input;
            }
            // Left / Right / Space cycles the entity-type chip.
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter => {
                self.filters.cycle_entity_type();
                // A filter change invalidates the previous result
                // set; stamp `last_input_change` so the debounce
                // re-fires with the new filter applied.
                self.last_input_change = Some(Instant::now());
                self.last_submitted_query.clear();
            }
            _ => {}
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        let in_flight = self.in_flight_id.is_some();
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
                self.focus = Focus::History;
                self.history_filter_active = true;
                self.history_filter.clear();
                self.history_selected = 0;
            }
            (KeyModifiers::CONTROL, KeyCode::Char('w')) => {
                self.delete_word_left();
                self.note_input_change();
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
                self.input.drain(..self.cursor);
                self.cursor = 0;
                self.note_input_change();
            }
            (_, KeyCode::Tab) => {
                self.focus = Focus::Filters;
            }
            (_, KeyCode::Enter) if !in_flight && !self.input.is_empty() => self.submit(),
            (_, KeyCode::Esc) => {
                if in_flight {
                    // Discard the in-flight query: bump the id so the
                    // pending response is treated as stale.
                    self.in_flight_id = None;
                    self.status = Some("cancelled".into());
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                    self.note_input_change();
                }
            }
            (_, KeyCode::Up) => {
                if !self.history.is_empty() {
                    self.focus = Focus::History;
                    self.history_selected = 0;
                }
            }
            (_, KeyCode::Down) => {
                if !self.hits.is_empty() {
                    self.focus = Focus::Hits;
                    self.selected = 0;
                }
            }
            (_, KeyCode::Left) => {
                self.cursor = self.cursor.saturating_sub(1);
            }
            (_, KeyCode::Right) => {
                if self.cursor < self.input.len() {
                    self.cursor += 1;
                }
            }
            (_, KeyCode::Home) => self.cursor = 0,
            (_, KeyCode::End) => self.cursor = self.input.len(),
            (_, KeyCode::Backspace) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                    self.note_input_change();
                }
            }
            (_, KeyCode::Delete) => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                    self.note_input_change();
                }
            }
            (_, KeyCode::Char(c)) => {
                self.input.insert(self.cursor, c);
                self.cursor += 1;
                self.note_input_change();
            }
            _ => {}
        }
    }

    /// Stamp the input-change timestamp + clear any error status so
    /// the next debounce tick re-fires the query. Also clears hits
    /// when the input drops below the live-search threshold so we
    /// don't display stale results from a longer query.
    fn note_input_change(&mut self) {
        self.last_input_change = Some(Instant::now());
        if self.input.len() < LIVE_SEARCH_MIN_CHARS {
            self.hits.clear();
            self.selected = 0;
            self.expanded = false;
            self.status = None;
        }
    }

    fn handle_hits_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => {
                if self.selected == 0 {
                    self.focus = Focus::Input;
                } else {
                    self.selected -= 1;
                    self.expanded = false;
                }
            }
            KeyCode::Down => {
                if self.selected + 1 < self.hits.len() {
                    self.selected += 1;
                    self.expanded = false;
                }
            }
            KeyCode::Char('e') => self.expanded = !self.expanded,
            KeyCode::Esc => {
                self.focus = Focus::Input;
                self.expanded = false;
            }
            _ => {}
        }
    }

    fn handle_history_key(&mut self, key: KeyEvent) {
        let recent = self.filtered_history();
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
                self.history_filter_active = false;
                self.history_filter.clear();
            }
            (_, KeyCode::Up) => {
                self.history_selected = self.history_selected.saturating_sub(1);
            }
            (_, KeyCode::Down) => {
                if self.history_selected + 1 < recent.len() {
                    self.history_selected += 1;
                }
            }
            (_, KeyCode::Enter) => {
                if let Some(entry) = recent.get(self.history_selected) {
                    self.input = entry.query.chars().collect();
                    self.cursor = self.input.len();
                }
                self.history_filter_active = false;
                self.history_filter.clear();
                self.focus = Focus::Input;
            }
            (_, KeyCode::Esc) => {
                self.history_filter_active = false;
                self.history_filter.clear();
                self.focus = Focus::Input;
            }
            (_, KeyCode::Backspace) if self.history_filter_active => {
                self.history_filter.pop();
                self.history_selected = 0;
            }
            (_, KeyCode::Char(c)) if self.history_filter_active => {
                self.history_filter.push(c);
                self.history_selected = 0;
            }
            _ => {}
        }
    }

    fn submit(&mut self) {
        let query = self.input_string();
        if query.trim().is_empty() {
            return;
        }
        self.query_id += 1;
        self.in_flight_id = Some(self.query_id);
        self.status = Some("in flight…".into());
        self.last_submitted_query.clone_from(&query);
        self.last_input_change = None;
        if let Some(worker) = &self.worker {
            let _ = worker.tx.send(WorkerRequest {
                query,
                filters: self.filters.to_recall_filters(),
                query_id: self.query_id,
                started_at: Instant::now(),
            });
        }
    }

    fn input_string(&self) -> String {
        self.input.iter().collect()
    }

    fn delete_word_left(&mut self) {
        // Walk left past whitespace, then past one word.
        let mut end = self.cursor;
        while end > 0 && self.input[end - 1].is_whitespace() {
            end -= 1;
        }
        while end > 0 && !self.input[end - 1].is_whitespace() {
            end -= 1;
        }
        self.input.drain(end..self.cursor);
        self.cursor = end;
    }

    fn filtered_history(&self) -> Vec<HistoryEntry> {
        let recent = self.history.recent(50);
        if self.history_filter.is_empty() {
            recent
        } else {
            let needle = self.history_filter.to_lowercase();
            recent
                .into_iter()
                .filter(|e| e.query.to_lowercase().contains(&needle))
                .collect()
        }
    }
}

impl Default for SearchState {
    /// Test-only default: no worker thread, no store. Renders the
    /// panel chrome but submitting a query is a no-op.
    fn default() -> Self {
        Self {
            input: Vec::new(),
            cursor: 0,
            last_input_change: None,
            last_submitted_query: String::new(),
            query_id: 0,
            in_flight_id: None,
            status: None,
            filters: SearchFilters::default(),
            hits: Vec::new(),
            selected: 0,
            expanded: false,
            history: History::default(),
            history_filter: String::new(),
            history_filter_active: false,
            history_selected: 0,
            focus: Focus::Input,
            worker: None,
            data_dir: PathBuf::new(),
        }
    }
}

fn spawn_worker(store: Arc<MemoryStore>) -> Worker {
    let (req_tx, req_rx) = std::sync::mpsc::channel::<WorkerRequest>();
    let (resp_tx, resp_rx) = std::sync::mpsc::channel::<WorkerResponse>();
    let handle = std::thread::Builder::new()
        .name("openmemory-tui-search".into())
        .spawn(move || {
            while let Ok(req) = req_rx.recv() {
                let resp = match store.recall(&req.query, RESULT_LIMIT, &req.filters) {
                    Ok(hits) => WorkerResponse::Hits {
                        query_id: req.query_id,
                        hits,
                        took_ms: req.started_at.elapsed().as_millis() as u32,
                    },
                    Err(e) => WorkerResponse::Error {
                        query_id: req.query_id,
                        message: e.to_string(),
                    },
                };
                if resp_tx.send(resp).is_err() {
                    // Receiver dropped — main loop exited; we should too.
                    break;
                }
            }
        })
        .expect("spawn recall worker thread");
    Worker {
        tx: req_tx,
        rx: resp_rx,
        _handle: handle,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ─────────────────────────────────── render ────────────────────────────────────

pub fn render(frame: &mut Frame, area: Rect, state: &SearchState, theme: Theme) {
    // Watch-face layout: no per-section boxes. Five regions stacked
    // with left-aligned tickmark labels between them. The new filter
    // chip strip sits on its own row above the input editor so it's
    // discoverable but doesn't disrupt the prompt flow.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // filter chips
            Constraint::Length(1), // blank
            Constraint::Length(2), // input prompt + status line
            Constraint::Length(1), // ── RESULTS tickmark
            Constraint::Length(1), // blank
            Constraint::Min(5),    // hits
            Constraint::Length(1), // ── HISTORY tickmark
            Constraint::Length(1), // blank
            Constraint::Length(7), // history
        ])
        .split(area);

    render_filters(frame, rows[0], state, theme);
    render_input(frame, rows[2], state, theme);
    crate::commands::tui::widgets::chrome::render_section_break(frame, rows[3], theme, "RESULTS");
    render_hits(frame, rows[5], state, theme);
    crate::commands::tui::widgets::chrome::render_section_break(frame, rows[6], theme, "HISTORY");
    render_history(frame, rows[8], state, theme);
}

/// Render the filter chip strip. v1 hosts only the entity-type chip;
/// adding more chips is purely additive (render them after the first
/// chip with the same `· chip-label · value` shape).
fn render_filters(frame: &mut Frame, area: Rect, state: &SearchState, theme: Theme) {
    let focused = state.focus == Focus::Filters;
    let (label_style, value_style, gutter_style) = if focused {
        (
            theme.section(),
            theme.fg().add_modifier(ratatui::style::Modifier::BOLD),
            theme.section(),
        )
    } else {
        (theme.muted(), theme.fg(), theme.border())
    };

    let hint_style = if focused {
        theme.muted()
    } else {
        theme.border()
    };

    let spans = vec![
        Span::raw("  "),
        Span::styled("filter", gutter_style),
        Span::raw("  "),
        Span::styled("type", label_style),
        Span::raw(" "),
        Span::styled(format!("[{}]", state.filters.entity_type_label()), value_style),
        Span::raw("       "),
        Span::styled(
            if focused {
                "← / → cycle  ·  esc / tab exit"
            } else {
                "tab to filter"
            },
            hint_style,
        ),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_input(frame: &mut Frame, area: Rect, state: &SearchState, theme: Theme) {
    let focused = state.focus == Focus::Input;
    let prompt_style = if focused { theme.section() } else { theme.muted() };

    let mut prompt = vec![Span::raw("  "), Span::styled("›  ", prompt_style)];
    if state.input.is_empty() {
        prompt.push(Span::styled(
            "start typing — search runs live as you go.",
            theme.muted(),
        ));
    } else {
        // Cursor rendered as an inline reversed-video character so the
        // user sees where the next keystroke will land.
        let before: String = state.input.iter().take(state.cursor).collect();
        let at = state.input.get(state.cursor).copied();
        let after: String = state.input.iter().skip(state.cursor + 1).collect();
        prompt.push(Span::styled(before, theme.fg()));
        if focused {
            let ch = at.unwrap_or(' ').to_string();
            prompt.push(Span::styled(ch, theme.highlight()));
            prompt.push(Span::styled(after, theme.fg()));
        } else {
            if let Some(c) = at {
                prompt.push(Span::styled(c.to_string(), theme.fg()));
            }
            prompt.push(Span::styled(after, theme.fg()));
        }
    }

    let status_line = state.status.as_deref().map_or_else(
        || Line::raw(""),
        |s| {
            Line::from(vec![
                Span::raw("     "),
                Span::styled(s.to_string(), theme.muted()),
            ])
        },
    );

    frame.render_widget(
        Paragraph::new(vec![Line::from(prompt), status_line]),
        area,
    );
}

fn render_hits(frame: &mut Frame, area: Rect, state: &SearchState, theme: Theme) {
    let focused = state.focus == Focus::Hits;

    if state.hits.is_empty() {
        let body = match state.in_flight_id {
            Some(_) => Line::from(vec![
                Span::raw("  "),
                Span::styled("running query…", theme.muted()),
            ]),
            None if state.status.as_deref().is_some_and(|s| s.starts_with("error")) => {
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        state.status.as_deref().unwrap_or("").to_string(),
                        theme.danger(),
                    ),
                ])
            }
            None if state.input.len() < LIVE_SEARCH_MIN_CHARS => Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!(
                        "type at least {LIVE_SEARCH_MIN_CHARS} characters to start searching."
                    ),
                    theme.muted(),
                ),
            ]),
            None => Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "no matches. try a different phrasing or relax the filter.",
                    theme.muted(),
                ),
            ]),
        };
        frame.render_widget(Paragraph::new(body), area);
        return;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(state.hits.len() * 3);
    for (i, h) in state.hits.iter().enumerate() {
        let selected = i == state.selected && focused;
        let marker = if selected { "▸" } else { " " };
        let mut header = Vec::with_capacity(6);
        header.push(Span::styled(
            format!(" {marker} "),
            if selected {
                theme.section()
            } else {
                theme.border()
            },
        ));
        header.push(Span::styled(
            h.entity_name.clone(),
            if selected {
                theme.section().add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                theme.section()
            },
        ));
        header.push(Span::styled("  ·  ", theme.border()));
        header.push(Span::styled(
            h.entity_type.as_str().to_string(),
            theme.muted(),
        ));
        header.push(Span::styled("  ·  ", theme.border()));
        header.push(Span::styled(format!("{:.3}", h.score), theme.muted()));
        lines.push(Line::from(header));

        let body_style = if selected { theme.fg() } else { theme.muted() };
        let body_text = if selected && state.expanded {
            h.observation.content.clone()
        } else {
            truncate(&h.observation.content, 80)
        };
        let highlight_style = theme
            .section()
            .add_modifier(ratatui::style::Modifier::BOLD);
        let query = state.input_string();
        let mut body_spans = vec![Span::raw("     ")];
        body_spans.extend(highlight_snippet(
            &body_text,
            &query,
            body_style,
            highlight_style,
        ));
        lines.push(Line::from(body_spans));

        if selected && state.expanded {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled(
                    format!(
                        "observed {} · source {}",
                        format_timestamp(h.observation.observed_at),
                        if h.observation.source.is_empty() {
                            "—"
                        } else {
                            h.observation.source.as_str()
                        },
                    ),
                    theme.muted(),
                ),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_history(frame: &mut Frame, area: Rect, state: &SearchState, theme: Theme) {
    let focused = state.focus == Focus::History;

    let recent = state.filtered_history();
    if recent.is_empty() {
        let mut spans = vec![Span::raw("  ")];
        if state.history_filter_active {
            spans.push(Span::styled("/", theme.section()));
            spans.push(Span::styled(state.history_filter.clone(), theme.fg()));
            spans.push(Span::styled("   no matches.", theme.muted()));
        } else {
            spans.push(Span::styled(
                "no history yet. submit a query to record one.",
                theme.muted(),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        // Suppress unused-warning while focused-only chrome state is
        // pending visualization (no inner border in watch-face mode).
        let _ = focused;
        return;
    }

    let lines: Vec<Line> = recent
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let selected = focused && i == state.history_selected;
            let marker = if selected { "▸" } else { " " };
            let query_style = if selected {
                theme.section().add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                theme.fg()
            };
            Line::from(vec![
                Span::styled(
                    format!(" {marker} "),
                    if selected {
                        theme.section()
                    } else {
                        theme.border()
                    },
                ),
                Span::styled(e.query.clone(), query_style),
                Span::styled("  ·  ", theme.border()),
                Span::styled(format!("{} hits", e.hits), theme.muted()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Split `snippet` into spans, alternating between `base_style` and
/// `match_style` so case-insensitive substring hits of `query` are
/// visually highlighted. Tokens are matched as a single whole-string
/// substring (no per-word splitting) which is good enough for the
/// inline preview without dragging in `regex`.
fn highlight_snippet(
    snippet: &str,
    query: &str,
    base_style: ratatui::style::Style,
    match_style: ratatui::style::Style,
) -> Vec<Span<'static>> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return vec![Span::styled(snippet.to_string(), base_style)];
    }
    let lower_snippet = snippet.to_lowercase();
    let lower_query = trimmed.to_lowercase();
    if lower_query.len() > snippet.len() || !lower_snippet.contains(&lower_query) {
        return vec![Span::styled(snippet.to_string(), base_style)];
    }
    let mut spans = Vec::new();
    let mut cursor = 0;
    while cursor < snippet.len() {
        let Some(idx) = lower_snippet[cursor..].find(&lower_query) else {
            spans.push(Span::styled(snippet[cursor..].to_string(), base_style));
            break;
        };
        let match_start = cursor + idx;
        let match_end = (match_start + lower_query.len()).min(snippet.len());
        if match_start > cursor {
            spans.push(Span::styled(
                snippet[cursor..match_start].to_string(),
                base_style,
            ));
        }
        spans.push(Span::styled(
            snippet[match_start..match_end].to_string(),
            match_style,
        ));
        cursor = match_end;
    }
    spans
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(initial: &str) -> SearchState {
        let mut s = SearchState::default();
        s.input = initial.chars().collect();
        s.cursor = s.input.len();
        s
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn typing_inserts_at_cursor() {
        let mut s = mk("");
        for c in "rust".chars() {
            s.handle_input_key(key(KeyCode::Char(c)));
        }
        assert_eq!(s.input_string(), "rust");
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn backspace_removes_left_of_cursor() {
        let mut s = mk("rust");
        s.handle_input_key(key(KeyCode::Backspace));
        assert_eq!(s.input_string(), "rus");
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn left_right_move_cursor_within_bounds() {
        let mut s = mk("abc");
        s.handle_input_key(key(KeyCode::Left));
        s.handle_input_key(key(KeyCode::Left));
        assert_eq!(s.cursor, 1);
        s.handle_input_key(key(KeyCode::Home));
        assert_eq!(s.cursor, 0);
        s.handle_input_key(key(KeyCode::Left));
        assert_eq!(s.cursor, 0, "saturating_sub keeps us at 0");
        s.handle_input_key(key(KeyCode::End));
        assert_eq!(s.cursor, 3);
        s.handle_input_key(key(KeyCode::Right));
        assert_eq!(s.cursor, 3, "Right at end of input is a no-op");
    }

    #[test]
    fn ctrl_w_deletes_previous_word_with_surrounding_spaces() {
        let mut s = mk("hello world  ");
        s.handle_input_key(ctrl('w'));
        assert_eq!(s.input_string(), "hello ");
        assert_eq!(s.cursor, 6);
        s.handle_input_key(ctrl('w'));
        assert_eq!(s.input_string(), "");
    }

    #[test]
    fn ctrl_u_clears_to_start() {
        let mut s = mk("hello world");
        s.handle_input_key(key(KeyCode::Left));
        s.handle_input_key(key(KeyCode::Left));
        // Cursor at 9; ctrl-u removes everything before it.
        s.handle_input_key(ctrl('u'));
        assert_eq!(s.input_string(), "ld");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn unicode_input_round_trips() {
        let mut s = mk("");
        for c in "café 🦀".chars() {
            s.handle_input_key(key(KeyCode::Char(c)));
        }
        assert_eq!(s.input_string(), "café 🦀");
        s.handle_input_key(key(KeyCode::Backspace));
        assert_eq!(s.input_string(), "café ");
    }

    #[test]
    fn esc_clears_input_when_not_in_flight() {
        let mut s = mk("partial");
        s.handle_input_key(key(KeyCode::Esc));
        assert_eq!(s.input_string(), "");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn up_arrow_from_input_focuses_history_when_history_exists() {
        let mut s = mk("");
        // Empty history -> no focus change.
        s.handle_input_key(key(KeyCode::Up));
        assert_eq!(s.focus, Focus::Input);

        // Add a history entry; now Up should focus history.
        s.history.push(
            Path::new("/tmp/openmemory-test-unused"),
            HistoryEntry {
                ts: 0,
                panel: "search".into(),
                query: "ratatui".into(),
                hits: 1,
                took_ms: 1,
            },
        );
        s.handle_input_key(key(KeyCode::Up));
        assert_eq!(s.focus, Focus::History);
    }

    #[test]
    fn hits_navigation_moves_selection_and_wraps_to_input_at_top() {
        // Synthesise two hits to exercise selection bounds.
        let mut s = SearchState {
            hits: vec![dummy_hit("alpha"), dummy_hit("beta")],
            focus: Focus::Hits,
            ..SearchState::default()
        };
        s.handle_hits_key(key(KeyCode::Down));
        assert_eq!(s.selected, 1);
        s.handle_hits_key(key(KeyCode::Down));
        assert_eq!(s.selected, 1, "Down at end of list is a no-op");
        s.handle_hits_key(key(KeyCode::Up));
        assert_eq!(s.selected, 0);
        s.handle_hits_key(key(KeyCode::Up));
        assert_eq!(s.focus, Focus::Input, "Up at top returns to Input");
    }

    fn dummy_hit(name: &str) -> RecallResult {
        use openmemory_graph::{EntityType, MemoryTier, Observation};
        RecallResult {
            observation: Observation {
                id: "obs-id".into(),
                entity_id: "ent-id".into(),
                content: format!("an observation about {name}"),
                observed_at: 0,
                valid_from: None,
                valid_until: None,
                confidence: 1.0,
                source: "test".into(),
                tombstoned: false,
                access_count: 0,
                memory_tier: MemoryTier::Episodic,
                title: None,
                summary: None,
                importance: None,
                source_kind: None,
                concepts: vec![],
                source_files: vec![],
            },
            entity_name: name.into(),
            entity_type: EntityType::Concept,
            raw_score: 0.5,
            score: 0.5,
        }
    }
}
