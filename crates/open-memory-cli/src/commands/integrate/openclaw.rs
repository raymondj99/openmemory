//! `open-memory integrate openclaw` — register in OpenClaw's config.
//!
//! See `docs/openclaw.md` for the full contract.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::cli::IntegrateOpenclawArgs;

use super::{
    apply, build_http_entry, build_stdio_entry, entry_name, write_atomic, IntegrationOutcome,
};

const DEFAULT_CONFIG: &str = ".openclaw/openclaw.json";
const SERVER_PATH: &[&str] = &["mcp", "servers"];

pub fn run(profile: &str, args: IntegrateOpenclawArgs) -> Result<()> {
    let config_path = resolve_path(args.config.as_deref())?;
    let name = entry_name(profile);
    let entry = if let Some(addr) = args.http.as_deref() {
        build_http_entry(addr)
    } else {
        build_stdio_entry(profile, &args.binary)?
    };

    let (outcome, new_value) = apply(&config_path, &name, &entry, SERVER_PATH)?;

    if matches!(outcome, IntegrationOutcome::Unchanged) {
        println!(
            "open-memory: openclaw config at {} already has matching `{name}` entry — no changes",
            config_path.display()
        );
        return Ok(());
    }

    write_atomic(&config_path, &new_value)?;
    print_outcome(&outcome, &name, &config_path);
    println!("open-memory: openclaw integration ready. Restart OpenClaw to pick it up.");
    Ok(())
}

fn resolve_path(override_arg: Option<&Path>) -> Result<PathBuf> {
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
    Ok(PathBuf::from(home).join(DEFAULT_CONFIG))
}

fn print_outcome(outcome: &IntegrationOutcome, name: &str, path: &Path) {
    match outcome {
        IntegrationOutcome::Created => {
            println!("open-memory: created openclaw config at {}", path.display());
        }
        IntegrationOutcome::Added => {
            println!(
                "open-memory: added `{name}` to {} (mcp.servers)",
                path.display()
            );
        }
        IntegrationOutcome::Updated => {
            println!(
                "open-memory: updated `{name}` in {} (mcp.servers)",
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

    fn args(http: Option<&str>) -> IntegrateOpenclawArgs {
        IntegrateOpenclawArgs {
            config: None,
            http: http.map(str::to_string),
            binary: None,
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
        with_home(dir.path(), || {
            std::env::set_var("OPENCLAW_CONFIG_PATH", dir.path().join("openclaw.json"));
            run("default", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            let parsed: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.path().join("openclaw.json")).unwrap(),
            )
            .unwrap();
            let entry = parsed
                .pointer("/mcp/servers/open-memory")
                .expect("entry present");
            assert_eq!(entry["command"], "open-memory");
            assert_eq!(entry["args"][0], "mcp");
        });
    }

    #[test]
    fn only_writes_to_resolved_path_not_legacy_mcp_json() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            let stray = dir.path().join(".openclaw").join("mcp.json");
            write_file(&stray, r#"{"servers":{"old":{}}}"#);
            std::env::set_var(
                "OPENCLAW_CONFIG_PATH",
                dir.path().join(".openclaw").join("openclaw.json"),
            );
            run("default", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            assert!(stray.exists());
            let body = std::fs::read_to_string(&stray).unwrap();
            assert!(body.contains("old"));
        });
    }

    #[test]
    fn preserves_sibling_servers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("openclaw.json");
        write_file(&cfg, r#"{"mcp":{"servers":{"other":{"command":"other"}}}}"#);
        with_home(dir.path(), || {
            std::env::set_var("OPENCLAW_CONFIG_PATH", &cfg);
            run("default", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            let parsed: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
            assert!(parsed.pointer("/mcp/servers/open-memory").is_some());
            assert!(parsed.pointer("/mcp/servers/other").is_some());
        });
    }

    #[test]
    fn updates_existing_entry_then_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("openclaw.json");
        write_file(
            &cfg,
            r#"{"mcp":{"servers":{"open-memory":{"command":"old-bin","args":["mcp"]}}}}"#,
        );
        with_home(dir.path(), || {
            std::env::set_var("OPENCLAW_CONFIG_PATH", &cfg);
            run("default", args(None)).unwrap();
            run("default", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            let parsed: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
            let entry = parsed.pointer("/mcp/servers/open-memory").unwrap();
            assert_eq!(entry["command"], "open-memory");
        });
    }

    #[test]
    fn errors_on_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("openclaw.json");
        write_file(&cfg, "{ this is not valid json5 ");
        with_home(dir.path(), || {
            std::env::set_var("OPENCLAW_CONFIG_PATH", &cfg);
            let err = run("default", args(None)).unwrap_err();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");
            assert!(err.to_string().contains("parsing") || err.to_string().contains("JSON5"));
        });
    }

    #[test]
    fn http_entry_uses_streamable_http_transport() {
        let entry = build_http_entry("127.0.0.1:7821");
        assert_eq!(entry["transport"], "streamable-http");
        assert_eq!(entry["url"], "http://127.0.0.1:7821/mcp");
        assert!(entry.get("command").is_none());
    }

    #[test]
    fn non_default_profile_renames_entry() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            std::env::set_var("OPENCLAW_CONFIG_PATH", dir.path().join("openclaw.json"));
            run("work", args(None)).unwrap();
            std::env::remove_var("OPENCLAW_CONFIG_PATH");

            let parsed: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.path().join("openclaw.json")).unwrap(),
            )
            .unwrap();
            assert!(parsed.pointer("/mcp/servers/open-memory-work").is_some());
            assert!(parsed.pointer("/mcp/servers/open-memory").is_none());
        });
    }
}
