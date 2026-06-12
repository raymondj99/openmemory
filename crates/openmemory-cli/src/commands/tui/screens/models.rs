//! Models panel.
//!
//! Browses the static [`ModelRegistry`] and shows which embedding
//! model is currently active. Mirrors `openmemory model list` output:
//! the active row pops in green (same `SUCCESS` accent), peer rows
//! render in the cool section accent, and a row whose weights aren't
//! on disk is tagged in `WARN` and made un-selectable for switching.
//!
//! Switching the active model goes through the exact same code path
//! as `commands::model::use_model`: write `config.default.model` and
//! save. We do **not** re-embed observations from inside the TUI;
//! the confirmation dialog explicitly tells the user to run
//! `openmemory consolidate` to pick up the new model.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use openmemory_core::config::Config;
use openmemory_embed::{Model, ModelManager, ModelRegistry};
use openmemory_graph::MemoryStore;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::commands::model::resolve_active;
use crate::commands::tui::theme::Theme;

/// Modal layered over the panel.
#[derive(Debug, Default, Clone)]
enum Overlay {
    #[default]
    None,
    /// "Switch active model to <name>?" yes/no.
    Confirm,
    /// `i` — registry metadata.
    Details,
    /// `w` — resolved on-disk path.
    WeightsPath { resolved: Option<String> },
    /// Transient "config saved" / "error: ..." line.
    Notice {
        message: String,
        kind: NoticeKind,
    },
}

#[derive(Debug, Clone, Copy)]
enum NoticeKind {
    Success,
    Error,
}

pub struct ModelsState {
    registry: ModelRegistry,
    /// Cached "is on disk" status for each model in registry order.
    /// Recomputed on every key handler to stay in sync with model
    /// downloads happening outside the TUI.
    downloaded: Vec<bool>,
    /// Resolved active model name (canonical, never an alias).
    active_name: String,
    /// Configured-but-unknown model name, if any.
    stale_configured: Option<String>,
    selected: usize,
    overlay: Overlay,
}

impl ModelsState {
    pub fn new() -> Self {
        let registry = ModelRegistry::default();
        let resolution = resolve_active(None, &registry);
        Self {
            registry,
            downloaded: Vec::new(),
            active_name: resolution.active.name.to_string(),
            stale_configured: None,
            selected: 0,
            overlay: Overlay::None,
        }
    }

    /// Re-read the config and on-disk weights state. Called on every
    /// keypress so an external `openmemory model download` immediately
    /// updates the panel's "downloaded?" column.
    fn refresh(&mut self) {
        let config = Config::load().unwrap_or_default();
        let resolution = resolve_active(config.default.model.as_deref(), &self.registry);
        self.active_name = resolution.active.name.to_string();
        self.stale_configured = resolution.unresolved;
        let models_dir = Config::models_dir().ok();
        self.downloaded = self
            .registry
            .all()
            .iter()
            .map(|m| {
                models_dir
                    .as_ref()
                    .and_then(|d| ModelManager::new(d.clone()).downloaded_model_dir(m))
                    .is_some()
            })
            .collect();
        if self.selected >= self.registry.all().len() {
            self.selected = self.registry.all().len().saturating_sub(1);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _store: &MemoryStore) {
        // Refresh on every key so the panel reflects external state
        // changes (download via CLI in a separate terminal, etc.).
        self.refresh();

        // Overlays consume input until dismissed.
        match self.overlay.clone() {
            Overlay::Confirm => self.handle_confirm_key(key),
            Overlay::Details | Overlay::WeightsPath { .. } | Overlay::Notice { .. } => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')
                ) {
                    self.overlay = Overlay::None;
                }
            }
            Overlay::None => self.handle_list_key(key),
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent) {
        let models = self.registry.all();
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                if self.selected + 1 < models.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Enter => {
                if self
                    .downloaded
                    .get(self.selected)
                    .copied()
                    .unwrap_or(false)
                {
                    self.overlay = Overlay::Confirm;
                } else {
                    self.overlay = Overlay::Notice {
                        message: format!(
                            "model not downloaded. run `openmemory model download {}` first.",
                            models[self.selected].name
                        ),
                        kind: NoticeKind::Error,
                    };
                }
            }
            KeyCode::Char('i') => self.overlay = Overlay::Details,
            KeyCode::Char('w') => {
                let model = models[self.selected];
                let resolved = Config::models_dir()
                    .ok()
                    .and_then(|d| ModelManager::new(d).downloaded_model_dir(model))
                    .map(|p| p.display().to_string());
                self.overlay = Overlay::WeightsPath { resolved };
            }
            _ => {}
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                let target = self.registry.all()[self.selected];
                match save_active_model(target) {
                    Ok(()) => {
                        self.active_name = target.name.to_string();
                        self.overlay = Overlay::Notice {
                            message: format!(
                                "active model set to {}. run `openmemory consolidate` to re-embed.",
                                target.name
                            ),
                            kind: NoticeKind::Success,
                        };
                    }
                    Err(e) => {
                        self.overlay = Overlay::Notice {
                            message: format!("failed to write config: {e}"),
                            kind: NoticeKind::Error,
                        };
                    }
                }
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.overlay = Overlay::None;
            }
            _ => {}
        }
    }
}

impl Default for ModelsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Persist `model` as the new active embedding model. Mirrors the
/// write path in [`crate::commands::model::use_model`] so the TUI and
/// the one-shot CLI cannot drift out of sync.
fn save_active_model(model: &Model) -> Result<()> {
    let config_path = Config::config_path()?;
    let mut config = Config::load().unwrap_or_default();
    config.default.model = Some(model.name.to_string());
    config.save(&config_path)?;
    Ok(())
}

// ─────────────────────────────────── render ────────────────────────────────────

pub fn render(frame: &mut Frame, area: Rect, state: &ModelsState, theme: Theme) {
    let body = list_lines(state, theme);
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), area);

    match &state.overlay {
        Overlay::None => {}
        Overlay::Confirm => render_confirm(frame, area, state, theme),
        Overlay::Details => render_details(frame, area, state, theme),
        Overlay::WeightsPath { resolved } => render_weights(frame, area, resolved.as_deref(), theme),
        Overlay::Notice { message, kind } => {
            render_notice(frame, area, message, *kind, theme);
        }
    }
}

fn list_lines(state: &ModelsState, theme: Theme) -> Vec<Line<'static>> {
    let models = state.registry.all();
    let mut out: Vec<Line> = Vec::with_capacity(models.len() * 3 + 2);

    if let Some(stale) = &state.stale_configured {
        out.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!(
                    "notice: configured model {stale:?} is no longer registered; using {} instead.",
                    state.active_name,
                ),
                theme.warn(),
            ),
        ]));
        out.push(Line::raw(""));
    }

    for (i, m) in models.iter().enumerate() {
        let is_active = m.name == state.active_name;
        let is_selected = i == state.selected;
        let downloaded = state.downloaded.get(i).copied().unwrap_or(false);
        let marker = if is_selected { "▸" } else { " " };
        let header_style = if is_active {
            theme.success()
        } else {
            theme.section()
        };
        let mut header = Vec::with_capacity(6);
        header.push(Span::styled(
            format!(" {marker} "),
            if is_selected {
                theme.section()
            } else {
                theme.border()
            },
        ));
        header.push(Span::styled(m.name.to_string(), header_style));

        let mut suffix = String::new();
        if is_active {
            suffix.push_str("active");
        }
        if !suffix.is_empty() {
            suffix.push_str("  ·  ");
        }
        suffix.push_str(if downloaded {
            "downloaded"
        } else {
            "not downloaded"
        });
        let suffix_style = if downloaded { theme.muted() } else { theme.warn() };
        header.push(Span::raw("   "));
        header.push(Span::styled(suffix, suffix_style));
        out.push(Line::from(header));

        let detail = if m.aliases.is_empty() {
            format!("     {} dim · pooling {:?}", m.dimensions, m.pooling)
        } else {
            format!(
                "     {} dim · pooling {:?} · aliases {}",
                m.dimensions,
                m.pooling,
                m.aliases.join(", ")
            )
        };
        out.push(Line::from(Span::styled(detail, theme.muted())));
        out.push(Line::raw(""));
    }
    out
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

fn overlay_block(theme: Theme, title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title(Span::styled(format!(" {title} "), theme.accent()))
}

fn render_confirm(frame: &mut Frame, area: Rect, state: &ModelsState, theme: Theme) {
    let target = state.registry.all()[state.selected];
    let popup = centered_rect(area, 64, 9);
    frame.render_widget(Clear, popup);
    let body = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("  switch active embedding model to "),
            Span::styled(target.name.to_string(), theme.section()),
            Span::raw("?"),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "  existing observations stay embedded with the old model until you",
            theme.muted(),
        )),
        Line::from(Span::styled(
            "  run `openmemory consolidate`, which re-embeds in the background.",
            theme.muted(),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("[y]", theme.success()),
            Span::raw(" confirm   "),
            Span::styled("[n]", theme.warn()),
            Span::raw(" cancel"),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(body)
            .block(overlay_block(theme, "confirm switch"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn render_details(frame: &mut Frame, area: Rect, state: &ModelsState, theme: Theme) {
    let m = state.registry.all()[state.selected];
    let popup = centered_rect(area, 70, 12);
    frame.render_widget(Clear, popup);
    let mut lines = vec![
        Line::raw(""),
        detail_kv(theme, "name", m.name),
        detail_kv(theme, "repo", m.repo_id),
        detail_kv(theme, "dimensions", &m.dimensions.to_string()),
        detail_kv(theme, "max tokens", &m.max_tokens.to_string()),
        detail_kv(theme, "pooling", &format!("{:?}", m.pooling)),
        detail_kv(theme, "output tensor", m.output_tensor),
    ];
    if !m.aliases.is_empty() {
        lines.push(detail_kv(theme, "aliases", &m.aliases.join(", ")));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  press Esc / Enter to close.",
        theme.muted(),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .block(overlay_block(theme, "model details"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn render_weights(frame: &mut Frame, area: Rect, resolved: Option<&str>, theme: Theme) {
    let popup = centered_rect(area, 70, 7);
    frame.render_widget(Clear, popup);
    let body = vec![
        Line::raw(""),
        Line::from(match resolved {
            Some(p) => vec![Span::raw("  "), Span::styled(p.to_string(), theme.fg())],
            None => vec![
                Span::raw("  "),
                Span::styled("(not downloaded)", theme.warn()),
            ],
        }),
        Line::raw(""),
        Line::from(Span::styled(
            "  press Esc / Enter to close.",
            theme.muted(),
        )),
    ];
    frame.render_widget(
        Paragraph::new(body)
            .block(overlay_block(theme, "weights path"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn render_notice(frame: &mut Frame, area: Rect, message: &str, kind: NoticeKind, theme: Theme) {
    let popup = centered_rect(area, 68, 5);
    frame.render_widget(Clear, popup);
    let style = match kind {
        NoticeKind::Success => theme.success(),
        NoticeKind::Error => theme.danger(),
    };
    let body = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(message.to_string(), style),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "  press Esc / Enter to close.",
            theme.muted(),
        )),
    ];
    frame.render_widget(
        Paragraph::new(body)
            .block(overlay_block(theme, "notice"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn detail_kv(theme: Theme, key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{key:<14}"), theme.fg()),
        Span::styled(value.to_string(), theme.muted()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_selects_first_row_with_no_overlay() {
        let s = ModelsState::new();
        assert_eq!(s.selected, 0);
        assert!(matches!(s.overlay, Overlay::None));
    }

    #[test]
    fn registry_is_non_empty() {
        let s = ModelsState::new();
        assert!(
            !s.registry.all().is_empty(),
            "registry must have at least one model"
        );
    }

    #[test]
    fn refresh_marks_active_model_from_default() {
        let mut s = ModelsState::new();
        s.refresh();
        // With no config override the active resolves to the registry default.
        let registry = ModelRegistry::default();
        assert_eq!(s.active_name, registry.default_model().name);
    }
}
