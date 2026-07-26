//! Typed changeset, history, diff, and manual-edit contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminChangeSetState {
    Proposed,
    Applied,
    Rejected,
    Conflicted,
    Reverted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminChangeOperation {
    pub object_kind: String,
    pub logical_id: String,
    pub operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_revision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_row_version: Option<u64>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChangeSetSummary {
    pub id: String,
    pub space_id: String,
    pub state: AdminChangeSetState,
    pub actor_principal: String,
    pub actor_kind: String,
    pub reason: String,
    pub source: String,
    pub base_generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed_generation: Option<u64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminChangeSetDetail {
    #[serde(flatten)]
    pub summary: AdminChangeSetSummary,
    pub operations: Vec<AdminChangeOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSubmitChangeSetRequest {
    pub space_id: String,
    pub idempotency_key: String,
    pub reason: String,
    pub source: String,
    #[serde(default)]
    pub propose: bool,
    pub operations: Vec<AdminChangeOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminChangeDecisionRequest {
    pub expected_state: AdminChangeSetState,
    pub authority_generation: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminRevertChangeSetRequest {
    pub idempotency_key: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminFieldChange {
    pub field: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminMemoryDiff {
    pub space_id: String,
    pub logical_id: String,
    pub object_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_revision: Option<String>,
    pub current_revision: Option<String>,
    pub stale: bool,
    pub changes: Vec<AdminFieldChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminRevisionSummary {
    pub revision_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_revision_id: Option<String>,
    pub semantic_hash: String,
    pub created_by_change_set: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminObjectHistory {
    pub space_id: String,
    pub logical_id: String,
    pub object_kind: String,
    pub revisions: Vec<AdminRevisionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminEditMemoryRequest {
    pub expected_revision_id: Option<String>,
    pub expected_row_version: u64,
    pub expected_lifecycle: String,
    pub reason: String,
    pub fields: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminLifecycleRequest {
    pub expected_revision_id: Option<String>,
    pub expected_row_version: u64,
    pub expected_lifecycle: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminRevertRequest {
    pub expected_revision_id: Option<String>,
    pub expected_row_version: u64,
    pub revision_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminDestroyPreviewRequest {
    pub object_kind: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminDestroyRequest {
    pub object_kind: String,
    pub scope: String,
    pub confirmation: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminDestroyInventory {
    pub canonical_rows: u64,
    pub revisions: u64,
    pub contributions: u64,
    pub related_change_requests: u64,
    pub affected_ids: Vec<String>,
    pub affected_ids_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminDestroyPreview {
    pub space_id: String,
    pub logical_id: String,
    pub object_kind: String,
    pub scope: String,
    pub inventory: AdminDestroyInventory,
    pub confirmation: String,
    pub expires_at: i64,
    pub expected_revision_id: Option<String>,
    pub expected_row_version: u64,
    pub expected_lifecycle: String,
    pub expected_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminDestroyReceipt {
    pub id: String,
    pub space_id: String,
    pub object_kind: String,
    pub scope: String,
    pub semantic_generation: u64,
    pub indexed_generation: u64,
    pub index_ready: bool,
    pub destroyed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCherryPickRequest {
    pub target_space_id: String,
    pub object_kind: String,
    pub idempotency_key: String,
    pub reason: String,
}
