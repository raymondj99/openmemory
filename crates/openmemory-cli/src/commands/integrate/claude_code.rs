//! `openmemory integrate claude-code` — register the MCP server in
//! Claude Code's config.
//!
//! Strategy:
//! 1. If `claude` is on PATH, delegate to `claude mcp add-json` for a
//!    validated, hot-applied registration.
//! 2. Otherwise, write directly to `~/.claude.json` under `mcpServers`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::cli::IntegrateClaudeCodeArgs;

use super::{
    apply, build_http_entry, build_stdio_entry, entry_name, write_atomic, IntegrationOutcome,
};

const DEFAULT_CONFIG: &str = ".claude.json";
const SERVER_PATH: &[&str] = &["mcpServers"];

pub fn run(profile: &str, args: IntegrateClaudeCodeArgs) -> Result<()> {
    let name = entry_name(profile);
    let entry = if let Some(addr) = args.http.as_deref() {
        build_http_entry(addr)
    } else {
        build_stdio_entry(profile, &args.binary)?
    };

    if !args.no_cli {
        match try_cli_add(&name, &entry)? {
            CliAddOutcome::Registered => {
                println!("openmemory: registered `{name}` via `claude mcp add-json`.");
                println!("openmemory: claude-code integration ready.");
                return Ok(());
            }
            CliAddOutcome::AlreadyExists => {
                // Claude Code's CLI treats an existing server as an
                // error. Fall through to the file writer so setup stays
                // idempotent and stale entries can still be updated.
            }
            CliAddOutcome::Unavailable => {}
        }
    }

    let config_path = resolve_path(args.config.as_deref())?;
    let (outcome, new_value) = apply(&config_path, &name, &entry, SERVER_PATH)?;

    if matches!(outcome, IntegrationOutcome::Unchanged) {
        println!(
            "openmemory: claude-code config at {} already has matching `{name}` entry — no changes",
            config_path.display()
        );
        return Ok(());
    }

    write_atomic(&config_path, &new_value)?;
    print_outcome(&outcome, &name, &config_path);
    println!("openmemory: claude-code integration ready.");
    Ok(())
}

enum CliAddOutcome {
    Registered,
    AlreadyExists,
    Unavailable,
}

/// Try to register via `claude mcp add-json`.
fn try_cli_add(name: &str, entry: &serde_json::Value) -> Result<CliAddOutcome> {
    let claude = match which_claude() {
        Some(p) => p,
        None => return Ok(CliAddOutcome::Unavailable),
    };

    let json = serde_json::to_string(entry).context("serialising entry for claude CLI")?;
    let output = Command::new(&claude)
        .args(["mcp", "add-json", name, &json, "-s", "user"])
        .output()
        .with_context(|| format!("running {}", claude.display()))?;

    if output.status.success() {
        return Ok(CliAddOutcome::Registered);
    }

    let details = command_output(&output);
    if is_already_exists_message(&details) {
        return Ok(CliAddOutcome::AlreadyExists);
    }

    if details.is_empty() {
        anyhow::bail!(
            "`claude mcp add-json` exited with status {}; \
             re-run with --no-cli to write the config file directly",
            output.status
        );
    }

    anyhow::bail!(
        "`claude mcp add-json` exited with status {}: {}; \
         re-run with --no-cli to write the config file directly",
        output.status,
        details
    );
}

fn command_output(output: &std::process::Output) -> String {
    let mut parts = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        parts.push(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        parts.push(stderr);
    }
    parts.join("; ")
}

fn is_already_exists_message(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    s.contains("mcp server") && s.contains("already exists")
}

fn which_claude() -> Option<PathBuf> {
    Command::new("claude")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| PathBuf::from("claude"))
}

fn resolve_path(override_arg: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = override_arg {
        return Ok(p.to_path_buf());
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| anyhow::anyhow!("could not resolve home directory"))?;
    Ok(PathBuf::from(home).join(DEFAULT_CONFIG))
}

fn print_outcome(outcome: &IntegrationOutcome, name: &str, path: &Path) {
    match outcome {
        IntegrationOutcome::Created => {
            println!(
                "openmemory: created claude-code config at {}",
                path.display()
            );
        }
        IntegrationOutcome::Added => {
            println!(
                "openmemory: added `{name}` to {} (mcpServers)",
                path.display()
            );
        }
        IntegrationOutcome::Updated => {
            println!(
                "openmemory: updated `{name}` in {} (mcpServers)",
                path.display()
            );
        }
        IntegrationOutcome::Unchanged => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;

    fn args() -> IntegrateClaudeCodeArgs {
        IntegrateClaudeCodeArgs {
            config: None,
            http: None,
            binary: None,
            no_cli: true,
        }
    }

    fn write_file(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn creates_new_config_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            run("default", a).unwrap();

            let parsed: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
            let entry = parsed
                .pointer("/mcpServers/openmemory")
                .expect("entry present");
            assert_eq!(entry["command"], "openmemory");
            assert_eq!(entry["args"][0], "mcp");
        });
    }

    #[test]
    fn preserves_sibling_servers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        write_file(&cfg, r#"{"mcpServers":{"other":{"command":"other"}}}"#);
        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            run("default", a).unwrap();

            let parsed: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
            assert!(parsed.pointer("/mcpServers/openmemory").is_some());
            assert!(parsed.pointer("/mcpServers/other").is_some());
        });
    }

    #[test]
    fn idempotent_on_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        with_home(dir.path(), || {
            let mut a1 = args();
            a1.config = Some(cfg.clone());
            run("default", a1).unwrap();

            let mut a2 = args();
            a2.config = Some(cfg.clone());
            run("default", a2).unwrap();

            let parsed: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
            assert_eq!(parsed["mcpServers"].as_object().unwrap().len(), 1);
        });
    }

    #[test]
    fn http_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            a.http = Some("127.0.0.1:7800".to_string());
            run("default", a).unwrap();

            let parsed: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
            let entry = parsed.pointer("/mcpServers/openmemory").unwrap();
            assert_eq!(entry["transport"], "streamable-http");
            assert!(entry.get("command").is_none());
        });
    }

    #[test]
    fn claude_cli_duplicate_message_is_idempotent() {
        assert!(is_already_exists_message(
            "MCP server openmemory already exists in user config"
        ));
    }

    #[test]
    fn claude_cli_duplicate_message_does_not_match_unrelated_errors() {
        assert!(!is_already_exists_message(
            "failed to parse MCP server JSON"
        ));
    }
}
