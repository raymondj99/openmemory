use std::path::{Path, PathBuf};
use std::sync::Arc;

use openmemory_admin::{
    AdminError, AdminErrorCode, AdminIntegrationClient, AdminIntegrationInstallResponse,
    AdminIntegrationOutcome, AdminIntegrationPreview, AdminIntegrationRequest,
    AdminIntegrationStatus, AdminIntegrationVerifyReport, AdminIntegrationsResponse, AdminJobState,
    ComponentHealth, IntegrationSummary,
};
use toml_edit::{value, Array, DocumentMut, Item, Table, Value as TomlValue};

use crate::state::JobRegistry;
use crate::{unix_now_secs, write_atomic, DaemonConfig};

pub(crate) fn parse_integration_client(value: &str) -> Result<AdminIntegrationClient, AdminError> {
    match value {
        "codex" => Ok(AdminIntegrationClient::Codex),
        "claude-code" => Ok(AdminIntegrationClient::ClaudeCode),
        _ => Err(AdminError::new(
            AdminErrorCode::ClientNotFound,
            format!("integration client {value:?} is not supported"),
            Some("Use one of: codex, claude-code."),
            false,
        )),
    }
}

fn integration_label(client: AdminIntegrationClient) -> &'static str {
    match client {
        AdminIntegrationClient::Codex => "Codex CLI",
        AdminIntegrationClient::ClaudeCode => "Claude Code",
    }
}

fn integration_needs_restart(client: AdminIntegrationClient) -> bool {
    matches!(client, AdminIntegrationClient::Codex)
}

fn integration_entry_name(profile: &str) -> String {
    if profile == "default" {
        "openmemory".to_string()
    } else {
        format!("openmemory-{profile}")
    }
}

pub(crate) fn integrations_response(config: &DaemonConfig) -> AdminIntegrationsResponse {
    let integrations: Vec<_> = [
        AdminIntegrationClient::Codex,
        AdminIntegrationClient::ClaudeCode,
    ]
    .into_iter()
    .map(|client| integration_status(config, client))
    .collect();
    let summary = IntegrationSummary {
        detected: integrations.iter().filter(|i| i.detected).count() as u32,
        configured: integrations.iter().filter(|i| i.configured).count() as u32,
        broken: integrations
            .iter()
            .filter(|i| matches!(i.health.state, openmemory_admin::ComponentState::Error))
            .count() as u32,
    };
    AdminIntegrationsResponse {
        integrations,
        summary,
    }
}

fn integration_status(
    config: &DaemonConfig,
    client: AdminIntegrationClient,
) -> AdminIntegrationStatus {
    let request = AdminIntegrationRequest::default();
    let path = resolve_integration_path(client, &request).unwrap_or_else(|_| PathBuf::new());
    let entry_name = integration_entry_name(config.active_profile());
    let detected = integration_detected(client, &path);
    let preview = integration_preview(config, client, &request);
    let (configured, health) = match preview {
        Ok(preview) if matches!(preview.outcome, AdminIntegrationOutcome::Unchanged) => (
            true,
            ComponentHealth::ok("integration is configured").with_details(serde_json::json!({
                "config_path": preview.config_path,
                "entry_name": preview.entry_name,
            })),
        ),
        Ok(preview) => (
            false,
            ComponentHealth::warning(
                AdminErrorCode::ClientConfigStale,
                "integration is not configured",
            )
            .with_details(serde_json::json!({
                "config_path": preview.config_path,
                "entry_name": preview.entry_name,
                "outcome": preview.outcome,
            })),
        ),
        Err(error) => (
            false,
            ComponentHealth::error(error.code, error.message).with_details(error.details),
        ),
    };

    AdminIntegrationStatus {
        client,
        label: integration_label(client).to_string(),
        detected,
        configured,
        config_path: path.display().to_string(),
        entry_name,
        needs_restart: integration_needs_restart(client),
        health,
    }
}

fn integration_detected(client: AdminIntegrationClient, path: &Path) -> bool {
    if path.exists() {
        return true;
    }
    match client {
        AdminIntegrationClient::Codex => bin_on_path("codex"),
        AdminIntegrationClient::ClaudeCode => bin_on_path("claude"),
    }
}

fn bin_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| executable_on_path(&dir, name))
}

#[cfg(unix)]
fn executable_on_path(dir: &Path, name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(dir.join(name))
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn executable_on_path(dir: &Path, name: &str) -> bool {
    let candidate = dir.join(name);
    if candidate.is_file() {
        return true;
    }
    let pathext = std::env::var_os("PATHEXT")
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    pathext.split(';').any(|extension| {
        let extension = extension.trim();
        !extension.is_empty() && dir.join(format!("{name}{extension}")).is_file()
    })
}

fn resolve_integration_path(
    client: AdminIntegrationClient,
    request: &AdminIntegrationRequest,
) -> Result<PathBuf, AdminError> {
    if let Some(path) = request.config_path.as_ref() {
        return Ok(PathBuf::from(path));
    }
    match client {
        AdminIntegrationClient::Codex => {
            if let Ok(home) = std::env::var("CODEX_HOME") {
                if !home.is_empty() {
                    return Ok(PathBuf::from(home).join("config.toml"));
                }
            }
            Ok(user_home()?.join(".codex").join("config.toml"))
        }
        AdminIntegrationClient::ClaudeCode => Ok(user_home()?.join(".claude.json")),
    }
}

fn user_home() -> Result<PathBuf, AdminError> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map_err(|_| {
            AdminError::new(
                AdminErrorCode::ClientConfigUnreadable,
                "could not resolve user home directory",
                Option::<String>::None,
                false,
            )
        })
}

pub(crate) fn integration_preview(
    config: &DaemonConfig,
    client: AdminIntegrationClient,
    request: &AdminIntegrationRequest,
) -> Result<AdminIntegrationPreview, AdminError> {
    match client {
        AdminIntegrationClient::Codex => codex_preview(config, request),
        AdminIntegrationClient::ClaudeCode => claude_code_preview(config, request),
    }
}

pub(crate) fn integration_install(
    config: &DaemonConfig,
    client: AdminIntegrationClient,
    request: &AdminIntegrationRequest,
) -> Result<AdminIntegrationInstallResponse, AdminError> {
    let preview = integration_preview(config, client, request)?;
    let changed = !matches!(preview.outcome, AdminIntegrationOutcome::Unchanged);
    if changed {
        write_text_atomic(Path::new(&preview.config_path), &preview.after)?;
    }
    Ok(AdminIntegrationInstallResponse { preview, changed })
}

fn codex_preview(
    config: &DaemonConfig,
    request: &AdminIntegrationRequest,
) -> Result<AdminIntegrationPreview, AdminError> {
    let path = resolve_integration_path(AdminIntegrationClient::Codex, request)?;
    let before = read_optional_text(&path)?;
    let mut doc = before
        .as_deref()
        .map(str::parse::<DocumentMut>)
        .transpose()
        .map_err(|error| {
            AdminError::new(
                AdminErrorCode::ClientConfigUnreadable,
                "Codex config is not valid TOML",
                Some("Fix the config file and retry."),
                false,
            )
            .with_details(serde_json::json!({ "error": error.to_string() }))
        })?
        .unwrap_or_default();
    let entry_name = integration_entry_name(config.active_profile());
    let desired = codex_desired_table(config, request);
    if !doc.contains_key("mcp_servers") {
        let mut parent = Table::new();
        parent.set_implicit(true);
        doc["mcp_servers"] = Item::Table(parent);
    }
    let parent = doc["mcp_servers"].as_table_mut().ok_or_else(|| {
        AdminError::new(
            AdminErrorCode::ClientConfigUnreadable,
            "`mcp_servers` is not a table in Codex config",
            Option::<String>::None,
            false,
        )
    })?;
    let outcome = match parent.get(&entry_name) {
        Some(Item::Table(existing))
            if toml_table_signature(existing) == toml_table_signature(&desired) =>
        {
            AdminIntegrationOutcome::Unchanged
        }
        Some(_) => AdminIntegrationOutcome::Updated,
        None if before.is_some() => AdminIntegrationOutcome::Added,
        None => AdminIntegrationOutcome::Created,
    };
    if !matches!(outcome, AdminIntegrationOutcome::Unchanged) {
        parent.insert(&entry_name, Item::Table(desired));
    }
    Ok(AdminIntegrationPreview {
        client: AdminIntegrationClient::Codex,
        label: integration_label(AdminIntegrationClient::Codex).to_string(),
        outcome,
        config_path: path.display().to_string(),
        entry_name,
        needs_restart: integration_needs_restart(AdminIntegrationClient::Codex),
        before,
        after: doc.to_string(),
    })
}

fn codex_desired_table(config: &DaemonConfig, request: &AdminIntegrationRequest) -> Table {
    let mut table = Table::new();
    table.set_implicit(false);
    if let Some(addr) = request.http_addr.as_ref() {
        table["url"] = value(format!("http://{addr}/mcp"));
        table["transport"] = value("streamable-http");
        return table;
    }
    table["command"] = value(
        request
            .binary
            .clone()
            .unwrap_or_else(|| "openmemory".to_string()),
    );
    let mut args = Array::new();
    args.push("mcp");
    table["args"] = value(args);
    let mut env = Table::new();
    env.set_implicit(false);
    env["OPENMEMORY_HOME"] = value(config.home().to_string_lossy().into_owned());
    env["OPENMEMORY_PROFILE"] = value(config.active_profile().to_string());
    table["env"] = Item::Table(env);
    table
}

fn claude_code_preview(
    config: &DaemonConfig,
    request: &AdminIntegrationRequest,
) -> Result<AdminIntegrationPreview, AdminError> {
    let path = resolve_integration_path(AdminIntegrationClient::ClaudeCode, request)?;
    let before = read_optional_text(&path)?;
    let mut root: serde_json::Value = before
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| {
            AdminError::new(
                AdminErrorCode::ClientConfigUnreadable,
                "Claude Code config is not valid JSON",
                Some("Fix the config file and retry."),
                false,
            )
            .with_details(serde_json::json!({ "error": error.to_string() }))
        })?
        .unwrap_or_else(|| serde_json::json!({}));
    let entry_name = integration_entry_name(config.active_profile());
    let desired = json_desired_entry(config, request);
    let servers = ensure_json_server_map(&mut root, "mcpServers")?;
    let outcome = match servers.get(&entry_name) {
        Some(existing) if existing == &desired => AdminIntegrationOutcome::Unchanged,
        Some(_) => AdminIntegrationOutcome::Updated,
        None if before.is_some() => AdminIntegrationOutcome::Added,
        None => AdminIntegrationOutcome::Created,
    };
    if !matches!(outcome, AdminIntegrationOutcome::Unchanged) {
        servers.insert(entry_name.clone(), desired);
    }
    let after = serde_json::to_string_pretty(&root)
        .map(|text| format!("{text}\n"))
        .map_err(|error| {
            AdminError::new(
                AdminErrorCode::Internal,
                "failed to encode Claude Code config",
                Option::<String>::None,
                true,
            )
            .with_details(serde_json::json!({ "error": error.to_string() }))
        })?;
    Ok(AdminIntegrationPreview {
        client: AdminIntegrationClient::ClaudeCode,
        label: integration_label(AdminIntegrationClient::ClaudeCode).to_string(),
        outcome,
        config_path: path.display().to_string(),
        entry_name,
        needs_restart: integration_needs_restart(AdminIntegrationClient::ClaudeCode),
        before,
        after,
    })
}

fn json_desired_entry(
    config: &DaemonConfig,
    request: &AdminIntegrationRequest,
) -> serde_json::Value {
    if let Some(addr) = request.http_addr.as_ref() {
        return serde_json::json!({
            "url": format!("http://{addr}/mcp"),
            "transport": "streamable-http",
        });
    }
    serde_json::json!({
        "command": request.binary.clone().unwrap_or_else(|| "openmemory".to_string()),
        "args": ["mcp"],
        "env": {
            "OPENMEMORY_HOME": config.home().to_string_lossy(),
            "OPENMEMORY_PROFILE": config.active_profile(),
        },
    })
}

fn ensure_json_server_map<'a>(
    root: &'a mut serde_json::Value,
    key: &str,
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>, AdminError> {
    if !root.is_object() {
        return Err(AdminError::new(
            AdminErrorCode::ClientConfigUnreadable,
            "client config root is not an object",
            Option::<String>::None,
            false,
        ));
    }
    let Some(object) = root.as_object_mut() else {
        return Err(AdminError::new(
            AdminErrorCode::ClientConfigUnreadable,
            "client config root is not an object",
            Option::<String>::None,
            false,
        ));
    };
    let value = object
        .entry(key.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    value.as_object_mut().ok_or_else(|| {
        AdminError::new(
            AdminErrorCode::ClientConfigUnreadable,
            format!("`{key}` is not an object in client config"),
            Option::<String>::None,
            false,
        )
    })
}

fn read_optional_text(path: &Path) -> Result<Option<String>, AdminError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AdminError::new(
            AdminErrorCode::ClientConfigUnreadable,
            "client config could not be read",
            Option::<String>::None,
            false,
        )
        .with_details(serde_json::json!({
            "path": path.display().to_string(),
            "error": error.to_string(),
        }))),
    }
}

fn write_text_atomic(path: &Path, text: &str) -> Result<(), AdminError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AdminError::new(
                AdminErrorCode::ClientConfigUnreadable,
                "client config directory could not be created",
                Option::<String>::None,
                false,
            )
            .with_details(serde_json::json!({
                "path": parent.display().to_string(),
                "error": error.to_string(),
            }))
        })?;
    }
    write_atomic(path, text.as_bytes()).map_err(|error| {
        AdminError::new(
            AdminErrorCode::ClientConfigUnreadable,
            "client config could not be written",
            Option::<String>::None,
            false,
        )
        .with_details(serde_json::json!({
            "path": path.display().to_string(),
            "error": error.to_string(),
        }))
    })
}

fn toml_table_signature(table: &Table) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, item) in table {
        if let Some(value) = toml_item_signature(item) {
            map.insert(key.to_string(), value);
        }
    }
    serde_json::Value::Object(map)
}

fn toml_item_signature(item: &Item) -> Option<serde_json::Value> {
    match item {
        Item::None => None,
        Item::Value(value) => Some(toml_value_signature(value)),
        Item::Table(table) => Some(toml_table_signature(table)),
        Item::ArrayOfTables(tables) => Some(serde_json::Value::Array(
            tables.iter().map(toml_table_signature).collect(),
        )),
    }
}

fn toml_value_signature(value: &TomlValue) -> serde_json::Value {
    match value {
        TomlValue::String(value) => serde_json::Value::String(value.value().clone()),
        TomlValue::Integer(value) => serde_json::Value::Number((*value.value()).into()),
        TomlValue::Float(value) => serde_json::Number::from_f64(*value.value())
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        TomlValue::Boolean(value) => serde_json::Value::Bool(*value.value()),
        TomlValue::Datetime(value) => serde_json::Value::String(value.value().to_string()),
        TomlValue::Array(values) => {
            serde_json::Value::Array(values.iter().map(toml_value_signature).collect())
        }
        TomlValue::InlineTable(table) => {
            let mut map = serde_json::Map::new();
            for (key, value) in table {
                map.insert(key.to_string(), toml_value_signature(value));
            }
            serde_json::Value::Object(map)
        }
    }
}

pub(crate) fn spawn_integration_verify_job(
    config: DaemonConfig,
    client: AdminIntegrationClient,
    request: AdminIntegrationRequest,
    job_id: String,
    jobs: Arc<JobRegistry>,
) {
    tokio::spawn(async move {
        let _ = jobs.update(&job_id, |job| {
            job.state = AdminJobState::Running;
            job.started_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
            job.message = Some("integration verification running".into());
        });
        let result = tokio::task::spawn_blocking(move || {
            integration_verify_blocking(&config, client, &request)
        })
        .await;
        match result {
            Ok(Ok(report)) => {
                let result = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Succeeded;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("integration verification completed".into());
                    job.result = result;
                    job.error = None;
                });
            }
            Ok(Err(error)) => {
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Failed;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("integration verification failed".into());
                    job.error = Some(error);
                });
            }
            Err(error) => {
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Failed;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("integration verification worker failed".into());
                    job.error = Some(
                        AdminError::new(
                            AdminErrorCode::Internal,
                            "integration verification worker failed",
                            Option::<String>::None,
                            true,
                        )
                        .with_details(serde_json::json!({ "error": error.to_string() })),
                    );
                });
            }
        }
    });
}

fn integration_verify_blocking(
    config: &DaemonConfig,
    client: AdminIntegrationClient,
    request: &AdminIntegrationRequest,
) -> Result<AdminIntegrationVerifyReport, AdminError> {
    let preview = integration_preview(config, client, request)?;
    Ok(AdminIntegrationVerifyReport {
        client,
        config_path: preview.config_path,
        configured: matches!(preview.outcome, AdminIntegrationOutcome::Unchanged),
    })
}
