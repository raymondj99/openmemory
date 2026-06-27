//! Local OpenMemory daemon and admin API.
//!
//! The daemon is intentionally small at this stage: loopback-only
//! transport, bearer-token auth, and `GET /admin/health`. Later
//! milestones can add profiles, jobs, events, integrations, and backup
//! without changing the desktop/daemon boundary.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::future::Future;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use openmemory_admin::{
    AdminBackupRequest, AdminConsolidateReport, AdminConsolidateRequest, AdminDiagnostic,
    AdminDoctorResponse, AdminEntityDetail, AdminEntitySummary, AdminError, AdminErrorCode,
    AdminErrorResponse, AdminEvent, AdminEventType, AdminIntegrationRequest, AdminJob,
    AdminJobKind, AdminJobState, AdminLogLevel, AdminLogsResponse, AdminObservation,
    AdminProfileSummary, AdminProfilesResponse, AdminRelation, AdminRestorePreflightRequest,
    AdminRestoreRequest, AdminSearchResult, AdminShutdownResponse, AdminTokenRotationResponse,
    ComponentHealth, DaemonRuntimeInfo, HealthResponse, IntegrationSummary, Page, PageRequest,
    ADMIN_API_VERSION,
};
use openmemory_core::config::Config;
#[cfg(feature = "embeddings")]
use openmemory_embed::{ModelManager, ModelRegistry};
use openmemory_engine::partition::DomainStore;
use openmemory_graph::recall::RecallFilters;
use openmemory_graph::ConsolidateConfig;
use openmemory_graph::{new_id, Entity, EntityListRow, EntityType, Observation, Relation};
use openmemory_index::traits::SearchMode;
use rand::RngCore;
use serde::Deserialize;
use thiserror::Error;
use tokio_stream::wrappers::{BroadcastStream, WatchStream};
use tokio_stream::StreamExt;

mod backup;
mod integrations;
mod product_store;
mod state;

use backup::{
    backup_preflight, restore_preflight, spawn_backup_create_job, spawn_restore_job,
    validate_restore_target_profile,
};
use integrations::{
    integration_install, integration_preview, integrations_response, parse_integration_client,
    spawn_integration_verify_job,
};
use state::{AdminState, JobRegistry, RedactedLogRing};

pub const RUN_DIR: &str = "run";
pub const ADMIN_TOKEN_FILE: &str = "admin-token";
pub const DAEMON_RUNTIME_FILE: &str = "daemon.json";
pub const DEFAULT_LOG_RING_CAPACITY: usize = 256;

/// Errors raised before an admin response can be produced.
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("admin token cannot be empty")]
    EmptyAdminToken,
    #[error("admin API bind address must be loopback, got {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("failed to read or write daemon runtime file: {0}")]
    RuntimeIo(#[from] std::io::Error),
    #[error("admin token file has insecure permissions at {path}: mode {mode:o}")]
    InsecureAdminTokenPermissions { path: PathBuf, mode: u32 },
    #[error("daemon runtime metadata is invalid: {0}")]
    RuntimeJson(#[from] serde_json::Error),
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
}

/// Redacted bearer token used by the local admin API.
#[derive(Clone)]
pub struct AdminToken {
    expected: Arc<str>,
}

impl AdminToken {
    /// Build a token from a non-empty string.
    ///
    /// The token is trimmed before storage. Empty or whitespace-only
    /// strings are rejected so the daemon cannot accidentally run with a
    /// trivially bypassed auth check.
    pub fn new(token: impl Into<String>) -> Result<Self, DaemonError> {
        let token = token.into();
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return Err(DaemonError::EmptyAdminToken);
        }
        Ok(Self {
            expected: Arc::from(trimmed.to_string()),
        })
    }

    fn matches(&self, candidate: &str) -> bool {
        constant_time_eq(self.expected.as_bytes(), candidate.as_bytes())
    }
}

impl std::fmt::Debug for AdminToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminToken")
            .field("expected", &"<redacted>")
            .finish()
    }
}

/// Configuration needed to start the daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    bind_addr: SocketAddr,
    admin_token: AdminToken,
    home: PathBuf,
    active_profile: String,
}

impl DaemonConfig {
    /// Create daemon config and reject non-loopback bind addresses.
    pub fn new(
        bind_addr: SocketAddr,
        admin_token: AdminToken,
        home: PathBuf,
        active_profile: impl Into<String>,
    ) -> Result<Self, DaemonError> {
        validate_loopback(bind_addr)?;
        Ok(Self {
            bind_addr,
            admin_token,
            home,
            active_profile: active_profile.into(),
        })
    }

    /// OpenMemory home used by this daemon.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Active profile reported by this daemon.
    #[must_use]
    pub fn active_profile(&self) -> &str {
        &self.active_profile
    }
}

#[derive(Debug, Deserialize)]
struct EntityListQuery {
    limit: Option<u32>,
    offset: Option<u32>,
    entity_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<u32>,
    entity_type: Option<String>,
    source: Option<String>,
    mode: Option<String>,
}

/// Load the per-home admin token or create one with owner-only
/// permissions where supported.
pub fn load_or_create_admin_token(home: &Path) -> Result<String, DaemonError> {
    let path = admin_token_path(home);
    if let Some(existing) = read_admin_token(&path)? {
        return Ok(existing);
    }

    let token = generate_token();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match write_new_token_file(&path, &token) {
        Ok(()) => Ok(token),
        Err(DaemonError::RuntimeIo(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_admin_token(&path)?.ok_or_else(|| {
                DaemonError::RuntimeIo(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "admin token file appeared but could not be read",
                ))
            })
        }
        Err(e) => Err(e),
    }
}

/// Atomically replace the per-home admin token and return the new secret.
pub fn rotate_admin_token(home: &Path) -> Result<String, DaemonError> {
    let token = generate_token();
    let path = admin_token_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_token_file_atomic(&path, &token)?;
    Ok(token)
}

/// Path to the local admin token file for an OpenMemory home.
#[must_use]
pub fn admin_token_path(home: &Path) -> PathBuf {
    home.join(RUN_DIR).join(ADMIN_TOKEN_FILE)
}

/// Load the per-home admin token without creating it.
pub fn load_admin_token(home: &Path) -> Result<Option<String>, DaemonError> {
    read_admin_token(&admin_token_path(home))
}

/// Path to the daemon runtime discovery file for an OpenMemory home.
#[must_use]
pub fn runtime_info_path(home: &Path) -> PathBuf {
    home.join(RUN_DIR).join(DAEMON_RUNTIME_FILE)
}

/// Build runtime metadata for a daemon bound to `bound_addr`.
pub fn runtime_info(
    config: &DaemonConfig,
    bound_addr: SocketAddr,
) -> Result<DaemonRuntimeInfo, DaemonError> {
    validate_loopback(bound_addr)?;
    Ok(DaemonRuntimeInfo {
        api_version: ADMIN_API_VERSION.to_string(),
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        bind_addr: bound_addr.to_string(),
        admin_url: format!("http://{bound_addr}"),
        home: config.home.display().to_string(),
        active_profile: config.active_profile.clone(),
        started_at_unix_secs: unix_now_secs()?,
    })
}

/// Write daemon runtime metadata under `<home>/run/daemon.json`.
pub fn write_runtime_info(home: &Path, info: &DaemonRuntimeInfo) -> Result<(), DaemonError> {
    let path = runtime_info_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_vec_pretty(info)?;
    write_atomic(&path, &content)?;
    Ok(())
}

/// Read daemon runtime metadata if the discovery file exists.
pub fn read_runtime_info(home: &Path) -> Result<Option<DaemonRuntimeInfo>, DaemonError> {
    let path = runtime_info_path(home);
    match std::fs::read(&path) {
        Ok(content) => Ok(Some(serde_json::from_slice(&content)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(DaemonError::RuntimeIo(e)),
    }
}

/// Remove daemon runtime metadata. Missing files are treated as already removed.
pub fn remove_runtime_info(home: &Path) -> Result<(), DaemonError> {
    match std::fs::remove_file(runtime_info_path(home)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::RuntimeIo(e)),
    }
}

/// Build the axum router for the local admin API.
pub fn build_router(config: DaemonConfig) -> Router {
    build_router_with_shutdown(config, None)
}

fn build_router_with_shutdown(
    config: DaemonConfig,
    shutdown: Option<tokio::sync::watch::Sender<bool>>,
) -> Router {
    let logs = Arc::new(RedactedLogRing::new(DEFAULT_LOG_RING_CAPACITY));
    let jobs = Arc::new(JobRegistry::open(config.home()));
    let (token_generation, _) = tokio::sync::watch::channel(0);
    logs.push(
        AdminLogLevel::Info,
        "admin_router_ready",
        "admin API router initialized",
        serde_json::json!({
            "home": config.home().display().to_string(),
            "active_profile": config.active_profile(),
            "job_registry": jobs.health().details,
        }),
    );

    let state = AdminState {
        token: Arc::new(RwLock::new(config.admin_token.clone())),
        token_generation,
        config,
        logs,
        jobs,
        shutdown,
    };

    Router::new()
        .route("/admin/health", get(handle_health))
        .route("/admin/doctor", get(handle_doctor))
        .route("/admin/shutdown", post(handle_shutdown))
        .route("/admin/logs", get(handle_logs))
        .route("/admin/auth/rotate", post(handle_rotate_token))
        .route("/admin/profiles", get(handle_profiles))
        .route("/admin/entities", get(handle_entities))
        .route("/admin/entities/{id}", get(handle_entity_detail))
        .route("/admin/search", get(handle_search))
        .route("/admin/consolidate", post(handle_consolidate))
        .route("/admin/jobs/{id}", get(handle_job))
        .route("/admin/events", get(handle_events))
        .route("/admin/integrations", get(handle_integrations))
        .route(
            "/admin/integrations/{client}/preview",
            post(handle_integration_preview),
        )
        .route(
            "/admin/integrations/{client}/install",
            post(handle_integration_install),
        )
        .route(
            "/admin/integrations/{client}/verify",
            post(handle_integration_verify),
        )
        .route("/admin/backup/preflight", post(handle_backup_preflight))
        .route("/admin/backup/create", post(handle_backup_create))
        .route("/admin/backups/preflight", post(handle_backup_preflight))
        .route("/admin/backups/create", post(handle_backup_create))
        .route("/admin/restore/preflight", post(handle_restore_preflight))
        .route("/admin/restore", post(handle_restore))
        .with_state(state)
}

/// Bind the daemon listener after validating loopback policy.
pub async fn bind_listener(config: &DaemonConfig) -> Result<tokio::net::TcpListener, DaemonError> {
    validate_loopback(config.bind_addr)?;
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    Ok(listener)
}

/// Serve the admin API forever on an already-bound listener.
pub async fn serve_listener(
    config: DaemonConfig,
    listener: tokio::net::TcpListener,
) -> Result<(), DaemonError> {
    serve_listener_until_shutdown(config, listener, std::future::pending::<()>()).await
}

/// Serve the admin API until the authenticated shutdown endpoint or an
/// external shutdown future completes.
pub async fn serve_listener_until_shutdown<F>(
    config: DaemonConfig,
    listener: tokio::net::TcpListener,
    shutdown: F,
) -> Result<(), DaemonError>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let app = build_router_with_shutdown(config, Some(shutdown_tx));
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                () = shutdown => {}
                result = shutdown_rx.changed() => {
                    if result.is_ok() {
                        while !*shutdown_rx.borrow() {
                            if shutdown_rx.changed().await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        })
        .await?;
    Ok(())
}

/// Bind and serve the admin API forever.
pub async fn serve(config: DaemonConfig) -> Result<(), DaemonError> {
    let listener = bind_listener(&config).await?;
    serve_listener(config, listener).await
}

#[cfg(test)]
fn health_response(config: &DaemonConfig) -> HealthResponse {
    health_response_with_jobs(config, None)
}

fn health_response_with_jobs(config: &DaemonConfig, jobs: Option<&JobRegistry>) -> HealthResponse {
    let loaded_config = load_config(config.home());
    HealthResponse {
        api_version: ADMIN_API_VERSION.to_string(),
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        home: config.home.display().to_string(),
        active_profile: config.active_profile.clone(),
        daemon: ComponentHealth::ok("daemon is running"),
        store: store_health(config, loaded_config.as_ref()),
        model: model_health(config.home(), loaded_config.as_ref()),
        mcp: mcp_health(),
        watcher: watcher_health(),
        jobs: jobs.map_or_else(
            || ComponentHealth::ok("job registry is not attached in this context"),
            JobRegistry::health,
        ),
        integrations: IntegrationSummary::default(),
    }
}

fn doctor_response(config: &DaemonConfig, jobs: Option<&JobRegistry>) -> AdminDoctorResponse {
    let health = health_response_with_jobs(config, jobs);
    let integrations = integrations_response(config);
    let mut diagnostics = Vec::new();
    collect_health_diagnostic("store", &health.store, &mut diagnostics);
    collect_health_diagnostic("model", &health.model, &mut diagnostics);
    collect_health_diagnostic("mcp", &health.mcp, &mut diagnostics);
    collect_health_diagnostic("watcher", &health.watcher, &mut diagnostics);
    collect_health_diagnostic("jobs", &health.jobs, &mut diagnostics);
    for integration in &integrations.integrations {
        collect_health_diagnostic(
            &format!("integration:{}", integration.label),
            &integration.health,
            &mut diagnostics,
        );
    }

    AdminDoctorResponse {
        api_version: ADMIN_API_VERSION.to_string(),
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        home: config.home.display().to_string(),
        active_profile: config.active_profile.clone(),
        health,
        integrations,
        diagnostics,
    }
}

fn mcp_health() -> ComponentHealth {
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

fn watcher_health() -> ComponentHealth {
    ComponentHealth::ok("watcher is CLI-managed for this daemon version")
}

fn collect_health_diagnostic(
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

fn load_config(home: &Path) -> Result<Config, String> {
    Config::load_from(home.join("config.toml")).map_err(|e| e.to_string())
}

fn profile_data_dir(home: &Path, profile: &str) -> PathBuf {
    home.join("data").join(profile)
}

fn store_health(config: &DaemonConfig, loaded_config: Result<&Config, &String>) -> ComponentHealth {
    let data_dir = profile_data_dir(config.home(), config.active_profile());
    let details = serde_json::json!({
        "profile": config.active_profile(),
        "data_dir": data_dir.display().to_string(),
    });

    let loaded_config = match loaded_config {
        Ok(config) => config,
        Err(error) => {
            return ComponentHealth::error(
                AdminErrorCode::ConfigInvalid,
                "OpenMemory config could not be loaded",
            )
            .with_details(merge_details(
                details,
                serde_json::json!({ "error": error }),
            ));
        }
    };

    if !data_dir.exists() {
        return ComponentHealth::warning(
            AdminErrorCode::ProfileNotInitialized,
            "active profile is not initialized on disk",
        )
        .with_details(details);
    }

    let store = match DomainStore::open_existing(loaded_config, &data_dir) {
        Ok(store) => store,
        Err(error) => {
            let message = error.to_string();
            return ComponentHealth::error(
                store_error_code(&message),
                "memory store is unreadable",
            )
            .with_details(merge_details(
                details,
                serde_json::json!({ "error": message }),
            ));
        }
    };
    let domains = store.domains();

    match store.status() {
        Ok(status) => ComponentHealth::ok(format!(
            "profile {} is ready: {} entities, {} observations",
            config.active_profile(),
            status.total_entities,
            status.total_observations
        ))
        .with_details(merge_details(
            details,
            serde_json::json!({
                "domains": domains,
                "schema_version": status.schema_version,
                "entities": status.total_entities,
                "observations": status.total_observations,
                "relations": status.total_relations,
                "tombstoned_observations": status.tombstoned_observations,
                "vector_count": status.vector_count,
                "reader_pool_size": status.reader_pool_size,
            }),
        )),
        Err(error) => {
            let message = error.to_string();
            ComponentHealth::error(store_error_code(&message), "memory store status failed")
                .with_details(merge_details(
                    details,
                    serde_json::json!({ "domains": domains, "error": message }),
                ))
        }
    }
}

fn store_error_code(message: &str) -> AdminErrorCode {
    if message.contains("newer than supported") {
        AdminErrorCode::SchemaTooNew
    } else {
        AdminErrorCode::StoreUnreadable
    }
}

#[cfg(feature = "embeddings")]
fn model_health(home: &Path, loaded_config: Result<&Config, &String>) -> ComponentHealth {
    let loaded_config = match loaded_config {
        Ok(config) => config,
        Err(error) => {
            return ComponentHealth::error(
                AdminErrorCode::ConfigInvalid,
                "OpenMemory config could not be loaded",
            )
            .with_details(serde_json::json!({ "error": error }));
        }
    };

    let registry = ModelRegistry::default();
    let candidate = model_candidate(loaded_config);
    let (model, unresolved) = match candidate.as_ref() {
        Some((_, name)) => match registry.get(name) {
            Some(model) => (model, None),
            None => (registry.default_model(), Some(name.as_str())),
        },
        None => (registry.default_model(), None),
    };

    let models_dir = home.join("models");
    let manager = ModelManager::new(models_dir.clone());
    let downloaded_dir = manager.downloaded_model_dir(model);
    let details = serde_json::json!({
        "model": model.name,
        "dimensions": model.dimensions,
        "models_dir": models_dir.display().to_string(),
        "downloaded": downloaded_dir.is_some(),
        "path": downloaded_dir.as_ref().map(|p| p.display().to_string()),
        "requested_source": candidate.as_ref().map(|(source, _)| *source),
        "requested_model": candidate.as_ref().map(|(_, name)| name.as_str()),
    });

    if let Some(name) = unresolved {
        return ComponentHealth::warning(
            AdminErrorCode::ModelMissing,
            format!(
                "configured embedding model {name:?} is not registered; using {}",
                model.name
            ),
        )
        .with_details(details);
    }

    if downloaded_dir.is_some() {
        ComponentHealth::ok(format!("embedding model {} is cached", model.name))
            .with_details(details)
    } else {
        ComponentHealth::warning(
            AdminErrorCode::ModelMissing,
            format!(
                "embedding model {} is not downloaded; semantic search will run keyword-only",
                model.name
            ),
        )
        .with_details(details)
    }
}

#[cfg(feature = "embeddings")]
fn model_candidate(config: &Config) -> Option<(&'static str, String)> {
    if let Ok(model) = std::env::var("OPENMEMORY_MODEL") {
        if !model.trim().is_empty() {
            return Some(("OPENMEMORY_MODEL", model));
        }
    }
    config
        .default
        .model
        .as_ref()
        .map(|model| ("config.default.model", model.clone()))
}

#[cfg(not(feature = "embeddings"))]
fn model_health(_home: &Path, _loaded_config: Result<&Config, &String>) -> ComponentHealth {
    ComponentHealth::unknown("embedding model health is unavailable in this build")
}

fn profiles_response(config: &DaemonConfig) -> AdminProfilesResponse {
    let mut names = BTreeSet::new();
    names.insert(config.active_profile().to_string());
    let data_root = config.home().join("data");
    if let Ok(entries) = std::fs::read_dir(&data_root) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    names.insert(name.to_string());
                }
            }
        }
    }

    let loaded_config = load_config(config.home());
    let profiles = names
        .into_iter()
        .map(|name| {
            let profile_config = DaemonConfig {
                bind_addr: config.bind_addr,
                admin_token: config.admin_token.clone(),
                home: config.home.clone(),
                active_profile: name.clone(),
            };
            let data_dir = profile_data_dir(config.home(), &name);
            AdminProfileSummary {
                active: name == config.active_profile(),
                initialized: data_dir.exists(),
                health: store_health(&profile_config, loaded_config.as_ref()),
                name,
                data_dir: data_dir.display().to_string(),
            }
        })
        .collect();

    AdminProfilesResponse {
        active_profile: config.active_profile().to_string(),
        profiles,
    }
}

fn spawn_consolidate_job(
    config: DaemonConfig,
    request: AdminConsolidateRequest,
    job_id: String,
    jobs: Arc<JobRegistry>,
) {
    tokio::spawn(async move {
        let _ = jobs.update(&job_id, |job| {
            job.state = AdminJobState::Running;
            job.started_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
            job.message = Some("consolidation running".into());
        });

        let blocking_config = config.clone();
        let result = tokio::task::spawn_blocking(move || {
            run_consolidate_blocking(&blocking_config, request)
        })
        .await;

        match result {
            Ok(Ok(report)) => {
                let result = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Succeeded;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("consolidation completed".into());
                    job.result = result;
                    job.error = None;
                });
            }
            Ok(Err(error)) => {
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Failed;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("consolidation failed".into());
                    job.error = Some(error);
                });
            }
            Err(error) => {
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Failed;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("consolidation worker failed".into());
                    job.error = Some(
                        AdminError::new(
                            AdminErrorCode::Internal,
                            "consolidation worker failed",
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

fn run_consolidate_blocking(
    daemon_config: &DaemonConfig,
    request: AdminConsolidateRequest,
) -> Result<AdminConsolidateReport, AdminError> {
    let config = load_config(daemon_config.home()).map_err(|error| {
        AdminError::new(
            AdminErrorCode::ConfigInvalid,
            "OpenMemory config could not be loaded",
            Some("Fix config.toml and retry."),
            false,
        )
        .with_details(serde_json::json!({ "error": error }))
    })?;
    let data_dir = profile_data_dir(daemon_config.home(), daemon_config.active_profile());
    if !data_dir.exists() {
        return Err(AdminError::new(
            AdminErrorCode::ProfileNotInitialized,
            "active profile is not initialized on disk",
            Some("Run `openmemory init` for this profile."),
            false,
        )
        .with_details(serde_json::json!({
            "profile": daemon_config.active_profile(),
            "data_dir": data_dir.display().to_string(),
        })));
    }
    let store = DomainStore::open_existing(&config, &data_dir)
        .map_err(|error| store_admin_error("memory store is unreadable", error))?;
    let mut consolidate_config = ConsolidateConfig::from_config(&config);
    if let Some(threshold) = request.dedup_threshold {
        consolidate_config.dedup_text_threshold = threshold.clamp(0.0, 1.0);
    }
    if let Some(prune_floor) = request.prune_floor {
        consolidate_config.prune_floor = prune_floor;
    }
    if let Some(min_age_secs) = request.min_age_secs {
        consolidate_config.min_age_secs = min_age_secs.max(0);
    }
    let report = store
        .consolidate(&consolidate_config)
        .map_err(|error| store_admin_error("consolidation failed", error))?;
    Ok(AdminConsolidateReport {
        duplicates_merged: report.duplicates_merged,
        observations_pruned: report.observations_pruned,
        entities_pruned: report.entities_pruned,
    })
}

fn open_active_store(config: &DaemonConfig) -> Result<DomainStore, (StatusCode, AdminError)> {
    let data_dir = profile_data_dir(config.home(), config.active_profile());
    if !data_dir.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            AdminError::new(
                AdminErrorCode::ProfileNotInitialized,
                "active profile is not initialized on disk",
                Some("Run `openmemory init` for this profile."),
                false,
            )
            .with_details(serde_json::json!({
                "profile": config.active_profile(),
                "data_dir": data_dir.display().to_string(),
            })),
        ));
    }
    let loaded_config = load_config(config.home()).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminError::new(
                AdminErrorCode::ConfigInvalid,
                "OpenMemory config could not be loaded",
                Some("Fix config.toml and retry."),
                false,
            )
            .with_details(serde_json::json!({ "error": error })),
        )
    })?;
    DomainStore::open_existing(&loaded_config, &data_dir).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            store_admin_error("memory store is unreadable", error),
        )
    })
}

fn parse_entity_type(value: Option<&str>) -> Result<Option<EntityType>, AdminError> {
    match value {
        Some(value) if !value.trim().is_empty() => EntityType::parse(value).map(Some).ok_or_else(|| {
            AdminError::new(
                AdminErrorCode::InvalidRequest,
                format!("unknown entity type {value:?}"),
                Some("Use one of: person, project, concept, tool, preference, fact, event, location, organization."),
                false,
            )
        }),
        _ => Ok(None),
    }
}

fn parse_search_mode(value: Option<&str>) -> Result<Option<SearchMode>, AdminError> {
    match value {
        Some("hybrid") | None => Ok(None),
        Some("keyword" | "keyword_only") => Ok(Some(SearchMode::KeywordOnly)),
        Some("vector" | "vector_only") => Ok(Some(SearchMode::VectorOnly)),
        Some(value) => Err(AdminError::new(
            AdminErrorCode::InvalidRequest,
            format!("unknown search mode {value:?}"),
            Some("Use one of: hybrid, keyword, vector."),
            false,
        )),
    }
}

fn store_admin_error(message: &str, error: impl std::fmt::Display) -> AdminError {
    let error = error.to_string();
    AdminError::new(
        store_error_code(&error),
        message,
        Option::<String>::None,
        false,
    )
    .with_details(serde_json::json!({ "error": error }))
}

fn admin_entity_summary(row: EntityListRow) -> AdminEntitySummary {
    admin_entity(&row.entity, row.observation_count)
}

fn admin_entity(entity: &Entity, observation_count: u64) -> AdminEntitySummary {
    AdminEntitySummary {
        id: entity.id.clone(),
        name: entity.name.clone(),
        entity_type: entity.entity_type.as_str().to_string(),
        created_at: entity.created_at,
        updated_at: entity.updated_at,
        confidence: entity.confidence,
        source: entity.source.clone(),
        observation_count,
    }
}

fn admin_observation(observation: Observation) -> AdminObservation {
    AdminObservation {
        id: observation.id,
        entity_id: observation.entity_id,
        content: observation.content,
        observed_at: observation.observed_at,
        valid_from: observation.valid_from,
        valid_until: observation.valid_until,
        confidence: observation.confidence,
        source: observation.source,
        memory_tier: observation.memory_tier.as_str().to_string(),
        title: observation.title,
        summary: observation.summary,
        importance: observation.importance,
        source_kind: observation.source_kind,
        concepts: observation.concepts,
        source_files: observation.source_files,
    }
}

fn admin_relation(relation: Relation) -> AdminRelation {
    AdminRelation {
        id: relation.id,
        from_entity: relation.from_entity,
        to_entity: relation.to_entity,
        relation_type: relation.relation_type,
        weight: relation.weight,
        created_at: relation.created_at,
        valid_from: relation.valid_from,
        valid_until: relation.valid_until,
        source: relation.source,
    }
}

fn merge_details(mut left: serde_json::Value, right: serde_json::Value) -> serde_json::Value {
    if let (Some(left), Some(right)) = (left.as_object_mut(), right.as_object()) {
        for (key, value) in right {
            left.insert(key.clone(), value.clone());
        }
    }
    left
}

fn redact_log_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_secret_key(key) {
                    *value = serde_json::Value::String("<redacted>".into());
                } else {
                    redact_log_value(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_log_value(value);
            }
        }
        serde_json::Value::String(text) => {
            *text = redact_log_text(text);
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("credential")
        || key == "authorization"
}

fn redact_log_text(text: &str) -> String {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        if looks_like_secret_word(word) {
            out.push("<redacted>");
        } else {
            out.push(word);
        }
    }
    out.join(" ")
}

fn looks_like_secret_word(word: &str) -> bool {
    let trimmed = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    trimmed.len() >= 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit())
}

async fn handle_health(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => Json(health_response_with_jobs(&state.config, Some(&state.jobs))).into_response(),
        Err((status, error)) => json_auth_error(status, error),
    }
}

async fn handle_doctor(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => Json(doctor_response(&state.config, Some(&state.jobs))).into_response(),
        Err((status, error)) => json_auth_error(status, error),
    }
}

async fn handle_shutdown(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    let shutting_down_at_unix_secs = unix_now_secs().unwrap_or(0);
    state.logs.push(
        AdminLogLevel::Info,
        "admin_shutdown_requested",
        "authenticated daemon shutdown requested",
        serde_json::json!({ "shutting_down_at_unix_secs": shutting_down_at_unix_secs }),
    );

    if let Some(shutdown) = &state.shutdown {
        let _ = shutdown.send(true);
        Json(AdminShutdownResponse {
            accepted: true,
            shutting_down_at_unix_secs,
        })
        .into_response()
    } else {
        json_error(
            StatusCode::CONFLICT,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::Conflict,
                "daemon shutdown is not available for this router",
                Some("Use a router created by the daemon server runtime."),
                false,
            )),
        )
    }
}

async fn handle_logs(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => Json(AdminLogsResponse {
            entries: state.logs.snapshot(),
        })
        .into_response(),
        Err((status, error)) => json_auth_error(status, error),
    }
}

async fn handle_rotate_token(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    match rotate_admin_token(state.config.home()) {
        Ok(token) => {
            let rotated_at_unix_secs = unix_now_secs().unwrap_or(0);
            let Ok(new_token) = AdminToken::new(token) else {
                state.logs.push(
                    AdminLogLevel::Error,
                    "admin_token_rotation_failed",
                    "generated admin token was invalid",
                    serde_json::Value::Null,
                );
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminErrorResponse::new(AdminError::new(
                        AdminErrorCode::TokenRotationFailed,
                        "admin token rotation failed",
                        Some("Restart the daemon and try again."),
                        true,
                    )),
                );
            };
            *state.token.write().unwrap_or_else(|e| e.into_inner()) = new_token;
            let next_generation = (*state.token_generation.borrow()).saturating_add(1);
            let _ = state.token_generation.send(next_generation);
            state.logs.push(
                AdminLogLevel::Info,
                "admin_token_rotated",
                "admin token rotated",
                serde_json::json!({
                    "token_path": admin_token_path(state.config.home()).display().to_string(),
                    "rotated_at_unix_secs": rotated_at_unix_secs,
                }),
            );
            Json(AdminTokenRotationResponse {
                rotated_at_unix_secs,
            })
            .into_response()
        }
        Err(error) => {
            state.logs.push(
                AdminLogLevel::Error,
                "admin_token_rotation_failed",
                "admin token rotation failed",
                serde_json::json!({ "error": error.to_string() }),
            );
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminErrorResponse::new(
                    AdminError::new(
                        AdminErrorCode::TokenRotationFailed,
                        "admin token rotation failed",
                        Some("Check permissions on the OpenMemory run directory."),
                        true,
                    )
                    .with_details(serde_json::json!({ "error": error.to_string() })),
                ),
            )
        }
    }
}

async fn handle_profiles(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => Json(profiles_response(&state.config)).into_response(),
        Err((status, error)) => json_auth_error(status, error),
    }
}

async fn handle_entities(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<EntityListQuery>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    let request = PageRequest {
        limit: query.limit.unwrap_or(PageRequest::DEFAULT_LIMIT),
        offset: query.offset,
    }
    .normalized();
    let entity_type = match parse_entity_type(query.entity_type.as_deref()) {
        Ok(entity_type) => entity_type,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, AdminErrorResponse::new(error)),
    };
    let store = match open_active_store(&state.config) {
        Ok(store) => store,
        Err((status, error)) => return json_error(status, AdminErrorResponse::new(error)),
    };
    let offset = request.offset.unwrap_or(0);
    let fetch = request.limit.saturating_add(1);
    let rows = match store.list_entities(entity_type, fetch as usize, offset as usize) {
        Ok(rows) => rows,
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminErrorResponse::new(store_admin_error("listing entities failed", error)),
            );
        }
    };

    let mut items: Vec<AdminEntitySummary> = rows.into_iter().map(admin_entity_summary).collect();
    let next_offset = if items.len() > request.limit as usize {
        items.truncate(request.limit as usize);
        Some(offset.saturating_add(request.limit))
    } else {
        None
    };

    Json(Page::new(items, next_offset)).into_response()
}

async fn handle_entity_detail(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    let store = match open_active_store(&state.config) {
        Ok(store) => store,
        Err((status, error)) => return json_error(status, AdminErrorResponse::new(error)),
    };
    let entity = match store.get_entity_by_id(&id) {
        Ok(Some(entity)) => entity,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                AdminErrorResponse::new(AdminError::new(
                    AdminErrorCode::EntityNotFound,
                    "entity was not found",
                    Option::<String>::None,
                    false,
                )),
            );
        }
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminErrorResponse::new(store_admin_error("entity lookup failed", error)),
            );
        }
    };
    let observations: Vec<AdminObservation> = match store.get_entity_observations(&entity.id) {
        Ok(observations) => observations.into_iter().map(admin_observation).collect(),
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminErrorResponse::new(store_admin_error(
                    "reading entity observations failed",
                    error,
                )),
            );
        }
    };
    let relations: Vec<AdminRelation> = match store.get_entity_relations(&entity.id) {
        Ok(relations) => relations.into_iter().map(admin_relation).collect(),
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminErrorResponse::new(store_admin_error(
                    "reading entity relations failed",
                    error,
                )),
            );
        }
    };
    let observation_count = observations.len() as u64;
    Json(AdminEntityDetail {
        entity: admin_entity(&entity, observation_count),
        observations,
        relations,
    })
    .into_response()
}

async fn handle_search(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    if query.q.trim().is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::InvalidRequest,
                "search query must not be empty",
                Option::<String>::None,
                false,
            )),
        );
    }
    let limit = query
        .limit
        .unwrap_or(PageRequest::DEFAULT_LIMIT)
        .clamp(1, PageRequest::MAX_LIMIT);
    let entity_type = match parse_entity_type(query.entity_type.as_deref()) {
        Ok(entity_type) => entity_type,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, AdminErrorResponse::new(error)),
    };
    let mode = match parse_search_mode(query.mode.as_deref()) {
        Ok(mode) => mode,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, AdminErrorResponse::new(error)),
    };
    let store = match open_active_store(&state.config) {
        Ok(store) => store,
        Err((status, error)) => return json_error(status, AdminErrorResponse::new(error)),
    };
    let mut filters = RecallFilters::new();
    filters.entity_type = entity_type;
    filters.source = query.source;
    filters.mode = mode;
    let results = match store.recall(&query.q, limit as usize, &filters) {
        Ok(results) => results
            .into_iter()
            .map(|result| AdminSearchResult {
                entity_name: result.entity_name,
                entity_type: result.entity_type.as_str().to_string(),
                raw_score: result.raw_score,
                score: result.score,
                observation: admin_observation(result.observation),
            })
            .collect(),
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminErrorResponse::new(store_admin_error("search failed", error)),
            );
        }
    };

    Json(Page::new(results, None)).into_response()
}

async fn handle_consolidate(
    State(state): State<AdminState>,
    headers: HeaderMap,
    body: Option<Json<AdminConsolidateRequest>>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    if let Err((status, error)) = open_active_store(&state.config).map(|_| ()) {
        return json_error(status, AdminErrorResponse::new(error));
    }

    let request = body.map_or_else(AdminConsolidateRequest::default, |Json(body)| body);
    let now = unix_now_secs().unwrap_or(0);
    let job = AdminJob {
        id: new_id(),
        kind: AdminJobKind::Consolidate,
        state: AdminJobState::Queued,
        profile: state.config.active_profile().to_string(),
        created_at_unix_secs: now,
        started_at_unix_secs: None,
        finished_at_unix_secs: None,
        message: Some("consolidation queued".into()),
        result: serde_json::Value::Null,
        error: None,
    };
    let job = state.jobs.insert(job);
    spawn_consolidate_job(
        state.config.clone(),
        request,
        job.id.clone(),
        state.jobs.clone(),
    );
    Json(job).into_response()
}

async fn handle_job(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    match state.jobs.get(&id) {
        Some(job) => Json(job).into_response(),
        None => json_error(
            StatusCode::NOT_FOUND,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::JobNotFound,
                "job was not found",
                Option::<String>::None,
                false,
            )),
        ),
    }
}

async fn handle_events(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    let last_event_id = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let replay = state.jobs.events_after(last_event_id, 256);
    let replay_stream = tokio_stream::iter(replay.into_iter().map(event_to_sse));
    let live_stream = BroadcastStream::new(state.jobs.subscribe())
        .map(|result| {
            let event = match result {
                Ok(event) => event,
                Err(error) => AdminEvent {
                    sequence: 0,
                    unix_secs: unix_now_secs().unwrap_or(0),
                    event_type: AdminEventType::Warning,
                    job: None,
                    message: Some(format!("event stream lagged: {error}")),
                },
            };
            Some(event_to_sse(event))
        })
        .merge(
            WatchStream::from_changes(state.token_generation.subscribe())
                .map(|_| None::<Result<Event, Infallible>>),
        )
        .map_while(|event| event);
    let stream = replay_stream.chain(live_stream);

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn event_to_sse(event: AdminEvent) -> Result<Event, Infallible> {
    let event_name = match event.event_type {
        AdminEventType::JobUpdated => "job.updated",
        AdminEventType::Warning => "warning",
    };
    let data = serde_json::to_string(&event)
        .unwrap_or_else(|_| "{\"event_type\":\"warning\"}".to_string());
    let sequence = event.sequence;
    let event = Event::default().event(event_name).data(data);
    if sequence == 0 {
        Ok(event)
    } else {
        Ok(event.id(sequence.to_string()))
    }
}

async fn handle_integrations(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    Json(integrations_response(&state.config)).into_response()
}

async fn handle_integration_preview(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(client): AxumPath<String>,
    body: Option<Json<AdminIntegrationRequest>>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    let client = match parse_integration_client(&client) {
        Ok(client) => client,
        Err(error) => return json_error(StatusCode::NOT_FOUND, AdminErrorResponse::new(error)),
    };
    let request = body.map_or_else(AdminIntegrationRequest::default, |Json(body)| body);
    match integration_preview(&state.config, client, &request) {
        Ok(preview) => Json(preview).into_response(),
        Err(error) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminErrorResponse::new(error),
        ),
    }
}

async fn handle_integration_install(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(client): AxumPath<String>,
    body: Option<Json<AdminIntegrationRequest>>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    let client = match parse_integration_client(&client) {
        Ok(client) => client,
        Err(error) => return json_error(StatusCode::NOT_FOUND, AdminErrorResponse::new(error)),
    };
    let request = body.map_or_else(AdminIntegrationRequest::default, |Json(body)| body);
    match integration_install(&state.config, client, &request) {
        Ok(response) => Json(response).into_response(),
        Err(error) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminErrorResponse::new(error),
        ),
    }
}

async fn handle_integration_verify(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(client): AxumPath<String>,
    body: Option<Json<AdminIntegrationRequest>>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    let client = match parse_integration_client(&client) {
        Ok(client) => client,
        Err(error) => return json_error(StatusCode::NOT_FOUND, AdminErrorResponse::new(error)),
    };
    let request = body.map_or_else(AdminIntegrationRequest::default, |Json(body)| body);
    let now = unix_now_secs().unwrap_or(0);
    let job = AdminJob {
        id: new_id(),
        kind: AdminJobKind::IntegrationVerify,
        state: AdminJobState::Queued,
        profile: state.config.active_profile().to_string(),
        created_at_unix_secs: now,
        started_at_unix_secs: None,
        finished_at_unix_secs: None,
        message: Some("integration verification queued".into()),
        result: serde_json::Value::Null,
        error: None,
    };
    let job = state.jobs.insert(job);
    spawn_integration_verify_job(
        state.config.clone(),
        client,
        request,
        job.id.clone(),
        state.jobs.clone(),
    );
    Json(job).into_response()
}

async fn handle_backup_preflight(
    State(state): State<AdminState>,
    headers: HeaderMap,
    body: Option<Json<AdminBackupRequest>>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }
    let request = body.map_or_else(AdminBackupRequest::default, |Json(body)| body);
    Json(backup_preflight(&state.config, &request)).into_response()
}

async fn handle_backup_create(
    State(state): State<AdminState>,
    headers: HeaderMap,
    body: Option<Json<AdminBackupRequest>>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }
    let request = body.map_or_else(AdminBackupRequest::default, |Json(body)| body);
    let preflight = backup_preflight(&state.config, &request);
    if !preflight.ready {
        return json_error(
            StatusCode::BAD_REQUEST,
            AdminErrorResponse::new(
                AdminError::new(
                    AdminErrorCode::BackupPreflightFailed,
                    "backup preflight failed",
                    Some("Resolve diagnostics before creating a backup."),
                    false,
                )
                .with_details(serde_json::json!({ "preflight": preflight })),
            ),
        );
    }

    let job = AdminJob {
        id: new_id(),
        kind: AdminJobKind::BackupCreate,
        state: AdminJobState::Queued,
        profile: state.config.active_profile().to_string(),
        created_at_unix_secs: unix_now_secs().unwrap_or(0),
        started_at_unix_secs: None,
        finished_at_unix_secs: None,
        message: Some("backup queued".into()),
        result: serde_json::Value::Null,
        error: None,
    };
    let job = state.jobs.insert(job);
    spawn_backup_create_job(
        state.config.clone(),
        request,
        job.id.clone(),
        state.jobs.clone(),
    );
    Json(job).into_response()
}

async fn handle_restore_preflight(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminRestorePreflightRequest>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }
    Json(restore_preflight(&request)).into_response()
}

async fn handle_restore(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminRestoreRequest>,
) -> Response {
    match authorize_state(&headers, &state) {
        Ok(()) => {}
        Err((status, error)) => return json_auth_error(status, error),
    }

    let preflight = restore_preflight(&AdminRestorePreflightRequest {
        backup_dir: request.backup_dir.clone(),
    });
    if !preflight.ready {
        return json_error(
            StatusCode::BAD_REQUEST,
            AdminErrorResponse::new(
                AdminError::new(
                    AdminErrorCode::RestorePreflightFailed,
                    "restore preflight failed",
                    Some("Resolve diagnostics before restoring a backup."),
                    false,
                )
                .with_details(serde_json::json!({ "preflight": preflight })),
            ),
        );
    }

    let Some(manifest) = preflight.manifest.as_ref() else {
        return json_error(
            StatusCode::BAD_REQUEST,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::RestorePreflightFailed,
                "restore manifest was not available after preflight",
                Option::<String>::None,
                false,
            )),
        );
    };
    let target_profile = request
        .target_profile
        .clone()
        .unwrap_or_else(|| manifest.profile.clone());
    if let Err(error) = validate_restore_target_profile(&target_profile) {
        return json_error(StatusCode::BAD_REQUEST, AdminErrorResponse::new(error));
    }
    let target_dir = profile_data_dir(state.config.home(), &target_profile);
    if target_dir.exists() && !request.replace_existing {
        return json_error(
            StatusCode::CONFLICT,
            AdminErrorResponse::new(
                AdminError::new(
                    AdminErrorCode::Conflict,
                    "restore target profile already exists",
                    Some("Set replace_existing to true after taking a fresh backup."),
                    false,
                )
                .with_details(serde_json::json!({
                    "target_profile": target_profile,
                    "target_dir": target_dir.display().to_string(),
                })),
            ),
        );
    }

    let job = AdminJob {
        id: new_id(),
        kind: AdminJobKind::Restore,
        state: AdminJobState::Queued,
        profile: target_profile,
        created_at_unix_secs: unix_now_secs().unwrap_or(0),
        started_at_unix_secs: None,
        finished_at_unix_secs: None,
        message: Some("restore queued".into()),
        result: serde_json::Value::Null,
        error: None,
    };
    let job = state.jobs.insert(job);
    spawn_restore_job(
        state.config.clone(),
        request,
        job.id.clone(),
        state.jobs.clone(),
    );
    Json(job).into_response()
}

fn authorize_state(
    headers: &HeaderMap,
    state: &AdminState,
) -> Result<(), (StatusCode, AdminErrorResponse)> {
    let token = state
        .token
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let result = authorize(headers, &token);
    if let Err((_, error)) = &result {
        state.logs.push(
            AdminLogLevel::Warning,
            "admin_auth_rejected",
            "admin authorization rejected",
            serde_json::json!({ "code": error.error.code }),
        );
    }
    result
}

fn authorize(
    headers: &HeaderMap,
    expected: &AdminToken,
) -> Result<(), (StatusCode, AdminErrorResponse)> {
    let Some(raw) = headers.get(header::AUTHORIZATION) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            auth_error(
                AdminErrorCode::AuthRequired,
                "admin bearer token required",
                "Start the daemon through OpenMemory Desktop or the openmemory CLI.",
            ),
        ));
    };

    let Ok(header_str) = raw.to_str() else {
        return Err((
            StatusCode::UNAUTHORIZED,
            auth_error(
                AdminErrorCode::AuthInvalid,
                "admin bearer token is invalid",
                "Use the current token for this OpenMemory home.",
            ),
        ));
    };

    let Some(token) = header_str.strip_prefix("Bearer ") else {
        return Err((
            StatusCode::UNAUTHORIZED,
            auth_error(
                AdminErrorCode::AuthInvalid,
                "admin bearer token is invalid",
                "Use an Authorization header in the form: Bearer <token>.",
            ),
        ));
    };

    if !expected.matches(token.trim()) {
        return Err((
            StatusCode::UNAUTHORIZED,
            auth_error(
                AdminErrorCode::AuthInvalid,
                "admin bearer token is invalid",
                "Use the current token for this OpenMemory home.",
            ),
        ));
    }

    Ok(())
}

fn auth_error(code: AdminErrorCode, message: &str, hint: &str) -> AdminErrorResponse {
    AdminErrorResponse::new(AdminError::new(code, message, Some(hint), false))
}

fn json_error(status: StatusCode, error: AdminErrorResponse) -> Response {
    let mut response = (status, Json(error)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn json_auth_error(status: StatusCode, error: AdminErrorResponse) -> Response {
    let mut response = json_error(status, error);
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn validate_loopback(addr: SocketAddr) -> Result<(), DaemonError> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err(DaemonError::NonLoopbackBind(addr))
    }
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn write_new_token_file(path: &Path, token: &str) -> Result<(), DaemonError> {
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }?;

    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;

    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn write_token_file_atomic(path: &Path, token: &str) -> Result<(), DaemonError> {
    let tmp = token_tmp_path(path);
    {
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)
        }?;

        #[cfg(not(unix))]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;

        file.write_all(token.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(DaemonError::RuntimeIo(e))
        }
    }
}

fn token_tmp_path(path: &Path) -> PathBuf {
    let suffix = generate_token();
    let name = path.file_name().map_or_else(
        || std::borrow::Cow::Borrowed("admin-token"),
        |name| name.to_string_lossy(),
    );
    path.with_file_name(format!(".{name}.tmp.{}", &suffix[..12]))
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<(), DaemonError> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(content)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(DaemonError::RuntimeIo(e))
        }
    }
}

fn read_admin_token(path: &Path) -> Result<Option<String>, DaemonError> {
    if !admin_token_file_exists_securely(path)? {
        return Ok(None);
    }
    match std::fs::read_to_string(path) {
        Ok(existing) => {
            let token = existing.trim();
            if token.is_empty() {
                Err(DaemonError::EmptyAdminToken)
            } else {
                Ok(Some(token.to_string()))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(DaemonError::RuntimeIo(e)),
    }
}

#[cfg(unix)]
fn admin_token_file_exists_securely(path: &Path) -> Result<bool, DaemonError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(DaemonError::RuntimeIo(e)),
    };
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.file_type().is_symlink()
        || (metadata.file_type().is_file() && metadata.permissions().mode() & 0o077 != 0)
    {
        return Err(DaemonError::InsecureAdminTokenPermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(true)
}

#[cfg(not(unix))]
fn admin_token_file_exists_securely(path: &Path) -> Result<bool, DaemonError> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(DaemonError::RuntimeIo(e)),
    }
}

fn constant_time_eq(expected: &[u8], provided: &[u8]) -> bool {
    if expected.len() != provided.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in expected.iter().zip(provided) {
        diff |= x ^ y;
    }
    diff == 0
}

fn unix_now_secs() -> Result<u64, DaemonError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| DaemonError::ClockBeforeUnixEpoch)?
        .as_secs())
}

#[cfg(test)]
mod tests;
