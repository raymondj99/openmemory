//! Recoverable catalog service for local semantic memory spaces.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use openmemory_core::config::Config;
use openmemory_core::space::{
    ActorKind, MemoryContext, PrincipalId, ProjectId, ReadSet, SpaceContext, SpaceGrant, SpaceId,
    SpaceOwner, SpaceRef, SpaceRole, TeamId,
};
use openmemory_engine::partition::DomainStore;
use openmemory_engine::space::{
    resolve_space_root, SpaceHandle, SpaceManifest, SPACE_MANIFEST_FILE,
    SPACE_MANIFEST_FORMAT_VERSION,
};
use thiserror::Error;

use crate::product_store::{
    CatalogSpace, CatalogSpaceState, NewCatalogSpace, ProductStore, ProductStoreError,
};

const LEGACY_ROOT_KEY: &str = "legacy-root";

/// A control-plane error which never exposes raw SQL details to callers.
#[derive(Debug, Error)]
pub enum SpaceServiceError {
    #[error("invalid space request: {0}")]
    Invalid(String),
    #[error("space was not found")]
    NotFound,
    #[error("space state conflict: {0}")]
    Conflict(String),
    #[error("space storage operation failed: {0}")]
    Storage(String),
}

impl From<ProductStoreError> for SpaceServiceError {
    fn from(error: ProductStoreError) -> Self {
        match error {
            ProductStoreError::InvalidInput(message) => Self::Invalid(message),
            ProductStoreError::NotFound(_) => Self::NotFound,
            ProductStoreError::Conflict(message) => Self::Conflict(message),
            other => Self::Storage(other.to_string()),
        }
    }
}

/// Input for recoverable catalog + manifest + graph-root creation.
#[derive(Debug, Clone)]
pub struct CreateSpace {
    pub profile: String,
    pub owner: SpaceOwner,
    pub context: SpaceContext,
    pub display_name: String,
    pub domain_count: usize,
}

/// Stable metadata exposed by the catalog service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceSummary {
    pub id: SpaceId,
    pub profile: String,
    pub owner: SpaceOwner,
    pub context: SpaceContext,
    pub display_name: String,
    pub state: String,
    pub domain_count: usize,
    pub catalog_generation: u64,
    pub root_key: String,
    pub manifest_hash: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub profile: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamSummary {
    pub id: TeamId,
    pub profile: String,
    pub display_name: String,
    pub authority_generation: u64,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipSummary {
    pub space_id: SpaceId,
    pub principal_id: PrincipalId,
    pub role: SpaceRole,
    pub authority_generation: u64,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub workspace_id: String,
    pub canonical_path: String,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadMode {
    Contextual,
    ProjectOnly,
    GlobalOnly,
    Explicit(Vec<SpaceId>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteSelection {
    Default,
    Personal,
    Team,
}

#[derive(Debug, Clone)]
pub struct ResolveContextRequest {
    pub principal: PrincipalId,
    pub actor_kind: ActorKind,
    pub profile: String,
    pub workspace: Option<PathBuf>,
    pub project: Option<ProjectId>,
    pub active_team: Option<TeamId>,
    pub read_mode: ReadMode,
    pub write_selection: WriteSelection,
}

/// Local authority and physical-root coordinator.
#[derive(Debug, Clone)]
pub struct LocalSpaceService {
    catalog: ProductStore,
    profile_root: PathBuf,
    config: Config,
}

impl LocalSpaceService {
    /// Open the product catalog and pin all new roots beneath `profile_root`.
    pub fn open(
        home: &Path,
        profile_root: &Path,
        config: Config,
    ) -> Result<Self, SpaceServiceError> {
        std::fs::create_dir_all(profile_root)
            .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        let profile_root = profile_root
            .canonicalize()
            .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        Ok(Self {
            catalog: ProductStore::open(home)?,
            profile_root,
            config,
        })
    }

    pub fn ensure_principal(
        &self,
        id: &PrincipalId,
        display_name: &str,
        now: i64,
    ) -> Result<(), SpaceServiceError> {
        self.catalog
            .ensure_principal(id, display_name, now)
            .map_err(Into::into)
    }

    /// Adopt the existing profile `DomainStore` as personal-global without
    /// moving any graph or index artifact. Repeated calls reconcile the same
    /// catalog row and manifest, including an interrupted `creating` state.
    pub fn bind_legacy_personal_global(
        &self,
        profile: &str,
        owner: &PrincipalId,
        display_name: &str,
        now: i64,
    ) -> Result<SpaceSummary, SpaceServiceError> {
        self.ensure_principal(owner, display_name, now)?;
        let existing = DomainStore::open_existing(&self.config, &self.profile_root)
            .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        let id = existing.space_id();
        let domain_count = existing.domains();
        drop(existing);
        let owner = SpaceOwner::User(owner.clone());

        if let Some(bound) = self
            .catalog
            .find_space(profile, &owner, SpaceContext::Global)?
        {
            if bound.id != id || bound.root_key != LEGACY_ROOT_KEY {
                return Err(SpaceServiceError::Conflict(
                    "personal-global catalog binding does not match the existing profile root"
                        .to_string(),
                ));
            }
            return match bound.state {
                CatalogSpaceState::Creating => {
                    self.materialize_creating_space(&bound, now).map(summary)
                }
                CatalogSpaceState::Active => {
                    self.verify_catalog_space(&bound)?;
                    Ok(summary(bound))
                }
                _ => Err(SpaceServiceError::Conflict(
                    "personal-global legacy binding is not active".to_string(),
                )),
            };
        }

        let row = self.catalog.insert_space_creating(&NewCatalogSpace {
            id,
            profile,
            owner: &owner,
            context: SpaceContext::Global,
            display_name,
            root_key: LEGACY_ROOT_KEY,
            domain_count,
            format_version: SPACE_MANIFEST_FORMAT_VERSION,
            manifest_hash: &[0_u8; 32],
            now,
        })?;
        self.materialize_creating_space(&row, now).map(summary)
    }

    /// Create a distinct physical root through `creating -> active`.
    pub fn create(
        &self,
        request: &CreateSpace,
        now: i64,
    ) -> Result<SpaceSummary, SpaceServiceError> {
        let id = SpaceId::new();
        let root_key = format!("spaces/{id}");
        let placeholder_hash = [0_u8; 32];
        let row = self.catalog.insert_space_creating(&NewCatalogSpace {
            id,
            profile: &request.profile,
            owner: &request.owner,
            context: request.context,
            display_name: &request.display_name,
            root_key: &root_key,
            domain_count: request.domain_count,
            format_version: SPACE_MANIFEST_FORMAT_VERSION,
            manifest_hash: &placeholder_hash,
            now,
        })?;

        match self.materialize_creating_space(&row, now) {
            Ok(active) => {
                if let SpaceOwner::Team(team) = &request.owner {
                    self.catalog
                        .copy_team_memberships_to_space(team, id, now)?;
                }
                Ok(summary(active))
            }
            Err(error) => {
                let _ = self.catalog.transition_space(
                    id,
                    CatalogSpaceState::Creating,
                    CatalogSpaceState::Error,
                    None,
                    now,
                );
                Err(error)
            }
        }
    }

    /// Retry a catalog row left in `creating` by a prior interruption.
    pub fn reconcile_creating(
        &self,
        id: SpaceId,
        now: i64,
    ) -> Result<SpaceSummary, SpaceServiceError> {
        let row = self
            .catalog
            .get_space(id)?
            .ok_or(SpaceServiceError::NotFound)?;
        if row.state != CatalogSpaceState::Creating {
            return Err(SpaceServiceError::Conflict(
                "only creating spaces can be reconciled".to_string(),
            ));
        }
        self.materialize_creating_space(&row, now).map(summary)
    }

    fn materialize_creating_space(
        &self,
        row: &CatalogSpace,
        now: i64,
    ) -> Result<CatalogSpace, SpaceServiceError> {
        let root = self.catalog_space_root(row)?;
        if row.root_key != LEGACY_ROOT_KEY {
            std::fs::create_dir_all(root.join("store"))
                .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        }
        let manifest = SpaceManifest::new(
            row.id,
            &row.profile,
            &row.owner,
            row.context,
            row.domain_count,
            row.created_at,
            &row.root_key,
        )
        .map_err(|error| SpaceServiceError::Invalid(error.to_string()))?;
        let manifest_path = root.join(SPACE_MANIFEST_FILE);
        if manifest_path.exists() {
            let existing = SpaceManifest::load(&manifest_path)
                .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
            existing
                .verify(row.id, &row.profile, &row.root_key, row.domain_count)
                .map_err(|error| SpaceServiceError::Conflict(error.to_string()))?;
        } else {
            manifest
                .write_atomic(&manifest_path)
                .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        }
        let store_root = if row.root_key == LEGACY_ROOT_KEY {
            root.clone()
        } else {
            root.join("store")
        };
        let _verified =
            DomainStore::open_scoped(&self.config, &store_root, row.domain_count, row.id)
                .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        let encoded = std::fs::read(&manifest_path)
            .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        let manifest_hash = blake3::hash(&encoded);
        self.catalog
            .transition_space(
                row.id,
                CatalogSpaceState::Creating,
                CatalogSpaceState::Active,
                Some(manifest_hash.as_bytes()),
                now,
            )
            .map_err(Into::into)
    }

    fn catalog_space_root(&self, row: &CatalogSpace) -> Result<PathBuf, SpaceServiceError> {
        if row.root_key == LEGACY_ROOT_KEY {
            return Ok(self.profile_root.clone());
        }
        let expected = format!("spaces/{}", row.id);
        if row.root_key != expected {
            return Err(SpaceServiceError::Conflict(
                "catalog root key does not match the opaque space ID".to_string(),
            ));
        }
        resolve_space_root(&self.profile_root, row.id)
            .map_err(|error| SpaceServiceError::Storage(error.to_string()))
    }

    fn verify_catalog_space(&self, row: &CatalogSpace) -> Result<(), SpaceServiceError> {
        let root = self.catalog_space_root(row)?;
        let manifest_path = root.join(SPACE_MANIFEST_FILE);
        let manifest = SpaceManifest::load(&manifest_path)
            .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        manifest
            .verify(row.id, &row.profile, &row.root_key, row.domain_count)
            .map_err(|error| SpaceServiceError::Conflict(error.to_string()))?;
        let encoded = std::fs::read(&manifest_path)
            .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        if row.manifest_hash != blake3::hash(&encoded).as_bytes() {
            return Err(SpaceServiceError::Conflict(
                "catalog manifest hash does not match the physical binding".to_string(),
            ));
        }
        let store_root = if row.root_key == LEGACY_ROOT_KEY {
            root
        } else {
            root.join("store")
        };
        DomainStore::open_scoped(&self.config, &store_root, row.domain_count, row.id)
            .map_err(|error| SpaceServiceError::Storage(error.to_string()))?;
        Ok(())
    }

    pub fn list(&self, profile: &str) -> Result<Vec<SpaceSummary>, SpaceServiceError> {
        Ok(self
            .catalog
            .list_spaces(profile)?
            .into_iter()
            .map(summary)
            .collect())
    }

    pub fn get(&self, id: SpaceId) -> Result<SpaceSummary, SpaceServiceError> {
        self.catalog
            .get_space(id)?
            .map(summary)
            .ok_or(SpaceServiceError::NotFound)
    }

    pub fn find(
        &self,
        profile: &str,
        owner: &SpaceOwner,
        context: SpaceContext,
    ) -> Result<Option<SpaceSummary>, SpaceServiceError> {
        Ok(self
            .catalog
            .find_space(profile, owner, context)?
            .map(summary))
    }

    pub fn rename(
        &self,
        id: SpaceId,
        display_name: &str,
        expected_catalog_generation: u64,
        now: i64,
    ) -> Result<SpaceSummary, SpaceServiceError> {
        self.catalog
            .rename_space(id, display_name, expected_catalog_generation, now)
            .map(summary)
            .map_err(Into::into)
    }

    pub fn close(&self, id: SpaceId, now: i64) -> Result<SpaceSummary, SpaceServiceError> {
        self.catalog
            .transition_space(
                id,
                CatalogSpaceState::Active,
                CatalogSpaceState::Closed,
                None,
                now,
            )
            .map(summary)
            .map_err(Into::into)
    }

    /// Mark a closed non-legacy space deleting. Physical removal belongs to a
    /// backup-confirmed admin job and is deliberately not hidden here.
    pub fn begin_delete(
        &self,
        id: SpaceId,
        backup_confirmed: bool,
        now: i64,
    ) -> Result<SpaceSummary, SpaceServiceError> {
        if !backup_confirmed {
            return Err(SpaceServiceError::Invalid(
                "deletion requires a verified backup or explicit no-backup confirmation"
                    .to_string(),
            ));
        }
        self.catalog
            .transition_space(
                id,
                CatalogSpaceState::Closed,
                CatalogSpaceState::Deleting,
                None,
                now,
            )
            .map(summary)
            .map_err(Into::into)
    }

    pub fn open_handle(&self, id: SpaceId) -> Result<Arc<SpaceHandle>, SpaceServiceError> {
        let row = self
            .catalog
            .get_space(id)?
            .ok_or(SpaceServiceError::NotFound)?;
        if row.state != CatalogSpaceState::Active {
            return Err(SpaceServiceError::Conflict(
                "space is not active".to_string(),
            ));
        }
        let root = self.catalog_space_root(&row)?;
        SpaceHandle::open(
            &self.config,
            &root,
            &row.profile,
            &row.root_key,
            id,
            row.domain_count,
        )
        .map(Arc::new)
        .map_err(|error| SpaceServiceError::Storage(error.to_string()))
    }

    pub fn create_team(
        &self,
        id: &TeamId,
        profile: &str,
        display_name: &str,
        now: i64,
    ) -> Result<(), SpaceServiceError> {
        self.catalog
            .create_team(id, profile, display_name, now)
            .map_err(Into::into)
    }

    pub fn list_teams(&self, profile: &str) -> Result<Vec<TeamSummary>, SpaceServiceError> {
        self.catalog
            .list_teams(profile)?
            .into_iter()
            .map(|row| {
                Ok(TeamSummary {
                    id: row.id,
                    profile: row.profile,
                    display_name: row.display_name,
                    authority_generation: row.authority_generation,
                    state: row.state,
                })
            })
            .collect()
    }

    pub fn list_team_memberships(
        &self,
        team: &TeamId,
    ) -> Result<Vec<MembershipSummary>, SpaceServiceError> {
        self.catalog
            .list_team_memberships(team)?
            .into_iter()
            .map(|row| {
                Ok(MembershipSummary {
                    space_id: row.space_id,
                    principal_id: row.principal_id,
                    role: row.role,
                    authority_generation: row.authority_generation,
                    expires_at: row.expires_at,
                })
            })
            .collect()
    }

    pub fn grant_team_membership(
        &self,
        team: &TeamId,
        principal: &PrincipalId,
        role: SpaceRole,
        expires_at: Option<i64>,
        expected_generation: u64,
        now: i64,
    ) -> Result<u64, SpaceServiceError> {
        self.catalog
            .grant_team_membership(
                team,
                principal,
                role,
                expires_at,
                expected_generation,
                now,
            )
            .map_err(Into::into)
    }

    pub fn revoke_team_membership(
        &self,
        team: &TeamId,
        principal: &PrincipalId,
        now: i64,
    ) -> Result<u64, SpaceServiceError> {
        self.catalog
            .revoke_team_membership(team, principal, now)
            .map_err(Into::into)
    }

    pub fn grant(
        &self,
        space: SpaceId,
        principal: &PrincipalId,
        role: SpaceRole,
        expires_at: Option<i64>,
        now: i64,
    ) -> Result<u64, SpaceServiceError> {
        self.catalog
            .grant_membership(space, principal, role, expires_at, now)
            .map_err(Into::into)
    }

    pub fn revoke(
        &self,
        space: SpaceId,
        principal: &PrincipalId,
        now: i64,
    ) -> Result<(), SpaceServiceError> {
        self.catalog
            .revoke_membership(space, principal, now)
            .map_err(Into::into)
    }

    pub fn authorize(
        &self,
        space: SpaceId,
        principal: &PrincipalId,
        now: i64,
    ) -> Result<Option<(SpaceRole, u64)>, SpaceServiceError> {
        let row = self
            .catalog
            .get_space(space)?
            .ok_or(SpaceServiceError::NotFound)?;
        self.catalog
            .authorize_space(&row, principal, now)
            .map_err(Into::into)
    }

    pub fn create_project(
        &self,
        id: ProjectId,
        profile: &str,
        display_name: &str,
        now: i64,
    ) -> Result<(), SpaceServiceError> {
        self.catalog
            .create_project(id, profile, display_name, now)
            .map_err(Into::into)
    }

    pub fn list_projects(&self, profile: &str) -> Result<Vec<ProjectSummary>, SpaceServiceError> {
        self.catalog
            .list_projects(profile)?
            .into_iter()
            .map(|row| {
                Ok(ProjectSummary {
                    id: row.id,
                    profile: row.profile,
                    display_name: row.display_name,
                })
            })
            .collect()
    }

    pub fn map_workspace(
        &self,
        profile: &str,
        workspace: &Path,
        project: ProjectId,
        vcs_fingerprint: Option<&str>,
        now: i64,
    ) -> Result<ProjectId, SpaceServiceError> {
        self.catalog
            .map_workspace(profile, workspace, project, vcs_fingerprint, now)
            .map_err(Into::into)
    }

    pub fn resolve_workspace(
        &self,
        profile: &str,
        workspace: &Path,
    ) -> Result<Option<ProjectId>, SpaceServiceError> {
        self.catalog
            .resolve_workspace(profile, workspace)
            .map_err(Into::into)
    }

    pub fn workspace_mapping(
        &self,
        profile: &str,
        workspace: &Path,
    ) -> Result<Option<WorkspaceSummary>, SpaceServiceError> {
        self.catalog
            .workspace_mapping(profile, workspace)?
            .map(|row| {
                Ok(WorkspaceSummary {
                    workspace_id: row.workspace_id,
                    canonical_path: row.canonical_path,
                    project_id: row.project_id,
                })
            })
            .transpose()
    }

    pub fn mint_context_capability(
        &self,
        context: &MemoryContext,
        bearer_generation: u64,
        now: i64,
        ttl_secs: i64,
    ) -> Result<String, SpaceServiceError> {
        self.catalog
            .mint_context_capability(context, bearer_generation, now, ttl_secs)
            .map_err(Into::into)
    }

    pub fn load_context_capability(
        &self,
        plaintext: &str,
        bearer_generation: u64,
        now: i64,
    ) -> Result<MemoryContext, SpaceServiceError> {
        let context = self
            .catalog
            .load_context_capability(plaintext, bearer_generation, now)?;
        let mut generations = Vec::with_capacity(context.read_set.len());
        for grant in context.read_set.iter() {
            let space = self
                .catalog
                .get_space(grant.space.id)?
                .ok_or(SpaceServiceError::NotFound)?;
            let (role, generation) = self
                .catalog
                .authorize_space(&space, &context.principal, now)?
                .ok_or(SpaceServiceError::NotFound)?;
            if generation != grant.authority_generation || !role.allows(grant.role) {
                return Err(SpaceServiceError::NotFound);
            }
            generations.push((space.id, generation, space.catalog_generation));
        }
        if authorization_generation(&generations) != context.authorization_generation {
            return Err(SpaceServiceError::NotFound);
        }
        context
            .validate(false)
            .map_err(|error| SpaceServiceError::Invalid(error.to_string()))?;
        Ok(context)
    }

    pub fn revoke_context_capabilities(
        &self,
        principal: &PrincipalId,
        now: i64,
    ) -> Result<usize, SpaceServiceError> {
        self.catalog
            .revoke_context_capabilities(principal, now)
            .map_err(Into::into)
    }

    /// Resolve an immutable, authorization-generation-bound semantic context.
    pub fn resolve_context(
        &self,
        request: &ResolveContextRequest,
        now: i64,
    ) -> Result<MemoryContext, SpaceServiceError> {
        let project = if let Some(project) = request.project {
            Some(project)
        } else {
            request
                .workspace
                .as_deref()
                .map(|path| self.catalog.resolve_workspace(&request.profile, path))
                .transpose()?
                .flatten()
        };
        let personal = SpaceOwner::User(request.principal.clone());
        let team = request.active_team.clone().map(SpaceOwner::Team);

        let mut ordered = Vec::<CatalogSpace>::new();
        let mut push =
            |owner: &SpaceOwner, context: SpaceContext| -> Result<(), SpaceServiceError> {
                if let Some(space) = self.catalog.find_space(&request.profile, owner, context)? {
                    if space.state == CatalogSpaceState::Active {
                        ordered.push(space);
                    }
                }
                Ok(())
            };
        match &request.read_mode {
            ReadMode::Contextual => {
                if let Some(project) = project {
                    push(&personal, SpaceContext::Project(project))?;
                    if let Some(team) = &team {
                        push(team, SpaceContext::Project(project))?;
                    }
                }
                push(&personal, SpaceContext::Global)?;
                if let Some(team) = &team {
                    push(team, SpaceContext::Global)?;
                }
            }
            ReadMode::ProjectOnly => {
                let project = project.ok_or_else(|| {
                    SpaceServiceError::Invalid(
                        "project-only context requires a mapped workspace".to_string(),
                    )
                })?;
                push(&personal, SpaceContext::Project(project))?;
                if let Some(team) = &team {
                    push(team, SpaceContext::Project(project))?;
                }
            }
            ReadMode::GlobalOnly => {
                push(&personal, SpaceContext::Global)?;
                if let Some(team) = &team {
                    push(team, SpaceContext::Global)?;
                }
            }
            ReadMode::Explicit(ids) => {
                if ids.is_empty() || ids.len() > openmemory_core::space::MAX_READ_SET {
                    return Err(SpaceServiceError::Invalid(
                        "explicit overlay requires 1..=4 spaces".to_string(),
                    ));
                }
                for id in ids {
                    let space = self
                        .catalog
                        .get_space(*id)?
                        .ok_or(SpaceServiceError::NotFound)?;
                    if space.profile != request.profile {
                        return Err(SpaceServiceError::NotFound);
                    }
                    ordered.push(space);
                }
            }
        }

        let mut grants = Vec::with_capacity(ordered.len());
        let mut generations = Vec::with_capacity(ordered.len());
        for space in &ordered {
            let (role, generation) = self
                .catalog
                .authorize_space(space, &request.principal, now)?
                .ok_or(SpaceServiceError::NotFound)?;
            grants.push(SpaceGrant {
                space: SpaceRef {
                    id: space.id,
                    owner: space.owner.clone(),
                    context: space.context,
                },
                role,
                authority_generation: generation,
            });
            generations.push((space.id, generation, space.catalog_generation));
        }
        let read_set =
            ReadSet::new(grants).map_err(|error| SpaceServiceError::Invalid(error.to_string()))?;
        let default_write = select_write(&read_set, request.write_selection)?;
        let authorization_generation = authorization_generation(&generations);
        let context = MemoryContext {
            principal: request.principal.clone(),
            actor_kind: request.actor_kind,
            profile: request.profile.clone(),
            project,
            active_team: request.active_team.clone(),
            read_set,
            default_write,
            authorization_generation,
        };
        context
            .validate(false)
            .map_err(|error| SpaceServiceError::Invalid(error.to_string()))?;
        Ok(context)
    }
}

fn select_write(
    read_set: &ReadSet,
    selection: WriteSelection,
) -> Result<SpaceId, SpaceServiceError> {
    read_set
        .iter()
        .find(|grant| {
            grant.role.allows(SpaceRole::Contributor)
                && match selection {
                    WriteSelection::Default => true,
                    WriteSelection::Personal => matches!(grant.space.owner, SpaceOwner::User(_)),
                    WriteSelection::Team => matches!(grant.space.owner, SpaceOwner::Team(_)),
                }
        })
        .map(|grant| grant.space.id)
        .ok_or_else(|| {
            SpaceServiceError::Invalid(
                "no selected read-space grants contributor write authority".to_string(),
            )
        })
}

fn authorization_generation(generations: &[(SpaceId, u64, u64)]) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"openmemory/context-authorization/v1\0");
    for (space, authority, catalog) in generations {
        hasher.update(space.to_string().as_bytes());
        hasher.update(&authority.to_le_bytes());
        hasher.update(&catalog.to_le_bytes());
    }
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..8]);
    (u64::from_le_bytes(bytes) & i64::MAX as u64).max(1)
}

fn summary(row: CatalogSpace) -> SpaceSummary {
    SpaceSummary {
        id: row.id,
        profile: row.profile,
        owner: row.owner,
        context: row.context,
        display_name: row.display_name,
        state: row.state.as_str().to_string(),
        domain_count: row.domain_count,
        catalog_generation: row.catalog_generation,
        root_key: row.root_key,
        manifest_hash: row.manifest_hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_core::space::ActorKind;

    fn service() -> (LocalSpaceService, tempfile::TempDir, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let service =
            LocalSpaceService::open(home.path(), profile.path(), Config::default()).unwrap();
        (service, home, profile)
    }

    #[test]
    fn creation_is_catalog_manifest_and_store_atomic_state_machine() {
        let (service, _home, profile) = service();
        let owner = SpaceOwner::User("local:owner".parse().unwrap());
        let created = service
            .create(
                &CreateSpace {
                    profile: "default".to_string(),
                    owner: owner.clone(),
                    context: SpaceContext::Global,
                    display_name: "Personal".to_string(),
                    domain_count: 2,
                },
                10,
            )
            .unwrap();
        assert_eq!(created.state, "active");
        let root = profile.path().join("spaces").join(created.id.to_string());
        assert!(root.join(SPACE_MANIFEST_FILE).is_file());
        let handle = service.open_handle(created.id).unwrap();
        assert_eq!(handle.id, created.id);
        assert_eq!(handle.domains().space_id(), created.id);
        assert_eq!(handle.domains().domains(), 2);
        assert_eq!(
            service
                .find("default", &owner, SpaceContext::Global)
                .unwrap()
                .unwrap()
                .id,
            created.id
        );
    }

    #[test]
    fn legacy_profile_is_bound_in_place_exactly_once() {
        let home = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let config = Config::default();
        let existing = DomainStore::open(&config, profile.path(), 1).unwrap();
        let before_id = existing.space_id();
        existing
            .remember(
                "Existing",
                openmemory_graph::EntityType::Concept,
                &[openmemory_graph::ObservationInput::new("preserved")],
                &[],
                "fixture",
            )
            .unwrap();
        drop(existing);

        let service = LocalSpaceService::open(home.path(), profile.path(), config).unwrap();
        let principal: PrincipalId = "local:installation".parse().unwrap();
        let first = service
            .bind_legacy_personal_global("default", &principal, "Personal global", 10)
            .unwrap();
        let second = service
            .bind_legacy_personal_global("default", &principal, "Personal global", 11)
            .unwrap();

        assert_eq!(first.id, before_id);
        assert_eq!(second.id, before_id);
        assert_eq!(service.list("default").unwrap().len(), 1);
        assert!(profile.path().join(SPACE_MANIFEST_FILE).is_file());
        let handle = service.open_handle(before_id).unwrap();
        assert!(handle.domains().get_entity("Existing").unwrap().is_some());
    }

    #[test]
    fn workspace_mapping_is_stable_and_capability_is_hashed() {
        let (service, _home, _profile) = service();
        let principal: PrincipalId = "local:user".parse().unwrap();
        service.ensure_principal(&principal, "User", 1).unwrap();
        let project = ProjectId::new();
        service
            .create_project(project, "default", "Project", 1)
            .unwrap();
        let workspace = tempfile::tempdir().unwrap();
        assert_eq!(
            service
                .map_workspace("default", workspace.path(), project, None, 2)
                .unwrap(),
            project
        );
        let space = service
            .create(
                &CreateSpace {
                    profile: "default".to_string(),
                    owner: SpaceOwner::User(principal.clone()),
                    context: SpaceContext::Project(project),
                    display_name: "Project memory".to_string(),
                    domain_count: 1,
                },
                3,
            )
            .unwrap();
        let context = service
            .resolve_context(
                &ResolveContextRequest {
                    principal,
                    actor_kind: ActorKind::Human,
                    profile: "default".to_string(),
                    workspace: Some(workspace.path().to_path_buf()),
                    project: None,
                    active_team: None,
                    read_mode: ReadMode::ProjectOnly,
                    write_selection: WriteSelection::Default,
                },
                4,
            )
            .unwrap();
        assert_eq!(context.default_write, space.id);
        assert_eq!(context.project, Some(project));
        let expected = context.clone();
        assert_eq!(
            expected,
            MemoryContext {
                principal: expected.principal.clone(),
                actor_kind: ActorKind::Human,
                profile: "default".to_string(),
                project: Some(project),
                active_team: None,
                read_set: expected.read_set.clone(),
                default_write: space.id,
                authorization_generation: expected.authorization_generation,
            }
        );
        let token = service
            .mint_context_capability(&context, 7, 10, 900)
            .unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(
            service.load_context_capability(&token, 7, 11).unwrap(),
            context
        );
        assert!(service.load_context_capability(&token, 8, 11).is_err());
    }
}
