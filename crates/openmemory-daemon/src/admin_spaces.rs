//! Thin authenticated HTTP adapters for space and context services.

use std::path::PathBuf;

use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use openmemory_admin::{
    AdminCapabilitiesResponse, AdminContextCapabilityResponse, AdminContextGrant,
    AdminCreateProjectRequest, AdminCreateSpaceRequest, AdminCreateTeamRequest,
    AdminDeleteSpaceRequest, AdminError, AdminErrorCode, AdminErrorResponse,
    AdminMapWorkspaceRequest, AdminMembership, AdminMemoryContext, AdminProjectSummary,
    AdminResolveContextRequest, AdminResolveWorkspaceRequest, AdminSpaceContext, AdminSpaceDetail,
    AdminSpaceOwner, AdminSpaceReadiness, AdminSpaceRole, AdminSpaceState, AdminSpaceSummary,
    AdminTeamSummary, AdminUpdateMembershipRequest, AdminUpdateSpaceRequest, AdminWorkspaceMapping,
    Page,
};
use openmemory_core::space::{
    ActorKind, MemoryContext, PrincipalId, ProjectId, SpaceContext, SpaceId, SpaceOwner, SpaceRole,
    TeamId,
};

use crate::spaces::{
    CreateSpace, LocalSpaceService, ReadMode, ResolveContextRequest, SpaceServiceError,
    SpaceSummary, WriteSelection,
};
use crate::state::AdminState;
use crate::{authorize_state, json_auth_error, json_error, unix_now_secs};

const INSTALLATION_PRINCIPAL: &str = "local:installation";

pub(crate) fn router() -> Router<AdminState> {
    Router::new()
        .route("/admin/capabilities", get(capabilities))
        .route("/admin/spaces", get(list_spaces).post(create_space))
        .route("/admin/spaces/{id}", get(get_space).patch(update_space))
        .route("/admin/spaces/{id}/close", post(close_space))
        .route("/admin/spaces/{id}/delete", post(delete_space))
        .route("/admin/projects", get(list_projects).post(create_project))
        .route("/admin/workspaces/resolve", post(resolve_workspace))
        .route(
            "/admin/workspaces/{workspace_id}/project",
            put(map_workspace),
        )
        .route("/admin/teams", get(list_teams).post(create_team))
        .route("/admin/teams/{id}/members", get(list_team_members))
        .route(
            "/admin/teams/{id}/members/{principal_id}",
            put(update_team_member).delete(delete_team_member),
        )
        .route("/admin/context", get(current_context))
        .route("/admin/context/resolve", post(resolve_context))
        .route("/admin/context/revoke", post(revoke_context))
}

async fn list_projects(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    match service.list_projects(state.config.active_profile()) {
        Ok(projects) => Json(Page::new(
            projects
                .into_iter()
                .map(|project| AdminProjectSummary {
                    id: project.id.to_string(),
                    profile: project.profile,
                    display_name: project.display_name,
                })
                .collect(),
            None,
        ))
        .into_response(),
        Err(error) => space_error(error),
    }
}

async fn create_project(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminCreateProjectRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let id = ProjectId::new();
    match service.create_project(
        id,
        state.config.active_profile(),
        &request.display_name,
        now(),
    ) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(AdminProjectSummary {
                id: id.to_string(),
                profile: state.config.active_profile().to_string(),
                display_name: request.display_name,
            }),
        )
            .into_response(),
        Err(error) => space_error(error),
    }
}

async fn resolve_workspace(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminResolveWorkspaceRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    match service.workspace_mapping(
        state.config.active_profile(),
        &PathBuf::from(request.workspace),
    ) {
        Ok(Some(mapping)) => Json(AdminWorkspaceMapping {
            workspace_id: mapping.workspace_id,
            canonical_path: mapping.canonical_path,
            project_id: mapping.project_id.to_string(),
        })
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => space_error(error),
    }
}

async fn map_workspace(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(workspace_id): AxumPath<String>,
    Json(request): Json<AdminMapWorkspaceRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let project = match parse_id::<ProjectId>("project ID", &request.project_id) {
        Ok(project) => project,
        Err(response) => return response,
    };
    let workspace = PathBuf::from(&request.workspace);
    if let Err(error) = service.map_workspace(
        state.config.active_profile(),
        &workspace,
        project,
        request.vcs_fingerprint.as_deref(),
        now(),
    ) {
        return space_error(error);
    }
    match service.workspace_mapping(state.config.active_profile(), &workspace) {
        Ok(Some(mapping)) if workspace_id == mapping.workspace_id => Json(AdminWorkspaceMapping {
            workspace_id: mapping.workspace_id,
            canonical_path: mapping.canonical_path,
            project_id: mapping.project_id.to_string(),
        })
        .into_response(),
        Ok(Some(_)) => invalid("workspace ID does not match canonical path"),
        Ok(None) => space_error(SpaceServiceError::NotFound),
        Err(error) => space_error(error),
    }
}

async fn list_teams(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    match service.list_teams(state.config.active_profile()) {
        Ok(teams) => Json(Page::new(
            teams
                .into_iter()
                .map(|team| AdminTeamSummary {
                    id: team.id.to_string(),
                    profile: team.profile,
                    display_name: team.display_name,
                    authority_generation: team.authority_generation,
                    state: team.state,
                })
                .collect(),
            None,
        ))
        .into_response(),
        Err(error) => space_error(error),
    }
}

async fn create_team(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminCreateTeamRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let id: TeamId = format!("team:{}", SpaceId::new())
        .parse()
        .expect("UUID-backed team ID is path-safe");
    let timestamp = now();
    if let Err(error) = service.create_team(
        &id,
        state.config.active_profile(),
        &request.display_name,
        timestamp,
    ) {
        return space_error(error);
    }
    let global = match service.create(
        &CreateSpace {
            profile: state.config.active_profile().to_string(),
            owner: SpaceOwner::Team(id.clone()),
            context: SpaceContext::Global,
            display_name: format!("{} — Global", request.display_name),
            domain_count: 1,
        },
        timestamp,
    ) {
        Ok(space) => space,
        Err(error) => return space_error(error),
    };
    let principal: PrincipalId = INSTALLATION_PRINCIPAL
        .parse()
        .expect("static installation principal is valid");
    if let Err(error) = service.grant_team_membership(
        &id,
        &principal,
        SpaceRole::Maintainer,
        None,
        1,
        timestamp,
    ) {
        return space_error(error);
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "team": AdminTeamSummary {
                id: id.to_string(),
                profile: state.config.active_profile().to_string(),
                display_name: request.display_name,
                authority_generation: 2,
                state: "active".to_string(),
            },
            "global_space_id": global.id.to_string(),
        })),
    )
        .into_response()
}

async fn list_team_members(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let team = match parse_id::<TeamId>("team ID", &id) {
        Ok(team) => team,
        Err(response) => return response,
    };
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    match service.list_team_memberships(&team) {
        Ok(memberships) => Json(Page::new(
            memberships
                .into_iter()
                .map(|membership| AdminMembership {
                    space_id: membership.space_id.to_string(),
                    principal_id: membership.principal_id.to_string(),
                    role: admin_role(membership.role),
                    authority_generation: membership.authority_generation,
                    expires_at: membership.expires_at,
                })
                .collect(),
            None,
        ))
        .into_response(),
        Err(error) => space_error(error),
    }
}

async fn update_team_member(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath((id, principal_id)): AxumPath<(String, String)>,
    Json(request): Json<AdminUpdateMembershipRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let team = match parse_id::<TeamId>("team ID", &id) {
        Ok(team) => team,
        Err(response) => return response,
    };
    let principal = match parse_id::<PrincipalId>("principal ID", &principal_id) {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    if let Err(error) = service.ensure_principal(&principal, &principal_id, now()) {
        return space_error(error);
    }
    match service.grant_team_membership(
        &team,
        &principal,
        core_role(request.role),
        request.expires_at,
        request.expected_authority_generation,
        now(),
    ) {
        Ok(generation) => Json(serde_json::json!({
            "principal_id": principal.to_string(),
            "authority_generation": generation,
        }))
        .into_response(),
        Err(error) => space_error(error),
    }
}

async fn delete_team_member(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath((id, principal_id)): AxumPath<(String, String)>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let team = match parse_id::<TeamId>("team ID", &id) {
        Ok(team) => team,
        Err(response) => return response,
    };
    let principal = match parse_id::<PrincipalId>("principal ID", &principal_id) {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    match service.revoke_team_membership(&team, &principal, now()) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => space_error(error),
    }
}

async fn capabilities(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let spaces = state.spaces.is_some() && state.space_registry.is_some();
    let readiness = active_readiness(&state).unwrap_or_default();
    let mut reasons = std::collections::BTreeMap::new();
    if !spaces {
        reasons.insert(
            "spaces".to_string(),
            "space catalog/runtime is unavailable".to_string(),
        );
    }
    if !readiness.history_ready {
        reasons.insert(
            "audit_observations".to_string(),
            "history backfill is not ready in every domain".to_string(),
        );
    }
    if !readiness.mirrors_ready {
        reasons.insert(
            "identity_review".to_string(),
            "entity/relation history or mirror backfill is pending".to_string(),
        );
        reasons.insert(
            "material_merge".to_string(),
            "canonical relation mirrors are not ready".to_string(),
        );
    }
    Json(AdminCapabilitiesResponse {
        spaces,
        audit_observations: readiness.history_ready && readiness.index_ready,
        manual_edit: readiness.history_ready && readiness.index_ready,
        team_spaces: spaces,
        identity_review: readiness.history_ready && readiness.mirrors_ready,
        material_merge: readiness.history_ready
            && readiness.index_ready
            && readiness.mirrors_ready
            && !readiness.recovery_required,
        reasons,
    })
    .into_response()
}

async fn list_spaces(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    match service.list(state.config.active_profile()) {
        Ok(rows) => {
            let items = rows
                .into_iter()
                .map(|row| admin_summary(service, row))
                .collect();
            Json(Page::new(items, None)).into_response()
        }
        Err(error) => space_error(error),
    }
}

async fn create_space(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminCreateSpaceRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let owner = match core_owner(request.owner) {
        Ok(owner) => owner,
        Err(error) => return invalid(error),
    };
    let context = match core_context(request.context) {
        Ok(context) => context,
        Err(error) => return invalid(error),
    };
    let now = now();
    match service.create(
        &CreateSpace {
            profile: state.config.active_profile().to_string(),
            owner,
            context,
            display_name: request.display_name,
            domain_count: request.domain_count,
        },
        now,
    ) {
        Ok(row) => (StatusCode::CREATED, Json(admin_summary(service, row))).into_response(),
        Err(error) => space_error(error),
    }
}

async fn get_space(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let id = match parse_id::<SpaceId>("space ID", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match service.get(id) {
        Ok(row) => Json(admin_detail(service, row)).into_response(),
        Err(error) => space_error(error),
    }
}

async fn update_space(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<AdminUpdateSpaceRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let id = match parse_id::<SpaceId>("space ID", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match service.rename(
        id,
        &request.display_name,
        request.expected_catalog_generation,
        now(),
    ) {
        Ok(row) => Json(admin_summary(service, row)).into_response(),
        Err(error) => space_error(error),
    }
}

async fn close_space(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let id = match parse_id::<SpaceId>("space ID", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match service.close(id, now()) {
        Ok(row) => Json(admin_summary(service, row)).into_response(),
        Err(error) => space_error(error),
    }
}

async fn delete_space(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<AdminDeleteSpaceRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let id = match parse_id::<SpaceId>("space ID", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let current = match service.get(id) {
        Ok(row) => row,
        Err(error) => return space_error(error),
    };
    if current.catalog_generation != request.expected_catalog_generation
        || request.confirmation != id.to_string()
    {
        return json_error(
            StatusCode::CONFLICT,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::Stale,
                "space deletion confirmation or catalog generation is stale",
                Some("Refresh the space and confirm using its exact opaque ID."),
                false,
            )),
        );
    }
    match service.begin_delete(id, request.no_backup, now()) {
        Ok(row) => Json(admin_summary(service, row)).into_response(),
        Err(error) => space_error(error),
    }
}

async fn current_context(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let request = AdminResolveContextRequest {
        principal_id: INSTALLATION_PRINCIPAL.to_string(),
        actor_kind: "human".to_string(),
        workspace: None,
        project_id: None,
        active_team_id: None,
        read_mode: "global_only".to_string(),
        write_target: "personal".to_string(),
    };
    resolve_context_inner(&state, request, false)
}

async fn resolve_context(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminResolveContextRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    resolve_context_inner(&state, request, true)
}

fn resolve_context_inner(
    state: &AdminState,
    request: AdminResolveContextRequest,
    mint: bool,
) -> Response {
    let service = match service(state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let principal = match request.principal_id.parse::<PrincipalId>() {
        Ok(principal) => principal,
        Err(error) => return invalid(format!("invalid principal ID: {error}")),
    };
    let actor_kind = match request.actor_kind.as_str() {
        "human" => ActorKind::Human,
        "agent" => ActorKind::Agent,
        "system" => ActorKind::System,
        _ => return invalid("actor_kind must be human, agent, or system"),
    };
    let active_team = match request.active_team_id {
        Some(id) => match id.parse::<TeamId>() {
            Ok(id) => Some(id),
            Err(error) => return invalid(format!("invalid team ID: {error}")),
        },
        None => None,
    };
    let project = match request.project_id {
        Some(id) => match id.parse::<ProjectId>() {
            Ok(id) => Some(id),
            Err(error) => return invalid(format!("invalid project ID: {error}")),
        },
        None => None,
    };
    let read_mode = match request.read_mode.as_str() {
        "" | "contextual" => ReadMode::Contextual,
        "project_only" => ReadMode::ProjectOnly,
        "global_only" => ReadMode::GlobalOnly,
        _ => return invalid("read_mode must be contextual, project_only, or global_only"),
    };
    let write_selection = match request.write_target.as_str() {
        "" | "default" => WriteSelection::Default,
        "personal" => WriteSelection::Personal,
        "team" => WriteSelection::Team,
        _ => return invalid("write_target must be default, personal, or team"),
    };
    let now = now();
    let context = match service.resolve_context(
        &ResolveContextRequest {
            principal,
            actor_kind,
            profile: state.config.active_profile().to_string(),
            workspace: request.workspace.map(PathBuf::from),
            project,
            active_team,
            read_mode,
            write_selection,
        },
        now,
    ) {
        Ok(context) => context,
        Err(error) => return space_error(error),
    };
    let admin = admin_context(&context, service);
    if !mint {
        return Json(admin).into_response();
    }
    let bearer_generation = (*state.token_generation.borrow()).saturating_add(1);
    match service.mint_context_capability(&context, bearer_generation, now, 900) {
        Ok(capability) => Json(AdminContextCapabilityResponse {
            context: admin,
            capability,
            expires_at: now.saturating_add(900),
        })
        .into_response(),
        Err(error) => space_error(error),
    }
}

async fn revoke_context(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminResolveContextRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match service(&state) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let principal = match request.principal_id.parse::<PrincipalId>() {
        Ok(principal) => principal,
        Err(error) => return invalid(format!("invalid principal ID: {error}")),
    };
    match service.revoke_context_capabilities(&principal, now()) {
        Ok(revoked) => Json(serde_json::json!({ "revoked": revoked })).into_response(),
        Err(error) => space_error(error),
    }
}

fn active_readiness(state: &AdminState) -> Option<AdminSpaceReadiness> {
    let service = state.spaces.as_ref()?;
    let principal = INSTALLATION_PRINCIPAL.parse::<PrincipalId>().ok()?;
    let summary = service
        .find(
            state.config.active_profile(),
            &SpaceOwner::User(principal),
            SpaceContext::Global,
        )
        .ok()
        .flatten()?;
    Some(readiness(service, summary.id))
}

fn admin_detail(service: &LocalSpaceService, row: SpaceSummary) -> AdminSpaceDetail {
    let root_key = row.root_key.clone();
    let manifest_hash = format!("blake3:{}", hex(&row.manifest_hash));
    let catalog_generation = row.catalog_generation;
    AdminSpaceDetail {
        summary: admin_summary(service, row),
        root_key,
        catalog_generation,
        manifest_hash,
    }
}

fn admin_summary(service: &LocalSpaceService, row: SpaceSummary) -> AdminSpaceSummary {
    AdminSpaceSummary {
        id: row.id.to_string(),
        profile: row.profile,
        display_name: row.display_name,
        owner: admin_owner(&row.owner),
        context: admin_space_context(row.context),
        state: admin_state(&row.state),
        role: match row.owner {
            SpaceOwner::User(_) => AdminSpaceRole::Maintainer,
            SpaceOwner::Team(_) => AdminSpaceRole::Reader,
        },
        domain_count: row.domain_count,
        readiness: readiness(service, row.id),
    }
}

fn readiness(service: &LocalSpaceService, id: SpaceId) -> AdminSpaceReadiness {
    let Ok(handle) = service.open_handle(id) else {
        return AdminSpaceReadiness {
            recovery_required: true,
            ..AdminSpaceReadiness::default()
        };
    };
    let mut result = AdminSpaceReadiness {
        manifest_ready: true,
        history_ready: true,
        index_ready: true,
        mirrors_ready: true,
        ..AdminSpaceReadiness::default()
    };
    for graph in handle.domains().stores() {
        let Ok(domain) = graph.semantic_readiness() else {
            result.recovery_required = true;
            result.history_ready = false;
            result.index_ready = false;
            result.mirrors_ready = false;
            continue;
        };
        result.history_ready &= domain.history_ready;
        result.index_ready &= domain.index_ready;
        result.mirrors_ready &= domain.mirrors_ready;
        result.semantic_generation = result
            .semantic_generation
            .saturating_add(domain.semantic_generation);
        result.indexed_generation = result
            .indexed_generation
            .saturating_add(domain.indexed_generation);
        result.mirror_generation = result
            .mirror_generation
            .saturating_add(domain.mirror_generation);
    }
    result
}

fn admin_context(context: &MemoryContext, service: &LocalSpaceService) -> AdminMemoryContext {
    AdminMemoryContext {
        profile: context.profile.clone(),
        principal_id: context.principal.to_string(),
        actor_kind: match context.actor_kind {
            ActorKind::Human => "human",
            ActorKind::Agent => "agent",
            ActorKind::System => "system",
        }
        .to_string(),
        project_id: context.project.map(|id| id.to_string()),
        active_team_id: context.active_team.as_ref().map(ToString::to_string),
        read_set: context
            .read_set
            .iter()
            .enumerate()
            .map(|(index, grant)| AdminContextGrant {
                space_id: grant.space.id.to_string(),
                display_name: service
                    .get(grant.space.id)
                    .map_or_else(|_| "Unavailable".to_string(), |space| space.display_name),
                role: admin_role(grant.role),
                authority_generation: grant.authority_generation,
                read_priority: u8::try_from(index).unwrap_or(u8::MAX),
            })
            .collect(),
        write_target: context.default_write.to_string(),
        authorization_generation: context.authorization_generation,
    }
}

fn core_owner(owner: AdminSpaceOwner) -> Result<SpaceOwner, String> {
    match owner {
        AdminSpaceOwner::User(id) => id
            .parse()
            .map(SpaceOwner::User)
            .map_err(|error| format!("invalid user principal: {error}")),
        AdminSpaceOwner::Team(id) => id
            .parse()
            .map(SpaceOwner::Team)
            .map_err(|error| format!("invalid team ID: {error}")),
    }
}

fn core_context(context: AdminSpaceContext) -> Result<SpaceContext, String> {
    match context {
        AdminSpaceContext::Global => Ok(SpaceContext::Global),
        AdminSpaceContext::Project(id) => id
            .parse()
            .map(SpaceContext::Project)
            .map_err(|error| format!("invalid project ID: {error}")),
    }
}

fn admin_owner(owner: &SpaceOwner) -> AdminSpaceOwner {
    match owner {
        SpaceOwner::User(id) => AdminSpaceOwner::User(id.to_string()),
        SpaceOwner::Team(id) => AdminSpaceOwner::Team(id.to_string()),
    }
}

fn admin_space_context(context: SpaceContext) -> AdminSpaceContext {
    match context {
        SpaceContext::Global => AdminSpaceContext::Global,
        SpaceContext::Project(id) => AdminSpaceContext::Project(id.to_string()),
    }
}

fn admin_role(role: SpaceRole) -> AdminSpaceRole {
    match role {
        SpaceRole::Reader => AdminSpaceRole::Reader,
        SpaceRole::Contributor => AdminSpaceRole::Contributor,
        SpaceRole::Reviewer => AdminSpaceRole::Reviewer,
        SpaceRole::Maintainer => AdminSpaceRole::Maintainer,
    }
}

fn core_role(role: AdminSpaceRole) -> SpaceRole {
    match role {
        AdminSpaceRole::Reader => SpaceRole::Reader,
        AdminSpaceRole::Contributor => SpaceRole::Contributor,
        AdminSpaceRole::Reviewer => SpaceRole::Reviewer,
        AdminSpaceRole::Maintainer => SpaceRole::Maintainer,
    }
}

fn admin_state(state: &str) -> AdminSpaceState {
    match state {
        "creating" => AdminSpaceState::Creating,
        "active" => AdminSpaceState::Active,
        "closed" => AdminSpaceState::Closed,
        "deleting" => AdminSpaceState::Deleting,
        _ => AdminSpaceState::Error,
    }
}

fn service(state: &AdminState) -> Result<&LocalSpaceService, Response> {
    state.spaces.as_ref().ok_or_else(|| {
        json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::RecoveryRequired,
                "space catalog/runtime is unavailable",
                Some("Run `openmemory doctor` for the active profile."),
                true,
            )),
        )
    })
}

fn space_error(error: SpaceServiceError) -> Response {
    let (status, code) = match error {
        SpaceServiceError::Invalid(_) => (StatusCode::BAD_REQUEST, AdminErrorCode::InvalidRequest),
        SpaceServiceError::NotFound => (StatusCode::NOT_FOUND, AdminErrorCode::NotFound),
        SpaceServiceError::Conflict(_) => (StatusCode::CONFLICT, AdminErrorCode::Conflict),
        SpaceServiceError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            AdminErrorCode::RecoveryRequired,
        ),
    };
    json_error(
        status,
        AdminErrorResponse::new(AdminError::new(
            code,
            error.to_string(),
            Option::<String>::None,
            status == StatusCode::SERVICE_UNAVAILABLE,
        )),
    )
}

fn invalid(message: impl Into<String>) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        AdminErrorResponse::new(AdminError::new(
            AdminErrorCode::InvalidRequest,
            message,
            Option::<String>::None,
            false,
        )),
    )
}

fn parse_id<T>(label: &str, value: &str) -> Result<T, Response>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| invalid(format!("invalid {label}: {error}")))
}

fn now() -> i64 {
    i64::try_from(unix_now_secs().unwrap_or(0)).unwrap_or(i64::MAX)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
