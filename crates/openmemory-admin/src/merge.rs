//! Typed identity review, merge preview, confirmation, and recovery contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminIdentityEvidence {
    pub evidence_id: String,
    pub kind: String,
    pub proof_class: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminIdentityCandidateSummary {
    pub id: String,
    pub merge_job_id: String,
    pub left_space_id: String,
    pub left_logical_id: String,
    pub right_space_id: String,
    pub right_logical_id: String,
    pub deterministic_state: String,
    pub proposal_state: String,
    pub packet_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminIdentityCandidateDetail {
    #[serde(flatten)]
    pub summary: AdminIdentityCandidateSummary,
    pub left_revision_id: String,
    pub right_revision_id: String,
    pub evidence: Vec<AdminIdentityEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_proposal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminIdentityDecisionRequest {
    pub decision: String,
    pub packet_hash: String,
    pub authority_generation: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminIdentityDecisionEvent {
    pub id: String,
    pub candidate_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes_event_id: Option<String>,
    pub decision: String,
    pub decision_source: String,
    pub principal_id: Option<String>,
    pub packet_hash: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminMergePreviewRequest {
    pub source_space_id: String,
    pub target_space_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub keep_undetermined_distinct: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminMergeConflict {
    pub object_kind: String,
    pub source_id: String,
    pub target_id: String,
    pub field: String,
    pub base_hash: Option<String>,
    pub source_hash: String,
    pub target_hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminMergeAccounting {
    pub target_entities_retained: usize,
    pub target_observations_retained: usize,
    pub target_relations_retained: usize,
    pub source_entities_accounted: usize,
    pub source_observations_accounted: usize,
    pub source_relations_accounted: usize,
    pub candidates_consumed: usize,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminMergePreview {
    pub job_id: String,
    pub source_space_id: String,
    pub target_space_id: String,
    pub source_snapshot_hash: String,
    pub target_snapshot_hash: String,
    pub plan_hash: String,
    pub predicted_result_hash: String,
    pub conflicts: Vec<AdminMergeConflict>,
    pub accounting: AdminMergeAccounting,
    pub dispositions: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminMergeResolutionRequest {
    pub expected_plan_hash: String,
    pub resolutions: Vec<serde_json::Value>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminMergeConfirmRequest {
    pub expected_plan_hash: String,
    pub expected_target_hash: String,
    pub confirmation: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminMergeJob {
    pub id: String,
    pub source_space_id: String,
    pub target_space_id: String,
    pub state: String,
    pub plan_hash: Option<String>,
    pub predicted_result_hash: Option<String>,
    pub accounting: Option<AdminMergeAccounting>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminRecoveryReport {
    pub job_id: Option<String>,
    pub target_space_id: String,
    pub outcome: String,
    pub live_hash: Option<String>,
    pub backup_root: Option<String>,
}
