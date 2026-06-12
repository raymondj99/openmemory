//! Graph panel.
//!
//! Renders the focused entity's neighbourhood in one of two modes
//! depending on density (see [`graph_layout::prepare`]):
//!
//! * **Shell mode** — focused entity at the centre of a Canvas;
//!   neighbours arranged in eight compass sectors with Braille-
//!   marker edges fanning out from the origin. Used when the
//!   neighbourhood is small enough to read radially.
//! * **List mode** — neighbours grouped by relation kind in a flat
//!   vertical list. Used as the fallback when shell mode would be
//!   too dense, when depth > 1, or when the viewport is too small
//!   to host a legible canvas.
//!
//! State machine:
//!
//! * **Focus stack** — [`GraphState::stack`] tracks navigation
//!   history so `Esc` pops back through prior focuses. `Enter` on a
//!   selected neighbour pushes the previous focus and re-roots on
//!   the neighbour.
//! * **Depth** — `+` / `-` flips between 1 and 2. Depth 2 expands
//!   neighbours-of-neighbours and forces list mode regardless of size.
//! * **Jump-to overlay** — `/` opens a substring filter over
//!   [`MemoryStore::list_entities`]. `Enter` re-roots on the
//!   selected match; `Esc` cancels.
//! * **Compass-aware navigation** — in shell mode the arrow keys
//!   pick the neighbour closest to the requested compass direction
//!   (`Up = N`, `Right = E`, `Down = S`, `Left = W`). In list mode
//!   `Up`/`Down` walk the flat list and `Left`/`Right` are inert.
//!   `Tab`/`Shift-Tab` always cycle clockwise / counter-clockwise
//!   through the flat sector order, in both modes.

use std::f64::consts::PI;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use openmemory_graph::{EntityListRow, MemoryStore};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Color;
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::commands::tui::screens::graph_layout::{
    self, Mode, Neighbor, NodePosition, PreparedLayout,
};
use crate::commands::tui::theme::Theme;

/// Page size for list_entities pagination inside the jump-to overlay.
const JUMP_LIMIT: usize = 100;

/// Minimum height of the canvas area for shell mode. Below this we
/// fall back to list-mode rendering even when [`prepare`] picked
/// shell; the canvas needs vertical room for the radial layout to
/// avoid label collisions.
const SHELL_MIN_HEIGHT: u16 = 12;

/// Canvas-coordinate bounds for the shell view. The x-axis is
/// stretched slightly because terminal cells are roughly twice as
/// tall as they are wide, so a perfect circle drawn in equal bounds
/// would appear squashed horizontally.
const CANVAS_X_BOUNDS: [f64; 2] = [-1.3, 1.3];
const CANVAS_Y_BOUNDS: [f64; 2] = [-1.0, 1.0];

#[derive(Debug, Clone, Default)]
struct FocusFrame {
    name: String,
    /// Cached neighbour list as of last refresh. Cleared whenever
    /// `depth` changes so the panel re-pulls.
    neighbors: Vec<Neighbor>,
    selected: usize,
}

#[derive(Debug, Clone)]
enum Overlay {
    None,
    JumpTo {
        filter: String,
        entities: Vec<EntityListRow>,
        selected: usize,
    },
}

pub struct GraphState {
    /// Navigation stack. The last entry is the current focus; earlier
    /// entries are pushed by `Enter`-refocus and restored by `Esc`.
    stack: Vec<FocusFrame>,
    depth: u8,
    overlay: Overlay,
    error: Option<String>,
}

impl Default for GraphState {
    fn default() -> Self {
        Self {
            stack: Vec::new(),
            depth: 1,
            overlay: Overlay::None,
            error: None,
        }
    }
}

impl GraphState {
    /// Re-read the focused entity's neighbours from the store.
    fn refresh(&mut self, store: &MemoryStore) {
        let Some(frame) = self.stack.last_mut() else {
            return;
        };
        match store.get_entity(&frame.name) {
            Ok(Some(entity)) => match store.get_entity_relations(&entity.id) {
                Ok(relations) => {
                    let mut neighbors = Vec::with_capacity(relations.len());
                    for r in relations {
                        let (other_id, outbound) = if r.from_entity == entity.id {
                            (r.to_entity.as_str(), true)
                        } else {
                            (r.from_entity.as_str(), false)
                        };
                        if let Ok(Some(other)) = store.get_entity_by_id(other_id) {
                            neighbors.push(Neighbor {
                                name: other.name,
                                entity_type: other.entity_type.as_str().to_string(),
                                relation_kind: r.relation_type,
                                outbound,
                            });
                        }
                    }
                    frame.neighbors = neighbors;
                    if frame.selected >= frame.neighbors.len() {
                        frame.selected = frame.neighbors.len().saturating_sub(1);
                    }
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("relations: {e}")),
            },
            Ok(None) => {
                self.error = Some(format!("entity {:?} not found", frame.name));
            }
            Err(e) => self.error = Some(format!("get_entity: {e}")),
        }
    }

    /// If the focus stack is empty, seed it with the most-recently
    /// updated entity so the first frame has content.
    fn seed_if_empty(&mut self, store: &MemoryStore) {
        if !self.stack.is_empty() {
            return;
        }
        if let Ok(rows) = store.list_entities(None, 1, 0) {
            if let Some(row) = rows.into_iter().next() {
                self.stack.push(FocusFrame {
                    name: row.entity.name,
                    neighbors: Vec::new(),
                    selected: 0,
                });
                self.refresh(store);
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, store: &MemoryStore) {
        self.seed_if_empty(store);

        if let Overlay::JumpTo { .. } = self.overlay {
            self.handle_jump_key(key, store);
            return;
        }

        match key.code {
            KeyCode::Char('/') => {
                let entities = store
                    .list_entities(None, JUMP_LIMIT, 0)
                    .unwrap_or_default();
                self.overlay = Overlay::JumpTo {
                    filter: String::new(),
                    entities,
                    selected: 0,
                };
            }
            KeyCode::Char('+') => {
                if self.depth < 2 {
                    self.depth += 1;
                    self.refresh(store);
                }
            }
            KeyCode::Char('-') => {
                if self.depth > 1 {
                    self.depth -= 1;
                    self.refresh(store);
                }
            }
            KeyCode::Esc => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                    self.refresh(store);
                }
            }
            KeyCode::Up => self.move_selection(Compass::N),
            KeyCode::Down => self.move_selection(Compass::S),
            KeyCode::Left => self.move_selection(Compass::W),
            KeyCode::Right => self.move_selection(Compass::E),
            KeyCode::Tab => self.step_sector(1),
            KeyCode::BackTab => self.step_sector(-1),
            // Shift-Tab on some terminals shows up as Tab + SHIFT.
            KeyCode::Char('\t') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.step_sector(-1);
            }
            KeyCode::Enter => self.refocus_on_selected(store),
            _ => {}
        }
    }

    /// Apply a compass-direction move on the cursor. In list mode
    /// `N`/`S` walk the flat list, `E`/`W` are inert. In shell mode
    /// we pick the neighbour with the smallest angular distance to
    /// the requested compass direction, restricted to a 180° window.
    fn move_selection(&mut self, dir: Compass) {
        let depth = self.depth;
        let Some(frame) = self.stack.last_mut() else {
            return;
        };
        let layout = graph_layout::prepare(&frame.name, &frame.neighbors, depth);

        match layout.mode {
            Mode::List => match dir {
                Compass::N => frame.selected = frame.selected.saturating_sub(1),
                Compass::S => {
                    if frame.selected + 1 < frame.neighbors.len() {
                        frame.selected += 1;
                    }
                }
                _ => {}
            },
            Mode::Shell => {
                if let Some(next) =
                    nearest_in_direction(&layout.positions, frame.selected, dir.angle())
                {
                    frame.selected = next;
                }
            }
        }
    }

    /// Step the cursor clockwise (`step > 0`) or counter-clockwise
    /// (`step < 0`) through the flat sector order. Works identically
    /// in both layout modes.
    fn step_sector(&mut self, step: i32) {
        let Some(frame) = self.stack.last_mut() else {
            return;
        };
        let n = frame.neighbors.len();
        if n == 0 {
            return;
        }
        let signed = frame.selected as i32 + step;
        let rem = signed.rem_euclid(n as i32);
        frame.selected = rem as usize;
    }

    /// Entity name the user is currently pointing at, for the
    /// entity-detail overlay. Prefers the selected neighbour;
    /// falls back to the focused entity itself when the neighbour
    /// list is empty.
    pub fn selected_entity_name(&self) -> Option<String> {
        let frame = self.stack.last()?;
        if let Some(n) = frame.neighbors.get(frame.selected) {
            return Some(n.name.clone());
        }
        Some(frame.name.clone())
    }

    /// Jump the focus stack onto `name` and refresh the cached
    /// neighbour list. Used by the entity-detail overlay when the
    /// user picks a relation with `Enter`.
    pub fn jump_focus(&mut self, name: String, store: &MemoryStore) {
        self.stack.push(FocusFrame {
            name,
            neighbors: Vec::new(),
            selected: 0,
        });
        self.refresh(store);
    }

    fn refocus_on_selected(&mut self, store: &MemoryStore) {
        let target = self
            .stack
            .last()
            .and_then(|f| f.neighbors.get(f.selected).map(|n| n.name.clone()));
        if let Some(name) = target {
            self.stack.push(FocusFrame {
                name,
                neighbors: Vec::new(),
                selected: 0,
            });
            self.refresh(store);
        }
    }

    fn handle_jump_key(&mut self, key: KeyEvent, store: &MemoryStore) {
        let Overlay::JumpTo {
            filter,
            entities,
            selected,
        } = &mut self.overlay
        else {
            return;
        };

        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => {
                let matches = filter_entities(entities, filter);
                if let Some(row) = matches.get(*selected).cloned() {
                    self.overlay = Overlay::None;
                    self.stack.push(FocusFrame {
                        name: row.entity.name.clone(),
                        neighbors: Vec::new(),
                        selected: 0,
                    });
                    self.refresh(store);
                }
            }
            KeyCode::Up => {
                *selected = selected.saturating_sub(1);
            }
            KeyCode::Down => {
                let matches = filter_entities(entities, filter);
                if *selected + 1 < matches.len() {
                    *selected += 1;
                }
            }
            KeyCode::Backspace => {
                filter.pop();
                *selected = 0;
            }
            KeyCode::Char(c) => {
                filter.push(c);
                *selected = 0;
            }
            _ => {}
        }
    }
}

/// Compass directions. `angle()` returns the target angle in
/// mathematical convention (radians from +x, counter-clockwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compass {
    N,
    E,
    S,
    W,
}

impl Compass {
    fn angle(self) -> f64 {
        match self {
            Self::E => 0.0,
            Self::N => PI / 2.0,
            Self::W => PI,
            Self::S => -PI / 2.0,
        }
    }
}

/// Pick the neighbour with the smallest absolute angular distance to
/// the target direction. Restricted to a 90° window on each side of
/// the target so that pressing `Up` from a south-sector node doesn't
/// teleport across the diagram.
fn nearest_in_direction(
    positions: &[NodePosition],
    current: usize,
    target_angle: f64,
) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, p) in positions.iter().enumerate() {
        if i == current {
            continue;
        }
        let pa = p.y.atan2(p.x);
        let delta = wrap_angle(pa - target_angle).abs();
        if delta > PI / 2.0 {
            continue;
        }
        match best {
            None => best = Some((i, delta)),
            Some((_, prior)) if delta < prior => best = Some((i, delta)),
            _ => {}
        }
    }
    best.map(|(i, _)| i)
}

/// Wrap `a` into `(-π, π]`.
fn wrap_angle(a: f64) -> f64 {
    let two_pi = 2.0 * PI;
    let mut r = a % two_pi;
    if r > PI {
        r -= two_pi;
    } else if r <= -PI {
        r += two_pi;
    }
    r
}

fn filter_entities(rows: &[EntityListRow], needle: &str) -> Vec<EntityListRow> {
    if needle.is_empty() {
        return rows.to_vec();
    }
    let needle = needle.to_lowercase();
    rows.iter()
        .filter(|r| r.entity.name.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

// ─────────────────────────────────── render ────────────────────────────────────

pub fn render(frame: &mut Frame, area: Rect, state: &GraphState, theme: Theme) {
    let Some(focus_frame) = state.stack.last() else {
        let body = vec![
            Line::raw(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "no entities yet. use `openmemory remember` to add one.",
                    theme.muted(),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), area);
        return;
    };

    let layout = graph_layout::prepare(&focus_frame.name, &focus_frame.neighbors, state.depth);

    // The shell renderer needs vertical room to lay out the radial
    // arrangement. Force list-mode when the viewport is too short.
    let use_shell = layout.mode == Mode::Shell && area.height >= SHELL_MIN_HEIGHT;

    if use_shell {
        render_shell(frame, area, focus_frame, &layout, state, theme);
    } else {
        let body = render_list(focus_frame, &layout, state, theme);
        frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), area);
    }

    if let Overlay::JumpTo { .. } = &state.overlay {
        render_jump_overlay(frame, area, state, theme);
    }
}

/// Header used by both shell and list modes: focus name + depth +
/// neighbour count + optional error.
fn header_lines(layout: &PreparedLayout, state: &GraphState, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(3);
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("focus  ", theme.muted()),
        Span::styled(layout.focus.clone(), theme.section()),
        Span::styled("  ·  ", theme.border()),
        Span::styled(format!("depth {}", state.depth), theme.muted()),
        Span::styled("  ·  ", theme.border()),
        Span::styled(
            format!("{} neighbors", layout.total_neighbors),
            theme.muted(),
        ),
    ]));
    if let Some(err) = &state.error {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("error: {err}"), theme.danger()),
        ]));
    }
    lines
}

/// Render the radial shell view: header + Canvas with edges + node
/// labels overlaid at the computed canvas positions.
fn render_shell(
    frame: &mut Frame,
    area: Rect,
    focus: &FocusFrame,
    layout: &PreparedLayout,
    state: &GraphState,
    theme: Theme,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header (line + optional error)
            Constraint::Length(1), // blank
            Constraint::Min(1),    // canvas + labels
        ])
        .split(area);

    frame.render_widget(Paragraph::new(header_lines(layout, state, theme)), rows[0]);

    let canvas_area = rows[2];
    if canvas_area.width < 20 || canvas_area.height < SHELL_MIN_HEIGHT {
        // Last-resort fallback inside an already-small terminal —
        // just print the list view instead of half-drawing a canvas.
        let body = render_list(focus, layout, state, theme);
        frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), canvas_area);
        return;
    }

    // Draw the edge polylines first; node labels overwrite their
    // anchor cells on top so the edge naturally terminates at the
    // label boundary.
    let edges = layout.edges.clone();
    let edge_color = Color::Cyan;
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds(CANVAS_X_BOUNDS)
        .y_bounds(CANVAS_Y_BOUNDS)
        .paint(move |ctx| {
            for e in &edges {
                ctx.draw(&CanvasLine {
                    x1: e.from_x,
                    y1: e.from_y,
                    x2: e.to_x,
                    y2: e.to_y,
                    color: edge_color,
                });
            }
        });
    frame.render_widget(canvas, canvas_area);

    // Overlay the focused entity at the origin.
    render_label_at(
        frame,
        canvas_area,
        0.0,
        0.0,
        &format!(" {} ", layout.focus),
        theme.accent(),
    );

    // Overlay each neighbour at its computed position.
    for (i, pos) in layout.positions.iter().enumerate() {
        let is_selected = i == focus.selected;
        let style = if is_selected {
            theme
                .section()
                .add_modifier(ratatui::style::Modifier::REVERSED)
        } else {
            theme.fg()
        };
        let arrow = if pos.outbound { "→" } else { "←" };
        let label = format!(" {arrow} {} ", pos.name);
        render_label_at(frame, canvas_area, pos.x, pos.y, &label, style);
    }
}

/// Map a canvas-space position to a screen pixel inside `area`, then
/// render `text` as a single-line `Paragraph` centred on that pixel.
/// Clamps so the label always sits fully inside `area`.
fn render_label_at(
    frame: &mut Frame,
    area: Rect,
    canvas_x: f64,
    canvas_y: f64,
    text: &str,
    style: ratatui::style::Style,
) {
    let (px, py) = canvas_to_pixel(canvas_x, canvas_y, area);
    let label_w = text.chars().count() as u16;
    let half = label_w / 2;
    // Centre the label on (px, py) while clamping into `area`.
    let max_x = area.x + area.width.saturating_sub(label_w);
    let label_x = px.saturating_sub(half).max(area.x).min(max_x);
    let label_y = py.max(area.y).min(area.y + area.height.saturating_sub(1));
    let label_rect = Rect {
        x: label_x,
        y: label_y,
        width: label_w,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text.to_string(), style))),
        label_rect,
    );
}

/// Convert a canvas-coordinate `(cx, cy)` (using the panel's
/// [`CANVAS_X_BOUNDS`] and [`CANVAS_Y_BOUNDS`]) into a screen pixel
/// position inside `area`. Flips `y` so that canvas-y = +1 is at the
/// top of the rect.
fn canvas_to_pixel(canvas_x: f64, canvas_y: f64, area: Rect) -> (u16, u16) {
    let (x_min, x_max) = (CANVAS_X_BOUNDS[0], CANVAS_X_BOUNDS[1]);
    let (y_min, y_max) = (CANVAS_Y_BOUNDS[0], CANVAS_Y_BOUNDS[1]);
    let nx = ((canvas_x - x_min) / (x_max - x_min)).clamp(0.0, 1.0);
    let ny = (1.0 - (canvas_y - y_min) / (y_max - y_min)).clamp(0.0, 1.0);
    let px = area.x + (nx * (f64::from(area.width) - 1.0)).round() as u16;
    let py = area.y + (ny * (f64::from(area.height) - 1.0)).round() as u16;
    (px, py)
}

/// Render the legacy adjacency-list view: header + groups, each
/// group a relation-kind label followed by its neighbours.
fn render_list(
    focus: &FocusFrame,
    layout: &PreparedLayout,
    state: &GraphState,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(layout.total_neighbors + 8);
    lines.push(Line::raw(""));
    lines.extend(header_lines(layout, state, theme));
    lines.push(Line::raw(""));

    if layout.total_neighbors == 0 {
        lines.push(Line::from(Span::styled(
            "  this entity has no relations.",
            theme.muted(),
        )));
        lines.push(Line::from(Span::styled(
            "  add one via the mcp `openmemory_add_relation` tool.",
            theme.muted(),
        )));
        return lines;
    }

    let mut visible_idx = 0;
    for group in &layout.groups {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(group.relation_kind.clone(), theme.section()),
            Span::styled(format!("  ({})", group.neighbors.len()), theme.muted()),
        ]));
        for n in &group.neighbors {
            let selected = visible_idx == focus.selected;
            let marker = if selected { "▸" } else { " " };
            let arrow = if n.outbound { "→" } else { "←" };
            let name_style = if selected {
                theme.section().add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                theme.fg()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("   {marker} {arrow} "),
                    if selected {
                        theme.section()
                    } else {
                        theme.border()
                    },
                ),
                Span::styled(n.name.clone(), name_style),
                Span::styled("  ·  ", theme.border()),
                Span::styled(n.entity_type.clone(), theme.muted()),
            ]));
            visible_idx += 1;
        }
        lines.push(Line::raw(""));
    }
    lines
}

fn render_jump_overlay(frame: &mut Frame, area: Rect, state: &GraphState, theme: Theme) {
    let Overlay::JumpTo {
        filter,
        entities,
        selected,
    } = &state.overlay
    else {
        return;
    };
    let popup = centered_rect(area, 60, 18);
    frame.render_widget(Clear, popup);

    let matches = filter_entities(entities, filter);
    let mut lines = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled("/", theme.section()),
        Span::raw(filter.clone()),
        Span::styled("_", theme.section()),
    ])];
    lines.push(Line::raw(""));
    if matches.is_empty() {
        lines.push(Line::from(Span::styled("  no matches.", theme.muted())));
    } else {
        for (i, m) in matches.iter().take(12).enumerate() {
            let is_sel = i == *selected;
            let marker = if is_sel { "▸" } else { " " };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {marker} "),
                    if is_sel {
                        theme.section()
                    } else {
                        theme.border()
                    },
                ),
                Span::styled(
                    m.entity.name.clone(),
                    if is_sel {
                        theme.section().add_modifier(ratatui::style::Modifier::BOLD)
                    } else {
                        theme.fg()
                    },
                ),
                Span::styled("  ·  ", theme.border()),
                Span::styled(m.entity.entity_type.as_str().to_string(), theme.muted()),
            ]));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title(Span::styled(" jump to entity ", theme.accent()));
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        popup,
    );
}

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(w)) / 2),
            Constraint::Length(w),
            Constraint::Min(0),
        ])
        .split(area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(h)) / 2),
            Constraint::Length(h),
            Constraint::Min(0),
        ])
        .split(horizontal[1]);
    vertical[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_has_empty_stack_and_depth_one() {
        let s = GraphState::default();
        assert!(s.stack.is_empty());
        assert_eq!(s.depth, 1);
        assert!(matches!(s.overlay, Overlay::None));
    }

    #[test]
    fn wrap_angle_keeps_values_in_principal_branch() {
        assert!((wrap_angle(0.0) - 0.0).abs() < 1e-9);
        assert!((wrap_angle(PI) - PI).abs() < 1e-9);
        assert!((wrap_angle(2.0 * PI) - 0.0).abs() < 1e-9);
        assert!((wrap_angle(-PI - 0.1) - (PI - 0.1)).abs() < 1e-9);
    }

    fn pos(name: &str, x: f64, y: f64, sector: u8) -> NodePosition {
        NodePosition {
            name: name.into(),
            entity_type: "concept".into(),
            relation_kind: "uses".into(),
            outbound: true,
            x,
            y,
            sector,
        }
    }

    #[test]
    fn nearest_north_picks_top_neighbor() {
        // Four neighbours, one per cardinal direction. Pressing N
        // from the East node should jump to the North node.
        let positions = vec![
            pos("east", 1.0, 0.0, 2),
            pos("north", 0.0, 1.0, 0),
            pos("west", -1.0, 0.0, 6),
            pos("south", 0.0, -1.0, 4),
        ];
        let target = Compass::N.angle();
        assert_eq!(nearest_in_direction(&positions, 0, target), Some(1));
    }

    #[test]
    fn nearest_skips_neighbors_beyond_90_degrees() {
        // From east, asking for north — only north (90° away) qualifies;
        // south (180°) is filtered.
        let positions = vec![
            pos("east", 1.0, 0.0, 2),
            pos("north", 0.0, 1.0, 0),
            pos("south", 0.0, -1.0, 4),
        ];
        // Pressing N from east -> north (1).
        assert_eq!(nearest_in_direction(&positions, 0, Compass::N.angle()), Some(1));
        // Pressing N from south -> still north, because south->north
        // is 180° but the function picks the smallest delta among the
        // candidates within the half-circle. North is exactly 90° from
        // south's heading, so it's allowed (= boundary).
        // To make sure we don't pick `south` itself (current), we
        // expect Some(1).
        let picked = nearest_in_direction(&positions, 2, Compass::N.angle());
        assert!(matches!(picked, Some(1)));
    }

    #[test]
    fn nearest_returns_none_when_no_candidate_in_window() {
        // Only one neighbour, which IS the current cursor — no other
        // candidates, no movement.
        let positions = vec![pos("only", 0.0, 1.0, 0)];
        assert_eq!(nearest_in_direction(&positions, 0, Compass::S.angle()), None);
    }
}
