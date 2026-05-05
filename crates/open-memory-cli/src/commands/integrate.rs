//! `open-memory integrate openclaw` — register the binary in OpenClaw's
//! config file.
//!
//! See `docs/02-openclaw-integration.md` for the full contract. In short:
//!
//! 1. Resolve `$OPENCLAW_CONFIG_PATH` or fall back to
//!    `~/.openclaw/openclaw.json`.
//! 2. Parse the existing file as JSON5 (works on plain JSON too, since JSON
//!    is a subset of JSON5). If the file does not exist, start with `{}`.
//! 3. Ensure `mcp.servers` exists; set the `open-memory[-<profile>]` key to
//!    the canonical entry shape. Idempotent: if the entry is already
//!    byte-equivalent we report no-op and exit zero.
//! 4. Write back atomically (temp file + rename) as plain JSON. Comments
//!    and trailing commas the JSON5 input may have carried are not
//!    preserved by the round-trip in v0.1; the upstream `openclaw mcp set`
//!    path (when the binary is on PATH) preserves them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use open_memory_core::config::Config;

use crate::cli::IntegrateOpenclawArgs;

/// Default OpenClaw config path under the user's home directory. Used when
/// `$OPENCLAW_CONFIG_PATH` is unset.
pub const DEFAULT_OPENCLAW_CONFIG: &str = ".openclaw/openclaw.json";

/// What a single `mcp.servers.<name>` entry change looks like, surfaced to
/// the caller for printing.
#[derive(Debug, Clone, PartialEq)]
pub enum IntegrationOutcome {
    /// Config didn't exist; created with our entry under `mcp.servers`.
    Created,
    /// Existing config didn't have an `open-memory` entry; added it.
    Added,
    /// Existing entry differed; replaced it.
    Updated,
    /// Existing entry already matched what we'd write; no file changes.
    Unchanged,
}

pub fn run(profile: &str, args: IntegrateOpenclawArgs) -> Result<()> {
    let config_path = resolve_openclaw_path(args.config.as_deref())?;
    let entry_name = if profile == "default" {
        "open-memory".to_string()
    } else {
        format!("open-memory-{profile}")
    };
    let entry = build_entry(profile, args.http.as_deref(), &args.binary)?;

    let (outcome, new_value) = apply(&config_path, &entry_name, &entry)?;

    if matches!(outcome, IntegrationOutcome::Unchanged) {
        println!(
            "open-memory: openclaw config at {} already has matching `{entry_name}` entry — no changes",
            config_path.display()
        );
        return Ok(());
    }

    write_atomic(&config_path, &new_value)?;

    match outcome {
        IntegrationOutcome::Unchanged => unreachable!(),
        IntegrationOutcome::Created => {
            println!(
                "open-memory: created openclaw config at {}",
                config_path.display()
            );
        }
        IntegrationOutcome::Added => {
            println!(
                "open-memory: added `{entry_name}` to {} (mcp.servers)",
                config_path.display()
            );
        }
        IntegrationOutcome::Updated => {
            println!(
                "open-memory: updated `{entry_name}` in {} (mcp.servers)",
                config_path.display()
            );
        }
    }

    println!("open-memory: openclaw integration ready. Restart OpenClaw to pick it up.");
    Ok(())
}

/// Resolve where to write OpenClaw config.
pub fn resolve_openclaw_path(override_arg: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = override_arg {
        return Ok(p.to_path_buf());
    }
    if let Ok(env) = std::env::var("OPENCLAW_CONFIG_PATH") {
        if !env.is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| anyhow::anyhow!("could not resolve home directory"))?;
    Ok(PathBuf::from(home).join(DEFAULT_OPENCLAW_CONFIG))
}

/// Build the canonical `mcp.servers.open-memory[-<profile>]` entry. With
/// `--http <addr>`, the entry uses streamable-http; without it, stdio.
pub fn build_entry(
    profile: &str,
    http: Option<&str>,
    binary_override: &Option<String>,
) -> Result<Value> {
    if let Some(addr) = http {
        let url = format!("http://{addr}/mcp");
        return Ok(json!({
            "url": url,
            "transport": "streamable-http",
        }));
    }

    let home = Config::home_dir().context("resolving open-memory home for env block")?;
    let binary = binary_override
        .clone()
        .unwrap_or_else(|| "open-memory".to_string());

    Ok(json!({
        "command": binary,
        "args": ["mcp"],
        "env": {
            "OPEN_MEMORY_HOME": home.to_string_lossy(),
            "OPEN_MEMORY_PROFILE": profile,
        },
    }))
}

/// Apply the entry to the config at `path`, returning the outcome and the
/// updated JSON value. Caller writes it back.
pub fn apply(path: &Path, entry_name: &str, entry: &Value) -> Result<(IntegrationOutcome, Value)> {
    let (mut root, existed) = load_or_default(path)?;

    let root_type = json_type(&root);
    let root_map = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("expected object at top level, got {root_type}"))?;

    let mcp = root_map
        .entry("mcp".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let mcp_type = json_type(mcp);
    let mcp_map = mcp
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("mcp is not an object (got {mcp_type}); refusing to overwrite"))?;

    let servers = mcp_map
        .entry("servers".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers_type = json_type(servers);
    let servers_map = servers.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!("mcp.servers is not an object (got {servers_type})")
    })?;

    let outcome = match servers_map.get(entry_name) {
        Some(existing) if existing == entry => IntegrationOutcome::Unchanged,
        Some(_) => IntegrationOutcome::Updated,
        None => {
            if existed {
                IntegrationOutcome::Added
            } else {
                IntegrationOutcome::Created
            }
        }
    };
    if matches!(outcome, IntegrationOutcome::Unchanged) {
        return Ok((outcome, root));
    }
    servers_map.insert(entry_name.to_string(), entry.clone());
    Ok((outcome, root))
}

fn load_or_default(path: &Path) -> Result<(Value, bool)> {
    if !path.exists() {
        return Ok((Value::Object(Map::new()), false));
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok((Value::Object(Map::new()), true));
    }
    let parsed: Value = json5::from_str(&content)
        .with_context(|| format!("parsing {} as JSON5", path.display()))?;
    if !parsed.is_object() {
        anyhow::bail!(
            "{} top-level value is not an object (got {})",
            path.display(),
            json_type(&parsed)
        );
    }
    Ok((parsed, true))
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent of {}", path.display()))?;
    }
    let mut text = serde_json::to_string_pretty(value).context("serialising config")?;
    text.push('\n');

    let tmp = path.with_extension("openclaw-tmp");
    std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into place: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;

    fn args(http: Option<&str>) -> IntegrateOpenclawArgs {
        IntegrateOpenclawArgs {
            config: None,
            http: http.map(str::to_string),
            binary: None,
        }
    }

    fn write(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    /// 1. No existing config: create one with the entry.
    #[test]
    fn integrate_creates_new_config_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            std::env::set_var(
                "OPENCLAW_CONFIG_PATH",
                dir.path().join("openclaw.json"),
            );
            run("default", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            let parsed: Value =
                serde_json::from_str(&std::fs::read_to_string(dir.path().join("openclaw.json")).unwrap())
                    .unwrap();
            let entry = parsed
                .pointer("/mcp/servers/open-memory")
                .expect("entry present");
            assert_eq!(entry["command"], "open-memory");
            assert_eq!(entry["args"][0], "mcp");
        });
    }

    /// 2. Pre-existing `mcp.json`-style file at the legacy spot has no
    ///    bearing — we only look at openclaw.json (or OPENCLAW_CONFIG_PATH).
    #[test]
    fn integrate_only_writes_to_resolved_path_not_legacy_mcp_json() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            // A stray mcp.json under ~/.openclaw should be left alone.
            let stray = dir.path().join(".openclaw").join("mcp.json");
            write(&stray, r#"{"servers":{"old":{}}}"#);
            std::env::set_var(
                "OPENCLAW_CONFIG_PATH",
                dir.path().join(".openclaw").join("openclaw.json"),
            );
            run("default", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            // Stray file is untouched.
            assert!(stray.exists());
            let body = std::fs::read_to_string(&stray).unwrap();
            assert!(body.contains("old"));
        });
    }

    /// 3. Existing openclaw.json with mcp.servers block — entry is added,
    ///    siblings preserved.
    #[test]
    fn integrate_preserves_sibling_servers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("openclaw.json");
        write(
            &cfg,
            r#"{"mcp":{"servers":{"other":{"command":"other"}}}}"#,
        );
        with_home(dir.path(), || {
            std::env::set_var("OPENCLAW_CONFIG_PATH", &cfg);
            run("default", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            let parsed: Value =
                serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
            assert!(parsed
                .pointer("/mcp/servers/open-memory")
                .is_some());
            assert!(parsed.pointer("/mcp/servers/other").is_some());
        });
    }

    /// 4. Existing `open-memory` entry: update it, idempotent on re-run.
    #[test]
    fn integrate_updates_existing_entry_then_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("openclaw.json");
        write(
            &cfg,
            r#"{"mcp":{"servers":{"open-memory":{"command":"old-bin","args":["mcp"]}}}}"#,
        );
        with_home(dir.path(), || {
            std::env::set_var("OPENCLAW_CONFIG_PATH", &cfg);
            run("default", args(None)).unwrap();
            // Run a second time; should be idempotent (Unchanged).
            run("default", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            let parsed: Value =
                serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
            let entry = parsed.pointer("/mcp/servers/open-memory").unwrap();
            assert_eq!(entry["command"], "open-memory");
        });
    }

    /// 5. Corrupt JSON in the file — bail with a helpful error.
    #[test]
    fn integrate_errors_on_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("openclaw.json");
        write(&cfg, "{ this is not valid json5 ");
        with_home(dir.path(), || {
            std::env::set_var("OPENCLAW_CONFIG_PATH", &cfg);
            let err = run("default", args(None)).unwrap_err();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");
            assert!(err.to_string().contains("parsing") || err.to_string().contains("JSON5"));
        });
    }

    #[test]
    fn http_entry_uses_streamable_http_transport() {
        let entry = build_entry("default", Some("127.0.0.1:7821"), &None).unwrap();
        assert_eq!(entry["transport"], "streamable-http");
        assert_eq!(entry["url"], "http://127.0.0.1:7821/mcp");
        assert!(entry.get("command").is_none());
    }

    #[test]
    fn non_default_profile_renames_entry() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            std::env::set_var(
                "OPENCLAW_CONFIG_PATH",
                dir.path().join("openclaw.json"),
            );
            run("work", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            let parsed: Value = serde_json::from_str(
                &std::fs::read_to_string(dir.path().join("openclaw.json")).unwrap(),
            )
            .unwrap();
            assert!(parsed
                .pointer("/mcp/servers/open-memory-work")
                .is_some());
            assert!(parsed.pointer("/mcp/servers/open-memory").is_none());
        });
    }
}
