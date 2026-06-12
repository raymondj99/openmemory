//! Interactive terminal UI: `openmemory tui`.
//!
//! Four panels — Stats, Search, Graph, Models — read from the local
//! memory store and present it as a single full-screen TUI. The entry
//! point [`run`] opens the store, installs a panic hook that restores
//! the terminal on unexpected exits, wires up a [`TerminalGuard`] so
//! Drop also restores it, and pumps the event loop until the user
//! quits.
//!
//! Subcommand is gated behind the `tui` cargo feature so downstream
//! packagers (and the `eval` minimal builds) can disable the ratatui
//! and crossterm dependencies without affecting any other subcommand.
//!
//! The TUI does **not** spawn the MCP server, edit data, or auto-launch
//! when `openmemory` is invoked with no arguments. It is purely a
//! read-mostly viewer; the only write is the active-model switch in the
//! Models panel, which goes through the exact same code path as
//! `openmemory model use`.
//!
//! ## Tracing during the TUI
//!
//! The crate's global tracing subscriber is installed in
//! `main::init_tracing` and detects TUI invocations from argv so it
//! can route log lines into `<home>/tui/log.jsonl` instead of stderr.
//! That keeps the alt-screen pristine — log output never corrupts the
//! rendered frame. `tail -f <home>/tui/log.jsonl` if you want to
//! watch live traces while the TUI is running.

use std::io::{self, IsTerminal};
use std::sync::Arc;

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_graph::MemoryStore;

pub mod app;
pub mod events;
pub mod history;
pub mod screens;
pub mod state;
pub mod theme;
pub mod widgets;

use app::App;

/// Entry point for `openmemory tui [--profile <name>]`.
///
/// Sanity-checks the terminal (we need a real TTY for keyboard input
/// and raw mode); opens the memory store read-only; installs the panic
/// hook + terminal guard; pumps the event loop. Errors out cleanly if
/// the profile has not been initialised — the TUI never auto-creates a
/// data directory.
pub fn run(profile: &str) -> Result<()> {
    if !io::stdout().is_terminal() {
        anyhow::bail!(
            "`openmemory tui` needs a real terminal on stdout. \
             Pipe-friendly subcommands like `status` or `list-entities` \
             work without a TTY."
        );
    }

    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        anyhow::bail!(
            "openmemory profile {profile:?} is not initialised (no data \
             directory at {}). Run `openmemory init` first.",
            data_dir.display()
        );
    }
    if openmemory_engine::partition::DomainStore::manifest_domains(&data_dir)? > 1 {
        anyhow::bail!(
            "the TUI does not support domain-partitioned profiles yet; \
             use the scriptable commands (`status`, `recall`, `list-entities`)"
        );
    }
    let store = MemoryStore::open(&config, &data_dir)
        .with_context(|| format!("opening memory store at {}", data_dir.display()))?;
    let store = Arc::new(store);

    // Install the panic hook before we touch the terminal so that any
    // panic during setup, draw, or input handling still restores the
    // user's shell instead of leaving them in a broken alt-screen.
    install_panic_hook();

    let _guard = app::TerminalGuard::new().context("entering terminal raw mode")?;
    let mut app = App::new(profile, store, data_dir.clone());
    app.run_event_loop()
}

/// Wrap the existing panic hook with a small prelude that restores the
/// terminal state (leaves alt screen, disables raw mode, shows cursor)
/// before the user's panic handler runs. We do not swallow the panic;
/// we only make sure the user can actually read the error message
/// after the program crashes.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show,
        );
        previous(info);
    }));
}
