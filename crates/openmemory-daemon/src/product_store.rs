use std::path::{Path, PathBuf};
use std::time::Duration;

use openmemory_admin::{AdminEvent, AdminJob};
use openmemory_core::migrations::Migrator;
use openmemory_core::space::{
    ActorKind, MemoryContext, PrincipalId, ProjectId, SpaceContext, SpaceId, SpaceOwner, SpaceRole,
    TeamId,
};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const PRODUCT_DIR: &str = "product";
const PRODUCT_DB_FILE: &str = "product.sqlite";
pub(crate) const PRODUCT_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Error)]
pub(crate) enum ProductStoreError {
    #[error("product metadata filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("product metadata database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("product metadata JSON operation failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("product metadata schema version {found} is newer than supported version {supported}")]
    UnsupportedSchema { found: i64, supported: i64 },
    #[error("product metadata migration failed: {0}")]
    Migration(String),
    #[error("invalid product metadata input: {0}")]
    InvalidInput(String),
    #[error("product metadata object not found: {0}")]
    NotFound(String),
    #[error("product metadata conflict: {0}")]
    Conflict(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogSpaceState {
    Creating,
    Active,
    Closed,
    Deleting,
    Error,
}

impl CatalogSpaceState {
    pub(crate) fn as_str(self) -> &'static str {
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
            _ => Err(ProductStoreError::InvalidInput(
                "invalid catalog space state".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogSpace {
    pub id: SpaceId,
    pub profile: String,
    pub owner: SpaceOwner,
    pub context: SpaceContext,
    pub display_name: String,
    pub root_key: String,
    pub domain_count: usize,
    pub format_version: u32,
    pub manifest_hash: Vec<u8>,
    pub state: CatalogSpaceState,
    pub catalog_generation: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogProject {
    pub id: ProjectId,
    pub profile: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogTeam {
    pub id: TeamId,
    pub profile: String,
    pub display_name: String,
    pub authority_generation: u64,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogMembership {
    pub space_id: SpaceId,
    pub principal_id: PrincipalId,
    pub role: SpaceRole,
    pub authority_generation: u64,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogWorkspace {
    pub workspace_id: String,
    pub canonical_path: String,
    pub project_id: ProjectId,
}

pub(crate) struct NewCatalogSpace<'a> {
    pub id: SpaceId,
    pub profile: &'a str,
    pub owner: &'a SpaceOwner,
    pub context: SpaceContext,
    pub display_name: &'a str,
    pub root_key: &'a str,
    pub domain_count: usize,
    pub format_version: u32,
    pub manifest_hash: &'a [u8],
    pub now: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct ProductStore {
    path: PathBuf,
}

impl ProductStore {
    pub(crate) fn open(home: &Path) -> Result<Self, ProductStoreError> {
        let dir = home.join(PRODUCT_DIR);
        std::fs::create_dir_all(&dir)?;
        let store = Self {
            path: dir.join(PRODUCT_DB_FILE),
        };
        store.initialize()?;
        Ok(store)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn load_jobs(&self) -> Result<Vec<AdminJob>, ProductStoreError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT job_json FROM daemon_jobs
             ORDER BY created_at_unix_secs ASC, id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(serde_json::from_str(&row?)?);
        }
        Ok(jobs)
    }

    pub(crate) fn next_event_sequence(&self) -> Result<u64, ProductStoreError> {
        let conn = self.connect()?;
        let next = conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM daemon_events",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        Ok(next)
    }

    pub(crate) fn upsert_job(&self, job: &AdminJob) -> Result<(), ProductStoreError> {
        let conn = self.connect()?;
        let job_json = serde_json::to_string(job)?;
        let state = serde_json::to_string(&job.state)?;
        let kind = serde_json::to_string(&job.kind)?;
        conn.execute(
            "INSERT INTO daemon_jobs (
                 id, kind_json, state_json, profile, created_at_unix_secs,
                 updated_at_unix_secs, job_json
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                 kind_json = excluded.kind_json,
                 state_json = excluded.state_json,
                 profile = excluded.profile,
                 updated_at_unix_secs = excluded.updated_at_unix_secs,
                 job_json = excluded.job_json",
            params![
                job.id,
                kind,
                state,
                job.profile,
                job.created_at_unix_secs,
                job.finished_at_unix_secs
                    .or(job.started_at_unix_secs)
                    .unwrap_or(job.created_at_unix_secs),
                job_json,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn insert_event(&self, event: &AdminEvent) -> Result<(), ProductStoreError> {
        let conn = self.connect()?;
        let event_json = serde_json::to_string(event)?;
        let event_type = serde_json::to_string(&event.event_type)?;
        let job_id = event.job.as_ref().map(|job| job.id.as_str());
        conn.execute(
            "INSERT INTO daemon_events (
                 sequence, unix_secs, event_type_json, job_id, event_json
             )
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.sequence,
                event.unix_secs,
                event_type,
                job_id,
                event_json,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn events_after(
        &self,
        sequence: u64,
        limit: usize,
    ) -> Result<Vec<AdminEvent>, ProductStoreError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT event_json FROM daemon_events
             WHERE sequence > ?1
             ORDER BY sequence ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![sequence, limit as u64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(serde_json::from_str(&row?)?);
        }
        Ok(events)
    }

    pub(crate) fn ensure_principal(
        &self,
        id: &PrincipalId,
        display_name: &str,
        now: i64,
    ) -> Result<(), ProductStoreError> {
        validate_bounded("principal display name", display_name, 1, 256)?;
        self.connect()?.execute(
            "INSERT INTO local_principals(id, display_name, state, created_at, updated_at)
             VALUES (?1, ?2, 'active', ?3, ?3)
             ON CONFLICT(id) DO UPDATE SET
                display_name = excluded.display_name,
                updated_at = excluded.updated_at",
            params![id.to_string(), display_name, now],
        )?;
        Ok(())
    }

    pub(crate) fn insert_space_creating(
        &self,
        request: &NewCatalogSpace<'_>,
    ) -> Result<CatalogSpace, ProductStoreError> {
        validate_profile(request.profile)?;
        validate_bounded("space display name", request.display_name, 1, 256)?;
        validate_root_key(request.root_key)?;
        if request.domain_count == 0 || request.domain_count > 1024 {
            return Err(ProductStoreError::InvalidInput(
                "space domain count must be in 1..=1024".to_string(),
            ));
        }
        if request.manifest_hash.len() != 32 {
            return Err(ProductStoreError::InvalidInput(
                "manifest hash must be 32 bytes".to_string(),
            ));
        }
        let (owner_kind, owner_id) = encode_owner(request.owner);
        let (context_kind, project_key) = encode_context(request.context);
        let conn = self.connect()?;
        let result = conn.execute(
            "INSERT INTO memory_spaces(
                id, profile, owner_kind, owner_id, context_kind, project_key,
                display_name, root_key, domain_count, format_version, manifest_hash,
                state, catalog_generation, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                     'creating', 1, ?12, ?12)",
            params![
                request.id.to_string(),
                request.profile,
                owner_kind,
                owner_id,
                context_kind,
                project_key,
                request.display_name,
                request.root_key,
                request.domain_count as i64,
                request.format_version,
                request.manifest_hash,
                request.now,
            ],
        );
        match result {
            Ok(_) => self
                .get_space(request.id)?
                .ok_or_else(|| ProductStoreError::NotFound(request.id.to_string())),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(ProductStoreError::Conflict(
                    "space owner/context or root key already exists".to_string(),
                ))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn get_space(&self, id: SpaceId) -> Result<Option<CatalogSpace>, ProductStoreError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT id, profile, owner_kind, owner_id, context_kind, project_key,
                    display_name, root_key, domain_count, format_version, manifest_hash,
                    state, catalog_generation, created_at, updated_at
             FROM memory_spaces WHERE id = ?1",
            [id.to_string()],
            decode_catalog_space,
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn find_space(
        &self,
        profile: &str,
        owner: &SpaceOwner,
        context: SpaceContext,
    ) -> Result<Option<CatalogSpace>, ProductStoreError> {
        let (owner_kind, owner_id) = encode_owner(owner);
        let (context_kind, project_key) = encode_context(context);
        self.connect()?
            .query_row(
                "SELECT id, profile, owner_kind, owner_id, context_kind, project_key,
                        display_name, root_key, domain_count, format_version, manifest_hash,
                        state, catalog_generation, created_at, updated_at
                 FROM memory_spaces
                 WHERE profile=?1 AND owner_kind=?2 AND owner_id=?3
                   AND context_kind=?4 AND project_key=?5",
                params![profile, owner_kind, owner_id, context_kind, project_key],
                decode_catalog_space,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn list_spaces(
        &self,
        profile: &str,
    ) -> Result<Vec<CatalogSpace>, ProductStoreError> {
        let conn = self.connect()?;
        let mut statement = conn.prepare(
            "SELECT id, profile, owner_kind, owner_id, context_kind, project_key,
                    display_name, root_key, domain_count, format_version, manifest_hash,
                    state, catalog_generation, created_at, updated_at
             FROM memory_spaces WHERE profile=?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([profile], decode_catalog_space)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn transition_space(
        &self,
        id: SpaceId,
        expected: CatalogSpaceState,
        next: CatalogSpaceState,
        manifest_hash: Option<&[u8]>,
        now: i64,
    ) -> Result<CatalogSpace, ProductStoreError> {
        if let Some(hash) = manifest_hash {
            if hash.len() != 32 {
                return Err(ProductStoreError::InvalidInput(
                    "manifest hash must be 32 bytes".to_string(),
                ));
            }
        }
        let conn = self.connect()?;
        let changed = conn.execute(
            "UPDATE memory_spaces SET
                state=?1,
                manifest_hash=COALESCE(?2, manifest_hash),
                catalog_generation=catalog_generation+1,
                updated_at=?3
             WHERE id=?4 AND state=?5",
            params![
                next.as_str(),
                manifest_hash,
                now,
                id.to_string(),
                expected.as_str()
            ],
        )?;
        if changed != 1 {
            return Err(ProductStoreError::Conflict(format!(
                "space {id} is not {}",
                expected.as_str()
            )));
        }
        self.get_space(id)?
            .ok_or_else(|| ProductStoreError::NotFound(id.to_string()))
    }

    pub(crate) fn rename_space(
        &self,
        id: SpaceId,
        display_name: &str,
        expected_catalog_generation: u64,
        now: i64,
    ) -> Result<CatalogSpace, ProductStoreError> {
        validate_bounded("space display name", display_name, 1, 256)?;
        let changed = self.connect()?.execute(
            "UPDATE memory_spaces SET display_name=?1,
                catalog_generation=catalog_generation+1, updated_at=?2
             WHERE id=?3 AND state IN ('active','closed')
               AND catalog_generation=?4",
            params![
                display_name,
                now,
                id.to_string(),
                expected_catalog_generation
            ],
        )?;
        if changed != 1 {
            return Err(ProductStoreError::Conflict(
                "space is not renameable".to_string(),
            ));
        }
        self.get_space(id)?
            .ok_or_else(|| ProductStoreError::NotFound(id.to_string()))
    }

    pub(crate) fn create_team(
        &self,
        id: &TeamId,
        profile: &str,
        display_name: &str,
        now: i64,
    ) -> Result<(), ProductStoreError> {
        validate_profile(profile)?;
        validate_bounded("team display name", display_name, 1, 256)?;
        self.connect()?.execute(
            "INSERT INTO local_teams(
                id, profile, display_name, authority_generation, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, 'active', ?4, ?4)",
            params![id.to_string(), profile, display_name, now],
        )?;
        Ok(())
    }

    pub(crate) fn list_teams(
        &self,
        profile: &str,
    ) -> Result<Vec<CatalogTeam>, ProductStoreError> {
        let conn = self.connect()?;
        let mut statement = conn.prepare(
            "SELECT id, profile, display_name, authority_generation, state
             FROM local_teams WHERE profile=?1 ORDER BY display_name, id LIMIT 256",
        )?;
        let rows = statement
            .query_map([profile], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .map(|row| {
                let row = row?;
                Ok(CatalogTeam {
                    id: row.0.parse().map_err(|error| {
                        ProductStoreError::InvalidInput(format!(
                            "invalid stored team ID: {error}"
                        ))
                    })?,
                    profile: row.1,
                    display_name: row.2,
                    authority_generation: row.3,
                    state: row.4,
                })
            })
            .collect();
        rows
    }

    pub(crate) fn list_team_memberships(
        &self,
        team: &TeamId,
    ) -> Result<Vec<CatalogMembership>, ProductStoreError> {
        let conn = self.connect()?;
        let mut statement = conn.prepare(
            "SELECT membership.space_id, membership.principal_id, membership.role,
                    membership.authority_generation, membership.expires_at
             FROM space_memberships AS membership
             JOIN memory_spaces AS space ON space.id=membership.space_id
             WHERE space.owner_kind='team' AND space.owner_id=?1
             ORDER BY membership.principal_id, membership.space_id LIMIT 1024",
        )?;
        let rows = statement
            .query_map([team.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })?
            .map(|row| {
                let row = row?;
                Ok(CatalogMembership {
                    space_id: row.0.parse().map_err(|error| {
                        ProductStoreError::InvalidInput(format!(
                            "invalid stored membership space ID: {error}"
                        ))
                    })?,
                    principal_id: row.1.parse().map_err(|error| {
                        ProductStoreError::InvalidInput(format!(
                            "invalid stored membership principal ID: {error}"
                        ))
                    })?,
                    role: parse_role(&row.2).ok_or_else(|| {
                        ProductStoreError::InvalidInput(
                            "invalid stored membership role".to_string(),
                        )
                    })?,
                    authority_generation: row.3,
                    expires_at: row.4,
                })
            })
            .collect();
        rows
    }

    pub(crate) fn grant_team_membership(
        &self,
        team: &TeamId,
        principal: &PrincipalId,
        role: SpaceRole,
        expires_at: Option<i64>,
        expected_generation: u64,
        now: i64,
    ) -> Result<u64, ProductStoreError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: u64 = tx
            .query_row(
                "SELECT authority_generation FROM local_teams
                 WHERE id=?1 AND state='active'",
                [team.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| ProductStoreError::NotFound("team".to_string()))?;
        if current != expected_generation {
            return Err(ProductStoreError::Conflict(
                "team authority generation is stale".to_string(),
            ));
        }
        let next = current.saturating_add(1);
        let mut statement = tx.prepare(
            "SELECT id FROM memory_spaces
             WHERE owner_kind='team' AND owner_id=?1 AND state IN ('active','closed')",
        )?;
        let spaces = statement
            .query_map([team.to_string()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for space in &spaces {
            tx.execute(
                "INSERT INTO space_memberships(
                    space_id, principal_id, role, authority_generation,
                    expires_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(space_id, principal_id) DO UPDATE SET
                    role=excluded.role,
                    authority_generation=excluded.authority_generation,
                    expires_at=excluded.expires_at,
                    updated_at=excluded.updated_at",
                params![
                    space,
                    principal.to_string(),
                    role_name(role),
                    next,
                    expires_at,
                    now
                ],
            )?;
            tx.execute(
                "UPDATE memory_spaces SET catalog_generation=catalog_generation+1,
                    updated_at=?1 WHERE id=?2",
                params![now, space],
            )?;
        }
        tx.execute(
            "UPDATE local_teams SET authority_generation=?1, updated_at=?2 WHERE id=?3",
            params![next, now, team.to_string()],
        )?;
        tx.execute(
            "UPDATE context_capabilities SET revoked_at=?1
             WHERE principal_id=?2 AND revoked_at IS NULL",
            params![now, principal.to_string()],
        )?;
        tx.commit()?;
        Ok(next)
    }

    pub(crate) fn revoke_team_membership(
        &self,
        team: &TeamId,
        principal: &PrincipalId,
        now: i64,
    ) -> Result<u64, ProductStoreError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: u64 = tx
            .query_row(
                "SELECT authority_generation FROM local_teams
                 WHERE id=?1 AND state='active'",
                [team.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| ProductStoreError::NotFound("team".to_string()))?;
        let next = current.saturating_add(1);
        let changed = tx.execute(
            "DELETE FROM space_memberships
             WHERE principal_id=?1 AND space_id IN (
                SELECT id FROM memory_spaces
                WHERE owner_kind='team' AND owner_id=?2
             )",
            params![principal.to_string(), team.to_string()],
        )?;
        if changed == 0 {
            return Err(ProductStoreError::NotFound(
                "team membership".to_string(),
            ));
        }
        tx.execute(
            "UPDATE memory_spaces SET catalog_generation=catalog_generation+1,
                updated_at=?1
             WHERE owner_kind='team' AND owner_id=?2",
            params![now, team.to_string()],
        )?;
        tx.execute(
            "UPDATE local_teams SET authority_generation=?1, updated_at=?2 WHERE id=?3",
            params![next, now, team.to_string()],
        )?;
        tx.execute(
            "UPDATE context_capabilities SET revoked_at=?1
             WHERE principal_id=?2 AND revoked_at IS NULL",
            params![now, principal.to_string()],
        )?;
        tx.commit()?;
        Ok(next)
    }

    pub(crate) fn copy_team_memberships_to_space(
        &self,
        team: &TeamId,
        destination: SpaceId,
        now: i64,
    ) -> Result<(), ProductStoreError> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO space_memberships(
                space_id, principal_id, role, authority_generation,
                expires_at, created_at, updated_at)
             SELECT ?1, membership.principal_id, membership.role,
                    membership.authority_generation, membership.expires_at, ?2, ?2
             FROM space_memberships AS membership
             JOIN memory_spaces AS source ON source.id=membership.space_id
             WHERE source.owner_kind='team' AND source.owner_id=?3
             GROUP BY membership.principal_id
             ON CONFLICT(space_id, principal_id) DO NOTHING",
            params![destination.to_string(), now, team.to_string()],
        )?;
        Ok(())
    }

    pub(crate) fn grant_membership(
        &self,
        space_id: SpaceId,
        principal: &PrincipalId,
        role: SpaceRole,
        expires_at: Option<i64>,
        now: i64,
    ) -> Result<u64, ProductStoreError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let generation: u64 = tx.query_row(
            "SELECT COALESCE(MAX(authority_generation), 0) + 1
                 FROM space_memberships WHERE space_id=?1",
            [space_id.to_string()],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO space_memberships(
                space_id, principal_id, role, authority_generation,
                expires_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(space_id, principal_id) DO UPDATE SET
                role=excluded.role,
                authority_generation=excluded.authority_generation,
                expires_at=excluded.expires_at,
                updated_at=excluded.updated_at",
            params![
                space_id.to_string(),
                principal.to_string(),
                role_name(role),
                generation,
                expires_at,
                now
            ],
        )?;
        tx.execute(
            "UPDATE memory_spaces SET catalog_generation=catalog_generation+1,
                updated_at=?1 WHERE id=?2",
            params![now, space_id.to_string()],
        )?;
        tx.execute(
            "UPDATE context_capabilities SET revoked_at=?1
             WHERE principal_id=?2 AND revoked_at IS NULL",
            params![now, principal.to_string()],
        )?;
        tx.commit()?;
        Ok(generation)
    }

    pub(crate) fn authorize_space(
        &self,
        space: &CatalogSpace,
        principal: &PrincipalId,
        now: i64,
    ) -> Result<Option<(SpaceRole, u64)>, ProductStoreError> {
        if space.state != CatalogSpaceState::Active {
            return Ok(None);
        }
        if matches!(&space.owner, SpaceOwner::User(owner) if owner == principal) {
            return Ok(Some((
                SpaceRole::Maintainer,
                space.catalog_generation.max(1),
            )));
        }
        if matches!(space.owner, SpaceOwner::User(_)) {
            return Ok(None);
        }
        let row = self
            .connect()?
            .query_row(
                "SELECT membership.role, membership.authority_generation
                 FROM space_memberships AS membership
                 JOIN local_principals AS principal
                   ON principal.id = membership.principal_id
                 LEFT JOIN local_teams AS team
                   ON team.id = ?3
                 WHERE membership.space_id=?1 AND membership.principal_id=?2
                   AND principal.state='active'
                   AND (membership.expires_at IS NULL OR membership.expires_at>?4)
                   AND (team.id IS NULL OR team.state='active')",
                params![
                    space.id.to_string(),
                    principal.to_string(),
                    match &space.owner {
                        SpaceOwner::Team(team) => Some(team.to_string()),
                        SpaceOwner::User(_) => None,
                    },
                    now
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
            )
            .optional()?;
        row.map(|(role, generation)| {
            parse_role(&role)
                .map(|role| (role, generation))
                .ok_or_else(|| {
                    ProductStoreError::InvalidInput("invalid stored membership role".to_string())
                })
        })
        .transpose()
    }

    pub(crate) fn revoke_membership(
        &self,
        space_id: SpaceId,
        principal: &PrincipalId,
        now: i64,
    ) -> Result<(), ProductStoreError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "DELETE FROM space_memberships WHERE space_id=?1 AND principal_id=?2",
            params![space_id.to_string(), principal.to_string()],
        )?;
        if changed != 1 {
            return Err(ProductStoreError::NotFound("space membership".to_string()));
        }
        tx.execute(
            "UPDATE memory_spaces SET catalog_generation=catalog_generation+1,
                updated_at=?1 WHERE id=?2",
            params![now, space_id.to_string()],
        )?;
        tx.execute(
            "UPDATE context_capabilities SET revoked_at=?1
             WHERE principal_id=?2 AND revoked_at IS NULL",
            params![now, principal.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn create_project(
        &self,
        id: ProjectId,
        profile: &str,
        display_name: &str,
        now: i64,
    ) -> Result<(), ProductStoreError> {
        validate_profile(profile)?;
        validate_bounded("project display name", display_name, 1, 256)?;
        self.connect()?.execute(
            "INSERT INTO projects(id, profile, display_name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id.to_string(), profile, display_name, now],
        )?;
        Ok(())
    }

    pub(crate) fn list_projects(
        &self,
        profile: &str,
    ) -> Result<Vec<CatalogProject>, ProductStoreError> {
        let conn = self.connect()?;
        let mut statement = conn.prepare(
            "SELECT id, profile, display_name FROM projects
             WHERE profile=?1 ORDER BY display_name, id LIMIT 1024",
        )?;
        let rows = statement
            .query_map([profile], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .map(|row| {
                let row = row?;
                Ok(CatalogProject {
                    id: row.0.parse().map_err(|error| {
                        ProductStoreError::InvalidInput(format!(
                            "invalid stored project ID: {error}"
                        ))
                    })?,
                    profile: row.1,
                    display_name: row.2,
                })
            })
            .collect();
        rows
    }

    pub(crate) fn map_workspace(
        &self,
        profile: &str,
        workspace: &Path,
        project: ProjectId,
        vcs_fingerprint: Option<&str>,
        now: i64,
    ) -> Result<ProjectId, ProductStoreError> {
        let canonical = workspace.canonicalize()?;
        if !canonical.is_dir() {
            return Err(ProductStoreError::InvalidInput(
                "workspace must be an existing directory".to_string(),
            ));
        }
        let canonical_text = canonical.to_str().ok_or_else(|| {
            ProductStoreError::InvalidInput("workspace path is not UTF-8".to_string())
        })?;
        validate_bounded("workspace path", canonical_text, 1, 4096)?;
        let workspace_id = blake3::hash(
            [
                b"openmemory/workspace/v1\0".as_slice(),
                canonical_text.as_bytes(),
            ]
            .concat()
            .as_slice(),
        )
        .to_hex()
        .to_string();
        let conn = self.connect()?;
        if let Some(existing) = conn
            .query_row(
                "SELECT project_id FROM workspace_projects
                 WHERE profile=?1 AND canonical_path=?2",
                params![profile, canonical_text],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let existing_project: ProjectId = existing.parse().map_err(|error| {
                ProductStoreError::InvalidInput(format!(
                    "invalid stored workspace project ID: {error}"
                ))
            })?;
            if existing_project != project {
                conn.execute(
                    "UPDATE workspace_projects SET project_id=?1, state='active', updated_at=?2
                     WHERE profile=?3 AND canonical_path=?4",
                    params![project.to_string(), now, profile, canonical_text],
                )?;
            }
            return Ok(project);
        }
        conn.execute(
            "INSERT INTO workspace_projects(
                workspace_id, profile, canonical_path, project_id, vcs_fingerprint,
                state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)",
            params![
                workspace_id,
                profile,
                canonical_text,
                project.to_string(),
                vcs_fingerprint,
                now
            ],
        )?;
        Ok(project)
    }

    pub(crate) fn resolve_workspace(
        &self,
        profile: &str,
        workspace: &Path,
    ) -> Result<Option<ProjectId>, ProductStoreError> {
        let canonical = workspace.canonicalize()?;
        let canonical = canonical.to_str().ok_or_else(|| {
            ProductStoreError::InvalidInput("workspace path is not UTF-8".to_string())
        })?;
        self.connect()?
            .query_row(
                "SELECT project_id FROM workspace_projects
                 WHERE profile=?1 AND canonical_path=?2 AND state='active'",
                params![profile, canonical],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|id| {
                id.parse().map_err(|error| {
                    ProductStoreError::InvalidInput(format!("invalid stored project ID: {error}"))
                })
            })
            .transpose()
    }

    pub(crate) fn workspace_mapping(
        &self,
        profile: &str,
        workspace: &Path,
    ) -> Result<Option<CatalogWorkspace>, ProductStoreError> {
        let canonical = workspace.canonicalize()?;
        let canonical = canonical.to_str().ok_or_else(|| {
            ProductStoreError::InvalidInput("workspace path is not UTF-8".to_string())
        })?;
        self.connect()?
            .query_row(
                "SELECT workspace_id, canonical_path, project_id
                 FROM workspace_projects
                 WHERE profile=?1 AND canonical_path=?2 AND state='active'",
                params![profile, canonical],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .map(|row| {
                Ok(CatalogWorkspace {
                    workspace_id: row.0,
                    canonical_path: row.1,
                    project_id: row.2.parse().map_err(|error| {
                        ProductStoreError::InvalidInput(format!(
                            "invalid stored project ID: {error}"
                        ))
                    })?,
                })
            })
            .transpose()
    }

    pub(crate) fn mint_context_capability(
        &self,
        context: &MemoryContext,
        bearer_generation: u64,
        now: i64,
        ttl_secs: i64,
    ) -> Result<String, ProductStoreError> {
        if ttl_secs <= 0 || ttl_secs > 3_600 {
            return Err(ProductStoreError::InvalidInput(
                "context capability TTL must be in 1..=3600 seconds".to_string(),
            ));
        }
        let encoded = serde_json::to_string(context)?;
        if encoded.len() > 16 * 1024 {
            return Err(ProductStoreError::InvalidInput(
                "context capability exceeds 16 KiB".to_string(),
            ));
        }
        let mut token = [0_u8; 32];
        rand::thread_rng().fill_bytes(&mut token);
        let plaintext = hex_encode(&token);
        let hash = blake3::hash(&token);
        self.connect()?.execute(
            "INSERT INTO context_capabilities(
                token_hash, bearer_generation, principal_id, actor_kind, profile,
                project_id, active_team_id, authorization_generation, context_json,
                created_at, expires_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)",
            params![
                hash.as_bytes().as_slice(),
                bearer_generation,
                context.principal.to_string(),
                actor_kind_name(context.actor_kind),
                context.profile,
                context.project.map(|id| id.to_string()),
                context.active_team.as_ref().map(ToString::to_string),
                context.authorization_generation,
                encoded,
                now,
                now.saturating_add(ttl_secs),
            ],
        )?;
        Ok(plaintext)
    }

    pub(crate) fn load_context_capability(
        &self,
        plaintext: &str,
        bearer_generation: u64,
        now: i64,
    ) -> Result<MemoryContext, ProductStoreError> {
        let token = hex_decode_32(plaintext)?;
        let hash = blake3::hash(&token);
        let encoded = self
            .connect()?
            .query_row(
                "SELECT context_json FROM context_capabilities
                 WHERE token_hash=?1 AND bearer_generation=?2
                   AND expires_at>?3 AND revoked_at IS NULL",
                params![hash.as_bytes().as_slice(), bearer_generation, now],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| ProductStoreError::NotFound("context capability".to_string()))?;
        let context: MemoryContext = serde_json::from_str(&encoded)?;
        Ok(context)
    }

    pub(crate) fn revoke_context_capabilities(
        &self,
        principal: &PrincipalId,
        now: i64,
    ) -> Result<usize, ProductStoreError> {
        let changed = self.connect()?.execute(
            "UPDATE context_capabilities SET revoked_at=?1
             WHERE principal_id=?2 AND revoked_at IS NULL",
            params![now, principal.to_string()],
        )?;
        Ok(changed)
    }

    fn initialize(&self) -> Result<(), ProductStoreError> {
        let conn = self.connect()?;
        let migrator = Migrator::new(&conn, "product_meta");
        let current = migrator
            .current()
            .map_err(|error| ProductStoreError::Migration(error.to_string()))?;
        if current > PRODUCT_SCHEMA_VERSION {
            return Err(ProductStoreError::UnsupportedSchema {
                found: i64::from(current),
                supported: i64::from(PRODUCT_SCHEMA_VERSION),
            });
        }
        migrator
            .apply(
                PRODUCT_SCHEMA_VERSION,
                &[
                    (1, PRODUCT_V1_SQL),
                    (2, PRODUCT_V2_SQL),
                    (3, PRODUCT_V3_SQL),
                    (4, PRODUCT_V4_SQL),
                ],
            )
            .map_err(|error| ProductStoreError::Migration(error.to_string()))?;
        conn.pragma_update(None, "user_version", PRODUCT_SCHEMA_VERSION)?;
        Ok(())
    }

    pub(crate) fn connect(&self) -> Result<Connection, ProductStoreError> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }
}

fn decode_catalog_space(row: &rusqlite::Row<'_>) -> rusqlite::Result<CatalogSpace> {
    let id: String = row.get(0)?;
    let owner_kind: String = row.get(2)?;
    let owner_id: String = row.get(3)?;
    let context_kind: String = row.get(4)?;
    let project_key: String = row.get(5)?;
    let state: String = row.get(11)?;
    let parsed = (|| {
        Ok(CatalogSpace {
            id: id
                .parse()
                .map_err(|error| format!("invalid space ID: {error}"))?,
            profile: row.get(1).map_err(|error| error.to_string())?,
            owner: decode_owner(&owner_kind, &owner_id)?,
            context: decode_context(&context_kind, &project_key)?,
            display_name: row.get(6).map_err(|error| error.to_string())?,
            root_key: row.get(7).map_err(|error| error.to_string())?,
            domain_count: usize::try_from(row.get::<_, i64>(8).map_err(|error| error.to_string())?)
                .map_err(|_| "invalid domain count".to_string())?,
            format_version: row.get(9).map_err(|error| error.to_string())?,
            manifest_hash: row.get(10).map_err(|error| error.to_string())?,
            state: CatalogSpaceState::parse(&state).map_err(|error| error.to_string())?,
            catalog_generation: row.get(12).map_err(|error| error.to_string())?,
            created_at: row.get(13).map_err(|error| error.to_string())?,
            updated_at: row.get(14).map_err(|error| error.to_string())?,
        })
    })();
    parsed.map_err(|message: String| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            )),
        )
    })
}

fn encode_owner(owner: &SpaceOwner) -> (&'static str, String) {
    match owner {
        SpaceOwner::User(id) => ("user", id.to_string()),
        SpaceOwner::Team(id) => ("team", id.to_string()),
    }
}

fn decode_owner(kind: &str, id: &str) -> Result<SpaceOwner, String> {
    match kind {
        "user" => id
            .parse()
            .map(SpaceOwner::User)
            .map_err(|error| error.to_string()),
        "team" => id
            .parse()
            .map(SpaceOwner::Team)
            .map_err(|error| error.to_string()),
        _ => Err("invalid owner kind".to_string()),
    }
}

fn encode_context(context: SpaceContext) -> (&'static str, String) {
    match context {
        SpaceContext::Global => ("global", String::new()),
        SpaceContext::Project(id) => ("project", id.to_string()),
    }
}

fn decode_context(kind: &str, key: &str) -> Result<SpaceContext, String> {
    match (kind, key) {
        ("global", "") => Ok(SpaceContext::Global),
        ("project", key) if !key.is_empty() => key
            .parse()
            .map(SpaceContext::Project)
            .map_err(|error| error.to_string()),
        _ => Err("invalid context binding".to_string()),
    }
}

fn role_name(role: SpaceRole) -> &'static str {
    match role {
        SpaceRole::Reader => "reader",
        SpaceRole::Contributor => "contributor",
        SpaceRole::Reviewer => "reviewer",
        SpaceRole::Maintainer => "maintainer",
    }
}

fn parse_role(role: &str) -> Option<SpaceRole> {
    match role {
        "reader" => Some(SpaceRole::Reader),
        "contributor" => Some(SpaceRole::Contributor),
        "reviewer" => Some(SpaceRole::Reviewer),
        "maintainer" => Some(SpaceRole::Maintainer),
        _ => None,
    }
}

fn actor_kind_name(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

fn validate_bounded(
    field: &str,
    value: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), ProductStoreError> {
    if value.len() < minimum || value.len() > maximum {
        return Err(ProductStoreError::InvalidInput(format!(
            "{field} must contain {minimum}..={maximum} bytes"
        )));
    }
    Ok(())
}

fn validate_profile(value: &str) -> Result<(), ProductStoreError> {
    validate_bounded("profile", value, 1, 64)?;
    if value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(ProductStoreError::InvalidInput(
            "profile is not path-safe".to_string(),
        ))
    }
}

fn validate_root_key(value: &str) -> Result<(), ProductStoreError> {
    validate_bounded("root key", value, 1, 256)?;
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        Err(ProductStoreError::InvalidInput(
            "root key is not a safe relative path".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn hex_decode_32(value: &str) -> Result<[u8; 32], ProductStoreError> {
    if value.len() != 64 {
        return Err(ProductStoreError::NotFound(
            "context capability".to_string(),
        ));
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        decoded[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(decoded)
}

fn hex_nibble(value: u8) -> Result<u8, ProductStoreError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(ProductStoreError::NotFound(
            "context capability".to_string(),
        )),
    }
}

const PRODUCT_V1_SQL: &str = "
CREATE TABLE IF NOT EXISTS daemon_jobs (
    id TEXT PRIMARY KEY,
    kind_json TEXT NOT NULL,
    state_json TEXT NOT NULL,
    profile TEXT NOT NULL,
    created_at_unix_secs INTEGER NOT NULL,
    updated_at_unix_secs INTEGER NOT NULL,
    job_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_daemon_jobs_created
    ON daemon_jobs(created_at_unix_secs, id);
CREATE INDEX IF NOT EXISTS idx_daemon_jobs_state
    ON daemon_jobs(state_json);
CREATE TABLE IF NOT EXISTS daemon_events (
    sequence INTEGER PRIMARY KEY,
    unix_secs INTEGER NOT NULL,
    event_type_json TEXT NOT NULL,
    job_id TEXT,
    event_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_daemon_events_job
    ON daemon_events(job_id, sequence);
";

const PRODUCT_V2_SQL: &str = "
CREATE TABLE memory_spaces (
    id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('user','team')),
    owner_id TEXT NOT NULL,
    context_kind TEXT NOT NULL CHECK(context_kind IN ('global','project')),
    project_key TEXT NOT NULL DEFAULT '',
    display_name TEXT NOT NULL,
    root_key TEXT NOT NULL UNIQUE,
    domain_count INTEGER NOT NULL CHECK(domain_count >= 1),
    format_version INTEGER NOT NULL,
    manifest_hash BLOB NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('creating','active','closed','deleting','error')),
    catalog_generation INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK((context_kind = 'global' AND project_key = '') OR
          (context_kind = 'project' AND project_key <> '')),
    UNIQUE(profile, owner_kind, owner_id, context_kind, project_key)
);
CREATE INDEX idx_memory_spaces_profile_state
    ON memory_spaces(profile, state, updated_at);

CREATE TABLE local_principals (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('active','disabled')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE local_teams (
    id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    display_name TEXT NOT NULL,
    authority_generation INTEGER NOT NULL DEFAULT 1,
    state TEXT NOT NULL CHECK(state IN ('active','archived')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE space_memberships (
    space_id TEXT NOT NULL REFERENCES memory_spaces(id),
    principal_id TEXT NOT NULL REFERENCES local_principals(id),
    role TEXT NOT NULL CHECK(role IN ('reader','contributor','reviewer','maintainer')),
    authority_generation INTEGER NOT NULL,
    expires_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(space_id, principal_id)
) WITHOUT ROWID;
CREATE INDEX idx_space_memberships_principal
    ON space_memberships(principal_id, space_id);

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    display_name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE workspace_projects (
    workspace_id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    canonical_path TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    vcs_fingerprint TEXT,
    state TEXT NOT NULL CHECK(state IN ('active','moved','detached')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(profile, canonical_path)
);

CREATE TABLE context_capabilities (
    token_hash BLOB PRIMARY KEY,
    bearer_generation INTEGER NOT NULL,
    principal_id TEXT NOT NULL REFERENCES local_principals(id),
    actor_kind TEXT NOT NULL CHECK(actor_kind IN ('human','agent','system')),
    profile TEXT NOT NULL,
    project_id TEXT,
    active_team_id TEXT,
    authorization_generation INTEGER NOT NULL,
    context_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER
);
CREATE INDEX idx_context_capabilities_expiry ON context_capabilities(expires_at);
";

const PRODUCT_V3_SQL: &str = "
CREATE TABLE identity_candidates (
    id TEXT PRIMARY KEY,
    merge_job_id TEXT NOT NULL,
    left_space_id TEXT NOT NULL,
    left_entity_id TEXT NOT NULL,
    left_revision_id TEXT NOT NULL,
    right_space_id TEXT NOT NULL,
    right_entity_id TEXT NOT NULL,
    right_revision_id TEXT NOT NULL,
    policy_generation INTEGER NOT NULL,
    packet_hash BLOB NOT NULL,
    packet_json TEXT NOT NULL,
    deterministic_state TEXT NOT NULL,
    proposal_state TEXT NOT NULL,
    current_decision TEXT,
    current_event_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(merge_job_id, left_space_id, left_entity_id,
           right_space_id, right_entity_id)
);
CREATE INDEX idx_identity_candidates_job_state
    ON identity_candidates(merge_job_id, proposal_state, id);

CREATE TABLE identity_decision_events (
    id TEXT PRIMARY KEY,
    candidate_id TEXT NOT NULL REFERENCES identity_candidates(id),
    supersedes_event_id TEXT,
    decision TEXT NOT NULL CHECK(decision IN ('same','different','undetermined')),
    decision_source TEXT NOT NULL CHECK(decision_source IN
        ('lineage','verified_identifier','human','agent_proposal')),
    principal_id TEXT,
    actor_kind TEXT NOT NULL,
    packet_hash BLOB NOT NULL,
    rationale_json TEXT NOT NULL,
    model_provenance_json TEXT,
    created_at INTEGER NOT NULL
);

CREATE TABLE merge_jobs (
    id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    source_space_id TEXT NOT NULL,
    target_space_id TEXT NOT NULL,
    requested_by TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN
        ('discovering','awaiting_review','planned','confirmed','staging','verified',
         'promoting','succeeded','cancelled','conflicted','failed','recovery_required')),
    source_snapshot_hash BLOB,
    target_snapshot_hash BLOB,
    target_generation_json TEXT,
    plan_hash BLOB,
    predicted_result_hash BLOB,
    confirmation_hash BLOB,
    confirmation_expires_at INTEGER,
    report_json TEXT NOT NULL DEFAULT '{}',
    error_json TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_merge_jobs_profile_state ON merge_jobs(profile, state, updated_at);
CREATE UNIQUE INDEX idx_merge_jobs_idempotency
    ON merge_jobs(profile, requested_by, idempotency_key);

CREATE TABLE merge_resolutions (
    merge_job_id TEXT NOT NULL REFERENCES merge_jobs(id),
    candidate_id TEXT NOT NULL REFERENCES identity_candidates(id),
    decision_event_id TEXT NOT NULL REFERENCES identity_decision_events(id),
    receipt_hash BLOB NOT NULL,
    PRIMARY KEY(merge_job_id, candidate_id)
) WITHOUT ROWID;

CREATE TABLE space_lineages (
    id TEXT PRIMARY KEY,
    source_space_id TEXT NOT NULL,
    target_space_id TEXT NOT NULL,
    merge_job_id TEXT NOT NULL REFERENCES merge_jobs(id),
    source_snapshot_hash BLOB NOT NULL,
    prior_target_hash BLOB NOT NULL,
    result_target_hash BLOB NOT NULL,
    manifest_relpath TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
";

const PRODUCT_V4_SQL: &str = "
CREATE TABLE maintenance_tasks (
    id TEXT PRIMARY KEY,
    profile TEXT NOT NULL,
    space_id TEXT,
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    cursor_json TEXT NOT NULL DEFAULT '{}',
    counters_json TEXT NOT NULL DEFAULT '{}',
    last_error_json TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_maintenance_tasks_state
    ON maintenance_tasks(profile, state, updated_at);
";
