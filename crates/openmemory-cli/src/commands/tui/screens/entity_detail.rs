//! Entity detail overlay.
//!
//! Triggered by `d` from any panel that exposes a "selected entity"
//! cursor (Search hits, Graph focus). Floats as a centered modal so
//! it works from any panel without enlarging the global panel switch.
//!
//! Layout (top to bottom):
//!
//! 1. Header — entity name + type + a 14-day write sparkline + total
//!    observation count + in/out relation counts.
//! 2. Observations — newest first, paginated with `j`/`k` (or arrow
//!    keys). Each row carries the observed-at timestamp and a content
//!    snippet.
//! 3. Relations — grouped in/out, navigated via Tab to a relation
//!    cursor; `Enter` on a relation refocuses the Graph panel onto
//!    that neighbour and closes the overlay.
//!
//! All data is loaded eagerly when the overlay opens via
//! [`EntityDetailState::open`] using the existing public
//! [`MemoryStore`] API. A dense entity (many observations + many
//! relations) costs one entity lookup, one observations query, one
//! relations query, plus one entity-by-id lookup per relation to
//! resolve the neighbour's display name — bounded and small.

use openmemory_graph::{Entity, MemoryStore, Observation};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::commands::tui::screens::stats::format_timestamp;
use crate::commands::tui::theme::Theme;

/// Sparkline window (same as the Stats panel's global ACTIVITY row).
const SPARKLINE_DAYS: usize = 14;

/// Maximum observations rendered per page; `j`/`k` navigate.
const OBSERVATIONS_PAGE_SIZE: usize = 6;

/// Which sub-region inside the overlay has the keyboard cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Focus {
    #[default]
    Observations,
    Relations,
}

/// Resolved relation entry. `kind` is the relation type; `other_name`
/// is the display name of the other endpoint; `outbound` indicates
/// direction.
#[derive(Debug, Clone)]
pub struct ResolvedRelation {
    pub kind: String,
    pub other_name: String,
    pub outbound: bool,
}

pub struct EntityDetailState {
    pub name: String,
    pub entity: Option<Entity>,
    pub observations: Vec<Observation>,
    pub relations: Vec<ResolvedRelation>,
    pub daily_writes: Vec<u64>,
    pub error: Option<String>,

    focus: Focus,
    observation_cursor: usize,
    relation_cursor: usize,

    /// Set when the user picked a relation with `Enter`. The Graph
    /// panel reads this on the next tick to refocus on the target,
    /// then the App clears the overlay. Avoids cross-panel coupling
    /// inside this module.
    pub pending_refocus: Option<String>,
}

impl EntityDetailState {
    /// Eagerly load observations + relations for `name`. Returns a
    /// state with `error` populated if the entity can't be found —
    /// the overlay still renders something useful in that case.
    pub fn open(store: &MemoryStore, name: &str) -> Self {
        let mut state = Self {
            name: name.to_string(),
            entity: None,
            observations: Vec::new(),
            relations: Vec::new(),
            daily_writes: vec![0; SPARKLINE_DAYS],
            error: None,
            focus: Focus::Observations,
            observation_cursor: 0,
            relation_cursor: 0,
            pending_refocus: None,
        };

        let entity = match store.get_entity(name) {
            Ok(Some(e)) => e,
            Ok(None) => {
                state.error = Some(format!("entity {name:?} not found"));
                return state;
            }
            Err(e) => {
                state.error = Some(format!("get_entity: {e}"));
                return state;
            }
        };

        match store.get_entity_observations(&entity.id) {
            Ok(observations) => {
                state.daily_writes = derive_daily_writes(&observations, SPARKLINE_DAYS);
                state.observations = observations;
            }
            Err(e) => state.error = Some(format!("observations: {e}")),
        }

        if let Ok(relations) = store.get_entity_relations(&entity.id) {
            for r in relations {
                let (other_id, outbound) = if r.from_entity == entity.id {
                    (r.to_entity.as_str(), true)
                } else {
                    (r.from_entity.as_str(), false)
                };
                if let Ok(Some(other)) = store.get_entity_by_id(other_id) {
                    state.relations.push(ResolvedRelation {
                        kind: r.relation_type,
                        other_name: other.name,
                        outbound,
                    });
                }
            }
        }

        state.entity = Some(entity);
        state
    }

    /// Move the cursor in the current focused section.
    pub fn move_cursor(&mut self, delta: i32) {
        match self.focus {
            Focus::Observations => {
                let n = self.observations.len();
                if n == 0 {
                    return;
                }
                let signed = self.observation_cursor as i32 + delta;
                let clamped = signed.clamp(0, n as i32 - 1);
                self.observation_cursor = clamped as usize;
            }
            Focus::Relations => {
                let n = self.relations.len();
                if n == 0 {
                    return;
                }
                let signed = self.relation_cursor as i32 + delta;
                let clamped = signed.clamp(0, n as i32 - 1);
                self.relation_cursor = clamped as usize;
            }
        }
    }

    /// Switch between the observations and relations sections.
    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Observations => Focus::Relations,
            Focus::Relations => Focus::Observations,
        };
    }

    /// If the user is in the relations section and presses `Enter`,
    /// stash the target name in `pending_refocus`. The App reads it
    /// after dispatching the key event, refocuses the Graph panel,
    /// and clears the overlay.
    pub fn confirm(&mut self) {
        if self.focus == Focus::Relations {
            if let Some(r) = self.relations.get(self.relation_cursor) {
                self.pending_refocus = Some(r.other_name.clone());
            }
        }
    }
}

/// Bucket the given observations into per-day write counts for the
/// last `days` UTC midnights. Index 0 is the oldest day; index
/// `days - 1` is today.
fn derive_daily_writes(observations: &[Observation], days: usize) -> Vec<u64> {
    if days == 0 {
        return Vec::new();
    }
    let now = current_unix_secs();
    let today_bucket = now.div_euclid(86_400);
    let oldest_bucket = today_bucket - days as i64 + 1;
    let mut buckets = vec![0_u64; days];
    for obs in observations {
        if obs.tombstoned {
            continue;
        }
        let bucket = obs.observed_at.div_euclid(86_400);
        if bucket < oldest_bucket {
            continue;
        }
        let idx = (bucket - oldest_bucket) as usize;
        if idx < days {
            buckets[idx] += 1;
        }
    }
    buckets
}

fn current_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sparkline_string(buckets: &[u64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = buckets.iter().copied().max().unwrap_or(0);
    buckets
        .iter()
        .map(|v| {
            if max == 0 || *v == 0 {
                BARS[0]
            } else {
                let scaled = (*v as f64 / max as f64) * (BARS.len() - 1) as f64;
                BARS[scaled.round() as usize]
            }
        })
        .collect()
}

// ─────────────────────────────────── render ────────────────────────────────────

pub fn render(frame: &mut Frame, area: Rect, state: &EntityDetailState, theme: Theme) {
    let popup = centered_rect(area, 88, 24);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("ENTITY", theme.muted()),
            Span::raw(" "),
            Span::styled(state.name.clone(), theme.section()),
            Span::raw(" "),
        ]));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // header (entity meta + sparkline)
            Constraint::Length(1), // break
            Constraint::Length(1), // OBSERVATIONS label
            Constraint::Length((OBSERVATIONS_PAGE_SIZE * 2) as u16),
            Constraint::Length(1), // RELATIONS label
            Constraint::Min(1),    // relations list
            Constraint::Length(1), // bottom hint
        ])
        .split(inner);

    render_header(frame, rows[0], state, theme);
    render_section_break(frame, rows[2], state, theme, "OBSERVATIONS");
    render_observations(frame, rows[3], state, theme);
    render_section_break(frame, rows[4], state, theme, "RELATIONS");
    render_relations(frame, rows[5], state, theme);
    render_hint(frame, rows[6], theme);
}

fn render_header(frame: &mut Frame, area: Rect, state: &EntityDetailState, theme: Theme) {
    let mut lines = Vec::with_capacity(3);
    let type_text = state
        .entity
        .as_ref()
        .map_or("?", |e| e.entity_type.as_str());
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("type", theme.muted()),
        Span::raw("  "),
        Span::styled(type_text.to_string(), theme.fg()),
        Span::styled("    ", theme.border()),
        Span::styled("observations", theme.muted()),
        Span::raw("  "),
        Span::styled(state.observations.len().to_string(), theme.fg()),
        Span::styled("    ", theme.border()),
        Span::styled("relations", theme.muted()),
        Span::raw("  "),
        Span::styled(state.relations.len().to_string(), theme.fg()),
    ]));
    if !state.daily_writes.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("last 14 days", theme.muted()),
            Span::raw("  "),
            Span::styled(sparkline_string(&state.daily_writes), theme.section()),
            Span::styled("    ", theme.border()),
            Span::styled("today", theme.muted()),
            Span::raw("  "),
            Span::styled(
                state.daily_writes.last().unwrap_or(&0).to_string(),
                theme.fg(),
            ),
        ]));
    }
    if let Some(err) = &state.error {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("error: {err}"), theme.danger()),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_section_break(
    frame: &mut Frame,
    area: Rect,
    state: &EntityDetailState,
    theme: Theme,
    label: &str,
) {
    let focused = matches!(
        (state.focus, label),
        (Focus::Observations, "OBSERVATIONS") | (Focus::Relations, "RELATIONS")
    );
    let label_style = if focused {
        theme.section().add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        theme.section()
    };
    let used = 2 + label.chars().count() + 2;
    let rule_len = (area.width as usize)
        .saturating_sub(used)
        .saturating_sub(2);
    let spans = vec![
        Span::raw("  "),
        Span::styled(label.to_string(), label_style),
        Span::raw("  "),
        Span::styled("─".repeat(rule_len.max(2)), theme.border()),
        Span::raw("  "),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_observations(frame: &mut Frame, area: Rect, state: &EntityDetailState, theme: Theme) {
    if state.observations.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled("no observations recorded for this entity.", theme.muted()),
            ])),
            area,
        );
        return;
    }

    // Paginate around the cursor: keep a window of size PAGE_SIZE
    // centred on `observation_cursor`.
    let cursor = state.observation_cursor.min(state.observations.len() - 1);
    let half = OBSERVATIONS_PAGE_SIZE / 2;
    let start = cursor.saturating_sub(half).min(
        state
            .observations
            .len()
            .saturating_sub(OBSERVATIONS_PAGE_SIZE),
    );
    let end = (start + OBSERVATIONS_PAGE_SIZE).min(state.observations.len());

    let mut lines = Vec::with_capacity((end - start) * 2);
    for (i, obs) in state.observations[start..end].iter().enumerate() {
        let abs_idx = start + i;
        let selected = abs_idx == cursor && state.focus == Focus::Observations;
        let marker = if selected { "▸" } else { " " };
        let header_style = if selected {
            theme
                .section()
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            theme.muted()
        };
        let source_label = if obs.source.is_empty() {
            "—".to_string()
        } else {
            obs.source.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {marker} "),
                if selected {
                    theme.section()
                } else {
                    theme.border()
                },
            ),
            Span::styled(format_timestamp(obs.observed_at), header_style),
            Span::styled("    ", theme.border()),
            Span::styled(source_label, theme.muted()),
        ]));
        lines.push(Line::from(vec![
            Span::raw("       "),
            Span::styled(
                truncate(&obs.content, 70),
                if selected { theme.fg() } else { theme.muted() },
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_relations(frame: &mut Frame, area: Rect, state: &EntityDetailState, theme: Theme) {
    if state.relations.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "no relations. add one with the mcp `openmemory_add_relation` tool.",
                    theme.muted(),
                ),
            ])),
            area,
        );
        return;
    }

    let visible = area.height.saturating_sub(0) as usize;
    let lines: Vec<Line> = state
        .relations
        .iter()
        .take(visible)
        .enumerate()
        .map(|(i, r)| {
            let selected = i == state.relation_cursor && state.focus == Focus::Relations;
            let marker = if selected { "▸" } else { " " };
            let arrow = if r.outbound { "→" } else { "←" };
            Line::from(vec![
                Span::styled(
                    format!("  {marker}  "),
                    if selected {
                        theme.section()
                    } else {
                        theme.border()
                    },
                ),
                Span::styled(r.kind.clone(), theme.section()),
                Span::raw(" "),
                Span::styled(arrow.to_string(), theme.border()),
                Span::raw(" "),
                Span::styled(
                    r.other_name.clone(),
                    if selected {
                        theme.fg().add_modifier(ratatui::style::Modifier::BOLD)
                    } else {
                        theme.fg()
                    },
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_hint(frame: &mut Frame, area: Rect, theme: Theme) {
    let spans = vec![
        Span::raw("  "),
        Span::styled("tab", theme.section()),
        Span::styled(" switch list  ·  ", theme.muted()),
        Span::styled("↑ ↓", theme.section()),
        Span::styled(" move  ·  ", theme.muted()),
        Span::styled("enter", theme.section()),
        Span::styled(" jump to relation  ·  ", theme.muted()),
        Span::styled("esc", theme.section()),
        Span::styled(" close", theme.muted()),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(area.width.saturating_sub(w) / 2),
            Constraint::Length(w),
            Constraint::Min(0),
        ])
        .split(area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(h) / 2),
            Constraint::Length(h),
            Constraint::Min(0),
        ])
        .split(horizontal[1]);
    vertical[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_graph::MemoryTier;

    fn obs(observed_at: i64, content: &str) -> Observation {
        Observation {
            id: format!("obs-{observed_at}"),
            entity_id: "ent".into(),
            content: content.into(),
            observed_at,
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
        }
    }

    #[test]
    fn derive_daily_writes_buckets_observations_into_the_right_day() {
        // Construct observations with explicit timestamps: today (now),
        // yesterday, and 13 days ago — the oldest still inside the
        // window. Anything older than `days` is dropped.
        let now = current_unix_secs();
        let today = now;
        let yesterday = today - 86_400;
        let thirteen_days_ago = today - 13 * 86_400;
        let fifteen_days_ago = today - 15 * 86_400;
        let observations = vec![
            obs(today, "a"),
            obs(today, "b"),
            obs(yesterday, "c"),
            obs(thirteen_days_ago, "d"),
            obs(fifteen_days_ago, "e"),
        ];
        let buckets = derive_daily_writes(&observations, SPARKLINE_DAYS);
        assert_eq!(buckets.len(), SPARKLINE_DAYS);
        // Today (last entry) has 2 observations.
        assert_eq!(*buckets.last().unwrap(), 2);
        // Yesterday has 1.
        assert_eq!(buckets[SPARKLINE_DAYS - 2], 1);
        // Day at index 0 (13 days ago) has 1; the 15-day-old is dropped.
        assert_eq!(buckets[0], 1);
        // Total should be 4 (one out-of-window observation is excluded).
        let total: u64 = buckets.iter().sum();
        assert_eq!(total, 4);
    }

    #[test]
    fn cycle_focus_toggles_between_sections() {
        let mut s = EntityDetailState {
            name: "x".into(),
            entity: None,
            observations: Vec::new(),
            relations: Vec::new(),
            daily_writes: vec![0; SPARKLINE_DAYS],
            error: None,
            focus: Focus::Observations,
            observation_cursor: 0,
            relation_cursor: 0,
            pending_refocus: None,
        };
        assert_eq!(s.focus, Focus::Observations);
        s.cycle_focus();
        assert_eq!(s.focus, Focus::Relations);
        s.cycle_focus();
        assert_eq!(s.focus, Focus::Observations);
    }

    #[test]
    fn move_cursor_stays_inside_bounds() {
        let mut s = EntityDetailState {
            name: "x".into(),
            entity: None,
            observations: vec![obs(0, "a"), obs(1, "b"), obs(2, "c")],
            relations: Vec::new(),
            daily_writes: vec![0; SPARKLINE_DAYS],
            error: None,
            focus: Focus::Observations,
            observation_cursor: 0,
            relation_cursor: 0,
            pending_refocus: None,
        };
        s.move_cursor(-5);
        assert_eq!(s.observation_cursor, 0);
        s.move_cursor(10);
        assert_eq!(s.observation_cursor, 2);
        s.move_cursor(-1);
        assert_eq!(s.observation_cursor, 1);
    }
}
