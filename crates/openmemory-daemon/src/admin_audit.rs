//! Authenticated audit, history, diff, and optimistic manual-edit adapters.

use std::ops::Deref;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use openmemory_admin::{
    AdminChangeDecisionRequest, AdminChangeOperation, AdminChangeSetDetail, AdminChangeSetState,
    AdminChangeSetSummary, AdminDestroyInventory, AdminDestroyPreview,
    AdminDestroyPreviewRequest, AdminDestroyReceipt, AdminDestroyRequest, AdminEditMemoryRequest,
    AdminError, AdminErrorCode, AdminErrorResponse, AdminFieldChange, AdminLifecycleRequest,
    AdminMemoryDiff, AdminObjectHistory, AdminRevertChangeSetRequest, AdminRevertRequest,
    AdminRevisionSummary, AdminSubmitChangeSetRequest, Page,
};
use openmemory_core::space::{
    ActorKind, ChangeSetId, PrincipalId, RevisionId, SpaceId, SpaceRole,
};
use openmemory_graph::{
    ChangeOperation, ChangeSetAuditRow, ChangeSetDetail, ChangeSetDraft, ChangeSetState,
    ExpectedHead, FieldChange, Lifecycle, ObjectKind, ObjectMutation, ObjectRef, ObservationValue,
    RevertToRevision, SubmitMode, SupersedeObservation,
};
use serde::Deserialize;

use crate::space_registry::SpaceLease;
use crate::state::AdminState;
use crate::{authorize_state, json_auth_error, json_error};

const INSTALLATION_PRINCIPAL: &str = "local:installation";

#[derive(Debug, Deserialize)]
struct ChangeSetListQuery {
    space_id: String,
    state: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct SpaceQuery {
    space_id: String,
    object_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    space_id: String,
    #[serde(default = "observation_kind")]
    object_kind: String,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct DiffQuery {
    space_id: String,
    #[serde(default = "observation_kind")]
    object_kind: String,
    from: Option<String>,
    to: Option<String>,
    expected: Option<String>,
}

fn observation_kind() -> String {
    "observation".to_string()
}

pub(crate) fn router() -> Router<AdminState> {
    Router::new()
        .route(
            "/admin/changesets",
            get(list_changesets).post(submit_changeset),
        )
        .route("/admin/changesets/{id}", get(get_changeset))
        .route("/admin/changesets/{id}/approve", post(approve_changeset))
        .route("/admin/changesets/{id}/reject", post(reject_changeset))
        .route("/admin/changesets/{id}/revert", post(revert_changeset))
        .route("/admin/memories/{logical_id}/history", get(memory_history))
        .route("/admin/memories/{logical_id}/diff", get(memory_diff))
        .route("/admin/memories/{logical_id}/edit", post(edit_memory))
        .route("/admin/memories/{logical_id}/retire", post(retire_memory))
        .route("/admin/memories/{logical_id}/restore", post(restore_memory))
        .route("/admin/memories/{logical_id}/revert", post(revert_memory))
        .route(
            "/admin/memories/{logical_id}/destroy/preview",
            post(preview_destroy_memory),
        )
        .route(
            "/admin/memories/{logical_id}/destroy",
            post(destroy_memory),
        )
}

async fn submit_changeset(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminSubmitChangeSetRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let space_id = match parse::<SpaceId>("space ID", &request.space_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let operations = match request
        .operations
        .into_iter()
        .map(parse_operation)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(operations) => operations,
        Err(response) => return response,
    };
    let principal: PrincipalId = INSTALLATION_PRINCIPAL
        .parse()
        .expect("static installation principal is valid");
    let draft = ChangeSetDraft {
        idempotency_key: request.idempotency_key,
        space_id,
        actor_principal: principal,
        actor_kind: ActorKind::Human,
        authorization_generation: 1,
        reason: request.reason,
        source: request.source,
        operations,
    };
    let store = match store(&state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.submit_changeset(
        &draft,
        if request.propose {
            SubmitMode::Propose
        } else {
            SubmitMode::ApplyImmediately
        },
    ) {
        Ok(receipt) => {
            let status = if request.propose {
                StatusCode::ACCEPTED
            } else {
                StatusCode::CREATED
            };
            (status, Json(receipt)).into_response()
        }
        Err(error) => graph_error(error),
    }
}

async fn list_changesets(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<ChangeSetListQuery>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let space_id = match parse::<SpaceId>("space ID", &query.space_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let store = match store(&state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 256);
    match store.list_changesets(None, limit) {
        Ok(rows) => {
            let requested_state = query.state.as_deref();
            let items = rows
                .into_iter()
                .filter(|row| requested_state.is_none_or(|state| state == state_name(row.state)))
                .map(|row| admin_summary(space_id, row))
                .collect();
            Json(Page::new(items, None)).into_response()
        }
        Err(error) => graph_error(error),
    }
}

async fn get_changeset(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let space_id = match parse::<SpaceId>("space ID", &query.space_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let id = match parse::<ChangeSetId>("changeset ID", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let store = match store(&state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.get_changeset(id) {
        Ok(Some(detail)) => Json(admin_detail(space_id, detail)).into_response(),
        Ok(None) => not_found("changeset was not found"),
        Err(error) => graph_error(error),
    }
}

async fn approve_changeset(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
    Json(request): Json<AdminChangeDecisionRequest>,
) -> Response {
    decide_changeset(state, headers, id, query, request, true).await
}

async fn reject_changeset(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
    Json(request): Json<AdminChangeDecisionRequest>,
) -> Response {
    decide_changeset(state, headers, id, query, request, false).await
}

async fn revert_changeset(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
    Json(request): Json<AdminRevertChangeSetRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let space_id = match parse::<SpaceId>("space ID", &query.space_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let id = match parse::<ChangeSetId>("changeset ID", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let principal: PrincipalId = INSTALLATION_PRINCIPAL
        .parse()
        .expect("static installation principal is valid");
    let store = match store(&state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let authority_generation = match authority_generation(
        &state,
        space_id,
        &principal,
        SpaceRole::Contributor,
    ) {
        Ok(generation) => generation,
        Err(response) => return response,
    };
    match store.revert_changeset(
        id,
        &principal,
        ActorKind::Human,
        authority_generation,
        &request.idempotency_key,
        &request.reason,
    ) {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => graph_error(error),
    }
}

fn authority_generation(
    state: &AdminState,
    space_id: SpaceId,
    principal: &PrincipalId,
    required: SpaceRole,
) -> Result<u64, Response> {
    let service = state.spaces.as_ref().ok_or_else(|| {
        json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::RecoveryRequired,
                "space authority is unavailable",
                Option::<String>::None,
                true,
            )),
        )
    })?;
    match service.authorize(space_id, principal, unix_now()) {
        Ok(Some((role, generation))) if role.allows(required) => Ok(generation),
        Ok(_) => Err(json_error(
            StatusCode::FORBIDDEN,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::AuthorizationDenied,
                "current space role does not authorize this operation",
                Option::<String>::None,
                false,
            )),
        )),
        Err(error) => Err(json_error(
            StatusCode::BAD_REQUEST,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::InvalidRequest,
                error.to_string(),
                Option::<String>::None,
                false,
            )),
        )),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

async fn decide_changeset(
    state: AdminState,
    headers: HeaderMap,
    id: String,
    query: SpaceQuery,
    request: AdminChangeDecisionRequest,
    approve: bool,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    if request.expected_state != AdminChangeSetState::Proposed {
        return conflict("changeset decision expected_state must be proposed");
    }
    if request.reason.trim().is_empty() {
        return invalid("review reason must not be empty");
    }
    let space_id = match parse::<SpaceId>("space ID", &query.space_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let id = match parse::<ChangeSetId>("changeset ID", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let reviewer: PrincipalId = INSTALLATION_PRINCIPAL
        .parse()
        .expect("static installation principal is valid");
    let current_generation =
        match authority_generation(&state, space_id, &reviewer, SpaceRole::Reviewer) {
            Ok(generation) => generation,
            Err(response) => return response,
        };
    if current_generation != request.authority_generation {
        return conflict("review authority generation is stale");
    }
    let store = match store(&state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let receipt = if approve {
        store.approve_changeset(
            id,
            &reviewer,
            ActorKind::Human,
            request.authority_generation,
        )
    } else {
        store.reject_changeset(
            id,
            &reviewer,
            ActorKind::Human,
            request.authority_generation,
        )
    };
    match receipt {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => graph_error(error),
    }
}

async fn memory_history(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(logical_id): AxumPath<String>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let (space_id, object) = match object(&query.space_id, &query.object_kind, logical_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let cursor = match query
        .cursor
        .as_deref()
        .map(parse_history_cursor)
        .transpose()
    {
        Ok(cursor) => cursor,
        Err(response) => return response,
    };
    let store = match store(&state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.object_history(
        object.clone(),
        cursor,
        query.limit.unwrap_or(50).clamp(1, 256),
    ) {
        Ok(page) => Json(AdminObjectHistory {
            space_id: page.space_id.to_string(),
            logical_id: object.logical_id,
            object_kind: kind_name(object.kind).to_string(),
            revisions: page
                .revisions
                .into_iter()
                .map(|revision| AdminRevisionSummary {
                    revision_id: revision.revision_id.to_string(),
                    parent_revision_id: revision.parent_revision_id.map(|id| id.to_string()),
                    semantic_hash: hex(&revision.semantic_hash),
                    created_by_change_set: revision.created_by_change_set.to_string(),
                    created_at: revision.created_at,
                })
                .collect(),
            next_cursor: page.next_cursor.map(|(time, id)| format!("{time}:{}", id)),
        })
        .into_response(),
        Err(error) => graph_error(error),
    }
}

async fn memory_diff(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(logical_id): AxumPath<String>,
    Query(query): Query<DiffQuery>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let (space_id, object) = match object(&query.space_id, &query.object_kind, logical_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let from = match optional_revision(query.from.as_deref()) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let to = match optional_revision(query.to.as_deref()) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let expected = match optional_revision(query.expected.as_deref()) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let store = match store(&state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.diff_revisions(object.clone(), from, to, expected) {
        Ok(diff) => Json(AdminMemoryDiff {
            space_id: diff.space_id.to_string(),
            logical_id: object.logical_id,
            object_kind: kind_name(object.kind).to_string(),
            from_revision: diff.from_revision.map(|id| id.to_string()),
            to_revision: diff.to_revision.map(|id| id.to_string()),
            current_revision: diff.current_revision.map(|id| id.to_string()),
            stale: diff.stale,
            changes: diff.fields.into_iter().map(admin_field_change).collect(),
        })
        .into_response(),
        Err(error) => graph_error(error),
    }
}

async fn edit_memory(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(logical_id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
    Json(request): Json<AdminEditMemoryRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let space_id = match parse::<SpaceId>("space ID", &query.space_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if query
        .object_kind
        .as_deref()
        .is_some_and(|kind| kind != "observation")
    {
        return invalid("typed field edit currently requires object_kind=observation");
    }
    let value: ObservationValue = match serde_json::from_value(request.fields) {
        Ok(value) => value,
        Err(error) => return invalid(format!("invalid observation fields: {error}")),
    };
    let expected = match expected(
        request.expected_revision_id.as_deref(),
        request.expected_row_version,
        &request.expected_lifecycle,
    ) {
        Ok(expected) => expected,
        Err(response) => return response,
    };
    apply_manual(
        &state,
        space_id,
        request.reason,
        ChangeOperation::SupersedeObservation(SupersedeObservation {
            logical_id,
            expected,
            value,
        }),
    )
}

async fn retire_memory(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(logical_id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
    Json(request): Json<AdminLifecycleRequest>,
) -> Response {
    lifecycle(state, headers, logical_id, query, request, false).await
}

async fn restore_memory(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(logical_id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
    Json(request): Json<AdminLifecycleRequest>,
) -> Response {
    lifecycle(state, headers, logical_id, query, request, true).await
}

async fn lifecycle(
    state: AdminState,
    headers: HeaderMap,
    logical_id: String,
    query: SpaceQuery,
    request: AdminLifecycleRequest,
    restore: bool,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let (space_id, object) = match object(
        &query.space_id,
        query.object_kind.as_deref().unwrap_or("observation"),
        logical_id,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let expected = match expected(
        request.expected_revision_id.as_deref(),
        request.expected_row_version,
        &request.expected_lifecycle,
    ) {
        Ok(expected) => expected,
        Err(response) => return response,
    };
    let mutation = ObjectMutation {
        object,
        expected,
    };
    apply_manual(
        &state,
        space_id,
        request.reason,
        if restore {
            ChangeOperation::Restore(mutation)
        } else {
            ChangeOperation::Retire(mutation)
        },
    )
}

async fn revert_memory(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(logical_id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
    Json(request): Json<AdminRevertRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let (space_id, object) = match object(
        &query.space_id,
        query.object_kind.as_deref().unwrap_or("observation"),
        logical_id,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let expected = match expected(
        request.expected_revision_id.as_deref(),
        request.expected_row_version,
        "active",
    ) {
        Ok(expected) => expected,
        Err(response) => return response,
    };
    let revision_id = match parse::<RevisionId>("revision ID", &request.revision_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    apply_manual(
        &state,
        space_id,
        request.reason,
        ChangeOperation::RevertToRevision(RevertToRevision {
            object,
            expected,
            revision_id,
        }),
    )
}

async fn preview_destroy_memory(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(logical_id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
    Json(request): Json<AdminDestroyPreviewRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let (space_id, object) = match object(&query.space_id, &request.object_kind, logical_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let principal: PrincipalId = INSTALLATION_PRINCIPAL
        .parse()
        .expect("static installation principal is valid");
    if let Err(response) =
        authority_generation(&state, space_id, &principal, SpaceRole::Maintainer)
    {
        return response;
    }
    let store = match store(&state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.preview_destroy(object, &request.scope) {
        Ok(preview) => Json(AdminDestroyPreview {
            space_id: space_id.to_string(),
            logical_id: preview.object.logical_id,
            object_kind: kind_name(preview.object.kind).to_string(),
            scope: preview.scope,
            inventory: AdminDestroyInventory {
                canonical_rows: preview.inventory.canonical_rows,
                revisions: preview.inventory.revisions,
                contributions: preview.inventory.contributions,
                related_change_requests: preview.inventory.related_change_requests,
                affected_ids: preview.inventory.affected_ids,
                affected_ids_truncated: preview.inventory.affected_ids_truncated,
            },
            confirmation: preview.confirmation,
            expires_at: preview.expires_at,
            expected_revision_id: preview.expected_revision_id.map(|id| id.to_string()),
            expected_row_version: preview.expected_row_version,
            expected_lifecycle: preview.expected_lifecycle,
            expected_generation: preview.expected_generation,
        })
        .into_response(),
        Err(error) => graph_error(error),
    }
}

async fn destroy_memory(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(logical_id): AxumPath<String>,
    Query(query): Query<SpaceQuery>,
    Json(request): Json<AdminDestroyRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let (space_id, object) = match object(&query.space_id, &request.object_kind, logical_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let principal: PrincipalId = INSTALLATION_PRINCIPAL
        .parse()
        .expect("static installation principal is valid");
    if let Err(response) =
        authority_generation(&state, space_id, &principal, SpaceRole::Maintainer)
    {
        return response;
    }
    let store = match store(&state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.destroy_previewed(
        object,
        &request.scope,
        &request.confirmation,
        &principal,
        ActorKind::Human,
        &request.reason,
    ) {
        Ok(receipt) => Json(AdminDestroyReceipt {
            id: receipt.id,
            space_id: space_id.to_string(),
            object_kind: kind_name(receipt.object_kind).to_string(),
            scope: receipt.scope,
            semantic_generation: receipt.semantic_generation,
            indexed_generation: receipt.indexed_generation,
            index_ready: receipt.index_ready,
            destroyed_at: receipt.destroyed_at,
        })
        .into_response(),
        Err(error) => graph_error(error),
    }
}

fn apply_manual(
    state: &AdminState,
    space_id: SpaceId,
    reason: String,
    operation: ChangeOperation,
) -> Response {
    if reason.trim().is_empty() {
        return invalid("manual mutation reason must not be empty");
    }
    let principal = INSTALLATION_PRINCIPAL
        .parse()
        .expect("static installation principal is valid");
    let generation =
        match authority_generation(state, space_id, &principal, SpaceRole::Contributor) {
            Ok(generation) => generation,
            Err(response) => return response,
        };
    let draft = ChangeSetDraft {
        idempotency_key: format!("admin:{}", ChangeSetId::new()),
        space_id,
        actor_principal: principal,
        actor_kind: ActorKind::Human,
        authorization_generation: generation,
        reason,
        source: "admin".to_string(),
        operations: vec![operation],
    };
    let store = match store(state, space_id) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.submit_changeset(&draft, SubmitMode::ApplyImmediately) {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => graph_error(error),
    }
}

fn parse_operation(operation: AdminChangeOperation) -> Result<ChangeOperation, Response> {
    macro_rules! decode {
        ($variant:ident, $ty:ty) => {
            serde_json::from_value::<$ty>(operation.payload)
                .map(ChangeOperation::$variant)
                .map_err(|error| {
                    invalid(format!("invalid {} payload: {error}", operation.operation))
                })
        };
    }
    match operation.operation.as_str() {
        "remember" => decode!(Remember, openmemory_graph::RememberChange),
        "supersede_observation" => {
            decode!(SupersedeObservation, openmemory_graph::SupersedeObservation)
        }
        "set_observation_tier" => {
            decode!(SetObservationTier, openmemory_graph::SetObservationTier)
        }
        "retire" => decode!(Retire, openmemory_graph::ObjectMutation),
        "restore" => decode!(Restore, openmemory_graph::ObjectMutation),
        "revert_to_revision" => {
            decode!(RevertToRevision, openmemory_graph::RevertToRevision)
        }
        "update_entity" => decode!(UpdateEntity, openmemory_graph::UpdateEntity),
        "update_relation" => decode!(UpdateRelation, openmemory_graph::UpdateRelation),
        "cherry_pick" => decode!(CherryPick, openmemory_graph::ProvenanceChange),
        "merge_contribution" => {
            decode!(MergeContribution, openmemory_graph::ProvenanceChange)
        }
        _ => Err(invalid("unknown changeset operation")),
    }
}

fn admin_detail(space_id: SpaceId, detail: ChangeSetDetail) -> AdminChangeSetDetail {
    let decided_at = detail.summary.decided_at;
    let decided_by = detail.summary.decided_by.clone();
    AdminChangeSetDetail {
        summary: admin_summary(space_id, detail.summary),
        operations: detail.operations.into_iter().map(admin_operation).collect(),
        decided_at,
        decided_by,
    }
}

fn admin_summary(space_id: SpaceId, row: ChangeSetAuditRow) -> AdminChangeSetSummary {
    AdminChangeSetSummary {
        id: row.id.to_string(),
        space_id: space_id.to_string(),
        state: admin_changeset_state(row.state),
        actor_principal: row.actor_principal,
        actor_kind: row.actor_kind,
        reason: row.reason,
        source: row.source,
        base_generation: row.base_generation,
        committed_generation: row.committed_generation,
        created_at: row.created_at,
    }
}

fn admin_operation(operation: ChangeOperation) -> AdminChangeOperation {
    let (kind, logical, name) = match &operation {
        ChangeOperation::Remember(change) => ("entity", change.entity.name.clone(), "remember"),
        ChangeOperation::SupersedeObservation(change) => (
            "observation",
            change.logical_id.clone(),
            "supersede_observation",
        ),
        ChangeOperation::SetObservationTier(change) => (
            "observation",
            change.logical_id.clone(),
            "set_observation_tier",
        ),
        ChangeOperation::Retire(change) => (
            kind_name(change.object.kind),
            change.object.logical_id.clone(),
            "retire",
        ),
        ChangeOperation::Restore(change) => (
            kind_name(change.object.kind),
            change.object.logical_id.clone(),
            "restore",
        ),
        ChangeOperation::RevertToRevision(change) => (
            kind_name(change.object.kind),
            change.object.logical_id.clone(),
            "revert_to_revision",
        ),
        ChangeOperation::UpdateEntity(change) => {
            ("entity", change.logical_id.clone(), "update_entity")
        }
        ChangeOperation::UpdateRelation(change) => {
            ("relation", change.logical_id.clone(), "update_relation")
        }
        ChangeOperation::CherryPick(_) => ("mixed", "provenance".to_string(), "cherry_pick"),
        ChangeOperation::MergeContribution(_) => {
            ("mixed", "provenance".to_string(), "merge_contribution")
        }
    };
    AdminChangeOperation {
        object_kind: kind.to_string(),
        logical_id: logical,
        operation: name.to_string(),
        expected_revision_id: None,
        expected_row_version: None,
        payload: serde_json::to_value(operation).unwrap_or(serde_json::Value::Null),
    }
}

fn admin_field_change(change: FieldChange) -> AdminFieldChange {
    match change {
        FieldChange::Text {
            field,
            before,
            after,
        } => AdminFieldChange {
            field,
            before: before.as_ref().map(|value| value.preview.clone()),
            after: after.as_ref().map(|value| value.preview.clone()),
            truncated: before.as_ref().is_some_and(|value| value.truncated)
                || after.as_ref().is_some_and(|value| value.truncated),
            before_hash: before.map(|value| hex(&value.content_hash)),
            after_hash: after.map(|value| hex(&value.content_hash)),
        },
        other => AdminFieldChange {
            field: match &other {
                FieldChange::Scalar { field, .. } | FieldChange::Set { field, .. } => field.clone(),
                FieldChange::Relation { .. } => "relation".to_string(),
                FieldChange::Contribution { .. } => "contribution".to_string(),
                FieldChange::Text { .. } => unreachable!(),
            },
            before: None,
            after: serde_json::to_string(&other).ok(),
            truncated: false,
            before_hash: None,
            after_hash: None,
        },
    }
}

fn expected(
    revision: Option<&str>,
    row_version: u64,
    lifecycle: &str,
) -> Result<ExpectedHead, Response> {
    Ok(ExpectedHead {
        revision_id: optional_revision(revision)?,
        row_version,
        lifecycle: match lifecycle {
            "active" => Lifecycle::Active,
            "retired" => Lifecycle::Retired,
            "destroyed" => Lifecycle::Destroyed,
            _ => return Err(invalid("expected_lifecycle is invalid")),
        },
    })
}

fn object(space: &str, kind: &str, logical_id: String) -> Result<(SpaceId, ObjectRef), Response> {
    Ok((
        parse::<SpaceId>("space ID", space)?,
        ObjectRef {
            kind: parse_kind(kind)?,
            logical_id,
        },
    ))
}

fn parse_kind(kind: &str) -> Result<ObjectKind, Response> {
    match kind {
        "entity" => Ok(ObjectKind::Entity),
        "observation" => Ok(ObjectKind::Observation),
        "relation" => Ok(ObjectKind::Relation),
        _ => Err(invalid(
            "object_kind must be entity, observation, or relation",
        )),
    }
}

fn kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Entity => "entity",
        ObjectKind::Observation => "observation",
        ObjectKind::Relation => "relation",
    }
}

fn state_name(state: ChangeSetState) -> &'static str {
    match state {
        ChangeSetState::Proposed => "proposed",
        ChangeSetState::Applied => "applied",
        ChangeSetState::Rejected => "rejected",
        ChangeSetState::Conflicted => "conflicted",
        ChangeSetState::Reverted => "reverted",
    }
}

fn admin_changeset_state(state: ChangeSetState) -> AdminChangeSetState {
    match state {
        ChangeSetState::Proposed => AdminChangeSetState::Proposed,
        ChangeSetState::Applied => AdminChangeSetState::Applied,
        ChangeSetState::Rejected => AdminChangeSetState::Rejected,
        ChangeSetState::Conflicted => AdminChangeSetState::Conflicted,
        ChangeSetState::Reverted => AdminChangeSetState::Reverted,
    }
}

fn parse_history_cursor(value: &str) -> Result<(i64, RevisionId), Response> {
    let (time, id) = value
        .split_once(':')
        .ok_or_else(|| invalid("history cursor is invalid"))?;
    let time = time
        .parse()
        .map_err(|_| invalid("history cursor time is invalid"))?;
    Ok((time, parse::<RevisionId>("history cursor revision", id)?))
}

fn optional_revision(value: Option<&str>) -> Result<Option<RevisionId>, Response> {
    value
        .map(|value| parse::<RevisionId>("revision ID", value))
        .transpose()
}

struct AdminSpaceLease {
    lease: SpaceLease,
}

impl Deref for AdminSpaceLease {
    type Target = openmemory_engine::partition::DomainStore;

    fn deref(&self) -> &Self::Target {
        &self.lease.domains
    }
}

fn store(state: &AdminState, space_id: SpaceId) -> Result<AdminSpaceLease, Response> {
    let service = state.spaces.as_ref().ok_or_else(|| {
        json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::RecoveryRequired,
                "space service is unavailable",
                Option::<String>::None,
                true,
            )),
        )
    })?;
    let registry = state.space_registry.as_ref().ok_or_else(|| {
        json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::RecoveryRequired,
                "space runtime registry is unavailable",
                Option::<String>::None,
                true,
            )),
        )
    })?;
    let summary = service.get(space_id).map_err(|error| {
        json_error(
            StatusCode::NOT_FOUND,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::NotFound,
                error.to_string(),
                Option::<String>::None,
                false,
            )),
        )
    })?;
    registry
        .lease(space_id, summary.catalog_generation, false)
        .map(|lease| AdminSpaceLease { lease })
        .map_err(|error| {
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                AdminErrorResponse::new(AdminError::new(
                    AdminErrorCode::RecoveryRequired,
                    error.to_string(),
                    Option::<String>::None,
                    true,
                )),
            )
        })
}

fn graph_error(error: openmemory_graph::MemoryError) -> Response {
    let message = error.to_string();
    let (status, code) = match &error {
        openmemory_graph::MemoryError::Authorization(_) => {
            (StatusCode::FORBIDDEN, AdminErrorCode::AuthorizationDenied)
        }
        openmemory_graph::MemoryError::ChangeSetStale(_)
        | openmemory_graph::MemoryError::IdempotencyConflict
        | openmemory_graph::MemoryError::ChangeSetCrossDomain => {
            (StatusCode::CONFLICT, AdminErrorCode::Stale)
        }
        openmemory_graph::MemoryError::IndexRepairRequired(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            AdminErrorCode::IndexRepairRequired,
        ),
        openmemory_graph::MemoryError::EntityNotFound(_)
        | openmemory_graph::MemoryError::ObservationNotFound(_) => {
            (StatusCode::NOT_FOUND, AdminErrorCode::NotFound)
        }
        _ => (StatusCode::BAD_REQUEST, AdminErrorCode::InvalidRequest),
    };
    json_error(
        status,
        AdminErrorResponse::new(AdminError::new(
            code,
            message,
            Option::<String>::None,
            status == StatusCode::SERVICE_UNAVAILABLE,
        )),
    )
}

fn parse<T>(label: &str, value: &str) -> Result<T, Response>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| invalid(format!("invalid {label}: {error}")))
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

fn conflict(message: impl Into<String>) -> Response {
    json_error(
        StatusCode::CONFLICT,
        AdminErrorResponse::new(AdminError::new(
            AdminErrorCode::Stale,
            message,
            Option::<String>::None,
            false,
        )),
    )
}

fn not_found(message: impl Into<String>) -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        AdminErrorResponse::new(AdminError::new(
            AdminErrorCode::NotFound,
            message,
            Option::<String>::None,
            false,
        )),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
