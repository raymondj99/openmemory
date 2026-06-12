# PLAN-1: Interactive TUI for `openmemory`

Status: proposed
Owner: TBD
Target: `openmemory` CLI, v0.5.x

## Goal

Add a `Claude Code`-like interactive mode to the `openmemory` CLI: a single full-screen TUI with four panels — Stats, Search (with history), Graph (relational context), Models — that read from the existing local store. Land it as four small PRs behind an opt-in cargo feature, with snapshot tests, a binary-size budget, and zero behavior change to existing scriptable commands.

## Non-goals

- Multi-pane editing or write workflows beyond what already exists as subcommands (no inline `remember`, no entity mutation UI in v1; switch-model is the one exception because it is a single-shot config write).
- Remote / multi-host operation. Local store only.
- Mouse support. Keyboard only in v1.
- Replacing any existing subcommand. The TUI is additive.

## Architecture

### Entry point

New subcommand:

```
openmemory tui [--profile <name>]
```

Bare `openmemory` with no args **does not** auto-launch the TUI in v1. The current bare behavior (prints help) stays. We can revisit auto-launch once the TUI has been in real use for a release.

### Crate layout

All new code lives in `crates/openmemory-cli`. No new workspace crates.

```
crates/openmemory-cli/
  Cargo.toml                    # new optional deps under `tui` feature
  src/
    cli.rs                      # add `Tui { profile }` variant
    commands/
      mod.rs                    # `#[cfg(feature = "tui")] pub mod tui;`
      tui/
        mod.rs                  # entry: run(profile) -> Result<()>
        app.rs                  # App state, event loop, focus model
        events.rs               # crossterm -> AppEvent normalizer + tick driver
        history.rs              # search history persistence
        state.rs                # tui_state.toml load/save
        theme.rs                # bridges ui::style palette -> ratatui Style
        widgets/
          chrome.rs             # title bar, tab strip, status bar
          help_overlay.rs       # `?` overlay
        screens/
          stats.rs
          search.rs
          graph.rs
          graph_layout.rs       # adjacency-shell positioner
          models.rs
  tests/
    tui_snapshot.rs             # ratatui TestBackend snapshots
```

### Cargo feature

```toml
[features]
default = ["fts5", "embeddings", "completions", "watch", "mcp-http", "tui"]
tui = ["dep:ratatui", "dep:crossterm"]
```

`tui` joins the default feature set so the published binary ships it, but downstream packagers (and the `eval` minimal builds) can disable it. Verify with `cargo build --no-default-features --features fts5,embeddings` after each PR.

### New dependencies

- `ratatui` (pinned to a workspace version; latest at time of writing is fine)
- `crossterm` (matched to ratatui's supported range; ratatui re-exports it but we use it directly for event sources)

No other new deps. We deliberately **do not** add:
- `tui-input` (we write a small line editor — ~80 lines)
- `tui-tree-widget` (graph view rolls its own renderer)
- `chrono`/`time` (timestamp formatting reuses `commands::status::format_timestamp`)

### Binary-size budget

`v0.1.7` baseline was 15.99 MB with 36% headroom under the 25 MB ceiling. We allow ratatui + crossterm to consume up to **+1.5 MB**. The release CI binary-size check (already in place per `cortex-workflows.md`) must continue to pass without raising the ceiling.

If the measured cost exceeds +1.5 MB, gate the feature out of default and document the opt-in in the README.

## Shared infrastructure

### Event loop

Single thread runs the loop. crossterm events are read with `event::poll(Duration::from_millis(POLL_MS))`. Tick driver fires a synthetic `AppEvent::Tick` every 2 s for live counters. No tokio inside the TUI loop — the store calls are synchronous already.

```rust
enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    Tick,
    DataRefreshed(StatsSnapshot),       // produced by a background thread
}
```

Background work (stats refresh, search execution) runs on a single worker thread with a `mpsc::Sender<AppEvent>` channel. This keeps redraws responsive when an embedding query takes 200 ms.

### Theme bridge

`tui::theme::Theme` reads `ui::color_choice()` and produces `ratatui::style::Style` values for: chrome, accent (warm), heading (cool), muted, success, warn, error. Box-drawing glyphs respect `ui::glyph_mode()`; in `Ascii` mode we use `Borders::PLAIN` with `+`/`-`/`|` characters.

`NO_COLOR`, `CLICOLOR_FORCE`, and `--color=` flags continue to flow through `ui::set_color_override`. The TUI calls `set_color_override` exactly once on launch and never reads env vars itself.

### Persistent state

Two files under `<data_dir>/tui/`:

- `history.jsonl` — append-only, one query per line:
  ```json
  {"ts": 1748359380, "panel": "search", "query": "ratatui rendering loop", "hits": 12, "took_ms": 47}
  ```
  Cap at 500 entries; rewrite-on-load if exceeded.
- `state.toml` — `{ last_panel: "search", graph_focus: "openmemory" }`. Best-effort; missing or malformed file falls back to defaults.

Both files are created lazily on first write. `tui` directory creation uses `std::fs::create_dir_all` with permission errors surfaced via `Banner::error` and the panel rendering the in-memory state regardless.

## Screens

### [1] Stats

Reads `MemoryStore::status()` on launch and every tick. Adds one new store query: `recent_activity(limit: usize) -> Vec<ActivityRow>`. Implementation can piggyback on observation `created_at` for v1 (newest N observations rendered as "remember" events); explicit recall logging is out of scope for this PR.

Layout: a top KV block, a middle bar-chart of entity-type counts (rendered with `ratatui::widgets::BarChart`), and a bottom activity list. Auto-recomputes on `Tick`.

Acceptance:
- 80×24 terminal renders without truncation.
- ANSI off mode passes snapshot.
- Status query latency under 50 ms at 100k observations (existing benchmark coverage in `openmemory-graph` should already satisfy this; if not, add a benchmark).

### [2] Search

Top: one-line input editor with cursor, supports `←` `→` `Home` `End` `Backspace` `Ctrl-W` `Ctrl-U`. `Enter` submits. Submit dispatches to the worker thread, which calls `MemoryStore::recall(...)` (must confirm exact signature in PR1 spike).

Middle: scored hits, `↑` `↓` to select, `e` to expand the selected row to show full observation + entity bundle, `y` to copy the observation text to the system clipboard via `arboard`... actually we omit clipboard in v1 to avoid the dep; `y` prints the text to a transient overlay the user can copy with mouse-select-on-terminal.

Bottom: history pane. `↑` from the input field jumps focus to history; `Enter` on a history row populates the input. `Ctrl-R` triggers fuzzy-over-history (we already do not depend on `fuzzy-matcher`; a small substring filter is sufficient for v1).

History writes happen on every successful query (>= 1 ms after results render) via the worker thread to keep the UI responsive.

Acceptance:
- Empty store renders an empty result list and a friendly "no results" message.
- 1k history entries: opening the Search panel completes in < 50 ms (load only the tail).
- Query cancellation: pressing `Esc` mid-query discards the in-flight result.

### [3] Graph

Adjacency-shell layout, depth 1 by default. Focused entity in the center; neighbors grouped by relation type around it. Cursor moves to neighbors with arrow keys; `Enter` refocuses on the cursor target and pushes the previous focus onto a stack; `Esc` pops.

Layout algorithm (`graph_layout.rs`):
1. Query `get_entity_relations(focused.id)` and group by `relation.kind`.
2. Lay out groups in 8 sectors around the focus (N, NE, E, SE, S, SW, W, NW).
3. If a group has more than `sector_capacity` (default 3) nodes, render the first 2 plus a `(+N more)` row and switch the whole panel to adjacency-list mode if total neighbors > 12.
4. Edges drawn as straight ASCII connectors with the relation kind label centered on the longest segment.

Depth slider (`+` / `-`) toggles between 1 and 2 hops. At depth 2 the rendering always falls back to adjacency-list view.

A `/` jump-to opens an overlay that types into a substring filter over `list_entities`; `Enter` refocuses the graph on the selection.

Acceptance:
- Dense neighborhoods (50+ neighbors) render in adjacency-list mode without panics or overflow.
- Focus stack survives navigation through 20+ refocuses (no leaks).
- An entity with zero relations renders a single-node view with a hint pointing at `openmemory_add_relation`.

### [4] Models

Lists the typed embedding model registry (from `openmemory-embed`). `↑` `↓` selects; `Enter` opens a confirmation dialog explaining the re-embed cost; on confirm, writes the new model into the profile config via the same code path `commands/model.rs` already uses. The TUI does not perform the re-embed itself; it surfaces a "run `openmemory consolidate` to re-embed" hint and exits cleanly.

`i` opens a details panel (runtime, license, dims, pooling, parameters). `w` shows the resolved on-disk weights path.

Acceptance:
- Registry rendering matches `openmemory model list` for the same profile.
- Switching is gated behind an explicit confirmation step (no accidental switches).
- If the active model is missing on disk (e.g., not yet downloaded), the entry is rendered with a warn glyph and switching is disabled with a hint message.

## Keybindings

Global:
| Key | Action |
|---|---|
| `1`–`4` | Jump to panel |
| `Tab` / `Shift-Tab` | Cycle panel |
| `?` | Help overlay |
| `q` / `Ctrl-C` | Quit |
| `Esc` | Pop focus / close overlay |

Per-panel bindings live in each `screens/*.rs` file's `handle_key` and are listed in the status bar contextually.

`Ctrl-C` produces a `SIGINT`-equivalent clean shutdown: leaves alternate screen, restores cursor, drains the worker channel. We install a `Drop` guard on the terminal that restores state even on panic; the panic message is then re-printed to stderr.

## Testing strategy

1. **Unit tests** per module (`history.rs`, `state.rs`, `graph_layout.rs`) live alongside the code.
2. **Snapshot tests** in `tests/tui_snapshot.rs` use `ratatui::backend::TestBackend` at three sizes: 80×24 (minimum supported), 100×30 (common), 160×48 (large). One snapshot per panel per size = 12 baselines. Snapshots are committed text files; updates require eyeballing the diff.
3. **Integration test** for the launch / event-loop wiring: drive a 100-tick loop with synthetic events and assert no panics, no leaked terminal state.
4. **CI** must run `cargo build --no-default-features --features fts5,embeddings` to confirm the TUI is genuinely optional.
5. **Manual**: a short script in `docs/tui-walkthrough.md` (added in PR1) listing the exact keystrokes a reviewer should run before approving a TUI PR.

## Phasing

Four PRs. Each is shippable on its own; users who only get PR1 still see a useful Stats view.

### PR 1 — Scaffold + Stats
- New `tui` feature, deps, `commands/tui/` skeleton, theme bridge, event loop, chrome.
- Stats panel only; the other tabs render a placeholder.
- TestBackend snapshots for Stats at three sizes.
- Binary-size measurement and update to release-check threshold if needed.

Risk: getting terminal save/restore right under panic. Mitigation: ratatui's `Terminal::draw` does not own the alt-screen — we wrap setup/teardown in a `Drop`-guarded `TerminalGuard` and add an integration test that panics inside `draw` and asserts the test harness's stdout is restored.

### PR 2 — Search + history
- Line editor, hits list, expand/yank-overlay.
- `history.jsonl` read/append, history pane, `Ctrl-R` filter.
- Worker thread + cancellable query.

Risk: latency. Mitigation: measure `recall` at p50/p99 on a 100k-observation store before merging; if p99 > 500 ms we add a debounce on the input editor.

### PR 3 — Models picker
- Registry browsing, details, switch-with-confirm.
- Wire into the existing `commands/model.rs` code path; do not duplicate logic.

Risk: switching mid-flight while the daemon is running (if any). Mitigation: switch writes the same config the existing CLI command writes; document that an interactive daemon must be restarted.

### PR 4 — Graph view
- Adjacency-shell layout, depth slider, jump-to overlay, adjacency-list fallback.
- New `MemoryStore::neighbors(name, depth) -> GraphSlice` helper if `get_entity_relations` is not sufficient.

Risk: layout correctness under unusual graphs. Mitigation: property tests on `graph_layout` that generate random adjacency lists and assert: (a) no node is drawn twice, (b) every neighbor is reachable from the focused node by a rendered edge, (c) output stays inside the viewport rectangle.

## Cross-cutting concerns

- **Logging**: `tracing` is already a workspace dep. The TUI installs a file appender at `<data_dir>/tui/log.jsonl` and disables stderr logging while the alt-screen is active. On exit, the file is flushed.
- **Panics**: `std::panic::set_hook` wrapping the existing hook to first restore the terminal, then resume. Tests cover this.
- **Localization**: out of scope. English-only strings, but routed through one `strings.rs` so a future i18n pass is mechanical.
- **Accessibility**: ANSI-off mode produces a readable layout with ASCII box-drawing; this is verified by snapshot tests.

## Open questions to resolve before PR 1

1. Does `MemoryStore::recall` accept a cancellation token or can it be aborted? If not, we live with one in-flight query at a time and disable the input editor while waiting.
2. Is there a stable "active model" reading in the registry, or do we need to introduce one? PR 1 only displays it; PR 3 needs it.
3. Should `--profile` be a global flag (matching every other subcommand) or a `tui` subcommand flag? Default to matching the existing convention.

## Out of scope (documented for future PRs)

- Inline `remember` / write workflows.
- Multi-profile switching from within the TUI.
- Daemon-style live tailing (only useful once we have a real event log).
- Mouse support and clipboard integration.
- Search facets / saved queries.
- Diff view between two entities.

## Acceptance for the whole feature

- All four screens reachable from a single launch.
- `openmemory tui` on a freshly initialized profile renders cleanly with no scary errors (zero entities path).
- All workspace tests pass; CI clippy `-D warnings` clean.
- Release binary size is below 18 MB.
- README has a one-paragraph TUI section with a screenshot.
