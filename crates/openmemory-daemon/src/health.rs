//! Health-component assembly and redacted diagnostics.

use openmemory_admin::{AdminDiagnostic, AdminErrorCode, ComponentHealth};

pub(crate) fn mcp_health() -> ComponentHealth {
    match std::env::current_exe() {
        Ok(path) if path.is_file() => ComponentHealth::ok("MCP stdio command is available")
            .with_details(serde_json::json!({ "binary": path.display().to_string() })),
        Ok(path) => ComponentHealth::error(
            AdminErrorCode::ClientConfigStale,
            "current executable is not a file",
        )
        .with_details(serde_json::json!({ "binary": path.display().to_string() })),
        Err(error) => ComponentHealth::error(
            AdminErrorCode::ClientConfigUnreadable,
            "current executable could not be resolved",
        )
        .with_details(serde_json::json!({ "error": error.to_string() })),
    }
}

pub(crate) fn watcher_health() -> ComponentHealth {
    ComponentHealth::ok("watcher is CLI-managed for this daemon version")
}

pub(crate) fn collect_health_diagnostic(
    component: &str,
    health: &ComponentHealth,
    diagnostics: &mut Vec<AdminDiagnostic>,
) {
    let Some(code) = health.code else {
        return;
    };
    diagnostics.push(AdminDiagnostic {
        component: component.to_string(),
        code,
        message: health
            .message
            .clone()
            .unwrap_or_else(|| "diagnostic".to_string()),
        hint: None,
        details: health.details.clone(),
    });
}
