//! Typed space, project, context, membership, and capability contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum AdminSpaceOwner {
    User(String),
    Team(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "project_id")]
pub enum AdminSpaceContext {
    Global,
    Project(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminSpaceRole {
    Reader,
    Contributor,
    Reviewer,
    Maintainer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminSpaceState {
    Creating,
    Active,
    Closed,
    Deleting,
    Error,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpaceReadiness {
    pub manifest_ready: bool,
    pub history_ready: bool,
    pub index_ready: bool,
    pub mirrors_ready: bool,
    pub recovery_required: bool,
    pub semantic_generation: u64,
    pub indexed_generation: u64,
    pub mirror_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpaceSummary {
    pub id: String,
    pub profile: String,
    pub display_name: String,
    pub owner: AdminSpaceOwner,
    pub context: AdminSpaceContext,
    pub state: AdminSpaceState,
    pub role: AdminSpaceRole,
    pub domain_count: usize,
    pub readiness: AdminSpaceReadiness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpaceDetail {
    #[serde(flatten)]
    pub summary: AdminSpaceSummary,
    pub root_key: String,
    pub catalog_generation: u64,
    pub manifest_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCreateSpaceRequest {
    pub owner: AdminSpaceOwner,
    pub context: AdminSpaceContext,
    pub display_name: String,
    #[serde(default = "one_domain")]
    pub domain_count: usize,
}

const fn one_domain() -> usize {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminUpdateSpaceRequest {
    pub display_name: String,
    pub expected_catalog_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminDeleteSpaceRequest {
    pub expected_catalog_generation: u64,
    pub confirmation: String,
    #[serde(default)]
    pub no_backup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminProjectSummary {
    pub id: String,
    pub profile: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCreateProjectRequest {
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminResolveWorkspaceRequest {
    pub workspace: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminMapWorkspaceRequest {
    pub workspace: String,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcs_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminTeamSummary {
    pub id: String,
    pub profile: String,
    pub display_name: String,
    pub authority_generation: u64,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCreateTeamRequest {
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminWorkspaceMapping {
    pub workspace_id: String,
    pub canonical_path: String,
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminResolveContextRequest {
    pub principal_id: String,
    pub actor_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_team_id: Option<String>,
    #[serde(default)]
    pub read_mode: String,
    #[serde(default)]
    pub write_target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminContextGrant {
    pub space_id: String,
    pub display_name: String,
    pub role: AdminSpaceRole,
    pub authority_generation: u64,
    pub read_priority: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminMemoryContext {
    pub profile: String,
    pub principal_id: String,
    pub actor_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_team_id: Option<String>,
    pub read_set: Vec<AdminContextGrant>,
    pub write_target: String,
    pub authorization_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminContextCapabilityResponse {
    pub context: AdminMemoryContext,
    pub capability: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminMembership {
    pub space_id: String,
    pub principal_id: String,
    pub role: AdminSpaceRole,
    pub authority_generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminUpdateMembershipRequest {
    pub role: AdminSpaceRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    pub expected_authority_generation: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminCapabilitiesResponse {
    pub spaces: bool,
    pub audit_observations: bool,
    pub manual_edit: bool,
    pub team_spaces: bool,
    pub identity_review: bool,
    pub material_merge: bool,
    #[serde(default)]
    pub reasons: std::collections::BTreeMap<String, String>,
}
