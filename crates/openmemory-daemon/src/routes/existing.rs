//! Existing v1alpha1 admin routes.
//!
//! Phase 0 only relocates route composition. Handlers and wire behavior remain
//! unchanged while future route families gain an explicit module owner.

use axum::routing::{get, post};
use axum::Router;

use crate::state::AdminState;
use crate::{
    handle_backup_create, handle_backup_preflight, handle_consolidate, handle_doctor,
    handle_entities, handle_entity_detail, handle_events, handle_health,
    handle_integration_install, handle_integration_preview, handle_integration_verify,
    handle_integrations, handle_job, handle_logs, handle_profiles, handle_restore,
    handle_restore_preflight, handle_rotate_token, handle_search, handle_shutdown,
};

pub(crate) fn router(state: AdminState, mcp_router: Option<Router>) -> Router {
    let admin = Router::new()
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
        .with_state(state);

    if let Some(mcp) = mcp_router {
        admin.merge(mcp)
    } else {
        admin
    }
}
