//! Product space, project, team, grant, and capability row ownership.
//!
//! This module is the only place that translates validated core identity
//! types into product SQLite rows.  It deliberately stores canonical strings
//! and closed-world tags, never display paths or caller-selected roots.

// These catalog operations are private service contracts until the audited
// human-admin surfaces land in Phase 5.
#![allow(dead_code)]

use std::str::FromStr;

use blake3::Hasher;
use openmemory_core::space::{
    ActorKind, AuthoritySnapshot, MemoryContext, PathPlatform, PrincipalId, ProfileName, ProjectId,
    SpaceContext, SpaceId, SpaceOwner, SpaceRef, SpaceRole, TeamId, WorkspacePathKey,
};
use rusqlite::{params, OptionalExtension, Transaction};

use super::{ProductStore, ProductStoreError};

const CATALOG_GENERATION_KEY: &str = "catalog_generation";
const CREDENTIAL_GENERATION_KEY: &str = "credential_generation";
const INSTALLATION_PRINCIPAL_KEY: &str = "installation_principal";
const MAX_LABEL_BYTES: usize = 256;
const MAX_CAPABILITY_BYTES: usize = 64 * 1024;

/// The stable root selector persisted in the product catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RootKey {
    LegacyRoot,
    Space(SpaceId),
}

impl RootKey {
    pub(crate) fn as_str(&self) -> String {
        match self {
            Self::LegacyRoot => "legacy-root".to_owned(),
            Self::Space(id) => format!("space:{id}"),
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, ProductStoreError> {
        if value == "legacy-root" {
            return Ok(Self::LegacyRoot);
        }
        let Some(id) = value.strip_prefix("space:") else {
            return Err(ProductStoreError::Corrupt(format!(
                "unknown catalog root key {value:?}"
            )));
        };
        Ok(Self::Space(SpaceId::from_str(id).map_err(|error| {
            ProductStoreError::Corrupt(format!("invalid space root key: {error}"))
        })?))
    }
}

/// Closed catalog lifecycle state.  Deletion is retained as a row state so a
/// closed root can never be silently reused by a new space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogSpaceState {
    Creating,
    Active,
    Closed,
    Deleting,
    Error,
}

impl CatalogSpaceState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Active => "active",
            Self::Closed => "closed",
            Self::Deleting => "deleting",
            Self::Error => "error",
        }
    }

    fn parse(value: &str) -> Result<Self, ProductStoreError> {
        match value {
            "creating" => Ok(Self::Creating),
            "active" => Ok(Self::Active),
            "closed" => Ok(Self::Closed),
            "deleting" => Ok(Self::Deleting),
            "error" => Ok(Self::Error),
            _ => Err(ProductStoreError::Corrupt(format!(
                "unknown memory-space state {value:?}"
            ))),
        }
    }
}

/// The validated catalog facts required before a registry may derive a root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogSpace {
    pub(crate) space: SpaceRef,
    pub(crate) profile: ProfileName,
    pub(crate) root_key: RootKey,
    pub(crate) domain_count: u8,
    pub(crate) manifest_hash: String,
    pub(crate) state: CatalogSpaceState,
    pub(crate) catalog_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityRecord {
    pub(crate) context: MemoryContext,
    pub(crate) expires_at_unix_secs: i64,
}

impl ProductStore {
    /// Load the random installation principal, creating it once in the same
    /// control plane that owns every later grant.  The opaque ID is not a
    /// filesystem component and is never derived from a profile or path.
    pub(crate) fn ensure_installation_principal(
        &self,
        now_unix_secs: i64,
    ) -> Result<PrincipalId, ProductStoreError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        if let Some(value) = meta_value(&tx, INSTALLATION_PRINCIPAL_KEY)? {
            let principal = PrincipalId::from_str(&value).map_err(|error| {
                ProductStoreError::Corrupt(format!("invalid installation principal: {error}"))
            })?;
            tx.commit()?;
            return Ok(principal);
        }

        let principal = PrincipalId::new(format!("local:{}", SpaceId::new()))
            .map_err(|error| ProductStoreError::InvalidValue(error.to_string()))?;
        tx.execute(
            "INSERT INTO local_principals(
                 id, display_label, state, generation, created_at_unix_secs, updated_at_unix_secs
             ) VALUES(?1, ?2, 'active', 1, ?3, ?3)",
            params![principal.as_str(), "This installation", now_unix_secs],
        )?;
        set_meta_value(&tx, INSTALLATION_PRINCIPAL_KEY, principal.as_str())?;
        set_meta_value(&tx, CREDENTIAL_GENERATION_KEY, "1")?;
        if meta_value(&tx, CATALOG_GENERATION_KEY)?.is_none() {
            set_meta_value(&tx, CATALOG_GENERATION_KEY, "0")?;
        }
        tx.commit()?;
        Ok(principal)
    }

    pub(crate) fn create_team(
        &self,
        profile: &ProfileName,
        label: &str,
        now_unix_secs: i64,
    ) -> Result<TeamId, ProductStoreError> {
        validate_label(label)?;
        let team = TeamId::new(format!("team:{}", SpaceId::new()))
            .map_err(|error| ProductStoreError::InvalidValue(error.to_string()))?;
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO local_teams(
                 id, profile, label, state, authority_generation, created_at_unix_secs, updated_at_unix_secs
             ) VALUES(?1, ?2, ?3, 'active', 1, ?4, ?4)",
            params![team.as_str(), profile.as_str(), label, now_unix_secs],
        )?;
        bump_catalog_generation(&tx)?;
        tx.commit()?;
        Ok(team)
    }

    pub(crate) fn grant_team_role(
        &self,
        team: &TeamId,
        principal: &PrincipalId,
        role: SpaceRole,
        expires_at_unix_secs: Option<i64>,
        now_unix_secs: i64,
    ) -> Result<(), ProductStoreError> {
        if expires_at_unix_secs.is_some_and(|expiry| expiry <= now_unix_secs) {
            return Err(ProductStoreError::InvalidValue(
                "membership expiry must be in the future".to_owned(),
            ));
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let team_active = tx
            .query_row(
                "SELECT state = 'active' FROM local_teams WHERE id = ?1",
                params![team.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .ok_or_else(|| ProductStoreError::InvalidValue("unknown team".to_owned()))?;
        let principal_active = tx
            .query_row(
                "SELECT state = 'active' FROM local_principals WHERE id = ?1",
                params![principal.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .ok_or_else(|| ProductStoreError::InvalidValue("unknown principal".to_owned()))?;
        if !team_active || !principal_active {
            return Err(ProductStoreError::InvalidValue(
                "team and principal must both be active".to_owned(),
            ));
        }
        tx.execute(
            "INSERT INTO team_memberships(
                 team_id, principal_id, role, authority_generation, expires_at_unix_secs,
                 created_at_unix_secs, updated_at_unix_secs
             ) VALUES(?1, ?2, ?3, 1, ?4, ?5, ?5)
             ON CONFLICT(team_id, principal_id) DO UPDATE SET
                 role = excluded.role,
                 authority_generation = team_memberships.authority_generation + 1,
                 expires_at_unix_secs = excluded.expires_at_unix_secs,
                 updated_at_unix_secs = excluded.updated_at_unix_secs",
            params![
                team.as_str(),
                principal.as_str(),
                role_tag(role),
                expires_at_unix_secs,
                now_unix_secs
            ],
        )?;
        tx.execute(
            "UPDATE local_teams SET authority_generation = authority_generation + 1,
             updated_at_unix_secs = ?2 WHERE id = ?1",
            params![team.as_str(), now_unix_secs],
        )?;
        bump_catalog_generation(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Revoke a membership in one transaction with its generation updates.
    pub(crate) fn revoke_team_member(
        &self,
        team: &TeamId,
        principal: &PrincipalId,
        now_unix_secs: i64,
    ) -> Result<bool, ProductStoreError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let removed = tx.execute(
            "DELETE FROM team_memberships WHERE team_id = ?1 AND principal_id = ?2",
            params![team.as_str(), principal.as_str()],
        )?;
        if removed == 1 {
            tx.execute(
                "UPDATE local_teams SET authority_generation = authority_generation + 1,
                 updated_at_unix_secs = ?2 WHERE id = ?1",
                params![team.as_str(), now_unix_secs],
            )?;
            bump_catalog_generation(&tx)?;
        }
        tx.commit()?;
        Ok(removed == 1)
    }

    pub(crate) fn create_project(
        &self,
        profile: &ProfileName,
        label: &str,
        now_unix_secs: i64,
    ) -> Result<ProjectId, ProductStoreError> {
        validate_label(label)?;
        let project = ProjectId::new();
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO projects(id, profile, label, created_at_unix_secs, updated_at_unix_secs)
             VALUES(?1, ?2, ?3, ?4, ?4)",
            params![project.to_string(), profile.as_str(), label, now_unix_secs],
        )?;
        bump_catalog_generation(&tx)?;
        tx.commit()?;
        Ok(project)
    }

    /// Persist a lossless workspace key.  The caller must construct it before
    /// this method; no display rendering or ambient working-directory lookup
    /// occurs inside the catalog.
    pub(crate) fn map_workspace(
        &self,
        profile: &ProfileName,
        workspace: &WorkspacePathKey,
        project: ProjectId,
        vcs_fingerprint: Option<&str>,
        now_unix_secs: i64,
    ) -> Result<(), ProductStoreError> {
        if vcs_fingerprint.is_some_and(|value| value.len() > MAX_LABEL_BYTES) {
            return Err(ProductStoreError::InvalidValue(
                "workspace VCS fingerprint exceeds its bound".to_owned(),
            ));
        }
        let workspace_id = workspace_row_id(profile, workspace);
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let exists = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1 AND profile = ?2)",
            params![project.to_string(), profile.as_str()],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(ProductStoreError::InvalidValue(
                "workspace project is absent from this profile".to_owned(),
            ));
        }
        tx.execute(
            "INSERT INTO workspace_projects(
                 workspace_id, profile, path_platform, normalizer_version, canonical_path,
                 project_id, vcs_fingerprint, state, created_at_unix_secs, updated_at_unix_secs
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?8)
             ON CONFLICT(profile, canonical_path) DO UPDATE SET
                 workspace_id = excluded.workspace_id,
                 path_platform = excluded.path_platform,
                 normalizer_version = excluded.normalizer_version,
                 project_id = excluded.project_id,
                 vcs_fingerprint = excluded.vcs_fingerprint,
                 state = 'active',
                 updated_at_unix_secs = excluded.updated_at_unix_secs",
            params![
                workspace_id,
                profile.as_str(),
                platform_tag(workspace.platform()),
                i64::from(workspace.normalizer_version()),
                workspace.path_text(),
                project.to_string(),
                vcs_fingerprint,
                now_unix_secs
            ],
        )?;
        bump_catalog_generation(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn project_for_workspace(
        &self,
        profile: &ProfileName,
        workspace: &WorkspacePathKey,
    ) -> Result<Option<ProjectId>, ProductStoreError> {
        let conn = self.connect()?;
        let row = conn
            .query_row(
                "SELECT project_id FROM workspace_projects
                 WHERE profile = ?1 AND canonical_path = ?2 AND path_platform = ?3
                   AND normalizer_version = ?4 AND state = 'active'",
                params![
                    profile.as_str(),
                    workspace.path_text(),
                    platform_tag(workspace.platform()),
                    i64::from(workspace.normalizer_version())
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        row.map(|id| {
            ProjectId::from_str(&id)
                .map_err(|error| ProductStoreError::Corrupt(format!("invalid project ID: {error}")))
        })
        .transpose()
    }

    // These independent validated facts form the durable catalog transaction;
    // keeping them explicit prevents callers from smuggling an unvalidated
    // `CatalogSpace` as if it were already persisted.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_space(
        &self,
        space: &SpaceRef,
        profile: &ProfileName,
        root_key: &RootKey,
        domain_count: u8,
        manifest_hash: &str,
        state: CatalogSpaceState,
        now_unix_secs: i64,
    ) -> Result<CatalogSpace, ProductStoreError> {
        if !(1..=64).contains(&domain_count) {
            return Err(ProductStoreError::InvalidValue(
                "space domain count must be 1..=64".to_owned(),
            ));
        }
        if !is_blake3_digest(manifest_hash) {
            return Err(ProductStoreError::InvalidValue(
                "space manifest hash must be canonical blake3".to_owned(),
            ));
        }
        let (owner_kind, owner_id) = owner_columns(space.owner());
        let (context_kind, project_key) = context_columns(space.context());
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let generation = bump_catalog_generation(&tx)?;
        tx.execute(
            "INSERT INTO memory_spaces(
                 id, profile, owner_kind, owner_id, context_kind, project_key, root_key,
                 domain_count, manifest_hash, state, catalog_generation,
                 created_at_unix_secs, updated_at_unix_secs
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
            params![
                space.id().to_string(),
                profile.as_str(),
                owner_kind,
                owner_id,
                context_kind,
                project_key,
                root_key.as_str(),
                i64::from(domain_count),
                manifest_hash,
                state.as_str(),
                i64::try_from(generation).map_err(|_| {
                    ProductStoreError::InvalidValue("catalog generation overflow".to_owned())
                })?,
                now_unix_secs
            ],
        )?;
        tx.commit()?;
        Ok(CatalogSpace {
            space: space.clone(),
            profile: profile.clone(),
            root_key: root_key.clone(),
            domain_count,
            manifest_hash: manifest_hash.to_owned(),
            state,
            catalog_generation: generation,
        })
    }

    /// Bind the compatibility root to its already persisted UUID.  Repeating
    /// this after any pre-activation crash always returns the same row.
    pub(crate) fn bind_legacy_personal_global(
        &self,
        profile: &ProfileName,
        principal: &PrincipalId,
        existing_id: SpaceId,
        domain_count: u8,
        manifest_hash: &str,
        now_unix_secs: i64,
    ) -> Result<CatalogSpace, ProductStoreError> {
        if let Some(existing) = self.space_by_root(&RootKey::LegacyRoot)? {
            if existing.space.id() != existing_id
                || existing.profile != *profile
                || existing.space.owner() != &SpaceOwner::User(principal.clone())
                || existing.space.context() != &SpaceContext::Global
                || existing.domain_count != domain_count
                || existing.manifest_hash != manifest_hash
            {
                return Err(ProductStoreError::Corrupt(
                    "legacy root disagrees with its catalog binding".to_owned(),
                ));
            }
            return Ok(existing);
        }
        self.insert_space(
            &SpaceRef::new(
                existing_id,
                SpaceOwner::User(principal.clone()),
                SpaceContext::Global,
            ),
            profile,
            &RootKey::LegacyRoot,
            domain_count,
            manifest_hash,
            CatalogSpaceState::Creating,
            now_unix_secs,
        )
    }

    pub(crate) fn activate_space(
        &self,
        id: SpaceId,
        manifest_hash: &str,
        now_unix_secs: i64,
    ) -> Result<CatalogSpace, ProductStoreError> {
        if !is_blake3_digest(manifest_hash) {
            return Err(ProductStoreError::InvalidValue(
                "space manifest hash must be canonical blake3".to_owned(),
            ));
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let generation = bump_catalog_generation(&tx)?;
        let changed = tx.execute(
            "UPDATE memory_spaces
             SET state = 'active', manifest_hash = ?2, catalog_generation = ?3,
                 updated_at_unix_secs = ?4
             WHERE id = ?1 AND state IN ('creating', 'active')",
            params![
                id.to_string(),
                manifest_hash,
                i64::try_from(generation).map_err(|_| {
                    ProductStoreError::InvalidValue("catalog generation overflow".to_owned())
                })?,
                now_unix_secs
            ],
        )?;
        if changed != 1 {
            return Err(ProductStoreError::InvalidValue(
                "space cannot transition to active".to_owned(),
            ));
        }
        tx.commit()?;
        self.space_by_id(id)?.ok_or_else(|| {
            ProductStoreError::Corrupt("activated catalog space disappeared".to_owned())
        })
    }

    pub(crate) fn space_by_id(
        &self,
        id: SpaceId,
    ) -> Result<Option<CatalogSpace>, ProductStoreError> {
        let conn = self.connect()?;
        load_space(
            &conn,
            "SELECT id, profile, owner_kind, owner_id, context_kind, project_key, root_key,
                    domain_count, manifest_hash, state, catalog_generation
             FROM memory_spaces WHERE id = ?1",
            params![id.to_string()],
        )
    }

    pub(crate) fn space_by_root(
        &self,
        root_key: &RootKey,
    ) -> Result<Option<CatalogSpace>, ProductStoreError> {
        let conn = self.connect()?;
        load_space(
            &conn,
            "SELECT id, profile, owner_kind, owner_id, context_kind, project_key, root_key,
                    domain_count, manifest_hash, state, catalog_generation
             FROM memory_spaces WHERE root_key = ?1",
            params![root_key.as_str()],
        )
    }

    pub(crate) fn active_spaces_page(
        &self,
        profile: &ProfileName,
        after_id: Option<SpaceId>,
        limit: usize,
    ) -> Result<Vec<CatalogSpace>, ProductStoreError> {
        let limit = limit.clamp(1, 256);
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT id, profile, owner_kind, owner_id, context_kind, project_key, root_key,
                    domain_count, manifest_hash, state, catalog_generation
             FROM memory_spaces
             WHERE profile = ?1 AND state = 'active' AND (?2 IS NULL OR id > ?2)
             ORDER BY id ASC LIMIT ?3",
        )?;
        let mut rows = stmt.query(params![
            profile.as_str(),
            after_id.map(|id| id.to_string()),
            i64::try_from(limit).unwrap_or(256)
        ])?;
        let mut spaces = Vec::new();
        while let Some(row) = rows.next()? {
            spaces.push(catalog_space_from_row(row)?);
        }
        Ok(spaces)
    }

    pub(crate) fn role_for(
        &self,
        principal: &PrincipalId,
        space: &CatalogSpace,
        now_unix_secs: i64,
    ) -> Result<Option<SpaceRole>, ProductStoreError> {
        let conn = self.connect()?;
        let principal_active = conn
            .query_row(
                "SELECT state = 'active' FROM local_principals WHERE id = ?1",
                params![principal.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        if !principal_active {
            return Ok(None);
        }
        match space.space.owner() {
            SpaceOwner::User(owner) => Ok((owner == principal).then_some(SpaceRole::Maintainer)),
            SpaceOwner::Team(team) => conn
                .query_row(
                    "SELECT membership.role
                     FROM team_memberships AS membership
                     JOIN local_teams AS team ON team.id = membership.team_id
                     WHERE membership.team_id = ?1 AND membership.principal_id = ?2
                       AND team.state = 'active'
                       AND (membership.expires_at_unix_secs IS NULL
                            OR membership.expires_at_unix_secs > ?3)",
                    params![team.as_str(), principal.as_str(), now_unix_secs],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|tag| parse_role(&tag))
                .transpose(),
        }
    }

    pub(crate) fn authority_snapshot_for(
        &self,
        principal: &PrincipalId,
        space: &CatalogSpace,
        now_unix_secs: i64,
    ) -> Result<AuthoritySnapshot, ProductStoreError> {
        let conn = self.connect()?;
        let principal_generation = generation_for(
            &conn,
            "SELECT generation FROM local_principals WHERE id = ?1 AND state = 'active'",
            principal.as_str(),
        )?
        .ok_or_else(|| ProductStoreError::InvalidValue("principal is not active".to_owned()))?;
        let credential_generation = meta_generation(&conn, CREDENTIAL_GENERATION_KEY)?;
        let catalog_generation = meta_generation(&conn, CATALOG_GENERATION_KEY)?;
        let mut generations = vec![
            credential_generation,
            catalog_generation,
            space.catalog_generation,
            principal_generation,
        ];
        if let SpaceOwner::Team(team) = space.space.owner() {
            let team_generation = generation_for(
                &conn,
                "SELECT authority_generation FROM local_teams WHERE id = ?1 AND state = 'active'",
                team.as_str(),
            )?
            .ok_or_else(|| ProductStoreError::InvalidValue("team is not active".to_owned()))?;
            let membership_generation = conn
                .query_row(
                    "SELECT authority_generation FROM team_memberships
                     WHERE team_id = ?1 AND principal_id = ?2
                       AND (expires_at_unix_secs IS NULL OR expires_at_unix_secs > ?3)",
                    params![team.as_str(), principal.as_str(), now_unix_secs],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    ProductStoreError::InvalidValue("membership is absent".to_owned())
                })?;
            generations.push(team_generation);
            generations.push(to_generation(membership_generation)?);
        }
        AuthoritySnapshot::from_generations(1, &generations)
            .map_err(|error| ProductStoreError::InvalidValue(error.to_string()))
    }

    /// Store only a BLAKE3 digest of a high-entropy bearer.  The full typed
    /// context is validated by serde before persistence and revalidated on
    /// every lookup.
    pub(crate) fn issue_context_capability(
        &self,
        bearer: &[u8],
        context: &MemoryContext,
        expires_at_unix_secs: i64,
        now_unix_secs: i64,
    ) -> Result<(), ProductStoreError> {
        if bearer.len() < 16 || bearer.len() > 4096 || expires_at_unix_secs <= now_unix_secs {
            return Err(ProductStoreError::InvalidValue(
                "capability bearer or expiry is outside its bound".to_owned(),
            ));
        }
        let context_json = serde_json::to_string(context)?;
        if context_json.len() > MAX_CAPABILITY_BYTES {
            return Err(ProductStoreError::InvalidValue(
                "capability context exceeds its bound".to_owned(),
            ));
        }
        let token_hash = capability_hash(bearer);
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO context_capabilities(
                 token_hash, principal_id, actor_kind, profile, context_json, authority_digest,
                 expires_at_unix_secs, revoked_at_unix_secs, created_at_unix_secs
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)",
            params![
                token_hash,
                context.principal().as_str(),
                actor_tag(context.actor_kind()),
                context.profile().as_str(),
                context_json,
                context.authority().to_string(),
                expires_at_unix_secs,
                now_unix_secs
            ],
        )?;
        Ok(())
    }

    pub(crate) fn consume_context_capability(
        &self,
        bearer: &[u8],
        now_unix_secs: i64,
    ) -> Result<Option<CapabilityRecord>, ProductStoreError> {
        let conn = self.connect()?;
        let record = conn
            .query_row(
                "SELECT context_json, expires_at_unix_secs FROM context_capabilities
                 WHERE token_hash = ?1 AND revoked_at_unix_secs IS NULL
                   AND expires_at_unix_secs > ?2",
                params![capability_hash(bearer), now_unix_secs],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        record
            .map(|(context, expires_at_unix_secs)| {
                if context.len() > MAX_CAPABILITY_BYTES {
                    return Err(ProductStoreError::Corrupt(
                        "persisted capability context exceeds its bound".to_owned(),
                    ));
                }
                let context = serde_json::from_str(&context)?;
                Ok(CapabilityRecord {
                    context,
                    expires_at_unix_secs,
                })
            })
            .transpose()
    }

    pub(crate) fn revoke_context_capability(
        &self,
        bearer: &[u8],
        now_unix_secs: i64,
    ) -> Result<bool, ProductStoreError> {
        let conn = self.connect()?;
        let changed = conn.execute(
            "UPDATE context_capabilities SET revoked_at_unix_secs = ?2
             WHERE token_hash = ?1 AND revoked_at_unix_secs IS NULL",
            params![capability_hash(bearer), now_unix_secs],
        )?;
        Ok(changed == 1)
    }
}

fn load_space<P>(
    conn: &rusqlite::Connection,
    sql: &str,
    params: P,
) -> Result<Option<CatalogSpace>, ProductStoreError>
where
    P: rusqlite::Params,
{
    conn.query_row(sql, params, catalog_space_from_row)
        .optional()
        .map_err(Into::into)
}

fn catalog_space_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CatalogSpace> {
    let id: String = row.get(0)?;
    let profile: String = row.get(1)?;
    let owner_kind: String = row.get(2)?;
    let owner_id: String = row.get(3)?;
    let context_kind: String = row.get(4)?;
    let project_key: String = row.get(5)?;
    let root_key: String = row.get(6)?;
    let domain_count: i64 = row.get(7)?;
    let manifest_hash: String = row.get(8)?;
    let state: String = row.get(9)?;
    let catalog_generation: i64 = row.get(10)?;
    decode_catalog_space(
        &id,
        &profile,
        &owner_kind,
        &owner_id,
        &context_kind,
        &project_key,
        &root_key,
        domain_count,
        manifest_hash,
        &state,
        catalog_generation,
    )
    .map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

#[allow(clippy::too_many_arguments)]
fn decode_catalog_space(
    id: &str,
    profile: &str,
    owner_kind: &str,
    owner_id: &str,
    context_kind: &str,
    project_key: &str,
    root_key: &str,
    domain_count: i64,
    manifest_hash: String,
    state: &str,
    catalog_generation: i64,
) -> Result<CatalogSpace, ProductStoreError> {
    let id = SpaceId::from_str(id).map_err(|error| {
        ProductStoreError::Corrupt(format!("invalid catalog space ID: {error}"))
    })?;
    let profile = ProfileName::from_str(profile)
        .map_err(|error| ProductStoreError::Corrupt(format!("invalid catalog profile: {error}")))?;
    let owner = match owner_kind {
        "user" => SpaceOwner::User(PrincipalId::from_str(owner_id).map_err(|error| {
            ProductStoreError::Corrupt(format!("invalid catalog principal: {error}"))
        })?),
        "team" => SpaceOwner::Team(TeamId::from_str(owner_id).map_err(|error| {
            ProductStoreError::Corrupt(format!("invalid catalog team: {error}"))
        })?),
        _ => {
            return Err(ProductStoreError::Corrupt(
                "invalid catalog owner tag".to_owned(),
            ))
        }
    };
    let context = match context_kind {
        "global" if project_key.is_empty() => SpaceContext::Global,
        "project" => SpaceContext::Project(ProjectId::from_str(project_key).map_err(|error| {
            ProductStoreError::Corrupt(format!("invalid catalog project: {error}"))
        })?),
        _ => {
            return Err(ProductStoreError::Corrupt(
                "invalid catalog context".to_owned(),
            ))
        }
    };
    let domain_count = u8::try_from(domain_count)
        .ok()
        .filter(|count| (1..=64).contains(count))
        .ok_or_else(|| ProductStoreError::Corrupt("invalid catalog domain count".to_owned()))?;
    if !is_blake3_digest(&manifest_hash) {
        return Err(ProductStoreError::Corrupt(
            "invalid catalog manifest hash".to_owned(),
        ));
    }
    Ok(CatalogSpace {
        space: SpaceRef::new(id, owner, context),
        profile,
        root_key: RootKey::parse(root_key)?,
        domain_count,
        manifest_hash,
        state: CatalogSpaceState::parse(state)?,
        catalog_generation: to_generation(catalog_generation)?,
    })
}

fn owner_columns(owner: &SpaceOwner) -> (&'static str, String) {
    match owner {
        SpaceOwner::User(id) => ("user", id.as_str().to_owned()),
        SpaceOwner::Team(id) => ("team", id.as_str().to_owned()),
    }
}

fn context_columns(context: &SpaceContext) -> (&'static str, String) {
    match context {
        SpaceContext::Global => ("global", String::new()),
        SpaceContext::Project(id) => ("project", id.to_string()),
    }
}

fn role_tag(role: SpaceRole) -> &'static str {
    match role {
        SpaceRole::Reader => "reader",
        SpaceRole::Contributor => "contributor",
        SpaceRole::Reviewer => "reviewer",
        SpaceRole::Maintainer => "maintainer",
    }
}

fn parse_role(value: &str) -> Result<SpaceRole, ProductStoreError> {
    match value {
        "reader" => Ok(SpaceRole::Reader),
        "contributor" => Ok(SpaceRole::Contributor),
        "reviewer" => Ok(SpaceRole::Reviewer),
        "maintainer" => Ok(SpaceRole::Maintainer),
        _ => Err(ProductStoreError::Corrupt(format!(
            "unknown membership role {value:?}"
        ))),
    }
}

fn actor_tag(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

fn platform_tag(platform: PathPlatform) -> &'static str {
    match platform {
        PathPlatform::Linux => "linux",
        PathPlatform::MacOs => "mac_os",
        PathPlatform::Windows => "windows",
    }
}

fn workspace_row_id(profile: &ProfileName, workspace: &WorkspacePathKey) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"openmemory/workspace-row/v1");
    hasher.update(&(profile.as_str().len() as u64).to_le_bytes());
    hasher.update(profile.as_str().as_bytes());
    hasher.update(&(workspace.path_text().len() as u64).to_le_bytes());
    hasher.update(workspace.path_text().as_bytes());
    format!("workspace:{}", hasher.finalize().to_hex())
}

fn capability_hash(bearer: &[u8]) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"openmemory/context-capability/v1");
    hasher.update(&(bearer.len() as u64).to_le_bytes());
    hasher.update(bearer);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn is_blake3_digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn validate_label(label: &str) -> Result<(), ProductStoreError> {
    if label.is_empty() || label.len() > MAX_LABEL_BYTES || label.chars().any(char::is_control) {
        return Err(ProductStoreError::InvalidValue(
            "label must be bounded non-control UTF-8".to_owned(),
        ));
    }
    Ok(())
}

fn meta_value(conn: &Transaction<'_>, key: &str) -> Result<Option<String>, ProductStoreError> {
    conn.query_row(
        "SELECT value FROM product_meta WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn set_meta_value(conn: &Transaction<'_>, key: &str, value: &str) -> Result<(), ProductStoreError> {
    conn.execute(
        "INSERT INTO product_meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn bump_catalog_generation(conn: &Transaction<'_>) -> Result<u64, ProductStoreError> {
    let current = meta_value(conn, CATALOG_GENERATION_KEY)?
        .unwrap_or_else(|| "0".to_owned())
        .parse::<u64>()
        .map_err(|_| ProductStoreError::Corrupt("invalid catalog generation".to_owned()))?;
    let next = current
        .checked_add(1)
        .ok_or_else(|| ProductStoreError::InvalidValue("catalog generation overflow".to_owned()))?;
    set_meta_value(conn, CATALOG_GENERATION_KEY, &next.to_string())?;
    Ok(next)
}

fn meta_generation(conn: &rusqlite::Connection, key: &str) -> Result<u64, ProductStoreError> {
    let value = conn
        .query_row(
            "SELECT value FROM product_meta WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_else(|| "0".to_owned());
    value
        .parse::<u64>()
        .map_err(|_| ProductStoreError::Corrupt(format!("invalid {key}")))
}

fn generation_for(
    conn: &rusqlite::Connection,
    sql: &str,
    id: &str,
) -> Result<Option<u64>, ProductStoreError> {
    conn.query_row(sql, params![id], |row| row.get::<_, i64>(0))
        .optional()?
        .map(to_generation)
        .transpose()
}

fn to_generation(value: i64) -> Result<u64, ProductStoreError> {
    u64::try_from(value)
        .map_err(|_| ProductStoreError::Corrupt("negative authority generation".to_owned()))
}
