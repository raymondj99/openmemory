//! `clap` surface for the `openmemory` binary.
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

/// Top-level CLI entry — `openmemory <subcommand>`.
#[derive(Debug, Parser)]
#[command(
    name = "openmemory",
    bin_name = "openmemory",
    version,
    about = "Persistent agent memory + hybrid text search, behind an MCP server",
    long_about = "Persistent agent memory + hybrid text search, behind a Model \
                  Context Protocol server. Stores entities, observations, and \
                  relations in SQLite; recalls them via hybrid (vector + keyword) \
                  search with Ebbinghaus decay scoring."
)]
pub struct Cli {
    /// Override the data root. Defaults to $OPENMEMORY_HOME or
    /// ~/.openmemory.
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
    /// One-shot end-to-end onboarding: init + detect MCP clients +
    /// register openmemory with each.
    Setup(SetupArgs),
    /// Print summary of memory + index state.
    Status,
    /// Start the MCP server (stdio by default; --http for HTTP).
    Mcp(McpArgs),
    /// Run dedup + decay/prune consolidation once.
    Consolidate(ConsolidateArgs),
    /// Register openmemory in an external integration's config.
    #[command(subcommand)]
    Integrate(IntegrateTarget),
    /// Append observations to an entity from the command line. Mainly for
    /// scripting / testing — the MCP `openmemory_remember` tool is the
    /// in-agent path.
    Remember(RememberArgs),
    /// Search memory by natural-language query. Use `--json` to emit one
    /// JSON line per result.
    Recall(RecallArgs),
    /// List entities, optionally filtered by type.
    ListEntities(ListEntitiesArgs),
    /// Hard-delete an entity. Requires `--yes` to confirm.
    ForgetEntity(ForgetEntityArgs),
    /// Manage embedding models (download, list).
    #[cfg(feature = "embeddings")]
    #[command(subcommand)]
    Model(ModelCommand),
    /// Emit shell completions for the named shell.
    #[cfg(feature = "completions")]
    Completions(CompletionsArgs),
    /// Watch a directory and incrementally re-index changed files.
    /// Requires the `watch` build feature (default-on).
    #[cfg(feature = "watch")]
    Watch(WatchArgs),
    /// Run a retrieval-quality benchmark and emit a JSON + text report.
    /// Requires the `eval` build feature.
    #[cfg(feature = "eval")]
    Eval(EvalArgs),
}

/// Subcommands for `openmemory integrate <target>`.
#[derive(Debug, Subcommand)]
pub enum IntegrateTarget {
    /// Add or update the `openmemory` MCP server entry in OpenClaw's
    /// JSON5 config.
    Openclaw(IntegrateOpenclawArgs),
    /// Register the MCP server in Claude Code's config. Prefers the
    /// `claude` CLI when available; falls back to writing ~/.claude.json.
    ClaudeCode(IntegrateClaudeCodeArgs),
    /// Register the MCP server in Claude Desktop's config at the
    /// platform-specific path.
    ClaudeDesktop(IntegrateClaudeDesktopArgs),
    /// Register the MCP server in Codex CLI's `~/.codex/config.toml`.
    Codex(IntegrateCodexArgs),
}

/// `remember` arguments.
#[derive(Debug, Args)]
pub struct RememberArgs {
    /// Entity name to attach observations to.
    pub entity: String,
    /// Entity type. Defaults to `concept`.
    #[arg(long, value_name = "TYPE", default_value = "concept")]
    pub entity_type: String,
    /// One or more facts. Pass multiple `--observation` flags to append
    /// several observations in one transaction.
    #[arg(long = "observation", value_name = "TEXT", required = true)]
    pub observations: Vec<String>,
    /// Optional relation. Format: `TYPE=NAME[:ENTITY_TYPE]`. Repeatable.
    #[arg(long, value_name = "TYPE=NAME[:ENTITY_TYPE]")]
    pub relation: Vec<String>,
    /// Origin tag for audit. Defaults to "cli".
    #[arg(long)]
    pub source: Option<String>,
    /// Emit a single-line JSON result instead of human text.
    #[arg(long)]
    pub json: bool,
}

/// `recall` arguments.
#[derive(Debug, Args)]
pub struct RecallArgs {
    /// Natural-language query.
    pub query: String,
    /// Maximum results to return.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Restrict to a single entity type.
    #[arg(long, value_name = "TYPE")]
    pub entity_type: Option<String>,
    /// Restrict to observations from this source.
    #[arg(long)]
    pub source: Option<String>,
    /// Minimum confidence in [0.0, 1.0].
    #[arg(long)]
    pub min_confidence: Option<f32>,
    /// Emit a JSON array instead of human text.
    #[arg(long)]
    pub json: bool,
}

/// `list-entities` arguments.
#[derive(Debug, Args)]
pub struct ListEntitiesArgs {
    /// Filter by entity type.
    #[arg(long, value_name = "TYPE")]
    pub entity_type: Option<String>,
    /// Maximum entities to return.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Skip first N entities.
    #[arg(long)]
    pub offset: Option<u32>,
    /// Emit a JSON array instead of human text.
    #[arg(long)]
    pub json: bool,
}

/// `forget-entity` arguments.
#[derive(Debug, Args)]
pub struct ForgetEntityArgs {
    /// Entity name to delete.
    pub entity: String,
    /// Confirm the destructive action. Required.
    #[arg(long)]
    pub yes: bool,
}

/// `completions` arguments.
#[cfg(feature = "completions")]
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Target shell.
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

/// `integrate openclaw` arguments.
#[derive(Debug, Args)]
pub struct IntegrateOpenclawArgs {
    /// Override the path to OpenClaw's config (defaults to
    /// $OPENCLAW_CONFIG_PATH or ~/.openclaw/openclaw.json).
    #[arg(long, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,
    /// Emit an HTTP-transport entry (`streamable-http`) pointing at the
    /// given address (e.g. 127.0.0.1:7821) instead of the stdio entry.
    #[arg(long, value_name = "ADDR")]
    pub http: Option<String>,
    /// Override the binary path written into the entry. Defaults to the
    /// bare `openmemory` (which OpenClaw resolves via $PATH).
    #[arg(long, value_name = "PATH")]
    pub binary: Option<String>,
}

/// `integrate claude-code` arguments.
#[derive(Debug, Args)]
pub struct IntegrateClaudeCodeArgs {
    /// Override the path to Claude Code's config (defaults to
    /// ~/.claude.json).
    #[arg(long, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,
    /// Emit an HTTP-transport entry instead of stdio.
    #[arg(long, value_name = "ADDR")]
    pub http: Option<String>,
    /// Override the binary path written into the entry.
    #[arg(long, value_name = "PATH")]
    pub binary: Option<String>,
    /// Skip the `claude` CLI and write the config file directly.
    #[arg(long)]
    pub no_cli: bool,
}

/// `integrate claude-desktop` arguments.
#[derive(Debug, Args)]
pub struct IntegrateClaudeDesktopArgs {
    /// Override the path to Claude Desktop's config. Platform defaults:
    /// macOS ~/Library/Application Support/Claude/claude_desktop_config.json,
    /// Linux ~/.config/Claude/claude_desktop_config.json,
    /// Windows %APPDATA%\Claude\claude_desktop_config.json.
    #[arg(long, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,
    /// Emit an HTTP-transport entry instead of stdio.
    #[arg(long, value_name = "ADDR")]
    pub http: Option<String>,
    /// Override the binary path written into the entry.
    #[arg(long, value_name = "PATH")]
    pub binary: Option<String>,
}

/// `integrate codex` arguments.
#[derive(Debug, Args)]
pub struct IntegrateCodexArgs {
    /// Override the path to Codex CLI's config. Defaults to
    /// `$CODEX_HOME/config.toml` or `~/.codex/config.toml`.
    #[arg(long, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,
    /// Emit an HTTP-transport entry (`streamable-http`) pointing at
    /// the given address (e.g. 127.0.0.1:7800) instead of stdio.
    #[arg(long, value_name = "ADDR")]
    pub http: Option<String>,
    /// Override the binary path written into the entry. Defaults to
    /// the bare `openmemory` (resolved via `$PATH`).
    #[arg(long, value_name = "PATH")]
    pub binary: Option<String>,
}

/// Subcommands for `openmemory model <action>`.
#[cfg(feature = "embeddings")]
#[derive(Debug, Subcommand)]
pub enum ModelCommand {
    /// Download the default (or named) embedding model from Hugging Face.
    Download(ModelDownloadArgs),
    /// List available embedding models and their download status.
    List,
    /// Set the active embedding model for all future commands.
    Use(ModelUseArgs),
}

/// `model download` arguments.
#[cfg(feature = "embeddings")]
#[derive(Debug, Args)]
pub struct ModelDownloadArgs {
    /// Model name or alias. Defaults to the built-in default
    /// (nomic-embed-text-v1.5).
    pub model: Option<String>,
}

/// `model use` arguments.
#[cfg(feature = "embeddings")]
#[derive(Debug, Args)]
pub struct ModelUseArgs {
    /// Model name or alias (e.g. `nomic`, `arctic`).
    pub model: String,
}

/// `setup` arguments.
#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Comma-separated list of clients to target. Valid values:
    /// `claude-code`, `claude-desktop`, `codex`, `openclaw`. If
    /// omitted, every detected client is targeted.
    #[arg(long, value_name = "LIST")]
    pub client: Option<String>,
    /// Register with every known client even if not detected.
    #[arg(long)]
    pub all: bool,
    /// Also download the default embedding model.
    #[cfg(feature = "embeddings")]
    #[arg(long)]
    pub with_model: bool,
    /// Non-interactive; assume yes to every prompt.
    #[arg(long)]
    pub yes: bool,
}

/// `init` arguments.
#[derive(Debug, Args)]
pub struct InitArgs {
    /// Overwrite an existing config.toml if one already lives at the
    /// resolved path. Without `--force`, init refuses to clobber.
    #[arg(long)]
    pub force: bool,
}

/// `mcp` arguments.
#[derive(Debug, Args)]
pub struct McpArgs {
    /// Bind an HTTP listener on the given address (e.g. 127.0.0.1:7800)
    /// instead of stdio. Requires the `mcp-http` build feature.
    #[arg(long, value_name = "ADDR")]
    pub http: Option<std::net::SocketAddr>,
}

/// `watch` arguments.
#[cfg(feature = "watch")]
#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Directory to watch. Walked recursively on startup, then tailed
    /// for create / modify / delete events.
    pub path: std::path::PathBuf,
    /// Debounce window for filesystem events, in milliseconds. Defaults
    /// to the value in `config.toml` (`[watch]` section, 200 ms by
    /// default).
    #[arg(long, value_name = "MS")]
    pub debounce_ms: Option<u64>,
    /// Comma-separated list of file extensions (without leading dot)
    /// to index. Overrides the curated default + any value in
    /// `config.toml`. Example: `--exts md,txt,rs`.
    #[arg(long, value_name = "LIST")]
    pub exts: Option<String>,
    /// Skip files larger than this many bytes. Defaults to 10 MiB.
    #[arg(long, value_name = "BYTES")]
    pub max_size: Option<u64>,
    /// Skip the initial-tree walk and only react to events. Mostly for
    /// debugging; production runs should leave this off so the index
    /// is consistent on startup.
    #[arg(long)]
    pub no_initial_scan: bool,
}

/// `eval` arguments. Behind the `eval` build feature.
#[cfg(feature = "eval")]
#[derive(Debug, Args)]
pub struct EvalArgs {
    /// Dataset adapter name. Built-in: `longmem-s`, `coding-mem`.
    #[arg(long, value_name = "NAME")]
    pub dataset: String,
    /// Path to the dataset fixture tree (JSONL files at the root).
    #[arg(long, value_name = "PATH")]
    pub dataset_path: std::path::PathBuf,
    /// Search mode override. One of `hybrid`, `keyword`, `vector`.
    #[arg(long, value_name = "MODE", default_value = "hybrid")]
    pub mode: String,
    /// Top-K to fetch from the engine; R@K and NDCG@K are computed
    /// from the same single ranked list.
    #[arg(long, value_name = "K", default_value = "10")]
    pub k: usize,
    /// Output path for the JSON report. Omit to skip the JSON write.
    #[arg(long, value_name = "PATH")]
    pub report: Option<std::path::PathBuf>,
    /// Optional baseline JSON to diff against; the run prints the
    /// metric deltas when present.
    #[arg(long, value_name = "PATH")]
    pub baseline: Option<std::path::PathBuf>,
}

/// `consolidate` arguments.
#[derive(Debug, Args)]
pub struct ConsolidateArgs {
    /// Override the dedup Jaccard text-similarity threshold (0.0–1.0).
    #[arg(long, value_name = "F")]
    pub dedup_threshold: Option<f32>,
    /// Override the decay-prune score floor.
    #[arg(long, value_name = "F")]
    pub prune_floor: Option<f32>,
    /// Minimum age (seconds) for an observation to be pruned.
    #[arg(long, value_name = "SECS")]
    pub min_age_secs: Option<i64>,
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
        std::env::set_var("OPENMEMORY_HOME", home);
    }

    match cli.command {
        Command::Init(args) => commands::init::run(&cli.profile, args),
        Command::Setup(args) => commands::setup::run(&cli.profile, args),
        Command::Status => commands::status::run(&cli.profile),
        Command::Mcp(args) => commands::mcp::run(&cli.profile, args),
        Command::Consolidate(args) => commands::consolidate::run(&cli.profile, args),
        Command::Integrate(IntegrateTarget::Openclaw(args)) => {
            commands::integrate::openclaw::run(&cli.profile, args)
        }
        Command::Integrate(IntegrateTarget::ClaudeCode(args)) => {
            commands::integrate::claude_code::run(&cli.profile, args)
        }
        Command::Integrate(IntegrateTarget::ClaudeDesktop(args)) => {
            commands::integrate::claude_desktop::run(&cli.profile, args)
        }
        Command::Integrate(IntegrateTarget::Codex(args)) => {
            commands::integrate::codex::run(&cli.profile, args)
        }
        Command::Remember(args) => commands::scriptable::remember(&cli.profile, args),
        Command::Recall(args) => commands::scriptable::recall(&cli.profile, args),
        Command::ListEntities(args) => commands::scriptable::list_entities(&cli.profile, args),
        Command::ForgetEntity(args) => commands::scriptable::forget_entity(&cli.profile, args),
        #[cfg(feature = "embeddings")]
        Command::Model(cmd) => commands::model::run(cmd),
        #[cfg(feature = "completions")]
        Command::Completions(args) => commands::completions::run(args.shell),
        #[cfg(feature = "watch")]
        Command::Watch(args) => commands::watch::run(&cli.profile, args),
        #[cfg(feature = "eval")]
        Command::Eval(args) => commands::eval::run(args),
    }
}

/// Shared mutex serialising tests that mutate `OPENMEMORY_HOME`. Multiple
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
    let prev = std::env::var("OPENMEMORY_HOME").ok();
    std::env::set_var("OPENMEMORY_HOME", dir);
    let result = f();
    match prev {
        Some(v) => std::env::set_var("OPENMEMORY_HOME", v),
        None => std::env::remove_var("OPENMEMORY_HOME"),
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
        let cli = Cli::parse_from(["openmemory", "init"]);
        assert!(matches!(cli.command, Command::Init(_)));
        assert_eq!(cli.profile, "default");
    }

    #[test]
    fn parse_status_with_profile_override() {
        let cli = Cli::parse_from(["openmemory", "--profile", "alt", "status"]);
        assert_eq!(cli.profile, "alt");
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn parse_init_with_force() {
        let cli = Cli::parse_from(["openmemory", "init", "--force"]);
        match cli.command {
            Command::Init(args) => assert!(args.force),
            other => panic!("expected init, got {other:?}"),
        }
    }

    #[test]
    fn parse_mcp_default_is_stdio() {
        let cli = Cli::parse_from(["openmemory", "mcp"]);
        match cli.command {
            Command::Mcp(args) => assert!(args.http.is_none()),
            other => panic!("expected mcp, got {other:?}"),
        }
    }

    #[test]
    fn parse_mcp_with_http() {
        let cli = Cli::parse_from(["openmemory", "mcp", "--http", "127.0.0.1:7800"]);
        match cli.command {
            Command::Mcp(args) => assert!(args.http.is_some()),
            other => panic!("expected mcp, got {other:?}"),
        }
    }

    #[test]
    fn parse_consolidate() {
        let cli = Cli::parse_from(["openmemory", "consolidate", "--dedup-threshold", "0.9"]);
        match cli.command {
            Command::Consolidate(args) => {
                assert_eq!(args.dedup_threshold, Some(0.9));
            }
            other => panic!("expected consolidate, got {other:?}"),
        }
    }

    #[test]
    fn parse_home_override() {
        let cli = Cli::parse_from(["openmemory", "--home", "/tmp/x", "status"]);
        assert_eq!(cli.home.unwrap().to_str().unwrap(), "/tmp/x");
    }
}
