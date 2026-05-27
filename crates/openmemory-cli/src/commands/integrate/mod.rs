//! `openmemory integrate <target>` — register the binary in an MCP
//! client's config file.
//!
//! Shared logic lives here; per-client modules handle path resolution
//! and JSON structure differences.

pub mod claude_code;
pub mod claude_desktop;
pub mod codex;
pub mod openclaw;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// What a single server entry change looks like, surfaced to the caller
/// for printing.
#[derive(Debug, Clone, PartialEq)]
pub enum IntegrationOutcome {
    /// Config didn't exist; created with our entry.
    Created,
    /// Existing config didn't have an `openmemory` entry; added it.
    Added,
    /// Existing entry differed; replaced it.
    Updated,
    /// Existing entry already matched what we'd write; no file changes.
    Unchanged,
}

/// Structured result of a per-client integration. Each `integrate::*::run`
/// returns this so callers (the standalone CLI dispatch *and* the setup
/// orchestrator) can render in their own house style.
#[derive(Debug, Clone)]
pub struct IntegrationReport {
    /// What changed on disk (or didn't).
    pub outcome: IntegrationOutcome,
    /// Resolved config path. For any non-`Unchanged` outcome this
    /// points at the file we wrote; for `Unchanged` it's the path we
    /// inspected.
    pub path: PathBuf,
    /// Free-form note appended to the success line. Used by
    /// `claude-code` to surface the `claude mcp add-json` path.
    pub note: Option<String>,
    /// Does the client need a manual restart for the new server to
    /// take effect? Claude Code's `claude mcp add-json` hot-applies;
    /// the other three write a config file the client reads at boot.
    pub needs_restart: bool,
}

impl IntegrationReport {
    /// Compact one-line summary used as a suffix on the step / detail
    /// line that announced this client. Format: `<action> · <path>`
    /// in Unicode terminals, with an ASCII separator in plain output.
    pub fn suffix(&self) -> String {
        let sep = crate::ui::glyph::Glyph::Separator.as_str();
        let action = match self.outcome {
            IntegrationOutcome::Created => "created",
            IntegrationOutcome::Added => "added",
            IntegrationOutcome::Updated => "updated",
            IntegrationOutcome::Unchanged => "already current",
        };
        if let Some(note) = &self.note {
            format!("{action} {sep} {note}")
        } else {
            format!("{action} {sep} {}", self.path.display())
        }
    }
}

/// Render a single-client integration result for the standalone
/// `openmemory integrate <client>` paths. Uses the same Steps look as
/// `setup`, plus a one-line restart hint when applicable.
pub fn render_standalone(client_label: &str, report: &IntegrationReport) {
    use crate::ui::{glyph::Glyph, paint, stdout_stream, steps::Steps, style};

    let mut stream = stdout_stream();
    {
        let mut steps = Steps::new(&mut stream).opener(false);
        steps
            .step(format!("registering with {client_label}"))
            .finish_ok(report.suffix());
    }
    if report.needs_restart && !matches!(report.outcome, IntegrationOutcome::Unchanged) {
        let arrow = paint(style::ACCENT_DIM, Glyph::Arrow.as_str());
        let hint = paint(
            style::MUTED,
            &format!("restart {client_label} to pick up the new server."),
        );
        let _ = std::io::Write::write_all(&mut stream, format!("  {arrow} {hint}\n").as_bytes());
    }
}

/// Build the canonical stdio MCP entry. Shared across all integration
/// targets; the entry shape is the same everywhere.
pub fn build_stdio_entry(profile: &str, binary_override: &Option<String>) -> Result<Value> {
    let home = openmemory_core::config::Config::home_dir().context("resolving openmemory home")?;
    let binary = binary_override
        .clone()
        .unwrap_or_else(|| "openmemory".to_string());

    Ok(serde_json::json!({
        "command": binary,
        "args": ["mcp"],
        "env": {
            "OPENMEMORY_HOME": home.to_string_lossy(),
            "OPENMEMORY_PROFILE": profile,
        },
    }))
}

/// Build an HTTP-transport entry (streamable-http).
pub fn build_http_entry(addr: &str) -> Value {
    let url = format!("http://{addr}/mcp");
    serde_json::json!({
        "url": url,
        "transport": "streamable-http",
    })
}

/// Compute the server entry name for a given profile.
pub fn entry_name(profile: &str) -> String {
    if profile == "default" {
        "openmemory".to_string()
    } else {
        format!("openmemory-{profile}")
    }
}

/// Navigate into a nested JSON object at `keys`, creating intermediate
/// objects as needed. Returns a mutable reference to the innermost map.
pub fn ensure_server_map<'a>(
    root: &'a mut Value,
    keys: &[&str],
) -> Result<&'a mut Map<String, Value>> {
    let mut current = root;
    for key in keys {
        let current_type = json_type(current);
        let map = current
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("expected object, got {current_type}"))?;
        current = map
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    let leaf_type = json_type(current);
    current
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("server map is not an object (got {leaf_type})"))
}

/// Apply an entry to a config file. The caller specifies the JSON key
/// path to the server map (e.g. `["mcp", "servers"]` for OpenClaw,
/// `["mcpServers"]` for Claude).
pub fn apply(
    path: &Path,
    entry_name: &str,
    entry: &Value,
    server_path: &[&str],
) -> Result<(IntegrationOutcome, Value)> {
    let (mut root, existed) = load_or_default(path)?;

    let servers_map = ensure_server_map(&mut root, server_path)?;

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

/// Load a JSON/JSON5 config file, or return an empty object if absent.
pub fn load_or_default(path: &Path) -> Result<(Value, bool)> {
    if !path.exists() {
        return Ok((Value::Object(Map::new()), false));
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
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

/// Write a JSON value to disk atomically (temp file + rename).
pub fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent of {}", path.display()))?;
    }
    let mut text = serde_json::to_string_pretty(value).context("serialising config")?;
    text.push('\n');

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming into place: {}", path.display()))?;
    Ok(())
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
