//! Human-only identity review and directional material-merge orchestration.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use openmemory_admin::{
    AdminError, AdminErrorCode, AdminErrorResponse, AdminIdentityCandidateDetail,
    AdminIdentityCandidateSummary, AdminIdentityDecisionRequest, AdminIdentityEvidence,
    AdminMergeAccounting, AdminMergeConfirmRequest, AdminMergeConflict, AdminMergeJob,
    AdminMergePreview, AdminMergePreviewRequest, AdminMergeResolutionRequest, AdminRecoveryReport,
    Page,
};
use openmemory_core::space::{ActorKind, MergeJobId, PrincipalId, SpaceId, SpaceRole};
use openmemory_engine::merge::{
    complete_promotion, materialize_snapshot, promote_staged_root, recover_promotion,
    MergePromotionIntent, PromotionMode,
};
use openmemory_engine::partition::DomainStore;
use openmemory_engine::space::{capture_space_snapshot, SpaceSnapshot};
use openmemory_merge::candidate::{discover_candidates, CandidateDiscovery, DEFAULT_CANDIDATE_CAP};
use openmemory_merge::hash::PacketHash;
use openmemory_merge::identity::{IdentityDecision, IdentityEvidence, IdentityResolutionReceipt};
use openmemory_merge::planner::{materialize_merge, CandidateSet, MergePlan, MergePolicy};
use serde::Deserialize;

use crate::identity::{IdentityCandidateRecord, IdentityService, IdentityServiceError};
use crate::merges::{MergeJobRecord, MergeService, MergeServiceError};
use crate::spaces::SpaceSummary;
use crate::state::{AdminState, StoreAdmissionPause, StoreRuntime};
use crate::{authorize_state, json_auth_error, json_error, unix_now_secs};

const INSTALLATION_PRINCIPAL: &str = "local:installation";

#[derive(Debug, Deserialize)]
struct CandidateQuery {
    state: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RecoveryQuery {
    job_id: String,
}

enum Planned {
    AwaitingReview {
        job: MergeJobRecord,
        source: SpaceSnapshot,
        target: SpaceSnapshot,
        unresolved: usize,
    },
    Ready {
        job: MergeJobRecord,
        source: SpaceSnapshot,
        target: SpaceSnapshot,
        plan: MergePlan,
    },
}

pub(crate) fn router() -> Router<AdminState> {
    Router::new()
        .route("/admin/merges/preview", post(preview))
        .route("/admin/merges/recover", post(recover))
        .route("/admin/merges/{job_id}", get(get_job))
        .route("/admin/merges/{job_id}/candidates", get(list_candidates))
        .route("/admin/identity/candidates/{id}", get(get_candidate))
        .route(
            "/admin/identity/candidates/{id}/decide",
            post(decide_candidate),
        )
        .route(
            "/admin/identity/decisions/{id}/revise",
            post(decide_candidate),
        )
        .route("/admin/merges/{job_id}/resolve", post(resolve))
        .route("/admin/merges/{job_id}/confirm", post(confirm))
        .route("/admin/merges/{job_id}/cancel", post(cancel))
}

async fn preview(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<AdminMergePreviewRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let source = match parse::<SpaceId>("source space ID", &request.source_space_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let target = match parse::<SpaceId>("target space ID", &request.target_space_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let principal = installation_principal();
    let role = match authorize_merge(&state, source, target, &principal) {
        Ok(role) => role,
        Err(response) => return response,
    };
    let service = match MergeService::open(state.config.home()) {
        Ok(service) => service,
        Err(error) => return merge_error(error),
    };
    let job = match service.create_job(
        state.config.active_profile(),
        source,
        target,
        &principal,
        &request.idempotency_key,
        role,
        now(),
    ) {
        Ok(job) => job,
        Err(error) => return merge_error(error),
    };
    match build_plan(&state, job, request.keep_undetermined_distinct) {
        Ok(Planned::AwaitingReview {
            job,
            source,
            target,
            unresolved,
        }) => (
            StatusCode::ACCEPTED,
            Json(AdminMergePreview {
                job_id: job.id.to_string(),
                source_space_id: job.source_space_id.to_string(),
                target_space_id: job.target_space_id.to_string(),
                source_snapshot_hash: source.canonical.snapshot_hash.to_string(),
                target_snapshot_hash: target.canonical.snapshot_hash.to_string(),
                plan_hash: String::new(),
                predicted_result_hash: String::new(),
                conflicts: Vec::new(),
                accounting: AdminMergeAccounting {
                    target_entities_retained: target.canonical.entities.len(),
                    target_observations_retained: target.canonical.observations.len(),
                    target_relations_retained: target.canonical.relations.len(),
                    source_entities_accounted: 0,
                    source_observations_accounted: 0,
                    source_relations_accounted: 0,
                    candidates_consumed: 0,
                    complete: false,
                },
                dispositions: vec![serde_json::json!({
                    "awaiting_identity_review": unresolved
                })],
            }),
        )
            .into_response(),
        Ok(Planned::Ready {
            job: planned_job,
            plan,
            ..
        }) => Json(admin_preview(planned_job, &plan)).into_response(),
        Err(response) => response,
    }
}

async fn get_job(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let id = match parse::<MergeJobId>("merge job ID", &job_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match MergeService::open(state.config.home()).and_then(|service| service.get_job(id)) {
        Ok(job) => Json(admin_job(job)).into_response(),
        Err(error) => merge_error(error),
    }
}

async fn list_candidates(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
    Query(query): Query<CandidateQuery>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match IdentityService::open(state.config.home()) {
        Ok(service) => service,
        Err(error) => return identity_error(error),
    };
    match service.list_candidates(
        &job_id,
        query.state.as_deref(),
        query.limit.unwrap_or(50).clamp(1, 256),
    ) {
        Ok(records) => Json(Page::new(
            records.into_iter().map(candidate_summary).collect(),
            None,
        ))
        .into_response(),
        Err(error) => identity_error(error),
    }
}

async fn get_candidate(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    match IdentityService::open(state.config.home()).and_then(|service| service.get_candidate(&id))
    {
        Ok(record) => Json(candidate_detail(record)).into_response(),
        Err(error) => identity_error(error),
    }
}

async fn decide_candidate(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<AdminIdentityDecisionRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let service = match IdentityService::open(state.config.home()) {
        Ok(service) => service,
        Err(error) => return identity_error(error),
    };
    let record = match service.get_candidate(&id) {
        Ok(record) => record,
        Err(error) => return identity_error(error),
    };
    let packet_hash = match request.packet_hash.parse::<PacketHash>() {
        Ok(hash) => hash,
        Err(error) => return invalid(format!("invalid packet hash: {error}")),
    };
    if packet_hash != record.packet.binding_hash {
        return conflict("identity packet moved; refresh before deciding");
    }
    let decision = match request.decision.as_str() {
        "same" => IdentityDecision::Same,
        "different" => IdentityDecision::Different,
        "undetermined" => IdentityDecision::Undetermined,
        _ => return invalid("decision must be same, different, or undetermined"),
    };
    let principal = installation_principal();
    let target_space = record.candidate.left.address.space_id;
    let (role, generation) =
        match state
            .spaces
            .as_ref()
            .ok_or_else(unavailable)
            .and_then(|spaces| {
                spaces
                    .authorize(target_space, &principal, now())
                    .map_err(|error| invalid(error.to_string()))
            }) {
            Ok(Some(grant)) => grant,
            Ok(None) => return forbidden("reviewer has no target-space authority"),
            Err(response) => return response,
        };
    if generation != request.authority_generation {
        return conflict("identity review authority generation is stale");
    }
    match service.decide_human(
        &id,
        &principal,
        ActorKind::Human,
        role,
        generation,
        decision,
        &request.reason,
        now(),
    ) {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => identity_error(error),
    }
}

async fn resolve(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
    Json(request): Json<AdminMergeResolutionRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    if !request.resolutions.is_empty() {
        return invalid("identity decisions must be submitted through candidate decision routes");
    }
    let id = match parse::<MergeJobId>("merge job ID", &job_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match build_plan(&state, id, true) {
        Ok(Planned::AwaitingReview { unresolved, .. }) => json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            AdminErrorResponse::new(
                AdminError::new(
                    AdminErrorCode::ValidationFailed,
                    "identity candidates remain unresolved",
                    Some("Decide every candidate before resolving the merge."),
                    false,
                )
                .with_details(serde_json::json!({ "unresolved": unresolved })),
            ),
        ),
        Ok(Planned::Ready { job, plan, .. }) => {
            if !request.expected_plan_hash.is_empty()
                && request.expected_plan_hash != plan.plan_hash.to_string()
            {
                return conflict("expected plan hash is stale");
            }
            let merges = match MergeService::open(state.config.home()) {
                Ok(service) => service,
                Err(error) => return merge_error(error),
            };
            match merges.mint_confirmation(id, &plan, now(), 900) {
                Ok(token) => Json(serde_json::json!({
                    "preview": admin_preview(job, &plan),
                    "confirmation_token": token
                }))
                .into_response(),
                Err(error) => merge_error(error),
            }
        }
        Err(response) => response,
    }
}

async fn confirm(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
    Json(request): Json<AdminMergeConfirmRequest>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let id = match parse::<MergeJobId>("merge job ID", &job_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let planned = match build_plan(&state, id, true) {
        Ok(Planned::Ready {
            job,
            source,
            target,
            plan,
        }) => (job, source, target, plan),
        Ok(Planned::AwaitingReview { .. }) => {
            return conflict("merge is still awaiting identity review")
        }
        Err(response) => return response,
    };
    let (job, source, target, plan) = planned;
    if request.expected_plan_hash != plan.plan_hash.to_string()
        || request.expected_target_hash != target.canonical.snapshot_hash.to_string()
    {
        return conflict("merge confirmation is bound to stale input");
    }
    let merges = match MergeService::open(state.config.home()) {
        Ok(service) => service,
        Err(error) => return merge_error(error),
    };
    let principal = installation_principal();
    let role = match authorize_merge(&state, job.source_space_id, job.target_space_id, &principal) {
        Ok(role) => role,
        Err(response) => return response,
    };
    if let Err(error) = merges.confirm(
        id,
        &request.confirmation,
        &plan,
        &target.canonical,
        role,
        now(),
    ) {
        return merge_error(error);
    }
    match apply_plan(&state, &merges, job, source, target, plan) {
        Ok(report) => (StatusCode::ACCEPTED, Json(report)).into_response(),
        Err(response) => response,
    }
}

async fn cancel(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let id = match parse::<MergeJobId>("merge job ID", &job_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match MergeService::open(state.config.home()).and_then(|service| service.cancel(id, now())) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => merge_error(error),
    }
}

async fn recover(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<RecoveryQuery>,
) -> Response {
    if let Err((status, error)) = authorize_state(&headers, &state) {
        return json_auth_error(status, error);
    }
    let job_id = match parse::<MergeJobId>("merge job ID", &query.job_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let merges = match MergeService::open(state.config.home()) {
        Ok(service) => service,
        Err(error) => return merge_error(error),
    };
    let job = match merges.get_job(job_id) {
        Ok(job) => job,
        Err(error) => return merge_error(error),
    };
    let config = match crate::load_config(state.config.home()) {
        Ok(config) => config,
        Err(error) => return invalid(error),
    };
    let profile = crate::profile_data_dir(state.config.home(), state.config.active_profile());
    match recover_promotion(&config, &profile, job.target_space_id) {
        Ok(outcome) => Json(AdminRecoveryReport {
            job_id: Some(job.id.to_string()),
            target_space_id: job.target_space_id.to_string(),
            outcome: format!("{outcome:?}").to_lowercase(),
            live_hash: None,
            backup_root: Some(format!(".merge-backup/{}", job.id)),
        })
        .into_response(),
        Err(error) => graph_error(error),
    }
}

fn build_plan(
    state: &AdminState,
    id: MergeJobId,
    keep_undetermined_distinct: bool,
) -> Result<Planned, Response> {
    let merges = MergeService::open(state.config.home()).map_err(merge_error)?;
    let job = merges.get_job(id).map_err(merge_error)?;
    if !matches!(
        job.state.as_str(),
        "discovering" | "awaiting_review" | "planned"
    ) {
        return Err(conflict("merge job is not plannable"));
    }
    let spaces = state.spaces.as_ref().ok_or_else(unavailable)?;
    let engine = state
        .mcp_runtime
        .as_ref()
        .and_then(openmemory_mcp::McpRuntimeController::engine);
    let engine_pause = engine.as_ref().map(|engine| engine.pause_admissions());
    let source = capture_registered(state, spaces, job.source_space_id)?;
    let target = capture_registered(state, spaces, job.target_space_id)?;
    drop(engine_pause);
    drop(engine);
    let discovery = discover_candidates(
        &source.canonical,
        &target.canonical,
        &[],
        1,
        1,
        DEFAULT_CANDIDATE_CAP,
    )
    .map_err(|error| invalid(error.to_string()))?;
    validate_discovery(&discovery)?;
    let identity = IdentityService::open(state.config.home()).map_err(identity_error)?;
    let mut candidates = Vec::new();
    let mut receipts = Vec::<IdentityResolutionReceipt>::new();
    let mut unresolved = 0;
    for page in discovery.pages {
        for record in page.candidates {
            identity
                .record_candidate(
                    &id.to_string(),
                    &record.candidate.candidate_id,
                    &record.packet,
                    now(),
                )
                .map_err(identity_error)?;
            candidates.push(record.candidate.clone());
            match identity
                .current_receipt(&record.candidate.candidate_id)
                .map_err(identity_error)?
            {
                Some(receipt) => receipts.push(receipt),
                None => unresolved += 1,
            }
        }
    }
    if unresolved != 0 {
        merges
            .mark_awaiting_review(
                id,
                source.canonical.snapshot_hash.as_bytes(),
                target.canonical.snapshot_hash.as_bytes(),
                unresolved,
                now(),
            )
            .map_err(merge_error)?;
        return Ok(Planned::AwaitingReview {
            job: merges.get_job(id).map_err(merge_error)?,
            source,
            target,
            unresolved,
        });
    }
    let plan = merges
        .plan(
            id,
            source.canonical.clone(),
            target.canonical.clone(),
            CandidateSet {
                candidates,
                truncated: false,
                truncation_acknowledged: false,
            },
            receipts,
            None,
            MergePolicy {
                generation: 1,
                keep_undetermined_distinct,
            },
            now(),
        )
        .map_err(merge_error)?;
    Ok(Planned::Ready {
        job: merges.get_job(id).map_err(merge_error)?,
        source,
        target,
        plan,
    })
}

fn capture_registered(
    state: &AdminState,
    spaces: &crate::spaces::LocalSpaceService,
    id: SpaceId,
) -> Result<SpaceSnapshot, Response> {
    let summary = spaces.get(id).map_err(|error| invalid(error.to_string()))?;
    let registry = state.space_registry.as_ref().ok_or_else(unavailable)?;
    let lease = registry
        .lease(id, summary.catalog_generation, false)
        .map_err(|error| unavailable_message(error.to_string()))?;
    capture_space_snapshot(&lease.domains).map_err(graph_error)
}

fn apply_plan(
    state: &AdminState,
    merges: &MergeService,
    job: MergeJobRecord,
    source: SpaceSnapshot,
    target: SpaceSnapshot,
    plan: MergePlan,
) -> Result<AdminMergeJob, Response> {
    let config = crate::load_config(state.config.home()).map_err(invalid)?;
    let result = materialize_merge(&target.canonical, &source.canonical, &plan)
        .map_err(|error| invalid(error.to_string()))?;
    let profile_root = crate::profile_data_dir(state.config.home(), state.config.active_profile());
    let stage_rel = format!(".merge-staging/{}", job.id);
    let stage = profile_root.join(&stage_rel);
    merges
        .transition(job.id, "confirmed", "staging", now())
        .map_err(merge_error)?;
    materialize_snapshot(&config, &stage, target.domains.len(), &result).map_err(graph_error)?;
    merges
        .transition(job.id, "staging", "verified", now())
        .map_err(merge_error)?;

    let spaces = state.spaces.as_ref().ok_or_else(unavailable)?;
    let target_row = spaces
        .get(job.target_space_id)
        .map_err(|error| invalid(error.to_string()))?;
    let (mode, live_root, manifest_relpath) = promotion_layout(&target_row);
    let intent = MergePromotionIntent::new(
        job.id,
        job.source_space_id,
        job.target_space_id,
        mode,
        target.domains.len(),
        live_root,
        stage_rel,
        format!(".merge-backup/{}", job.id),
        target.canonical.snapshot_hash.to_string(),
        result.snapshot_hash.to_string(),
        plan.plan_hash.to_string(),
    )
    .map_err(graph_error)?;
    let registry = state.space_registry.as_ref().ok_or_else(unavailable)?;
    registry
        .close_for_promotion(job.target_space_id, Duration::from_secs(30))
        .map_err(|error| conflict(error.to_string()))?;
    let mut registry_guard = RegistryPromotionGuard {
        registry: registry.clone(),
        target: job.target_space_id,
        reopen_on_drop: true,
    };
    let mut legacy_pause = if mode == PromotionMode::LegacyArtifacts {
        Some(close_legacy_runtime(state, &config)?)
    } else {
        None
    };
    let fresh_target = {
        let handle = match spaces.open_handle(job.target_space_id) {
            Ok(handle) => handle,
            Err(error) => {
                return Err(reopen_after_failed_precondition(
                    state,
                    &config,
                    legacy_pause.take(),
                    error.to_string(),
                ));
            }
        };
        match capture_space_snapshot(handle.domains()) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Err(reopen_after_failed_precondition(
                    state,
                    &config,
                    legacy_pause.take(),
                    error.to_string(),
                ));
            }
        }
    };
    if fresh_target.canonical.snapshot_hash != target.canonical.snapshot_hash
        || fresh_target.domains != target.domains
    {
        if let Some(pause) = legacy_pause.take() {
            reopen_legacy_runtime(state, &config, pause)?;
        }
        return Err(conflict("target moved after confirmation"));
    }
    if let Err(error) = merges.transition(job.id, "verified", "promoting", now()) {
        if let Some(pause) = legacy_pause.take() {
            reopen_legacy_runtime(state, &config, pause)?;
        }
        return Err(merge_error(error));
    }
    let promoted_hash = match promote_staged_root(&config, &profile_root, intent) {
        Ok(report) => {
            registry_guard.reopen_on_drop = false;
            report.promoted_hash
        }
        Err(error) => {
            match recover_promotion(&config, &profile_root, job.target_space_id)
                .map_err(graph_error)?
            {
                openmemory_engine::merge::PromotionRecovery::ReadyForCatalogCommit
                | openmemory_engine::merge::PromotionRecovery::Complete => {
                    registry_guard.reopen_on_drop = false;
                    result.snapshot_hash.to_string()
                }
                openmemory_engine::merge::PromotionRecovery::NoIntent
                | openmemory_engine::merge::PromotionRecovery::RolledBack => {
                    if let Some(pause) = legacy_pause.take() {
                        reopen_legacy_runtime(state, &config, pause)?;
                    }
                    return Err(graph_error(error));
                }
            }
        }
    };
    merges
        .commit_promotion(
            job.id,
            source.canonical.snapshot_hash.as_bytes(),
            target.canonical.snapshot_hash.as_bytes(),
            result.snapshot_hash.as_bytes(),
            &manifest_relpath,
            now(),
        )
        .map_err(merge_error)?;
    complete_promotion(&profile_root, job.target_space_id, job.id).map_err(graph_error)?;
    if let Some(pause) = legacy_pause.take() {
        reopen_legacy_runtime(state, &config, pause)?;
    }
    registry_guard.reopen();
    let source_advanced = capture_registered(state, spaces, job.source_space_id)
        .map(|snapshot| snapshot.canonical.snapshot_hash != source.canonical.snapshot_hash)
        .unwrap_or(true);
    let mut completed = admin_job(merges.get_job(job.id).map_err(merge_error)?);
    completed.accounting = Some(admin_accounting(&plan));
    completed.predicted_result_hash = Some(promoted_hash);
    if source_advanced {
        state.logs.push(
            openmemory_admin::AdminLogLevel::Warning,
            "merge_source_advanced",
            "source advanced after immutable merge snapshot; promoted target remains valid",
            serde_json::json!({ "job_id": job.id.to_string() }),
        );
    }
    Ok(completed)
}

struct RegistryPromotionGuard {
    registry: crate::space_registry::SpaceRegistry,
    target: SpaceId,
    reopen_on_drop: bool,
}

impl RegistryPromotionGuard {
    fn reopen(mut self) {
        self.registry.reopen_after_promotion(self.target);
        self.reopen_on_drop = false;
    }
}

impl Drop for RegistryPromotionGuard {
    fn drop(&mut self) {
        if self.reopen_on_drop {
            self.registry.reopen_after_promotion(self.target);
        }
    }
}

struct ClosedLegacyRuntime {
    admission: StoreAdmissionPause,
    mcp: Option<openmemory_mcp::PausedMcpRuntime>,
}

fn close_legacy_runtime(
    state: &AdminState,
    _config: &openmemory_core::config::Config,
) -> Result<ClosedLegacyRuntime, Response> {
    let admission = state
        .store_admission
        .pause(Duration::from_secs(30))
        .map_err(|error| conflict(error.to_string()))?;
    let mcp = match &state.mcp_runtime {
        Some(runtime) => match runtime.pause_and_close(Duration::from_secs(30)) {
            Ok(pause) => Some(pause),
            Err(error) => {
                admission.resume();
                return Err(conflict(error.to_string()));
            }
        },
        None => None,
    };
    let old = {
        let mut runtime = state
            .store
            .write()
            .unwrap_or_else(|error| error.into_inner());
        std::mem::replace(
            &mut *runtime,
            StoreRuntime::Unavailable(AdminError::new(
                AdminErrorCode::RecoveryRequired,
                "legacy target is closed for material promotion",
                Option::<String>::None,
                true,
            )),
        )
    };
    drop(old);
    Ok(ClosedLegacyRuntime { admission, mcp })
}

fn reopen_legacy_runtime(
    state: &AdminState,
    config: &openmemory_core::config::Config,
    pause: ClosedLegacyRuntime,
) -> Result<(), Response> {
    let profile_root = crate::profile_data_dir(state.config.home(), state.config.active_profile());
    let memory = Arc::new(DomainStore::open_existing(config, &profile_root).map_err(graph_error)?);
    {
        let mut runtime = state
            .store
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *runtime = StoreRuntime::Ready(Arc::clone(&memory));
    }
    if let Some(mcp) = pause.mcp {
        let mut engine_config = config.clone();
        engine_config.engine.enabled = true;
        engine_config.engine.journal = true;
        mcp.resume(&engine_config, memory)
            .map_err(|error| unavailable_message(error.to_string()))?;
    }
    pause.admission.resume();
    Ok(())
}

fn reopen_after_failed_precondition(
    state: &AdminState,
    config: &openmemory_core::config::Config,
    pause: Option<ClosedLegacyRuntime>,
    original: String,
) -> Response {
    if let Some(pause) = pause {
        if let Err(response) = reopen_legacy_runtime(state, config, pause) {
            return response;
        }
    }
    unavailable_message(original)
}

fn promotion_layout(space: &SpaceSummary) -> (PromotionMode, String, String) {
    if space.root_key == "legacy-root" {
        (
            PromotionMode::LegacyArtifacts,
            ".".to_string(),
            "space.toml".to_string(),
        )
    } else {
        (
            PromotionMode::Directory,
            format!("{}/store", space.root_key),
            format!("{}/space.toml", space.root_key),
        )
    }
}

fn validate_discovery(discovery: &CandidateDiscovery) -> Result<(), Response> {
    if discovery.pages.iter().any(|page| page.truncated) {
        return Err(json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            AdminErrorResponse::new(AdminError::new(
                AdminErrorCode::ValidationFailed,
                "identity candidate page reached the hard review bound",
                Some("Refine identity evidence before material merge."),
                false,
            )),
        ));
    }
    Ok(())
}

fn authorize_merge(
    state: &AdminState,
    source: SpaceId,
    target: SpaceId,
    principal: &PrincipalId,
) -> Result<SpaceRole, Response> {
    let spaces = state.spaces.as_ref().ok_or_else(unavailable)?;
    let source_role = spaces
        .authorize(source, principal, now())
        .map_err(|error| invalid(error.to_string()))?
        .map(|grant| grant.0)
        .ok_or_else(|| forbidden("source space is inaccessible"))?;
    let target_role = spaces
        .authorize(target, principal, now())
        .map_err(|error| invalid(error.to_string()))?
        .map(|grant| grant.0)
        .ok_or_else(|| forbidden("target space is inaccessible"))?;
    if !source_role.allows(SpaceRole::Reader) || !target_role.allows(SpaceRole::Maintainer) {
        return Err(forbidden(
            "material merge requires source Reader and target Maintainer",
        ));
    }
    Ok(target_role)
}

fn admin_preview(job: MergeJobRecord, plan: &MergePlan) -> AdminMergePreview {
    AdminMergePreview {
        job_id: job.id.to_string(),
        source_space_id: job.source_space_id.to_string(),
        target_space_id: job.target_space_id.to_string(),
        source_snapshot_hash: plan.source_snapshot_hash.to_string(),
        target_snapshot_hash: plan.target_snapshot_hash.to_string(),
        plan_hash: plan.plan_hash.to_string(),
        predicted_result_hash: plan.predicted_result_hash.to_string(),
        conflicts: plan
            .conflicts
            .iter()
            .map(|conflict| AdminMergeConflict {
                object_kind: conflict.object_kind.clone(),
                source_id: conflict.source_id.clone(),
                target_id: conflict.target_id.clone(),
                field: conflict.field.clone(),
                base_hash: None,
                source_hash: String::new(),
                target_hash: String::new(),
            })
            .collect(),
        accounting: admin_accounting(plan),
        dispositions: plan
            .dispositions
            .iter()
            .filter_map(|item| serde_json::to_value(item).ok())
            .collect(),
    }
}

fn admin_accounting(plan: &MergePlan) -> AdminMergeAccounting {
    AdminMergeAccounting {
        target_entities_retained: plan.accounting.target_entities_retained,
        target_observations_retained: plan.accounting.target_observations_retained,
        target_relations_retained: plan.accounting.target_relations_retained,
        source_entities_accounted: plan.accounting.source_entities_accounted,
        source_observations_accounted: plan.accounting.source_observations_accounted,
        source_relations_accounted: plan.accounting.source_relations_accounted,
        candidates_consumed: plan.accounting.candidates_consumed,
        complete: plan.accounting.complete,
    }
}

fn admin_job(job: MergeJobRecord) -> AdminMergeJob {
    let report = serde_json::from_str::<serde_json::Value>(&job.report_json).ok();
    AdminMergeJob {
        id: job.id.to_string(),
        source_space_id: job.source_space_id.to_string(),
        target_space_id: job.target_space_id.to_string(),
        state: job.state,
        plan_hash: job.plan_hash.as_deref().map(hash_bytes),
        predicted_result_hash: job.predicted_result_hash.as_deref().map(hash_bytes),
        accounting: report
            .and_then(|value| value.get("accounting").cloned())
            .and_then(|value| serde_json::from_value(value).ok()),
        created_at: job.created_at,
        updated_at: job.updated_at,
    }
}

fn candidate_summary(record: IdentityCandidateRecord) -> AdminIdentityCandidateSummary {
    AdminIdentityCandidateSummary {
        id: record.candidate.candidate_id,
        merge_job_id: record.merge_job_id,
        left_space_id: record.candidate.left.address.space_id.to_string(),
        left_logical_id: record.candidate.left.address.logical_id,
        right_space_id: record.candidate.right.address.space_id.to_string(),
        right_logical_id: record.candidate.right.address.logical_id,
        deterministic_state: format!("{:?}", record.deterministic).to_lowercase(),
        proposal_state: record.proposal_state,
        packet_hash: record.packet.binding_hash.to_string(),
    }
}

fn candidate_detail(record: IdentityCandidateRecord) -> AdminIdentityCandidateDetail {
    let current_decision = record
        .current_decision
        .map(|decision| format!("{decision:?}").to_lowercase());
    let left_revision_id = record.candidate.left.revision_id.to_string();
    let right_revision_id = record.candidate.right.revision_id.to_string();
    let evidence = record.packet.evidence.iter().map(admin_evidence).collect();
    AdminIdentityCandidateDetail {
        summary: candidate_summary(record),
        left_revision_id,
        right_revision_id,
        evidence,
        current_decision,
        agent_proposal: None,
    }
}

fn admin_evidence(evidence: &IdentityEvidence) -> AdminIdentityEvidence {
    let (kind, proof_class, summary) = match evidence {
        IdentityEvidence::Lineage { copied_from, .. } => (
            "lineage",
            "proof_same",
            format!("{}:{}", copied_from.space_id, copied_from.logical_id),
        ),
        IdentityEvidence::VerifiedIdentifier {
            namespace,
            left_value,
            right_value,
            ..
        } => (
            "verified_identifier",
            if left_value == right_value {
                "proof_same"
            } else {
                "proof_different"
            },
            format!("{namespace}: {left_value} / {right_value}"),
        ),
        IdentityEvidence::ControlledKind { compatibility, .. } => (
            "controlled_kind",
            if format!("{compatibility:?}") == "Incompatible" {
                "proof_different"
            } else {
                "context"
            },
            format!("{compatibility:?}"),
        ),
        IdentityEvidence::Context { signal, detail, .. } => {
            ("context", "context", format!("{signal:?}: {detail}"))
        }
    };
    AdminIdentityEvidence {
        evidence_id: evidence.evidence_id().to_string(),
        kind: kind.to_string(),
        proof_class: proof_class.to_string(),
        summary,
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", hex(bytes))
}

fn installation_principal() -> PrincipalId {
    INSTALLATION_PRINCIPAL
        .parse()
        .expect("static installation principal is valid")
}

fn now() -> i64 {
    i64::try_from(unix_now_secs().unwrap_or(0)).unwrap_or(i64::MAX)
}

fn merge_error(error: MergeServiceError) -> Response {
    match error {
        MergeServiceError::Invalid(message) => invalid(message),
        MergeServiceError::Stale => conflict("merge job is stale or missing"),
        MergeServiceError::Unauthorized => forbidden("merge action is unauthorized"),
        MergeServiceError::Storage(message) => unavailable_message(message),
    }
}

fn identity_error(error: IdentityServiceError) -> Response {
    match error {
        IdentityServiceError::Invalid(message) => invalid(message),
        IdentityServiceError::Stale => conflict("identity candidate is stale or missing"),
        IdentityServiceError::Unauthorized => forbidden("identity action is unauthorized"),
        IdentityServiceError::Storage(message) => unavailable_message(message),
    }
}

fn graph_error(error: openmemory_graph::MemoryError) -> Response {
    unavailable_message(error.to_string())
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

fn forbidden(message: impl Into<String>) -> Response {
    json_error(
        StatusCode::FORBIDDEN,
        AdminErrorResponse::new(AdminError::new(
            AdminErrorCode::AuthorizationDenied,
            message,
            Option::<String>::None,
            false,
        )),
    )
}

fn unavailable() -> Response {
    unavailable_message("space/merge service is unavailable")
}

fn unavailable_message(message: impl Into<String>) -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        AdminErrorResponse::new(AdminError::new(
            AdminErrorCode::RecoveryRequired,
            message,
            Option::<String>::None,
            true,
        )),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
