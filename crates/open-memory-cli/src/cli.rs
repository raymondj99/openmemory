//! `clap` surface for the `open-memory` binary.
//!
//! The CLI is intentionally thin: every subcommand is a small adapter to a
//! function in the [`crate::commands`] module that does the real work.
//! Keeping this file argument-shape-only means doc/help output, the
//! shell-completion generator, and the cargo-test harness can all read
//! the same source of truth.

use std::ffi::OsString;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::commands;

/// Top-level CLI entry — `open-memory <subcommand>`.
#[derive(Debug, Parser)]
#[command(
    name = "open-memory",
    bin_name = "open-memory",
    version,
    about = "Persistent agent memory + hybrid text search, behind an MCP server",
    long_about = "Persistent agent memory + hybrid text search, behind a Model \
                  Context Protocol server. Stores entities, observations, and \
                  relations in SQLite; recalls them via hybrid (vector + keyword) \
                  search with Ebbinghaus decay scoring."
)]
pub struct Cli {
    /// Override the data root. Defaults to $OPEN_MEMORY_HOME or
    /// ~/.open-memory.
    #[arg(long, global = true, value_name = "PATH")]
    pub home: Option<std::path::PathBuf>,

    /// Memory profile under the data root. Defaults to "default".
    #[arg(long, global = true, value_name = "NAME", default_value = "default")]
    pub profile: String,

    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialise the data directory and write a default config file.
    Init(InitArgs),
    /// Print summary of memory + index state.
    Status,
}

/// `init` arguments.
#[derive(Debug, Args)]
pub struct InitArgs {
    /// Overwrite an existing config.toml if one already lives at the
    /// resolved path. Without `--force`, init refuses to clobber.
    #[arg(long)]
    pub force: bool,
}

/// Parse and dispatch the CLI. Pulled out so tests can drive `Cli` with
/// a synthetic argv. Errors propagate up as `anyhow::Error` so `main`
/// can render them uniformly.
pub fn run<I, T>(args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    if let Some(home) = &cli.home {
        std::env::set_var("OPEN_MEMORY_HOME", home);
    }

    match cli.command {
        Command::Init(args) => commands::init::run(&cli.profile, args),
        Command::Status => commands::status::run(&cli.profile),
    }
}

/// Shared mutex serialising tests that mutate `OPEN_MEMORY_HOME`. Multiple
/// per-file mutexes wouldn't actually synchronise — env vars are process
/// global. Making this `pub(crate)` keeps the lock in one place.
#[cfg(test)]
pub(crate) static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn with_home<F, R>(dir: &std::path::Path, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var("OPEN_MEMORY_HOME").ok();
    std::env::set_var("OPEN_MEMORY_HOME", dir);
    let result = f();
    match prev {
        Some(v) => std::env::set_var("OPEN_MEMORY_HOME", v),
        None => std::env::remove_var("OPEN_MEMORY_HOME"),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_renders_long_help() {
        // Verify the clap surface compiles and the help text renders.
        let mut cmd = Cli::command();
        cmd.build();
    }

    #[test]
    fn parse_init() {
        let cli = Cli::parse_from(["open-memory", "init"]);
        assert!(matches!(cli.command, Command::Init(_)));
        assert_eq!(cli.profile, "default");
    }

    #[test]
    fn parse_status_with_profile_override() {
        let cli = Cli::parse_from(["open-memory", "--profile", "alt", "status"]);
        assert_eq!(cli.profile, "alt");
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn parse_init_with_force() {
        let cli = Cli::parse_from(["open-memory", "init", "--force"]);
        match cli.command {
            Command::Init(args) => assert!(args.force),
            Command::Status => panic!("expected init"),
        }
    }

    #[test]
    fn parse_home_override() {
        let cli = Cli::parse_from(["open-memory", "--home", "/tmp/x", "status"]);
        assert_eq!(cli.home.unwrap().to_str().unwrap(), "/tmp/x");
    }
}
