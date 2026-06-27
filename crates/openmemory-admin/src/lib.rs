//! Typed contracts for the local OpenMemory admin API.
//!
//! This crate intentionally contains only serializable request/response
//! shapes, error codes, and small helpers. The daemon owns transport,
//! auth, storage handles, jobs, and event delivery.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Version label for the first daemon/admin API contract.
pub const ADMIN_API_VERSION: &str = "v1alpha1";

/// Stable machine-readable admin API error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdminErrorCode {
    AuthRequired,
    AuthInvalid,
    BindNonLoopback,
    ConfigInvalid,
    DaemonNotFound,
    DaemonUnreachable,
    ProfileNotInitialized,
    RuntimeMetadataInvalid,
    SchemaTooNew,
    StoreUnreadable,
    ModelMissing,
    TokenRotationFailed,
    InvalidRequest,
    EntityNotFound,
    ClientNotFound,
    ClientConfigUnreadable,
    ClientConfigStale,
    IntegrationVerifyFailed,
    BackupPreflightFailed,
    RestorePreflightFailed,
    JobNotFound,
    Conflict,
    Internal,
}

/// Error payload returned by every failing admin endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminError {
    pub code: AdminErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: serde_json::Value,
}

impl AdminError {
    /// Build an error without extra details.
    #[must_use]
    pub fn new(
        code: AdminErrorCode,
        message: impl Into<String>,
        hint: Option<impl Into<String>>,
        retryable: bool,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.map(Into::into),
            retryable,
            details: serde_json::Value::Null,
        }
    }

    /// Attach structured details for clients that need repair metadata.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }
}

/// Top-level envelope for failing admin responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminErrorResponse {
    pub error: AdminError,
}

impl AdminErrorResponse {
    #[must_use]
    pub fn new(error: AdminError) -> Self {
        Self { error }
    }
}

/// Coarse component state shown in health and dashboard views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
    Ok,
    Warning,
    Error,
    Unknown,
}

/// Health summary for one local component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub state: ComponentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<AdminErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: serde_json::Value,
}

impl ComponentHealth {
    #[must_use]
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            state: ComponentState::Ok,
            code: None,
            message: Some(message.into()),
            details: serde_json::Value::Null,
        }
    }

    #[must_use]
    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            state: ComponentState::Unknown,
            code: None,
            message: Some(message.into()),
            details: serde_json::Value::Null,
        }
    }

    #[must_use]
    pub fn warning(code: AdminErrorCode, message: impl Into<String>) -> Self {
        Self {
            state: ComponentState::Warning,
            code: Some(code),
            message: Some(message.into()),
            details: serde_json::Value::Null,
        }
    }

    #[must_use]
    pub fn error(code: AdminErrorCode, message: impl Into<String>) -> Self {
        Self {
            state: ComponentState::Error,
            code: Some(code),
            message: Some(message.into()),
            details: serde_json::Value::Null,
        }
    }

    /// Attach structured component metadata for dashboard clients.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }
}

/// Aggregated integration status for the dashboard.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationSummary {
    pub detected: u32,
    pub configured: u32,
    pub broken: u32,
}

/// Response from `GET /admin/health`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub api_version: String,
    pub daemon_version: String,
    pub home: String,
    pub active_profile: String,
    pub daemon: ComponentHealth,
    pub store: ComponentHealth,
    pub model: ComponentHealth,
    pub mcp: ComponentHealth,
    pub watcher: ComponentHealth,
    pub jobs: ComponentHealth,
    pub integrations: IntegrationSummary,
}

/// Severity for daemon-local diagnostic log entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminLogLevel {
    Info,
    Warning,
    Error,
}

/// One redacted daemon diagnostic event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminLogEntry {
    pub sequence: u64,
    pub unix_secs: u64,
    pub level: AdminLogLevel,
    pub event: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: serde_json::Value,
}

/// Bounded in-memory daemon diagnostic log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminLogsResponse {
    pub entries: Vec<AdminLogEntry>,
}

/// Response from `POST /admin/auth/rotate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminTokenRotationResponse {
    pub rotated_at_unix_secs: u64,
}

/// One known local memory profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminProfileSummary {
    pub name: String,
    pub data_dir: String,
    pub active: bool,
    pub initialized: bool,
    pub health: ComponentHealth,
}

/// Response from `GET /admin/profiles`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminProfilesResponse {
    pub active_profile: String,
    pub profiles: Vec<AdminProfileSummary>,
}

/// Entity row used by list and detail endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminEntitySummary {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub confidence: f32,
    pub source: String,
    pub observation_count: u64,
}

/// Observation payload used by detail and search endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminObservation {
    pub id: String,
    pub entity_id: String,
    pub content: String,
    pub observed_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<i64>,
    pub confidence: f32,
    pub source: String,
    pub memory_tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importance: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub concepts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_files: Vec<String>,
}

/// Relation payload used by entity detail endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminRelation {
    pub id: String,
    pub from_entity: String,
    pub to_entity: String,
    pub relation_type: String,
    pub weight: f32,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<i64>,
    pub source: String,
}

/// Response from `GET /admin/entities/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminEntityDetail {
    pub entity: AdminEntitySummary,
    pub observations: Vec<AdminObservation>,
    pub relations: Vec<AdminRelation>,
}

/// One recall/search result row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminSearchResult {
    pub entity_name: String,
    pub entity_type: String,
    pub raw_score: f32,
    pub score: f32,
    pub observation: AdminObservation,
}

/// Request body for `POST /admin/consolidate`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdminConsolidateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedup_threshold: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prune_floor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_age_secs: Option<i64>,
}

/// Consolidation counts returned in a completed job result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminConsolidateReport {
    pub duplicates_merged: usize,
    pub observations_pruned: usize,
    pub entities_pruned: usize,
}

/// Long-running admin job kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminJobKind {
    Consolidate,
    IntegrationVerify,
    BackupCreate,
    Restore,
}

/// Long-running admin job state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminJobState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

/// Durable-enough in-process job summary for desktop/admin clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminJob {
    pub id: String,
    pub kind: AdminJobKind,
    pub state: AdminJobState,
    pub profile: String,
    pub created_at_unix_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_unix_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_unix_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub result: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AdminError>,
}

/// Event type emitted by `GET /admin/events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminEventType {
    JobUpdated,
    Warning,
}

/// One server-sent admin event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminEvent {
    pub sequence: u64,
    pub unix_secs: u64,
    pub event_type: AdminEventType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<AdminJob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Supported local MCP client integration target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdminIntegrationClient {
    Codex,
    ClaudeCode,
}

/// Outcome of comparing desired integration config with disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminIntegrationOutcome {
    Created,
    Added,
    Updated,
    Unchanged,
}

/// One client integration status row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminIntegrationStatus {
    pub client: AdminIntegrationClient,
    pub label: String,
    pub detected: bool,
    pub configured: bool,
    pub config_path: String,
    pub entry_name: String,
    pub needs_restart: bool,
    pub health: ComponentHealth,
}

/// Response from `GET /admin/integrations`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminIntegrationsResponse {
    pub integrations: Vec<AdminIntegrationStatus>,
    pub summary: IntegrationSummary,
}

/// Request body for integration preview/install/verify endpoints.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminIntegrationRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_addr: Option<String>,
}

/// Preview of one integration config mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminIntegrationPreview {
    pub client: AdminIntegrationClient,
    pub label: String,
    pub outcome: AdminIntegrationOutcome,
    pub config_path: String,
    pub entry_name: String,
    pub needs_restart: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    pub after: String,
}

/// Response from `POST /admin/integrations/{client}/install`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminIntegrationInstallResponse {
    pub preview: AdminIntegrationPreview,
    pub changed: bool,
}

/// Result payload for an integration verification job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminIntegrationVerifyReport {
    pub client: AdminIntegrationClient,
    pub config_path: String,
    pub configured: bool,
}

/// One typed diagnostic item for doctor-style troubleshooting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminDiagnostic {
    pub component: String,
    pub code: AdminErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: serde_json::Value,
}

/// Detailed local diagnostics response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminDoctorResponse {
    pub api_version: String,
    pub daemon_version: String,
    pub home: String,
    pub active_profile: String,
    pub health: HealthResponse,
    pub integrations: AdminIntegrationsResponse,
    pub diagnostics: Vec<AdminDiagnostic>,
}

/// Request body for backup preflight/create endpoints.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminBackupRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_dir: Option<String>,
}

/// Manifest written into every daemon-created backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminBackupManifest {
    pub api_version: String,
    pub profile: String,
    pub created_at_unix_secs: u64,
    pub source_dir: String,
    pub files_copied: u64,
    pub bytes_copied: u64,
}

/// Response from `POST /admin/backups/preflight`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminBackupPreflightResponse {
    pub profile: String,
    pub source_dir: String,
    pub destination_dir: String,
    pub estimated_files: u64,
    pub estimated_bytes: u64,
    pub ready: bool,
    pub diagnostics: Vec<AdminDiagnostic>,
}

/// Result payload for a completed backup create job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminBackupCreateReport {
    pub backup_dir: String,
    pub manifest_path: String,
    pub manifest: AdminBackupManifest,
}

/// Request body for restore preflight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminRestorePreflightRequest {
    pub backup_dir: String,
}

/// Response from `POST /admin/restore/preflight`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminRestorePreflightResponse {
    pub backup_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<AdminBackupManifest>,
    pub ready: bool,
    pub diagnostics: Vec<AdminDiagnostic>,
}

/// Request body for `POST /admin/restore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminRestoreRequest {
    pub backup_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_profile: Option<String>,
    #[serde(default)]
    pub replace_existing: bool,
}

/// Result payload for a completed restore job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminRestoreReport {
    pub backup_dir: String,
    pub restored_profile: String,
    pub restored_dir: String,
    pub replaced_existing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_dir: Option<String>,
}

/// Response from `POST /admin/shutdown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminShutdownResponse {
    pub accepted: bool,
    pub shutting_down_at_unix_secs: u64,
}

/// Runtime discovery metadata written by a running local daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRuntimeInfo {
    pub api_version: String,
    pub daemon_version: String,
    pub pid: u32,
    pub bind_addr: String,
    pub admin_url: String,
    pub home: String,
    pub active_profile: String,
    pub started_at_unix_secs: u64,
}

/// Reachability state returned by `openmemory daemon status --json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonStatusState {
    NotStarted,
    Running,
    Unreachable,
}

/// Local daemon status assembled from runtime discovery and `/admin/health`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatusResponse {
    pub state: DaemonStatusState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<DaemonRuntimeInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AdminError>,
}

impl DaemonStatusResponse {
    #[must_use]
    pub fn not_started(error: AdminError) -> Self {
        Self {
            state: DaemonStatusState::NotStarted,
            runtime: None,
            health: None,
            error: Some(error),
        }
    }

    #[must_use]
    pub fn running(runtime: DaemonRuntimeInfo, health: HealthResponse) -> Self {
        Self {
            state: DaemonStatusState::Running,
            runtime: Some(runtime),
            health: Some(health),
            error: None,
        }
    }

    #[must_use]
    pub fn unreachable(runtime: Option<DaemonRuntimeInfo>, error: AdminError) -> Self {
        Self {
            state: DaemonStatusState::Unreachable,
            runtime,
            health: None,
            error: Some(error),
        }
    }
}

/// Result returned by `openmemory daemon stop --json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStopResponse {
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<DaemonRuntimeInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AdminError>,
}

impl DaemonStopResponse {
    #[must_use]
    pub fn stopped(runtime: DaemonRuntimeInfo) -> Self {
        Self {
            stopped: true,
            runtime: Some(runtime),
            error: None,
        }
    }

    #[must_use]
    pub fn not_stopped(runtime: Option<DaemonRuntimeInfo>, error: AdminError) -> Self {
        Self {
            stopped: false,
            runtime,
            error: Some(error),
        }
    }
}

/// Cursor pagination request fields shared by collection endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    pub limit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

impl PageRequest {
    pub const DEFAULT_LIMIT: u32 = 50;
    pub const MAX_LIMIT: u32 = 250;

    #[must_use]
    pub fn normalized(self) -> Self {
        let limit = self.limit.clamp(1, Self::MAX_LIMIT);
        Self {
            limit,
            offset: self.offset,
        }
    }
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            limit: Self::DEFAULT_LIMIT,
            offset: None,
        }
    }
}

/// Paginated response shared by collection endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
}

impl<T> Page<T> {
    #[must_use]
    pub fn new(items: Vec<T>, next_offset: Option<u32>) -> Self {
        Self { items, next_offset }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_serialize_as_snake_case() {
        let body = AdminErrorResponse::new(AdminError::new(
            AdminErrorCode::AuthRequired,
            "admin token required",
            Some("Start the daemon through OpenMemory Desktop or pass a token."),
            false,
        ));

        let value = serde_json::to_value(body).unwrap();
        assert_eq!(value["error"]["code"], "auth_required");
        assert_eq!(value["error"]["retryable"], false);
    }

    #[test]
    fn page_request_clamps_limit() {
        let request = PageRequest {
            limit: 1_000,
            offset: Some(10),
        }
        .normalized();

        assert_eq!(request.limit, PageRequest::MAX_LIMIT);
        assert_eq!(request.offset, Some(10));
    }

    #[test]
    fn component_health_can_carry_structured_details() {
        let health = ComponentHealth::ok("ready")
            .with_details(serde_json::json!({ "entities": 2, "domains": 1 }));

        let value = serde_json::to_value(health).unwrap();
        assert_eq!(value["state"], "ok");
        assert_eq!(value["details"]["entities"], 2);
        assert_eq!(value["details"]["domains"], 1);
    }

    #[test]
    fn daemon_status_serializes_state_as_snake_case() {
        let status = DaemonStatusResponse::not_started(AdminError::new(
            AdminErrorCode::DaemonNotFound,
            "daemon runtime metadata was not found",
            Option::<String>::None,
            true,
        ));

        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["state"], "not_started");
        assert_eq!(value["error"]["code"], "daemon_not_found");
    }
}
