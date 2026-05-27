//! `openmemory integrate codex` - register the MCP server in
//! Codex CLI's config.
//!
//! Codex stores MCP servers in `~/.codex/config.toml` (override via
//! `$CODEX_HOME`) under `[mcp_servers.<name>]` blocks. We use
//! `toml_edit` so unrelated tables and comments survive round-trips.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use toml_edit::{value, Array, DocumentMut, Item, Table, Value as TomlValue};

use crate::cli::IntegrateCodexArgs;

use super::{entry_name, IntegrationOutcome, IntegrationReport};

const DEFAULT_CONFIG: &str = ".codex/config.toml";

pub fn run(profile: &str, args: IntegrateCodexArgs) -> Result<IntegrationReport> {
    let config_path = resolve_path(args.config.as_deref())?;
    let name = entry_name(profile);
    let desired = build_entry_table(profile, &args.binary, args.http.as_deref())?;

    let (outcome, doc) = apply(&config_path, &name, &desired)?;
    if !matches!(outcome, IntegrationOutcome::Unchanged) {
        write_atomic(&config_path, &doc.to_string())?;
    }
    Ok(IntegrationReport {
        outcome,
        path: config_path,
        note: None,
        needs_restart: true,
    })
}

/// Resolve the codex config path, honouring `$CODEX_HOME` and
/// `--config` overrides.
fn resolve_path(override_arg: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = override_arg {
        return Ok(p.to_path_buf());
    }
    if let Ok(env) = std::env::var("CODEX_HOME") {
        if !env.is_empty() {
            return Ok(PathBuf::from(env).join("config.toml"));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| anyhow::anyhow!("could not resolve home directory"))?;
    Ok(PathBuf::from(home).join(DEFAULT_CONFIG))
}

/// Build the TOML table representing our desired `[mcp_servers.<name>]`
/// entry. Stdio mode emits `command`/`args`/`env`. HTTP mode emits
/// `url`/`transport`.
fn build_entry_table(
    profile: &str,
    binary_override: &Option<String>,
    http: Option<&str>,
) -> Result<Table> {
    let mut table = Table::new();
    table.set_implicit(false);

    if let Some(addr) = http {
        table["url"] = value(format!("http://{addr}/mcp"));
        table["transport"] = value("streamable-http");
        return Ok(table);
    }

    let home = openmemory_core::config::Config::home_dir().context("resolving openmemory home")?;
    let binary = binary_override
        .clone()
        .unwrap_or_else(|| "openmemory".to_string());

    table["command"] = value(binary);
    let mut args = Array::new();
    args.push("mcp");
    table["args"] = value(args);

    let mut env = Table::new();
    env.set_implicit(false);
    env["OPENMEMORY_HOME"] = value(home.to_string_lossy().into_owned());
    env["OPENMEMORY_PROFILE"] = value(profile.to_string());
    table["env"] = Item::Table(env);

    Ok(table)
}

/// Compare desired vs existing entry, mutating the document only if
/// the user's table differs.
fn apply(
    path: &Path,
    entry_name: &str,
    desired: &Table,
) -> Result<(IntegrationOutcome, DocumentMut)> {
    let (mut doc, existed) = load_or_default(path)?;

    // Ensure the parent `[mcp_servers]` table exists.
    if !doc.contains_key("mcp_servers") {
        let mut parent = Table::new();
        parent.set_implicit(true);
        doc["mcp_servers"] = Item::Table(parent);
    }
    let parent = doc["mcp_servers"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`mcp_servers` is not a table in {}", path.display()))?;

    let outcome = match parent.get(entry_name) {
        Some(Item::Table(existing)) if tables_equal(existing, desired) => {
            IntegrationOutcome::Unchanged
        }
        Some(_) => IntegrationOutcome::Updated,
        None => {
            if existed {
                IntegrationOutcome::Added
            } else {
                IntegrationOutcome::Created
            }
        }
    };

    if !matches!(outcome, IntegrationOutcome::Unchanged) {
        parent.insert(entry_name, Item::Table(desired.clone()));
    }
    Ok((outcome, doc))
}

/// Structural equality between two TOML tables, ignoring whitespace,
/// comments, and formatting trivia.
fn tables_equal(a: &Table, b: &Table) -> bool {
    table_signature(a) == table_signature(b)
}

fn table_signature(t: &Table) -> JsonValue {
    let mut map = JsonMap::new();
    for (key, item) in t {
        if let Some(value) = item_signature(item) {
            map.insert(key.to_string(), value);
        }
    }
    JsonValue::Object(map)
}

fn item_signature(item: &Item) -> Option<JsonValue> {
    match item {
        Item::None => None,
        Item::Value(value) => Some(value_signature(value)),
        Item::Table(table) => Some(table_signature(table)),
        Item::ArrayOfTables(tables) => Some(JsonValue::Array(
            tables.iter().map(table_signature).collect::<Vec<_>>(),
        )),
    }
}

fn value_signature(value: &TomlValue) -> JsonValue {
    match value {
        TomlValue::String(v) => JsonValue::String(v.value().clone()),
        TomlValue::Integer(v) => JsonValue::Number((*v.value()).into()),
        TomlValue::Float(v) => {
            JsonNumber::from_f64(*v.value()).map_or(JsonValue::Null, JsonValue::Number)
        }
        TomlValue::Boolean(v) => JsonValue::Bool(*v.value()),
        TomlValue::Datetime(v) => JsonValue::String(v.value().to_string()),
        TomlValue::Array(values) => {
            JsonValue::Array(values.iter().map(value_signature).collect::<Vec<_>>())
        }
        TomlValue::InlineTable(table) => {
            let mut map = JsonMap::new();
            for (key, value) in table {
                map.insert(key.to_string(), value_signature(value));
            }
            JsonValue::Object(map)
        }
    }
}

fn load_or_default(path: &Path) -> Result<(DocumentMut, bool)> {
    if !path.exists() {
        return Ok((DocumentMut::new(), false));
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok((DocumentMut::new(), true));
    }
    let doc: DocumentMut = content
        .parse()
        .with_context(|| format!("parsing {} as TOML", path.display()))?;
    Ok((doc, true))
}

fn write_atomic(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent of {}", path.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming into place: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;

    fn args() -> IntegrateCodexArgs {
        IntegrateCodexArgs {
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

    fn read_doc(path: &Path) -> DocumentMut {
        std::fs::read_to_string(path).unwrap().parse().unwrap()
    }

    #[test]
    fn creates_new_config_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            run("default", a).unwrap();

            let doc = read_doc(&cfg);
            let entry = doc["mcp_servers"]["openmemory"].as_table().unwrap();
            assert_eq!(entry["command"].as_str().unwrap(), "openmemory");
            assert_eq!(
                entry["args"].as_array().unwrap().get(0).unwrap().as_str(),
                Some("mcp")
            );
            let env = entry["env"].as_table().unwrap();
            assert_eq!(env["OPENMEMORY_PROFILE"].as_str().unwrap(), "default");
        });
    }

    #[test]
    fn preserves_sibling_blocks_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        write_file(
            &cfg,
            "# top-level note\n\
             [model_providers.openai]\n\
             name = \"OpenAI\"\n\
             base_url = \"https://api.openai.com/v1\"\n\
             \n\
             [mcp_servers.other]\n\
             command = \"other\"\n",
        );
        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            run("default", a).unwrap();
        });

        let raw = std::fs::read_to_string(&cfg).unwrap();
        assert!(raw.contains("# top-level note"), "comment preserved");
        assert!(raw.contains("[model_providers.openai]"));
        assert!(raw.contains("name = \"OpenAI\""));
        assert!(raw.contains("[mcp_servers.other]"));
        assert!(raw.contains("[mcp_servers.openmemory]"));
    }

    #[test]
    fn updates_existing_entry_without_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        write_file(
            &cfg,
            "[mcp_servers.openmemory]\n\
             command = \"old-bin\"\n\
             args = [\"mcp\"]\n",
        );
        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            run("default", a).unwrap();
        });

        let doc = read_doc(&cfg);
        let parent = doc["mcp_servers"].as_table().unwrap();
        assert_eq!(parent.len(), 1, "single openmemory entry");
        assert_eq!(
            parent["openmemory"]["command"].as_str().unwrap(),
            "openmemory"
        );
    }

    #[test]
    fn idempotent_on_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        with_home(dir.path(), || {
            let mut a1 = args();
            a1.config = Some(cfg.clone());
            run("default", a1).unwrap();
            let mtime_before = std::fs::metadata(&cfg).unwrap().modified().unwrap();

            // Sleep a moment so any rewrite would change mtime.
            #[allow(clippy::disallowed_methods)]
            std::thread::sleep(std::time::Duration::from_millis(50));

            let mut a2 = args();
            a2.config = Some(cfg.clone());
            run("default", a2).unwrap();
            let mtime_after = std::fs::metadata(&cfg).unwrap().modified().unwrap();
            assert_eq!(mtime_before, mtime_after, "no rewrite on idempotent run");
        });
    }

    #[test]
    fn idempotent_when_existing_entry_only_differs_by_comments() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let home = dir.path().to_string_lossy();
        let body = format!(
            "[mcp_servers.openmemory]\n\
             # Keep the binary line documented.\n\
             command = \"openmemory\"\n\
             args = [\"mcp\"] # stdio transport\n\
             \n\
             [mcp_servers.openmemory.env]\n\
             OPENMEMORY_HOME = \"{home}\"\n\
             OPENMEMORY_PROFILE = \"default\"\n"
        );
        write_file(&cfg, &body);

        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            run("default", a).unwrap();
        });

        let after = std::fs::read_to_string(&cfg).unwrap();
        assert_eq!(after, body, "semantic match should not rewrite comments");
    }

    #[test]
    fn http_mode_emits_url_and_transport() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            a.http = Some("127.0.0.1:7800".to_string());
            run("default", a).unwrap();
        });

        let doc = read_doc(&cfg);
        let entry = doc["mcp_servers"]["openmemory"].as_table().unwrap();
        assert_eq!(entry["url"].as_str().unwrap(), "http://127.0.0.1:7800/mcp");
        assert_eq!(entry["transport"].as_str().unwrap(), "streamable-http");
        assert!(entry.get("command").is_none());
    }

    #[test]
    fn non_default_profile_renames_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        with_home(dir.path(), || {
            let mut a = args();
            a.config = Some(cfg.clone());
            run("work", a).unwrap();
        });

        let doc = read_doc(&cfg);
        let parent = doc["mcp_servers"].as_table().unwrap();
        assert!(parent.contains_key("openmemory-work"));
        assert!(!parent.contains_key("openmemory"));
    }

    #[test]
    fn codex_home_override_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let codex_home = dir.path().join("custom-codex");
        std::env::set_var("CODEX_HOME", &codex_home);
        let resolved = resolve_path(None).unwrap();
        std::env::remove_var("CODEX_HOME");
        assert_eq!(resolved, codex_home.join("config.toml"));
    }
}
