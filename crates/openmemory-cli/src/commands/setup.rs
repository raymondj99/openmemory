//! `openmemory setup` - single end-to-end onboarding command.
//!
//! Runs `init` if needed, detects which MCP clients are installed,
//! registers openmemory with each, and prints a final summary plus a
//! "try this" snippet. Designed so a fresh `curl | bash` install can
//! chain straight into a working agent setup without follow-up steps.

use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::cli::{
    InitArgs, IntegrateClaudeCodeArgs, IntegrateClaudeDesktopArgs, IntegrateCodexArgs,
    IntegrateOpenclawArgs, SetupArgs,
};

use super::{init, integrate};

/// Known clients. Order is stable so the summary is deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Openclaw,
}

impl Client {
    fn slug(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude-code",
            Client::ClaudeDesktop => "claude-desktop",
            Client::Codex => "codex",
            Client::Openclaw => "openclaw",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::ClaudeDesktop => "Claude Desktop",
            Client::Codex => "Codex CLI",
            Client::Openclaw => "OpenClaw",
        }
    }

    /// Does this client need a manual restart for the new server to
    /// take effect? Claude Code's `claude mcp add-json` hot-applies;
    /// the other three write a config file the client reads at boot.
    fn needs_restart(self) -> bool {
        !matches!(self, Client::ClaudeCode)
    }

    fn all() -> [Client; 4] {
        [
            Client::ClaudeCode,
            Client::ClaudeDesktop,
            Client::Codex,
            Client::Openclaw,
        ]
    }
}

/// Output of one per-client integration attempt.
struct ClientResult {
    client: Client,
    outcome: Result<()>,
}

pub fn run(profile: &str, args: SetupArgs) -> Result<()> {
    // 1. Initialise the data root. Idempotent without `--force`.
    println!("openmemory: initialising data root...");
    init::run(profile, InitArgs { force: false })?;

    // 2. Decide which clients to register with.
    let requested = parse_client_filter(args.client.as_deref())?;
    let candidates: Vec<Client> = if let Some(set) = requested.as_ref() {
        Client::all()
            .into_iter()
            .filter(|c| set.contains(c))
            .collect()
    } else {
        Client::all().to_vec()
    };

    println!("openmemory: detecting MCP clients...");
    let mut targets: Vec<Client> = Vec::new();
    for client in &candidates {
        let detected = detect(*client);
        let status = if detected {
            "found"
        } else if args.all {
            "not detected (forced via --all)"
        } else {
            "not detected, skipping"
        };
        println!("  - {:<16} {status}", client.label());
        if detected || args.all {
            targets.push(*client);
        }
    }

    if targets.is_empty() {
        println!(
            "openmemory: no MCP clients detected. Pass --all to register with every \
             supported client anyway, or run `openmemory integrate <client>` directly."
        );
        print_try_snippet();
        return Ok(());
    }

    // 3. Register with each target.
    println!("openmemory: registering...");
    let results: Vec<ClientResult> = targets
        .into_iter()
        .map(|client| {
            let outcome = integrate_one(profile, client);
            ClientResult { client, outcome }
        })
        .collect();

    // 4. Optional model download.
    #[cfg(feature = "embeddings")]
    if args.with_model {
        println!("openmemory: downloading default embedding model...");
        if let Err(e) = super::model::run(crate::cli::ModelCommand::Download(
            crate::cli::ModelDownloadArgs { model: None },
        )) {
            eprintln!("openmemory: model download failed: {e:#}");
        }
    }

    // 5. Verification. Skippable via env so the test harness (which
    // sees the test binary as `current_exe`) doesn't print noise.
    let verify_result = if std::env::var("OPENMEMORY_SETUP_SKIP_VERIFY")
        .ok()
        .as_deref()
        == Some("1")
    {
        Ok(())
    } else {
        print!("openmemory: verifying `openmemory mcp` starts... ");
        let _ = std::io::stdout().flush();
        let verify = verify_mcp_starts();
        match &verify {
            Ok(()) => println!("ok"),
            Err(e) => println!("FAILED ({e})"),
        }
        verify
    };

    // 6. Final summary.
    let any_success = results.iter().any(|r| r.outcome.is_ok());
    let all_failed = !any_success;

    println!();
    println!("Setup summary:");
    for r in &results {
        match &r.outcome {
            Ok(()) => println!("  - {:<16} ok", r.client.label()),
            Err(e) => println!("  - {:<16} FAILED: {e:#}", r.client.label()),
        }
    }

    let restart_needed: Vec<&'static str> = results
        .iter()
        .filter(|r| r.outcome.is_ok() && r.client.needs_restart())
        .map(|r| r.client.label())
        .collect();
    if !restart_needed.is_empty() {
        println!();
        println!(
            "Restart these clients to pick up the new server: {}.",
            restart_needed.join(", ")
        );
    }

    print_try_snippet();

    // Suppress unused-warning when stdin is a TTY (we currently only
    // check non-interactivity for future prompt skipping).
    let _ = std::io::stdin().is_terminal();
    let _ = args.yes;

    if all_failed {
        anyhow::bail!("setup: every client registration failed");
    }
    verify_result.context("setup verification failed")?;
    Ok(())
}

fn integrate_one(profile: &str, client: Client) -> Result<()> {
    match client {
        Client::ClaudeCode => integrate::claude_code::run(
            profile,
            IntegrateClaudeCodeArgs {
                config: None,
                http: None,
                binary: None,
                no_cli: false,
            },
        ),
        Client::ClaudeDesktop => integrate::claude_desktop::run(
            profile,
            IntegrateClaudeDesktopArgs {
                config: None,
                http: None,
                binary: None,
            },
        ),
        Client::Codex => integrate::codex::run(
            profile,
            IntegrateCodexArgs {
                config: None,
                http: None,
                binary: None,
            },
        ),
        Client::Openclaw => integrate::openclaw::run(
            profile,
            IntegrateOpenclawArgs {
                config: None,
                http: None,
                binary: None,
            },
        ),
    }
}

fn parse_client_filter(s: Option<&str>) -> Result<Option<BTreeSet<Client>>> {
    let Some(s) = s else { return Ok(None) };
    let mut out = BTreeSet::new();
    for tok in s.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let client = Client::all()
            .into_iter()
            .find(|c| c.slug() == tok)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown client `{tok}`; valid values: claude-code, claude-desktop, codex, openclaw"
                )
            })?;
        out.insert(client);
    }
    Ok(Some(out))
}

fn detect(client: Client) -> bool {
    match client {
        Client::ClaudeCode => bin_on_path("claude") || home_path(".claude.json").exists(),
        Client::ClaudeDesktop => claude_desktop_detected(),
        Client::Codex => {
            if let Ok(env) = std::env::var("CODEX_HOME") {
                if !env.is_empty() && PathBuf::from(env).join("config.toml").exists() {
                    return true;
                }
            }
            bin_on_path("codex") || home_path(".codex/config.toml").exists()
        }
        Client::Openclaw => {
            if let Ok(env) = std::env::var("OPENCLAW_CONFIG_PATH") {
                if !env.is_empty() && PathBuf::from(env).exists() {
                    return true;
                }
            }
            home_path(".openclaw/openclaw.json").exists()
        }
    }
}

fn claude_desktop_detected() -> bool {
    #[cfg(target_os = "macos")]
    {
        if PathBuf::from("/Applications/Claude.app").exists() {
            return true;
        }
        home_path("Library/Application Support/Claude/claude_desktop_config.json").exists()
    }
    #[cfg(target_os = "linux")]
    {
        let config = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.config")
        });
        PathBuf::from(config)
            .join("Claude/claude_desktop_config.json")
            .exists()
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .map(|a| {
                PathBuf::from(a)
                    .join("Claude/claude_desktop_config.json")
                    .exists()
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

fn bin_on_path(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn home_path(rel: &str) -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join(rel)
}

/// Spawn `openmemory mcp` against our own binary, send a single
/// `initialize` JSON-RPC frame on stdin, and assert we get an
/// initialize response within a short window. This catches "binary on
/// PATH but broken" cases such as missing dylibs, wrong architecture,
/// or an unwritable data directory.
fn verify_mcp_starts() -> Result<()> {
    let exe = std::env::current_exe().context("resolving current binary path")?;
    let mut child = Command::new(&exe)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawning openmemory mcp")?;

    let stdout = child.stdout.take().context("no stdout pipe")?;
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .map(|n| if n == 0 { None } else { Some(line) });
        let _ = tx.send(result);
    });

    let init_frame = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"openmemory-setup","version":"0"}}}"#;
    let write_result = (|| -> Result<()> {
        let mut stdin = child.stdin.take().context("no stdin pipe")?;
        writeln!(stdin, "{init_frame}").context("writing initialize frame")?;
        Ok(())
    })();

    let result = match write_result {
        Ok(()) => match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(Some(line))) => validate_initialize_response(&line),
            Ok(Ok(None)) => anyhow::bail!("mcp exited before returning initialize response"),
            Ok(Err(e)) => Err(e).context("reading initialize response"),
            Err(_) => anyhow::bail!("timed out waiting for initialize response"),
        },
        Err(e) => Err(e),
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    result
}

fn validate_initialize_response(line: &str) -> Result<()> {
    let response: serde_json::Value =
        serde_json::from_str(line.trim()).context("parsing initialize response")?;
    match response.get("id").and_then(serde_json::Value::as_i64) {
        Some(1) => {}
        _ => anyhow::bail!("initialize response did not include id 1"),
    }
    if response.get("result").is_none() {
        if let Some(error) = response.get("error") {
            anyhow::bail!("initialize returned error: {error}");
        }
        anyhow::bail!("initialize response missing result");
    }
    Ok(())
}

fn print_try_snippet() {
    println!();
    println!("Try this in your next Claude Code / Codex / Claude Desktop session:");
    println!("  > remember that I prefer Rust over Python");
    println!("  > what do you remember about my language preferences?");
}

/// Helper for `setup` tests to seed a config file. Lives outside the
/// `tests` module so callers don't need to thread it through.
#[cfg(test)]
fn touch(path: &std::path::Path) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, "").unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;

    fn args() -> SetupArgs {
        SetupArgs {
            client: None,
            all: false,
            #[cfg(feature = "embeddings")]
            with_model: false,
            yes: true,
        }
    }

    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }
    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// With no clients available, setup exits cleanly with zero.
    /// Hermetic: we also clear PATH so a `codex` binary that happens
    /// to live on the dev machine doesn't cause detection to flip and
    /// fail this test (real bug observed when codex was brew-installed
    /// mid-development).
    #[test]
    fn no_clients_detected_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            let _skip = EnvGuard::set("OPENMEMORY_SETUP_SKIP_VERIFY", "1");
            let _home = EnvGuard::set("HOME", dir.path().to_str().unwrap());
            let _codex = EnvGuard::set("CODEX_HOME", "/nonexistent-codex");
            let _path = EnvGuard::set("PATH", "");

            let mut a = args();
            // Restrict to codex so we don't depend on whether `claude`
            // happens to be on PATH on the dev machine.
            a.client = Some("codex".to_string());
            let res = run("default", a);
            assert!(
                res.is_ok(),
                "setup should not fail with no clients: {res:?}"
            );
        });
    }

    /// When codex's config exists, setup detects and writes the
    /// entry.
    #[test]
    fn registers_codex_when_detected() {
        let dir = tempfile::tempdir().unwrap();
        let codex_home = dir.path().join("codex-home");
        let cfg = codex_home.join("config.toml");
        touch(&cfg);

        with_home(dir.path(), || {
            let _skip = EnvGuard::set("OPENMEMORY_SETUP_SKIP_VERIFY", "1");
            let _home = EnvGuard::set("HOME", dir.path().to_str().unwrap());
            let _codex = EnvGuard::set("CODEX_HOME", codex_home.to_str().unwrap());

            let mut a = args();
            a.client = Some("codex".to_string());
            let res = run("default", a);
            assert!(res.is_ok(), "{res:?}");

            let body = std::fs::read_to_string(&cfg).unwrap();
            assert!(
                body.contains("[mcp_servers.openmemory]"),
                "codex config should contain openmemory entry:\n{body}"
            );
        });
    }

    /// `--all` writes entries even when detection finds nothing.
    #[test]
    fn all_forces_registration() {
        let dir = tempfile::tempdir().unwrap();
        let codex_home = dir.path().join("codex-home");

        with_home(dir.path(), || {
            let _skip = EnvGuard::set("OPENMEMORY_SETUP_SKIP_VERIFY", "1");
            let _home = EnvGuard::set("HOME", dir.path().to_str().unwrap());
            let _codex = EnvGuard::set("CODEX_HOME", codex_home.to_str().unwrap());

            let mut a = args();
            a.client = Some("codex".to_string());
            a.all = true;
            run("default", a).unwrap();

            let cfg = codex_home.join("config.toml");
            assert!(cfg.exists(), "codex config should be created when --all");
            assert!(std::fs::read_to_string(&cfg)
                .unwrap()
                .contains("[mcp_servers.openmemory]"));
        });
    }

    #[test]
    fn parse_client_filter_rejects_unknown() {
        assert!(parse_client_filter(Some("nope")).is_err());
    }

    #[test]
    fn parse_client_filter_accepts_known() {
        let set = parse_client_filter(Some("codex,claude-code"))
            .unwrap()
            .unwrap();
        assert!(set.contains(&Client::Codex));
        assert!(set.contains(&Client::ClaudeCode));
    }

    #[test]
    fn validate_initialize_response_accepts_success() {
        validate_initialize_response(
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05"}}"#,
        )
        .unwrap();
    }

    #[test]
    fn validate_initialize_response_rejects_error() {
        let err = validate_initialize_response(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"boom"}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("initialize returned error"));
    }
}
