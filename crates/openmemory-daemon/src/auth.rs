//! Local admin bearer-token ownership and request authentication.

// Phase 2 keeps catalog authorization private until the reviewed admin/MCP
// surface arrives in Phase 5. These contracts are nevertheless constructed
// during daemon startup and covered by production-path tests.
#![allow(dead_code)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use openmemory_admin::{AdminError, AdminErrorCode, AdminErrorResponse, AdminLogLevel};
use openmemory_core::space::{
    ActorKind, AuthoritySnapshot, PrincipalId, SpaceGrant, SpaceRole, TeamId,
};
use rand::RngCore;
use thiserror::Error;

use crate::product_store::{CatalogSpace, ProductStore, ProductStoreError};
use crate::state::AdminState;
use crate::{json_error, DaemonError, ADMIN_TOKEN_FILE, RUN_DIR};

/// Redacted bearer token used by the local admin API.
#[derive(Clone)]
pub struct AdminToken {
    pub(crate) expected: Arc<str>,
}

impl AdminToken {
    /// Build a token from a non-empty string.
    ///
    /// The token is trimmed before storage. Empty or whitespace-only strings
    /// are rejected so the daemon cannot accidentally run with a trivially
    /// bypassed auth check.
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

    pub(crate) fn matches(&self, candidate: &str) -> bool {
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

/// Load the per-home admin token or create one with owner-only permissions
/// where supported.
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

pub(crate) fn authorize_state(
    headers: &HeaderMap,
    state: &AdminState,
) -> Result<(), (StatusCode, AdminErrorResponse)> {
    let token = state
        .token
        .read()
        .unwrap_or_else(|error| error.into_inner())
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

pub(crate) fn json_auth_error(status: StatusCode, error: AdminErrorResponse) -> Response {
    let mut response = json_error(status, error);
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
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

/// Semantic operation checked against a catalog grant.  This closed world
/// keeps team-agent restrictions at the authority owner rather than relying on
/// later route handlers to remember them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorizedAction {
    Read,
    PersonalWrite,
    TeamProposal,
    Review,
    Membership,
    Promotion,
}

#[derive(Debug, Error)]
pub(crate) enum AuthorityError {
    #[error("product authority operation failed: {0}")]
    Product(#[from] ProductStoreError),
    #[error("principal has no current grant for this space")]
    Unauthorized,
    #[error("actor kind cannot perform this operation")]
    ActorForbidden,
    #[error("current role cannot perform this operation")]
    RoleForbidden,
    #[error("authority is being revoked; retry after the transition")]
    Revoking,
    #[error("authorization lease is stale")]
    StaleLease,
}

#[derive(Debug)]
struct GateState {
    active_leases: usize,
    revoking: bool,
    epoch: u64,
}

#[derive(Debug)]
struct AuthorizationGate {
    state: Mutex<GateState>,
    drained: Condvar,
}

/// Daemon-owned local authority service.  The gate makes lease lifetime
/// explicit: revocation waits every prior lease, then advances generations
/// before a new lease can begin.
#[derive(Debug, Clone)]
pub(crate) struct AuthorizationService {
    catalog: ProductStore,
    gate: Arc<AuthorizationGate>,
}

impl AuthorizationService {
    pub(crate) fn new(catalog: ProductStore) -> Self {
        Self {
            catalog,
            gate: Arc::new(AuthorizationGate {
                state: Mutex::new(GateState {
                    active_leases: 0,
                    revoking: false,
                    epoch: 0,
                }),
                drained: Condvar::new(),
            }),
        }
    }

    pub(crate) fn lease(
        &self,
        principal: PrincipalId,
        actor: ActorKind,
        space: &CatalogSpace,
        action: AuthorizedAction,
        now_unix_secs: i64,
    ) -> Result<AuthorizationLease, AuthorityError> {
        let epoch = {
            let mut state = self
                .gate
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.revoking {
                return Err(AuthorityError::Revoking);
            }
            state.active_leases += 1;
            state.epoch
        };
        let result = self.grant_for(&principal, actor, space, action, now_unix_secs);
        match result {
            Ok(grant) => Ok(AuthorizationLease {
                principal,
                actor,
                space: space.clone(),
                grant,
                epoch,
                service: self.clone(),
            }),
            Err(error) => {
                self.release_lease();
                Err(error)
            }
        }
    }

    /// Hold the exclusive authority transition while revoking a team grant.
    /// No old lease can publish after this method returns because it drains
    /// before the product transaction advances the relevant generations.
    pub(crate) fn revoke_team_member(
        &self,
        team: &TeamId,
        principal: &PrincipalId,
        now_unix_secs: i64,
    ) -> Result<bool, AuthorityError> {
        self.begin_revocation();
        let result = self
            .catalog
            .revoke_team_member(team, principal, now_unix_secs);
        self.finish_revocation();
        result.map_err(Into::into)
    }

    fn grant_for(
        &self,
        principal: &PrincipalId,
        actor: ActorKind,
        space: &CatalogSpace,
        action: AuthorizedAction,
        now_unix_secs: i64,
    ) -> Result<SpaceGrant, AuthorityError> {
        let role = self
            .catalog
            .role_for(principal, space, now_unix_secs)?
            .ok_or(AuthorityError::Unauthorized)?;
        authorize_action(actor, role, space, action)?;
        let authority = self
            .catalog
            .authority_snapshot_for(principal, space, now_unix_secs)?;
        Ok(SpaceGrant::new(space.space.clone(), role, authority))
    }

    fn begin_revocation(&self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.revoking = true;
        while state.active_leases != 0 {
            state = self
                .gate
                .drained
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    fn finish_revocation(&self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.epoch = state.epoch.saturating_add(1);
        state.revoking = false;
        self.gate.drained.notify_all();
    }

    fn release_lease(&self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active_leases = state.active_leases.saturating_sub(1);
        if state.active_leases == 0 {
            self.gate.drained.notify_all();
        }
    }

    fn current_epoch(&self) -> u64 {
        self.gate
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .epoch
    }
}

/// RAII authority lease retained through a read, graph transaction, or a
/// later publication boundary.  `reauthorize` is required before publication
/// of long-running work.
#[derive(Debug)]
pub(crate) struct AuthorizationLease {
    principal: PrincipalId,
    actor: ActorKind,
    space: CatalogSpace,
    grant: SpaceGrant,
    epoch: u64,
    service: AuthorizationService,
}

impl AuthorizationLease {
    #[must_use]
    pub(crate) fn grant(&self) -> &SpaceGrant {
        &self.grant
    }

    #[must_use]
    pub(crate) const fn authority(&self) -> AuthoritySnapshot {
        self.grant.authority()
    }

    /// Revalidate the live catalog lease and translate it into the graph's
    /// review capability. Production proposal transitions use this seam
    /// rather than constructing review authority from caller-supplied fields.
    pub(crate) fn review_authorization(
        &self,
        now_unix_secs: i64,
    ) -> Result<openmemory_graph::ReviewAuthorization, AuthorityError> {
        let fresh = self.reauthorize(AuthorizedAction::Review, now_unix_secs)?;
        openmemory_graph::ReviewAuthorization::new(
            self.actor,
            self.principal.to_string(),
            fresh.role(),
            fresh.authority(),
        )
        .map_err(|_| AuthorityError::Unauthorized)
    }

    /// Recheck role and every relevant authority generation immediately before
    /// publication.  A generation change is fail-closed even if a role later
    /// happens to be granted again.
    pub(crate) fn reauthorize(
        &self,
        action: AuthorizedAction,
        now_unix_secs: i64,
    ) -> Result<SpaceGrant, AuthorityError> {
        if self.service.current_epoch() != self.epoch {
            return Err(AuthorityError::StaleLease);
        }
        let fresh = self.service.grant_for(
            &self.principal,
            self.actor,
            &self.space,
            action,
            now_unix_secs,
        )?;
        if fresh.authority() != self.grant.authority() {
            return Err(AuthorityError::StaleLease);
        }
        Ok(fresh)
    }
}

impl Drop for AuthorizationLease {
    fn drop(&mut self) {
        self.service.release_lease();
    }
}

fn authorize_action(
    actor: ActorKind,
    role: SpaceRole,
    space: &CatalogSpace,
    action: AuthorizedAction,
) -> Result<(), AuthorityError> {
    match action {
        AuthorizedAction::Read
            if matches!(
                actor,
                ActorKind::Human | ActorKind::Agent | ActorKind::System
            ) =>
        {
            Ok(())
        }
        AuthorizedAction::PersonalWrite
            if matches!(
                space.space.owner(),
                openmemory_core::space::SpaceOwner::User(_)
            ) && role.can_write() =>
        {
            Ok(())
        }
        AuthorizedAction::TeamProposal
            if matches!(
                space.space.owner(),
                openmemory_core::space::SpaceOwner::Team(_)
            ) && role.can_write() =>
        {
            Ok(())
        }
        AuthorizedAction::Review if matches!(actor, ActorKind::Human) && role.can_review() => {
            Ok(())
        }
        AuthorizedAction::Membership | AuthorizedAction::Promotion
            if matches!(actor, ActorKind::Human) && matches!(role, SpaceRole::Maintainer) =>
        {
            Ok(())
        }
        AuthorizedAction::Read => Err(AuthorityError::ActorForbidden),
        AuthorizedAction::PersonalWrite | AuthorizedAction::TeamProposal => {
            if !role.can_write() {
                Err(AuthorityError::RoleForbidden)
            } else {
                Err(AuthorityError::ActorForbidden)
            }
        }
        AuthorizedAction::Review | AuthorizedAction::Membership | AuthorizedAction::Promotion => {
            if matches!(actor, ActorKind::Human) {
                Err(AuthorityError::RoleForbidden)
            } else {
                Err(AuthorityError::ActorForbidden)
            }
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

pub(crate) fn constant_time_eq(expected: &[u8], provided: &[u8]) -> bool {
    if expected.len() != provided.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in expected.iter().zip(provided) {
        diff |= x ^ y;
    }
    diff == 0
}
