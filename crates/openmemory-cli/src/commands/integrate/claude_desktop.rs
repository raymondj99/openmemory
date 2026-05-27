//! `openmemory integrate claude-desktop` — register the MCP server in
//! Claude Desktop's config.
//!
//! Config path varies by platform:
//! - macOS:   ~/Library/Application Support/Claude/claude_desktop_config.json
//! - Linux:   ~/.config/Claude/claude_desktop_config.json
//! - Windows: %APPDATA%\Claude\claude_desktop_config.json

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::cli::IntegrateClaudeDesktopArgs;

use super::{
    apply, build_http_entry, build_stdio_entry, entry_name, write_atomic, IntegrationOutcome,
    IntegrationReport,
};

const SERVER_PATH: &[&str] = &["mcpServers"];

pub fn run(profile: &str, args: IntegrateClaudeDesktopArgs) -> Result<IntegrationReport> {
    let config_path = resolve_path(args.config.as_deref())?;
    let name = entry_name(profile);
    let entry = if let Some(addr) = args.http.as_deref() {
        build_http_entry(addr)
    } else {
        build_stdio_entry(profile, &args.binary)?
    };

    let (outcome, new_value) = apply(&config_path, &name, &entry, SERVER_PATH)?;
    if !matches!(outcome, IntegrationOutcome::Unchanged) {
        write_atomic(&config_path, &new_value)?;
    }
    Ok(IntegrationReport {
        outcome,
        path: config_path,
        note: None,
        needs_restart: true,
    })
}

fn resolve_path(override_arg: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = override_arg {
        return Ok(p.to_path_buf());
    }
    platform_default_path()
}

fn platform_default_path() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME")
            .map_err(|_| anyhow::anyhow!("could not resolve home directory"))?;
        Ok(PathBuf::from(home)
            .join("Library/Application Support/Claude/claude_desktop_config.json"))
    }

    #[cfg(target_os = "linux")]
    {
        let config = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.config")
        });
        Ok(PathBuf::from(config).join("Claude/claude_desktop_config.json"))
    }

    #[cfg(target_os = "windows")]
    {
        let appdata =
            std::env::var("APPDATA").map_err(|_| anyhow::anyhow!("could not resolve %APPDATA%"))?;
        Ok(PathBuf::from(appdata).join("Claude/claude_desktop_config.json"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        anyhow::bail!(
            "unsupported platform; pass --config to specify the Claude Desktop config path"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;

    fn args() -> IntegrateClaudeDesktopArgs {
        IntegrateClaudeDesktopArgs {
            config: None,
            http: None,
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
        let cfg = dir.path().join("claude_desktop_config.json");
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
        let cfg = dir.path().join("claude_desktop_config.json");
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
        let cfg = dir.path().join("claude_desktop_config.json");
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
    fn non_default_profile_renames_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("claude_desktop_config.json");
        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            run("work", a).unwrap();

            let parsed: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
            assert!(parsed.pointer("/mcpServers/openmemory-work").is_some());
            assert!(parsed.pointer("/mcpServers/openmemory").is_none());
        });
    }
}
