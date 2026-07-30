//! Audited graph mutations, immutable revisions, and durable index repair.
//!
//! This is intentionally a small changeset protocol rather than a generic
//! event-sourcing framework.  Canonical graph rows remain the current read
//! projection; a changeset, revision, event, and index outbox explain each
//! semantic mutation and make derived-state failure recoverable.

use std::collections::BTreeSet;
use std::str::FromStr;

use blake3::Hasher;
use openmemory_core::space::{ActorKind, AuthoritySnapshot, SpaceId, SpaceRole};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, MemoryResult};
use crate::outbox::IndexRepairReport;
use crate::remember::{write_group, ObservationInput, RelationInput};
use crate::store::MemoryStore;
use crate::types::{EntityType, MemoryTier, Observation};

const REQUEST_VERSION: u16 = 1;
const MAX_IDEMPOTENCY_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 1_024;
const MAX_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_LIST_ITEMS: usize = 256;
const MAX_OPERATION_COUNT: usize = 256;
const DEFAULT_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
const MAX_DESTRUCTION_CONFIRMATION_TTL_SECS: i64 = 300;

/// Whether a changeset is applied immediately or waits for review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitMode {
    ApplyImmediately,
    Propose,
}

impl SubmitMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::ApplyImmediately => "apply_immediately",
            Self::Propose => "propose",
        }
    }
}

/// Durable changeset lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetState {
    Proposed,
    Applied,
    Rejected,
    Conflicted,
    Reverted,
}

impl ChangeSetState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
            Self::Conflicted => "conflicted",
            Self::Reverted => "reverted",
        }
    }

    fn parse(value: &str) -> MemoryResult<Self> {
        match value {
            "proposed" => Ok(Self::Proposed),
            "applied" => Ok(Self::Applied),
            "rejected" => Ok(Self::Rejected),
            "conflicted" => Ok(Self::Conflicted),
            "reverted" => Ok(Self::Reverted),
            other => Err(MemoryError::Schema(format!(
                "unknown changeset state {other:?}"
            ))),
        }
    }
}

/// Closed-world object kind used by requests and events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Entity,
    Observation,
    Relation,
}

impl ObjectKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Entity => "entity",
            Self::Observation => "observation",
            Self::Relation => "relation",
        }
    }
}

/// A bounded, typed operation accepted by the graph audit protocol.
// This enum keeps the public request shape readable; the bounded operation
// count and payload cap make the larger observation variant intentional.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ChangeOperation {
    Remember {
        entity_name: String,
        entity_type: EntityType,
        observations: Vec<ObservationInput>,
        relations: Vec<RelationInput>,
    },
    UpdateObservation {
        observation: Observation,
        expected_row_version: u64,
        expected_revision_id: Option<String>,
    },
    RetireObservation {
        observation_id: String,
        expected_row_version: u64,
    },
    RestoreObservation {
        observation_id: String,
        expected_row_version: u64,
    },
    RevertObservation {
        observation_id: String,
        revision_id: String,
        expected_row_version: u64,
    },
    DestroyObservation {
        observation_id: String,
        expected_row_version: u64,
        expected_revision_id: Option<String>,
        confirmation_hash: String,
    },
}

impl ChangeOperation {
    fn object_kind(&self) -> ObjectKind {
        match self {
            Self::Remember { .. } => ObjectKind::Entity,
            Self::UpdateObservation { .. }
            | Self::RetireObservation { .. }
            | Self::RestoreObservation { .. }
            | Self::RevertObservation { .. }
            | Self::DestroyObservation { .. } => ObjectKind::Observation,
        }
    }

    fn operation_name(&self) -> &'static str {
        match self {
            Self::Remember { .. } => "remember",
            Self::UpdateObservation { .. } => "update_observation",
            Self::RetireObservation { .. } => "retire_observation",
            Self::RestoreObservation { .. } => "restore_observation",
            Self::RevertObservation { .. } => "revert_observation",
            Self::DestroyObservation { .. } => "destroy_observation",
        }
    }

    fn logical_id(&self) -> String {
        match self {
            Self::Remember { entity_name, .. } => entity_name.clone(),
            Self::UpdateObservation { observation, .. } => observation.id.clone(),
            Self::RetireObservation { observation_id, .. }
            | Self::RestoreObservation { observation_id, .. }
            | Self::RevertObservation { observation_id, .. }
            | Self::DestroyObservation { observation_id, .. } => observation_id.clone(),
        }
    }

    fn expected_row_version(&self) -> Option<u64> {
        match self {
            Self::UpdateObservation {
                expected_row_version,
                ..
            }
            | Self::RetireObservation {
                expected_row_version,
                ..
            }
            | Self::RestoreObservation {
                expected_row_version,
                ..
            }
            | Self::RevertObservation {
                expected_row_version,
                ..
            }
            | Self::DestroyObservation {
                expected_row_version,
                ..
            } => Some(*expected_row_version),
            Self::Remember { .. } => None,
        }
    }
}

/// Validated changeset intent.  The request hash is computed from these
/// fields with explicit framing; persisted JSON is inspection payload only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeSetDraft {
    idempotency_key: String,
    space_id: SpaceId,
    actor_kind: ActorKind,
    actor_id: String,
    authority: AuthoritySnapshot,
    reason: String,
    source: String,
    operations: Vec<ChangeOperation>,
}

impl ChangeSetDraft {
    /// Construct and validate an immutable changeset draft.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        idempotency_key: impl Into<String>,
        space_id: SpaceId,
        actor_kind: ActorKind,
        actor_id: impl Into<String>,
        authority: AuthoritySnapshot,
        reason: impl Into<String>,
        source: impl Into<String>,
        operations: Vec<ChangeOperation>,
    ) -> MemoryResult<Self> {
        let draft = Self {
            idempotency_key: idempotency_key.into(),
            space_id,
            actor_kind,
            actor_id: actor_id.into(),
            authority,
            reason: reason.into(),
            source: source.into(),
            operations,
        };
        validate_draft(&draft)?;
        Ok(draft)
    }

    #[must_use]
    pub fn space_id(&self) -> SpaceId {
        self.space_id
    }

    #[must_use]
    pub fn operations(&self) -> &[ChangeOperation] {
        &self.operations
    }
}

/// A stable receipt returned by submit, approval, rejection, and repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSetReceipt {
    pub id: String,
    pub state: ChangeSetState,
    pub state_version: u64,
    pub semantic_generation: u64,
    pub indexed_generation: u64,
    pub index_repair_required: bool,
}

/// Daemon-verified human review authority. Construction rejects agents,
/// systems, and roles that cannot review; the daemon must create it from a
/// live authorization lease so the embedded digest is current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewAuthorization {
    actor_id: String,
    role: SpaceRole,
    authority: AuthoritySnapshot,
}

impl ReviewAuthorization {
    pub fn new(
        actor_kind: ActorKind,
        actor_id: impl Into<String>,
        role: SpaceRole,
        authority: AuthoritySnapshot,
    ) -> MemoryResult<Self> {
        let actor_id = actor_id.into();
        if actor_kind != ActorKind::Human || !role.can_review() {
            return Err(MemoryError::InvalidInput(
                "proposal review requires a human reviewer or maintainer".into(),
            ));
        }
        if actor_id.trim().is_empty()
            || actor_id.len() > MAX_TEXT_BYTES
            || actor_id.chars().any(char::is_control)
        {
            return Err(MemoryError::InvalidInput(
                "reviewer identity is invalid".into(),
            ));
        }
        Ok(Self {
            actor_id,
            role,
            authority,
        })
    }
}

/// Bounded lazy audit-baseline result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BackfillReport {
    pub baselines_created: u64,
    pub next_cursor: Option<String>,
    pub complete: bool,
}

/// Bounded maintenance result for expired rejected proposal payloads.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProposalRetentionReport {
    pub changesets_compacted: u64,
    pub request_rows_removed: u64,
}

/// Bounded preview and opaque one-time confirmation for irreversible
/// observation destruction.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestructionPreview {
    pub space_id: SpaceId,
    pub observation_id: String,
    pub expected_revision_id: Option<String>,
    pub expected_row_version: u64,
    pub revision_count: u64,
    pub side_table_row_count: u64,
    pub backup_impact: String,
    pub preview_hash: String,
    pub expires_at_unix_secs: i64,
    confirmation_token: String,
}

impl std::fmt::Debug for DestructionPreview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DestructionPreview")
            .field("space_id", &self.space_id)
            .field("observation_id", &self.observation_id)
            .field("expected_revision_id", &self.expected_revision_id)
            .field("expected_row_version", &self.expected_row_version)
            .field("revision_count", &self.revision_count)
            .field("side_table_row_count", &self.side_table_row_count)
            .field("backup_impact", &self.backup_impact)
            .field("preview_hash", &self.preview_hash)
            .field("expires_at_unix_secs", &self.expires_at_unix_secs)
            .field("confirmation_token", &"<redacted>")
            .finish()
    }
}

impl DestructionPreview {
    /// Opaque one-time token. Surfaces should treat it as a secret and send it
    /// only to the matching confirmation endpoint.
    #[must_use]
    pub fn confirmation_token(&self) -> &str {
        &self.confirmation_token
    }
}

#[derive(Debug, Clone)]
struct ObservationRow {
    observation: Observation,
    current_revision_id: Option<String>,
    row_version: u64,
    lifecycle: String,
}

#[derive(Debug)]
struct IndexRepairItem {
    id: i64,
    operation: String,
    logical_id: String,
    observation: Option<ObservationRow>,
    entity_name: Option<String>,
}

impl MemoryStore {
    /// Compatibility tier mutation routed through the audited update path.
    /// Missing or retired observations remain a no-op for the legacy API.
    pub(crate) fn set_observation_memory_tier_audited(
        &self,
        observation_id: &str,
        tier: MemoryTier,
    ) -> MemoryResult<bool> {
        let current = self.with_reader(|conn| {
            let current: Option<(Observation, u64, Option<String>)> = conn.query_row(
                "SELECT id, entity_id, content, observed_at, valid_from, valid_until,
                        confidence, source, tombstoned, access_count, memory_tier,
                        title, summary, importance, source_kind, row_version,
                        current_revision_id
                 FROM observations WHERE id = ?1",
                params![observation_id],
                |row| {
                    Ok((
                        crate::store::row_to_observation(row)?,
                        row.get::<_, i64>(15)? as u64,
                        row.get(16)?,
                    ))
                },
            ).optional()?;
            let Some((mut observation, row_version, revision_id)) = current else {
                return Ok(None);
            };
            let concepts = crate::store::load_observation_side_table(
                conn,
                "SELECT observation_id, concept FROM observation_concepts WHERE observation_id IN",
                &[observation_id],
            )?;
            let files = crate::store::load_observation_side_table(
                conn,
                "SELECT observation_id, file_path FROM observation_source_files WHERE observation_id IN",
                &[observation_id],
            )?;
            observation.concepts = concepts.get(observation_id).cloned().unwrap_or_default();
            observation.source_files = files.get(observation_id).cloned().unwrap_or_default();
            Ok(Some((observation, row_version, revision_id)))
        })?;
        let Some((mut observation, row_version, current_revision_id)) = current else {
            return Ok(false);
        };
        if observation.tombstoned {
            return Ok(false);
        }
        observation.memory_tier = tier;
        let space_id = self.compatibility_space_id()?;
        let authority = AuthoritySnapshot::from_generations(1, &[0])
            .map_err(|error| MemoryError::InvalidInput(error.to_string()))?;
        self.submit_changeset(
            ChangeSetDraft::new(
                format!("legacy-tier:{observation_id}:{}", self.clock().now_secs()),
                space_id,
                ActorKind::Human,
                "legacy",
                authority,
                "legacy memory tier update",
                "compatibility",
                vec![ChangeOperation::UpdateObservation {
                    observation,
                    expected_row_version: row_version,
                    expected_revision_id: current_revision_id,
                }],
            )?,
            SubmitMode::ApplyImmediately,
        )?;
        Ok(true)
    }

    /// Compatibility retirement used by the legacy forget surface. Missing
    /// or already-retired rows are idempotent no-ops; an existing active row
    /// becomes an audited lifecycle changeset.
    pub fn retire_observation(&self, observation_id: &str) -> MemoryResult<bool> {
        let current: Option<(u64, bool)> = self.with_reader(|conn| {
            conn.query_row(
                "SELECT row_version, tombstoned FROM observations WHERE id = ?1",
                params![observation_id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
            .map_err(Into::into)
        })?;
        let Some((row_version, tombstoned)) = current else {
            return Ok(false);
        };
        if tombstoned {
            return Ok(false);
        }
        let space_id = self.compatibility_space_id()?;
        let draft = ChangeSetDraft::new(
            format!("legacy-retire:{observation_id}:{}", self.clock().now_secs()),
            space_id,
            ActorKind::Human,
            "legacy",
            AuthoritySnapshot::from_generations(1, &[0])
                .map_err(|error| MemoryError::InvalidInput(error.to_string()))?,
            "legacy forget",
            "compatibility",
            vec![ChangeOperation::RetireObservation {
                observation_id: observation_id.to_owned(),
                expected_row_version: row_version,
            }],
        )?;
        self.submit_changeset(draft, SubmitMode::ApplyImmediately)?;
        Ok(true)
    }

    /// Compatibility entity-forget behavior for agent/MCP surfaces: retire
    /// every active observation in one audited changeset without exposing
    /// irreversible entity destruction.
    ///
    /// Returns [`MemoryError::AmbiguousEntityName`] when the name matches
    /// more than one entity. Retiring every observation of whichever row
    /// SQLite reached first would silently empty the wrong entity, and
    /// the live profile store already contains a name carried by two
    /// entities. Disambiguate and call
    /// [`Self::retire_entity_observations_by_id`].
    pub fn retire_entity_observations(&self, name: &str) -> MemoryResult<usize> {
        if name.trim().is_empty() {
            return Err(MemoryError::InvalidInput(
                "entity name must not be empty".into(),
            ));
        }
        let entity_id = match self.resolve_entity(name)? {
            crate::resolve::EntityResolution::NotFound => {
                return Err(MemoryError::EntityNotFound(name.to_owned()));
            }
            crate::resolve::EntityResolution::Unique(entity) => entity.id,
            crate::resolve::EntityResolution::Ambiguous(candidates) => {
                return Err(MemoryError::AmbiguousEntityName {
                    name: name.to_owned(),
                    candidates: candidates.labels(),
                });
            }
        };
        self.retire_entity_observations_by_id(&entity_id)
    }

    /// Retire every active observation of the entity with this immutable
    /// id. The unambiguous form of [`Self::retire_entity_observations`].
    pub fn retire_entity_observations_by_id(&self, entity_id: &str) -> MemoryResult<usize> {
        if entity_id.trim().is_empty() {
            return Err(MemoryError::InvalidInput(
                "entity id must not be empty".into(),
            ));
        }
        let observation_ids = self.with_reader(|conn| {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM entities WHERE id = ?1)",
                params![entity_id],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(MemoryError::EntityNotFound(entity_id.to_owned()));
            }
            let mut stmt = conn.prepare(
                "SELECT id FROM observations
                 WHERE entity_id = ?1 AND lifecycle = 'active'
                 ORDER BY id",
            )?;
            let ids = stmt
                .query_map(params![entity_id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ids)
        })?;
        self.retire_observations_for_maintenance(&observation_ids, "compatibility entity forget")
    }

    /// Create a short-lived, actor/authority/head-bound preview for an
    /// irreversible observation destroy operation.
    pub fn preview_observation_destruction(
        &self,
        observation_id: &str,
        review: &ReviewAuthorization,
        ttl_secs: i64,
    ) -> MemoryResult<DestructionPreview> {
        if review.role != SpaceRole::Maintainer {
            return Err(MemoryError::InvalidInput(
                "observation destruction requires a human maintainer".to_owned(),
            ));
        }
        let ttl_secs = ttl_secs.clamp(1, MAX_DESTRUCTION_CONFIRMATION_TTL_SECS);
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let space_id = bound_space_id(&tx)?;
        let row = load_observation(&tx, observation_id)?
            .ok_or_else(|| MemoryError::ObservationNotFound(observation_id.to_owned()))?;
        let revision_count: u64 = tx.query_row(
            "SELECT COUNT(*) FROM observation_revisions WHERE observation_id = ?1",
            params![observation_id],
            |row| row.get(0),
        )?;
        let concepts: u64 = tx.query_row(
            "SELECT COUNT(*) FROM observation_concepts WHERE observation_id = ?1",
            params![observation_id],
            |row| row.get(0),
        )?;
        let files: u64 = tx.query_row(
            "SELECT COUNT(*) FROM observation_source_files WHERE observation_id = ?1",
            params![observation_id],
            |row| row.get(0),
        )?;
        let side_table_row_count = concepts.saturating_add(files);
        let preview_hash = destruction_preview_hash(
            space_id,
            observation_id,
            row.current_revision_id.as_deref(),
            row.row_version,
            revision_count,
            side_table_row_count,
        );
        let confirmation_token = openmemory_core::space::ChangeSetId::new().to_string();
        let token_hash = destruction_token_hash(&confirmation_token);
        let now = self.clock().now_secs();
        let expires_at_unix_secs = now.saturating_add(ttl_secs);
        tx.execute(
            "INSERT INTO destruction_confirmations(
                 token_hash, space_id, object_kind, logical_id,
                 expected_revision_id, expected_row_version, preview_hash,
                 actor_id, authority_snapshot, expires_at, created_at
             ) VALUES(?1, ?2, 'observation', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                token_hash,
                space_id.to_string(),
                observation_id,
                row.current_revision_id,
                i64::try_from(row.row_version)
                    .map_err(|_| MemoryError::InvalidInput("row version overflow".to_owned()))?,
                preview_hash,
                review.actor_id,
                review.authority.to_string(),
                expires_at_unix_secs,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(DestructionPreview {
            space_id,
            observation_id: observation_id.to_owned(),
            expected_revision_id: row.current_revision_id,
            expected_row_version: row.row_version,
            revision_count,
            side_table_row_count,
            backup_impact: "immutable revisions remain in backups and retained history".to_owned(),
            preview_hash,
            expires_at_unix_secs,
            confirmation_token,
        })
    }

    /// Consume a matching one-time preview and destroy the canonical
    /// observation through the normal audited changeset transaction.
    pub fn destroy_observation(
        &self,
        preview: &DestructionPreview,
        review: ReviewAuthorization,
        idempotency_key: impl Into<String>,
        reason: impl Into<String>,
    ) -> MemoryResult<ChangeSetReceipt> {
        if review.role != SpaceRole::Maintainer {
            return Err(MemoryError::InvalidInput(
                "observation destruction requires a human maintainer".to_owned(),
            ));
        }
        if preview.space_id != self.compatibility_space_id()? {
            return Err(MemoryError::InvalidInput(
                "destruction preview belongs to another space".to_owned(),
            ));
        }
        let draft = ChangeSetDraft::new(
            idempotency_key,
            preview.space_id,
            ActorKind::Human,
            review.actor_id,
            review.authority,
            reason,
            "human-admin",
            vec![ChangeOperation::DestroyObservation {
                observation_id: preview.observation_id.clone(),
                expected_row_version: preview.expected_row_version,
                expected_revision_id: preview.expected_revision_id.clone(),
                confirmation_hash: destruction_token_hash(&preview.confirmation_token),
            }],
        )?;
        self.submit_changeset(draft, SubmitMode::ApplyImmediately)
    }

    /// Route maintenance retirement through the same audited transaction as
    /// interactive mutations. Access-count rollups remain operational state
    /// and may be applied separately after this semantic commit succeeds.
    pub(crate) fn retire_observations_for_maintenance(
        &self,
        observation_ids: &[String],
        reason: &str,
    ) -> MemoryResult<usize> {
        let operations = self.with_reader(|conn| {
            let mut operations = Vec::with_capacity(observation_ids.len());
            for observation_id in observation_ids {
                let row: Option<(i64, String)> = conn
                    .query_row(
                        "SELECT row_version, lifecycle FROM observations WHERE id = ?1",
                        params![observation_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if let Some((row_version, lifecycle)) = row {
                    if lifecycle == "active" {
                        operations.push(ChangeOperation::RetireObservation {
                            observation_id: observation_id.clone(),
                            expected_row_version: row_version.try_into().map_err(|_| {
                                MemoryError::Schema("negative observation row version".to_owned())
                            })?,
                        });
                    }
                }
            }
            Ok(operations)
        })?;
        if operations.is_empty() {
            return Ok(0);
        }
        let retired = operations.len();
        let draft = ChangeSetDraft::new(
            format!(
                "maintenance-retire:{}",
                openmemory_core::space::ChangeSetId::new()
            ),
            self.compatibility_space_id()?,
            ActorKind::System,
            "maintenance",
            AuthoritySnapshot::from_generations(1, &[0])
                .map_err(|error| MemoryError::InvalidInput(error.to_string()))?,
            reason,
            "maintenance",
            operations,
        )?;
        self.submit_changeset(draft, SubmitMode::ApplyImmediately)?;
        Ok(retired)
    }

    /// Submit a bounded audited request. Proposals persist intent only;
    /// immediate requests mutate canonical rows, revisions, events, and the
    /// index outbox in one SQLite transaction.
    pub fn submit_changeset(
        &self,
        draft: ChangeSetDraft,
        mode: SubmitMode,
    ) -> MemoryResult<ChangeSetReceipt> {
        validate_draft(&draft)?;
        if mode == SubmitMode::Propose
            && draft
                .operations
                .iter()
                .any(|operation| matches!(operation, ChangeOperation::DestroyObservation { .. }))
        {
            return Err(MemoryError::InvalidInput(
                "destruction confirmations cannot be proposed".to_owned(),
            ));
        }
        let request_hash = canonical_request_hash(&draft, mode);
        let barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_bound_space(&tx, draft.space_id)?;

        if let Some(existing) = load_existing_receipt(&tx, &draft, &request_hash)? {
            tx.commit()?;
            return Ok(existing);
        }

        let now = self.clock().now_secs();
        let base_generation = current_generation(&tx)?;
        let changeset_id = openmemory_core::space::ChangeSetId::new();
        let initial_state = if mode == SubmitMode::Propose {
            ChangeSetState::Proposed
        } else {
            ChangeSetState::Applied
        };
        tx.execute(
            "INSERT INTO change_sets(
                 id, space_id, idempotency_key, request_version, request_hash,
                 submit_mode, actor_kind, actor_id, authority_snapshot, reason,
                 source, base_generation, committed_generation, state,
                 state_version, created_at, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 1, ?15, ?15)",
            params![
                changeset_id.to_string(),
                draft.space_id.to_string(),
                draft.idempotency_key,
                i64::from(REQUEST_VERSION),
                request_hash,
                mode.as_str(),
                serde_json::to_string(&draft.actor_kind)?,
                draft.actor_id,
                draft.authority.to_string(),
                draft.reason,
                draft.source,
                i64::try_from(base_generation)
                    .map_err(|_| MemoryError::InvalidInput("generation overflow".into()))?,
                if initial_state == ChangeSetState::Applied {
                    Some(
                        i64::try_from(base_generation + 1)
                            .map_err(|_| MemoryError::InvalidInput("generation overflow".into()))?,
                    )
                } else {
                    None
                },
                initial_state.as_str(),
                now,
            ],
        )?;
        insert_requests(&tx, changeset_id, &draft.operations)?;

        let committed_generation = if mode == SubmitMode::ApplyImmediately {
            let generation = base_generation
                .checked_add(1)
                .ok_or_else(|| MemoryError::InvalidInput("generation overflow".into()))?;
            for (ordinal, operation) in draft.operations.iter().enumerate() {
                apply_operation_tx(
                    &tx,
                    changeset_id,
                    operation,
                    generation,
                    ordinal as u64,
                    now,
                    self,
                )?;
            }
            tx.execute(
                "UPDATE domain_state SET semantic_generation = ?1,
                     index_repair_required = CASE
                         WHEN EXISTS(SELECT 1 FROM index_outbox WHERE applied_at IS NULL)
                         THEN 1 ELSE index_repair_required END,
                     updated_at = ?2 WHERE id = 1",
                params![
                    i64::try_from(generation)
                        .map_err(|_| MemoryError::InvalidInput("generation overflow".into()))?,
                    now
                ],
            )?;
            Some(generation)
        } else {
            None
        };
        tx.commit()?;

        drop(conn);
        drop(barrier);
        if committed_generation.is_some() {
            let _ = self.repair_index();
        }
        self.changeset_receipt(changeset_id)
    }

    /// Approve a proposal with an exact state-version precondition. A stale
    /// approval changes nothing and returns a typed conflict.
    pub fn approve_changeset(
        &self,
        id: &str,
        expected_state_version: u64,
        review: ReviewAuthorization,
    ) -> MemoryResult<ChangeSetReceipt> {
        self.transition_proposal(id, expected_state_version, review)
    }

    /// Reject a proposal without touching canonical graph or index state.
    pub fn reject_changeset(
        &self,
        id: &str,
        expected_state_version: u64,
        review: ReviewAuthorization,
        reason: impl Into<String>,
    ) -> MemoryResult<ChangeSetReceipt> {
        let reason = reason.into();
        if reason.trim().is_empty()
            || reason.len() > MAX_TEXT_BYTES
            || reason.chars().any(char::is_control)
        {
            return Err(MemoryError::InvalidInput(
                "rejection reason is invalid".into(),
            ));
        }
        let _barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, version, space_id) = proposal_header(&tx, id)?;
        if state == ChangeSetState::Rejected {
            tx.commit()?;
            drop(conn);
            return self.changeset_receipt(
                openmemory_core::space::ChangeSetId::from_str(id)
                    .map_err(|_| MemoryError::InvalidInput("invalid changeset id".into()))?,
            );
        }
        if state != ChangeSetState::Proposed || version != expected_state_version {
            return Err(MemoryError::ChangeSetStale);
        }
        ensure_bound_space(&tx, space_id)?;
        ensure_distinct_reviewer(&tx, id, &review)?;
        let now = self.clock().now_secs();
        let changed = tx.execute(
            "UPDATE change_sets SET state = 'rejected', state_version = state_version + 1,
             updated_at = ?1 WHERE id = ?2 AND state = 'proposed' AND state_version = ?3",
            params![
                now,
                id,
                i64::try_from(expected_state_version)
                    .map_err(|_| MemoryError::InvalidInput("state version overflow".into()))?
            ],
        )?;
        if changed != 1 {
            return Err(MemoryError::ChangeSetStale);
        }
        insert_review_event(&tx, id, "rejected", &review, &reason, now)?;
        tx.commit()?;
        drop(conn);
        self.changeset_receipt(
            openmemory_core::space::ChangeSetId::from_str(id)
                .map_err(|_| MemoryError::InvalidInput("invalid changeset id".into()))?,
        )
    }

    /// Remove immutable request payload rows for rejected proposals after the
    /// retention window. Canonical request hashes, safe changeset metadata,
    /// and review events remain durable.
    #[allow(clippy::let_and_return)]
    pub fn compact_rejected_proposals(
        &self,
        retention_secs: i64,
        limit: usize,
    ) -> MemoryResult<ProposalRetentionReport> {
        let cutoff = self
            .clock()
            .now_secs()
            .saturating_sub(retention_secs.max(0));
        let limit = limit.clamp(1, 1_024);
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM change_sets
                 WHERE state = 'rejected' AND updated_at <= ?1
                   AND EXISTS(
                       SELECT 1 FROM change_requests
                       WHERE change_requests.changeset_id = change_sets.id
                   )
                 ORDER BY updated_at, id LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(
                    params![cutoff, i64::try_from(limit).unwrap_or(1_024)],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut request_rows_removed = 0_u64;
        for id in &ids {
            request_rows_removed = request_rows_removed.saturating_add(tx.execute(
                "DELETE FROM change_requests WHERE changeset_id = ?1",
                params![id],
            )? as u64);
        }
        tx.commit()?;
        Ok(ProposalRetentionReport {
            changesets_compacted: ids.len() as u64,
            request_rows_removed,
        })
    }

    /// Apply a bounded page of legacy baselines. No graph scan runs at open;
    /// callers explicitly advance the cursor and can resume after failure.
    #[allow(clippy::let_and_return)]
    pub fn backfill_audit(&self, limit: usize) -> MemoryResult<BackfillReport> {
        let limit = limit.clamp(1, 1_024);
        let _barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.clock().now_secs();
        let space_id: String = tx
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'space_id'",
                [],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                MemoryError::InvalidInput("memory store is not bound to a catalog space".to_owned())
            })?;
        let cursor: Option<String> = tx.query_row(
            "SELECT backfill_cursor FROM domain_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let rows: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM observations
                 WHERE current_revision_id IS NULL AND (?1 IS NULL OR id > ?1)
                 ORDER BY id LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(
                    params![cursor, i64::try_from(limit).unwrap_or(1_024)],
                    |row| row.get(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        if rows.is_empty() {
            tx.execute(
                "UPDATE domain_state SET backfill_complete = 1, updated_at = ?1 WHERE id = 1",
                params![now],
            )?;
            tx.commit()?;
            return Ok(BackfillReport {
                baselines_created: 0,
                next_cursor: cursor,
                complete: true,
            });
        }
        let baseline_id = openmemory_core::space::ChangeSetId::new();
        let authority = AuthoritySnapshot::from_generations(1, &[0])
            .map_err(|error| MemoryError::InvalidInput(error.to_string()))?;
        tx.execute(
            "INSERT INTO change_sets(
                 id, space_id, idempotency_key, request_version, request_hash,
                 submit_mode, actor_kind, actor_id, authority_snapshot, reason,
                 source, base_generation, committed_generation, state, state_version,
                 created_at, updated_at
             ) VALUES(?1, ?2, ?3, 1, ?4, 'apply_immediately', 'system',
                      'backfill', ?5, 'legacy baseline', 'backfill', 0, 0, 'applied', 1, ?6, ?6)",
            params![
                baseline_id.to_string(),
                space_id,
                format!("baseline:{baseline_id}"),
                format!(
                    "blake3:{}",
                    blake3::hash(baseline_id.to_string().as_bytes()).to_hex()
                ),
                authority.to_string(),
                now
            ],
        )?;
        for (ordinal, id) in rows.iter().enumerate() {
            let row = load_observation(&tx, id)?
                .ok_or_else(|| MemoryError::ObservationNotFound(id.clone()))?;
            let revision = insert_observation_revision(&tx, &row, None, baseline_id, now)?;
            tx.execute(
                "UPDATE observations SET current_revision_id = ?1 WHERE id = ?2 AND current_revision_id IS NULL",
                params![revision, id],
            )?;
            tx.execute(
                "INSERT INTO change_events(changeset_id, ordinal, event_type, object_kind, logical_id, revision_id, payload_json, created_at)
                 VALUES(?1, ?2, 'baseline', 'observation', ?3, ?4, '{}', ?5)",
                params![baseline_id.to_string(), i64::try_from(ordinal).unwrap_or(i64::MAX), id, revision, now],
            )?;
        }
        let cursor = rows.last().cloned();
        tx.execute(
            "UPDATE domain_state SET backfill_cursor = ?1, backfill_complete = ?2, updated_at = ?3 WHERE id = 1",
            params![cursor, i64::from(u8::from(rows.len() < limit)), now],
        )?;
        tx.commit()?;
        Ok(BackfillReport {
            baselines_created: rows.len() as u64,
            next_cursor: cursor,
            complete: rows.len() < limit,
        })
    }

    /// Drain durable index outbox rows. Derived failure leaves canonical
    /// truth committed, retains the outbox, and marks repair required.
    pub fn repair_index(&self) -> MemoryResult<IndexRepairReport> {
        let pending: Vec<IndexRepairItem> = self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, semantic_generation, operation, logical_id FROM index_outbox
                 WHERE applied_at IS NULL ORDER BY semantic_generation, ordinal LIMIT 1024",
            )?;
            let raw = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut pending = Vec::with_capacity(raw.len());
            for (id, operation, logical_id) in raw {
                let observation = load_observation(conn, &logical_id)?;
                let entity_name = observation
                    .as_ref()
                    .map(|row| {
                        conn.query_row(
                            "SELECT name FROM entities WHERE id = ?1",
                            params![row.observation.entity_id],
                            |entity| entity.get(0),
                        )
                    })
                    .transpose()?;
                pending.push(IndexRepairItem {
                    id,
                    operation,
                    logical_id,
                    observation,
                    entity_name,
                });
            }
            Ok(pending)
        })?;
        if pending.is_empty() {
            let (remaining, generation) = self.mark_index_rows_applied(&[])?;
            return Ok(IndexRepairReport {
                applied: 0,
                remaining,
                generation,
            });
        }

        // Embedding is deliberately outside the SQLite writer and rebuild
        // locks. The outbox remains authoritative if a concurrent write adds
        // a newer generation before this batch acquires the publish barrier.
        let active_documents: Vec<(usize, String)> = pending
            .iter()
            .enumerate()
            .filter_map(
                |(index, item)| match (item.operation.as_str(), &item.observation) {
                    ("upsert", Some(row))
                        if !row.observation.tombstoned && row.lifecycle == "active" =>
                    {
                        Some((
                            index,
                            MemoryStore::search_body_text(
                                item.entity_name.as_deref().unwrap_or_default(),
                                &row.observation.content,
                            ),
                        ))
                    }
                    _ => None,
                },
            )
            .collect();
        let embedded = self.embed_documents_batch(
            &active_documents
                .iter()
                .map(|(_, text)| text.clone())
                .collect::<Vec<_>>(),
        );
        let mut vectors = vec![Vec::new(); pending.len()];
        for ((index, _), vector) in active_documents.into_iter().zip(embedded) {
            vectors[index] = vector;
        }

        let _barrier = self.write_rebuild();
        for (item, vector) in pending.iter().zip(vectors) {
            let result = if item.operation == "delete" {
                self.engine()
                    .engine
                    .delete_by_uri(&format!("memory://observation/{}", item.logical_id))
                    .map(drop)
            } else if let Some(row) = &item.observation {
                let mut entry = openmemory_index::IndexEntry::new(
                    format!("memory://observation/{}", item.logical_id),
                    MemoryStore::search_body_text(
                        item.entity_name.as_deref().unwrap_or_default(),
                        &row.observation.content,
                    ),
                )
                .with_title(row.observation.title.clone())
                .with_summary(row.observation.summary.clone())
                .with_concepts(row.observation.concepts.clone())
                .with_source_files(row.observation.source_files.clone())
                .with_source_kind(row.observation.source_kind.clone());
                if !vector.is_empty() {
                    entry = entry.with_vector(vector);
                }
                if row.observation.tombstoned || row.lifecycle != "active" {
                    self.engine()
                        .engine
                        .delete_by_uri(&format!("memory://observation/{}", item.logical_id))
                        .map(drop)
                } else {
                    self.engine().engine.insert(&[entry]).map(drop)
                }
            } else {
                self.engine()
                    .engine
                    .delete_by_uri(&format!("memory://observation/{}", item.logical_id))
                    .map(drop)
            };
            if let Err(error) = result {
                self.record_index_failure(item.id, &error.to_string())?;
                return Err(error.into());
            }
        }

        if let Err(error) = self.persist_search_index() {
            self.record_index_failure(pending[0].id, &error.to_string())?;
            return Err(error);
        }
        let ids = pending.iter().map(|row| row.id).collect::<Vec<_>>();
        let (remaining, generation) = self.mark_index_rows_applied(&ids)?;
        Ok(IndexRepairReport {
            applied: ids.len() as u64,
            remaining,
            generation,
        })
    }

    fn record_index_failure(&self, id: i64, error: &str) -> MemoryResult<()> {
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.clock().now_secs();
        tx.execute(
            "UPDATE index_outbox SET attempts = attempts + 1, last_error = ?1
             WHERE id = ?2 AND applied_at IS NULL",
            params![error, id],
        )?;
        tx.execute(
            "UPDATE domain_state SET index_repair_required = 1, updated_at = ?1 WHERE id = 1",
            params![now],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn mark_index_rows_applied(&self, ids: &[i64]) -> MemoryResult<(u64, u64)> {
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.clock().now_secs();
        for id in ids {
            tx.execute(
                "UPDATE index_outbox SET applied_at = ?1, attempts = attempts + 1,
                     last_error = NULL WHERE id = ?2 AND applied_at IS NULL",
                params![now, id],
            )?;
        }
        let remaining: u64 = tx.query_row(
            "SELECT COUNT(*) FROM index_outbox WHERE applied_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        let first_pending: Option<u64> = tx
            .query_row(
                "SELECT MIN(semantic_generation) FROM index_outbox WHERE applied_at IS NULL",
                [],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let semantic = current_generation(&tx)?;
        let indexed = first_pending.map_or(semantic, |generation| generation.saturating_sub(1));
        tx.execute(
            "UPDATE domain_state SET indexed_generation = ?1,
                 index_repair_required = ?2, updated_at = ?3 WHERE id = 1",
            params![
                i64::try_from(indexed)
                    .map_err(|_| MemoryError::InvalidInput("generation overflow".into()))?,
                i64::from(u8::from(remaining != 0)),
                now
            ],
        )?;
        tx.commit()?;
        Ok((remaining, indexed))
    }

    pub(crate) fn mark_index_generations_current(&self, generations: &[u64]) -> MemoryResult<()> {
        let ids = self.with_reader(|conn| {
            let mut ids = Vec::new();
            for generation in generations {
                let mut stmt = conn.prepare(
                    "SELECT id FROM index_outbox
                     WHERE semantic_generation = ?1 AND applied_at IS NULL ORDER BY ordinal",
                )?;
                ids.extend(
                    stmt.query_map(
                        params![i64::try_from(*generation).map_err(|_| {
                            MemoryError::InvalidInput("generation overflow".into())
                        })?],
                        |row| row.get::<_, i64>(0),
                    )?
                    .collect::<Result<Vec<_>, _>>()?,
                );
            }
            Ok(ids)
        })?;
        self.mark_index_rows_applied(&ids).map(drop)
    }

    #[allow(clippy::let_and_return)]
    fn transition_proposal(
        &self,
        id: &str,
        expected_state_version: u64,
        review: ReviewAuthorization,
    ) -> MemoryResult<ChangeSetReceipt> {
        let barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, version, space_id) = proposal_header(&tx, id)?;
        if state == ChangeSetState::Applied {
            tx.commit()?;
            drop(conn);
            drop(barrier);
            let _ = self.repair_index();
            return self.changeset_receipt(
                openmemory_core::space::ChangeSetId::from_str(id)
                    .map_err(|_| MemoryError::InvalidInput("invalid changeset id".into()))?,
            );
        }
        if state != ChangeSetState::Proposed || version != expected_state_version {
            return Err(MemoryError::ChangeSetStale);
        }
        ensure_bound_space(&tx, space_id)?;
        ensure_distinct_reviewer(&tx, id, &review)?;
        let now = self.clock().now_secs();
        let operations: Vec<ChangeOperation> = {
            let mut stmt = tx.prepare(
                "SELECT payload_json FROM change_requests WHERE changeset_id = ?1 ORDER BY ordinal",
            )?;
            let operations = stmt
                .query_map(params![id], |row| {
                    let payload: String = row.get(0)?;
                    serde_json::from_str(&payload).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            operations
        };
        verify_proposal_hash(&tx, id, &operations)?;
        let changeset_id = openmemory_core::space::ChangeSetId::from_str(id)
            .map_err(|_| MemoryError::InvalidInput("invalid changeset id".into()))?;
        let mut conflicted = false;
        for operation in &operations {
            if !operation_precondition_holds(&tx, operation)? {
                conflicted = true;
                break;
            }
        }
        if conflicted {
            let changed = tx.execute(
                "UPDATE change_sets SET state = 'conflicted',
                     state_version = state_version + 1, updated_at = ?1
                 WHERE id = ?2 AND state = 'proposed' AND state_version = ?3",
                params![
                    now,
                    id,
                    i64::try_from(expected_state_version)
                        .map_err(|_| MemoryError::InvalidInput("state version overflow".into()))?
                ],
            )?;
            if changed != 1 {
                return Err(MemoryError::ChangeSetStale);
            }
            insert_review_event(
                &tx,
                id,
                "conflicted",
                &review,
                "proposal head precondition is stale",
                now,
            )?;
            tx.commit()?;
            drop(conn);
            drop(barrier);
            return self.changeset_receipt(changeset_id);
        }
        let base = current_generation(&tx)?;
        let generation = base
            .checked_add(1)
            .ok_or_else(|| MemoryError::InvalidInput("generation overflow".into()))?;
        for (ordinal, operation) in operations.iter().enumerate() {
            apply_operation_tx(
                &tx,
                changeset_id,
                operation,
                generation,
                ordinal as u64,
                now,
                self,
            )?;
        }
        let changed = tx.execute(
            "UPDATE change_sets SET state = 'applied', state_version = state_version + 1,
             committed_generation = ?1, updated_at = ?2
             WHERE id = ?3 AND state = 'proposed' AND state_version = ?4",
            params![
                i64::try_from(generation).unwrap_or(i64::MAX),
                now,
                id,
                i64::try_from(expected_state_version).unwrap_or(i64::MAX)
            ],
        )?;
        if changed != 1 {
            return Err(MemoryError::ChangeSetStale);
        }
        insert_review_event(&tx, id, "approved", &review, "", now)?;
        tx.execute(
            "UPDATE domain_state SET semantic_generation = ?1,
                 index_repair_required = CASE
                     WHEN EXISTS(SELECT 1 FROM index_outbox WHERE applied_at IS NULL)
                     THEN 1 ELSE index_repair_required END,
                 updated_at = ?2 WHERE id = 1",
            params![i64::try_from(generation).unwrap_or(i64::MAX), now],
        )?;
        tx.commit()?;
        drop(conn);
        drop(barrier);
        let _ = self.repair_index();
        self.changeset_receipt(changeset_id)
    }

    fn changeset_receipt(
        &self,
        id: openmemory_core::space::ChangeSetId,
    ) -> MemoryResult<ChangeSetReceipt> {
        self.with_reader(|conn| {
            conn.query_row(
                "SELECT state, state_version, COALESCE(committed_generation, base_generation),
                        (SELECT indexed_generation FROM domain_state),
                        (SELECT index_repair_required FROM domain_state)
                 FROM change_sets WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok(ChangeSetReceipt {
                        id: id.to_string(),
                        state: ChangeSetState::parse(&row.get::<_, String>(0)?).map_err(
                            |error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)),
                        )?,
                        state_version: row.get::<_, i64>(1)? as u64,
                        semantic_generation: row.get::<_, i64>(2)? as u64,
                        indexed_generation: row.get::<_, i64>(3)? as u64,
                        index_repair_required: row.get::<_, i64>(4)? != 0,
                    })
                },
            )
            .map_err(Into::into)
        })
    }

    pub(crate) fn compatibility_space_id(&self) -> MemoryResult<SpaceId> {
        let conn = self.lock_db();
        let existing: Option<String> = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'space_id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            return SpaceId::from_str(&existing)
                .map_err(|error| MemoryError::Schema(error.to_string()));
        }
        let id = SpaceId::new();
        conn.execute(
            "INSERT INTO memory_meta(key, value) VALUES('space_id', ?1)",
            params![id.to_string()],
        )?;
        Ok(id)
    }
}

fn validate_draft(draft: &ChangeSetDraft) -> MemoryResult<()> {
    if draft.idempotency_key.is_empty() || draft.idempotency_key.len() > MAX_IDEMPOTENCY_BYTES {
        return Err(MemoryError::InvalidInput(
            "idempotency key exceeds 1..=128 bytes".into(),
        ));
    }
    for value in [&draft.actor_id, &draft.reason, &draft.source] {
        if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.chars().any(char::is_control) {
            return Err(MemoryError::InvalidInput(
                "changeset text exceeds its bound".into(),
            ));
        }
    }
    if draft.operations.is_empty() || draft.operations.len() > MAX_OPERATION_COUNT {
        return Err(MemoryError::InvalidInput(
            "changeset operation count exceeds 1..=256".into(),
        ));
    }
    let payload = serde_json::to_vec(&draft.operations)?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(MemoryError::InvalidInput(
            "changeset payload exceeds the 4 MiB hard limit".into(),
        ));
    }
    if payload.len() > DEFAULT_MAX_PAYLOAD_BYTES {
        return Err(MemoryError::InvalidInput(
            "changeset payload exceeds the configured 1 MiB limit".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    for operation in &draft.operations {
        validate_operation(operation)?;
        if !seen.insert((operation.object_kind(), operation.logical_id())) {
            return Err(MemoryError::InvalidInput(
                "duplicate operation on logical head".into(),
            ));
        }
    }
    Ok(())
}

fn validate_operation(operation: &ChangeOperation) -> MemoryResult<()> {
    fn text(value: &str, field: &str, max: usize, allow_empty: bool) -> MemoryResult<()> {
        if (!allow_empty && value.trim().is_empty())
            || value.len() > max
            || value.chars().any(|character| character == '\0')
        {
            return Err(MemoryError::InvalidInput(format!(
                "{field} is empty, contains NUL, or exceeds {max} bytes"
            )));
        }
        Ok(())
    }

    struct ObservationValues<'a> {
        content: &'a str,
        confidence: f32,
        source: &'a str,
        valid_from: Option<i64>,
        valid_until: Option<i64>,
        title: Option<&'a str>,
        summary: Option<&'a str>,
        importance: Option<f32>,
        source_kind: Option<&'a str>,
        concepts: &'a [String],
        source_files: &'a [String],
    }

    fn observation_values(values: ObservationValues<'_>) -> MemoryResult<()> {
        text(
            values.content,
            "observation content",
            MAX_CONTENT_BYTES,
            false,
        )?;
        text(values.source, "observation source", MAX_TEXT_BYTES, true)?;
        if let Some(title) = values.title {
            text(title, "observation title", MAX_TEXT_BYTES, true)?;
        }
        if let Some(summary) = values.summary {
            text(summary, "observation summary", MAX_TEXT_BYTES, true)?;
        }
        if let Some(source_kind) = values.source_kind {
            text(source_kind, "observation source kind", MAX_TEXT_BYTES, true)?;
        }
        if !values.confidence.is_finite() || !(0.0..=1.0).contains(&values.confidence) {
            return Err(MemoryError::InvalidInput(
                "observation confidence must be finite and within 0..=1".into(),
            ));
        }
        if values
            .importance
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(MemoryError::InvalidInput(
                "observation importance must be finite and within 0..=1".into(),
            ));
        }
        if values
            .valid_from
            .zip(values.valid_until)
            .is_some_and(|(from, until)| until < from)
        {
            return Err(MemoryError::InvalidInput(
                "observation validity interval is inverted".into(),
            ));
        }
        for (field, values) in [
            ("observation concepts", values.concepts),
            ("observation source files", values.source_files),
        ] {
            if values.len() > MAX_LIST_ITEMS {
                return Err(MemoryError::InvalidInput(format!(
                    "{field} exceeds {MAX_LIST_ITEMS} entries"
                )));
            }
            let mut unique = BTreeSet::new();
            for value in values {
                text(value, field, MAX_TEXT_BYTES, false)?;
                if !unique.insert(value) {
                    return Err(MemoryError::InvalidInput(format!(
                        "{field} contains duplicates"
                    )));
                }
            }
        }
        Ok(())
    }

    match operation {
        ChangeOperation::Remember {
            entity_name,
            observations,
            relations,
            ..
        } => {
            text(entity_name, "entity name", MAX_TEXT_BYTES, false)?;
            if observations.len() > MAX_LIST_ITEMS || relations.len() > MAX_LIST_ITEMS {
                return Err(MemoryError::InvalidInput(
                    "remember operation exceeds its nested item bound".into(),
                ));
            }
            for observation in observations {
                observation_values(ObservationValues {
                    content: &observation.content,
                    confidence: observation.confidence,
                    source: &observation.source,
                    valid_from: observation.valid_from,
                    valid_until: observation.valid_until,
                    title: observation.title.as_deref(),
                    summary: observation.summary.as_deref(),
                    importance: observation.importance,
                    source_kind: observation.source_kind.as_deref(),
                    concepts: &observation.concepts,
                    source_files: &observation.source_files,
                })?;
            }
            for relation in relations {
                text(
                    &relation.relation_type,
                    "relation type",
                    MAX_TEXT_BYTES,
                    false,
                )?;
                text(
                    &relation.target_name,
                    "relation target",
                    MAX_TEXT_BYTES,
                    false,
                )?;
                text(&relation.source, "relation source", MAX_TEXT_BYTES, true)?;
                if !relation.weight.is_finite() || relation.weight < 0.0 {
                    return Err(MemoryError::InvalidInput(
                        "relation weight must be finite and non-negative".into(),
                    ));
                }
            }
        }
        ChangeOperation::UpdateObservation { observation, .. } => {
            text(&observation.id, "observation id", MAX_TEXT_BYTES, false)?;
            text(
                &observation.entity_id,
                "observation entity id",
                MAX_TEXT_BYTES,
                false,
            )?;
            observation_values(ObservationValues {
                content: &observation.content,
                confidence: observation.confidence,
                source: &observation.source,
                valid_from: observation.valid_from,
                valid_until: observation.valid_until,
                title: observation.title.as_deref(),
                summary: observation.summary.as_deref(),
                importance: observation.importance,
                source_kind: observation.source_kind.as_deref(),
                concepts: &observation.concepts,
                source_files: &observation.source_files,
            })?;
        }
        ChangeOperation::RetireObservation { observation_id, .. }
        | ChangeOperation::RestoreObservation { observation_id, .. } => {
            text(observation_id, "observation id", MAX_TEXT_BYTES, false)?;
        }
        ChangeOperation::RevertObservation {
            observation_id,
            revision_id,
            ..
        } => {
            text(observation_id, "observation id", MAX_TEXT_BYTES, false)?;
            text(revision_id, "revision id", MAX_TEXT_BYTES, false)?;
        }
        ChangeOperation::DestroyObservation {
            observation_id,
            expected_revision_id,
            confirmation_hash,
            ..
        } => {
            text(observation_id, "observation id", MAX_TEXT_BYTES, false)?;
            if let Some(revision_id) = expected_revision_id {
                text(revision_id, "revision id", MAX_TEXT_BYTES, false)?;
            }
            if !is_blake3_hash(confirmation_hash) {
                return Err(MemoryError::InvalidInput(
                    "destruction confirmation hash is invalid".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn is_blake3_hash(value: &str) -> bool {
    value.len() == 71
        && value
            .strip_prefix("blake3:")
            .is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn canonical_request_hash(draft: &ChangeSetDraft, mode: SubmitMode) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"openmemory/changeset/v1");
    frame_u16(&mut hasher, REQUEST_VERSION);
    frame_str(&mut hasher, mode.as_str());
    frame_str(&mut hasher, &draft.space_id.to_string());
    frame_str(&mut hasher, "home-domain:0");
    frame_str(&mut hasher, actor_kind_str(draft.actor_kind));
    frame_str(&mut hasher, &draft.actor_id);
    frame_u16(&mut hasher, draft.authority.version());
    hasher.update(&draft.authority.digest());
    frame_str(&mut hasher, &draft.reason);
    frame_str(&mut hasher, &draft.source);
    frame_u64(&mut hasher, draft.operations.len() as u64);
    for operation in &draft.operations {
        hash_operation(&mut hasher, operation);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn destruction_token_hash(token: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"openmemory/destruction-confirmation-token/v1");
    frame_str(&mut hasher, token);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn destruction_preview_hash(
    space_id: SpaceId,
    observation_id: &str,
    expected_revision_id: Option<&str>,
    expected_row_version: u64,
    revision_count: u64,
    side_table_row_count: u64,
) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"openmemory/observation-destruction-preview/v1");
    frame_str(&mut hasher, &space_id.to_string());
    frame_str(&mut hasher, observation_id);
    frame_optional_str(&mut hasher, expected_revision_id);
    frame_u64(&mut hasher, expected_row_version);
    frame_u64(&mut hasher, revision_count);
    frame_u64(&mut hasher, side_table_row_count);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn frame(hasher: &mut Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn frame_str(hasher: &mut Hasher, value: &str) {
    frame(hasher, value.as_bytes());
}

fn frame_u16(hasher: &mut Hasher, value: u16) {
    frame(hasher, &value.to_le_bytes());
}

fn frame_u64(hasher: &mut Hasher, value: u64) {
    frame(hasher, &value.to_le_bytes());
}

fn frame_i64(hasher: &mut Hasher, value: i64) {
    frame(hasher, &value.to_le_bytes());
}

fn frame_f32(hasher: &mut Hasher, value: f32) {
    frame(hasher, &value.to_bits().to_le_bytes());
}

fn frame_optional_i64(hasher: &mut Hasher, value: Option<i64>) {
    match value {
        Some(value) => {
            frame(hasher, &[1]);
            frame_i64(hasher, value);
        }
        None => frame(hasher, &[0]),
    }
}

fn frame_optional_str(hasher: &mut Hasher, value: Option<&str>) {
    match value {
        Some(value) => {
            frame(hasher, &[1]);
            frame_str(hasher, value);
        }
        None => frame(hasher, &[0]),
    }
}

fn frame_strings(hasher: &mut Hasher, values: &[String]) {
    frame_u64(hasher, values.len() as u64);
    for value in values {
        frame_str(hasher, value);
    }
}

fn frame_sorted_strings(hasher: &mut Hasher, values: &[String]) {
    let mut values = values.to_vec();
    values.sort();
    frame_strings(hasher, &values);
}

fn actor_kind_str(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

fn hash_operation(hasher: &mut Hasher, operation: &ChangeOperation) {
    match operation {
        ChangeOperation::Remember {
            entity_name,
            entity_type,
            observations,
            relations,
        } => {
            frame_str(hasher, "remember");
            frame_str(hasher, entity_name);
            frame_str(hasher, entity_type.as_str());
            frame_u64(hasher, observations.len() as u64);
            for observation in observations {
                frame_str(hasher, &observation.content);
                frame_f32(hasher, observation.confidence);
                frame_str(hasher, &observation.source);
                frame_optional_i64(hasher, observation.valid_from);
                frame_optional_i64(hasher, observation.valid_until);
                frame_str(hasher, observation.memory_tier.as_str());
                frame_optional_str(hasher, observation.title.as_deref());
                frame_optional_str(hasher, observation.summary.as_deref());
                match observation.importance {
                    Some(value) => {
                        frame(hasher, &[1]);
                        frame_f32(hasher, value);
                    }
                    None => frame(hasher, &[0]),
                }
                frame_optional_str(hasher, observation.source_kind.as_deref());
                frame_sorted_strings(hasher, &observation.concepts);
                frame_sorted_strings(hasher, &observation.source_files);
            }
            frame_u64(hasher, relations.len() as u64);
            for relation in relations {
                frame_str(hasher, &relation.relation_type);
                frame_str(hasher, &relation.target_name);
                frame_str(hasher, relation.target_type.as_str());
                frame_f32(hasher, relation.weight);
                frame_str(hasher, &relation.source);
            }
        }
        ChangeOperation::UpdateObservation {
            observation,
            expected_row_version,
            expected_revision_id,
        } => {
            frame_str(hasher, "update_observation");
            frame_str(hasher, &observation.id);
            frame_str(hasher, &observation.entity_id);
            frame_str(hasher, &observation.content);
            frame_i64(hasher, observation.observed_at);
            frame_optional_i64(hasher, observation.valid_from);
            frame_optional_i64(hasher, observation.valid_until);
            frame_f32(hasher, observation.confidence);
            frame_str(hasher, &observation.source);
            frame(hasher, &[u8::from(observation.tombstoned)]);
            frame_str(hasher, observation.memory_tier.as_str());
            frame_optional_str(hasher, observation.title.as_deref());
            frame_optional_str(hasher, observation.summary.as_deref());
            match observation.importance {
                Some(value) => {
                    frame(hasher, &[1]);
                    frame_f32(hasher, value);
                }
                None => frame(hasher, &[0]),
            }
            frame_optional_str(hasher, observation.source_kind.as_deref());
            frame_sorted_strings(hasher, &observation.concepts);
            frame_sorted_strings(hasher, &observation.source_files);
            frame_u64(hasher, *expected_row_version);
            frame_optional_str(hasher, expected_revision_id.as_deref());
        }
        ChangeOperation::RetireObservation {
            observation_id,
            expected_row_version,
        } => {
            frame_str(hasher, "retire_observation");
            frame_str(hasher, observation_id);
            frame_u64(hasher, *expected_row_version);
        }
        ChangeOperation::RestoreObservation {
            observation_id,
            expected_row_version,
        } => {
            frame_str(hasher, "restore_observation");
            frame_str(hasher, observation_id);
            frame_u64(hasher, *expected_row_version);
        }
        ChangeOperation::RevertObservation {
            observation_id,
            revision_id,
            expected_row_version,
        } => {
            frame_str(hasher, "revert_observation");
            frame_str(hasher, observation_id);
            frame_str(hasher, revision_id);
            frame_u64(hasher, *expected_row_version);
        }
        ChangeOperation::DestroyObservation {
            observation_id,
            expected_row_version,
            expected_revision_id,
            confirmation_hash,
        } => {
            frame_str(hasher, "destroy_observation");
            frame_str(hasher, observation_id);
            frame_u64(hasher, *expected_row_version);
            frame_optional_str(hasher, expected_revision_id.as_deref());
            frame_str(hasher, confirmation_hash);
        }
    }
}

fn ensure_bound_space(tx: &Transaction<'_>, space_id: SpaceId) -> MemoryResult<()> {
    let existing = bound_space_id(tx)?;
    if existing != space_id {
        Err(MemoryError::InvalidInput(format!(
            "memory store is bound to space {existing}, not {space_id}"
        )))
    } else {
        Ok(())
    }
}

fn bound_space_id(tx: &Transaction<'_>) -> MemoryResult<SpaceId> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT value FROM memory_meta WHERE key = 'space_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        Some(value) => {
            SpaceId::from_str(&value).map_err(|error| MemoryError::Schema(error.to_string()))
        }
        None => Err(MemoryError::InvalidInput(
            "memory store is not bound to a catalog space".to_owned(),
        )),
    }
}

fn current_generation(tx: &Transaction<'_>) -> MemoryResult<u64> {
    tx.query_row(
        "SELECT semantic_generation FROM domain_state WHERE id = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?
    .try_into()
    .map_err(|_| MemoryError::Schema("negative semantic generation".into()))
}

fn current_index_generation(tx: &Transaction<'_>) -> MemoryResult<u64> {
    tx.query_row(
        "SELECT indexed_generation FROM domain_state WHERE id = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?
    .try_into()
    .map_err(|_| MemoryError::Schema("negative indexed generation".into()))
}

fn insert_requests(
    tx: &Transaction<'_>,
    changeset_id: openmemory_core::space::ChangeSetId,
    operations: &[ChangeOperation],
) -> MemoryResult<()> {
    for (ordinal, operation) in operations.iter().enumerate() {
        let payload = serde_json::to_string(operation)?;
        tx.execute(
            "INSERT INTO change_requests(
                 changeset_id, ordinal, object_kind, logical_id, operation,
                 payload_version, payload_json, expected_revision_id,
                 expected_row_version, expected_lifecycle
             ) VALUES(?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9)",
            params![
                changeset_id.to_string(),
                i64::try_from(ordinal)
                    .map_err(|_| MemoryError::InvalidInput("operation ordinal overflow".into()))?,
                operation.object_kind().as_str(),
                operation.logical_id(),
                operation.operation_name(),
                payload,
                expected_revision_id(operation),
                operation
                    .expected_row_version()
                    .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
                match operation {
                    ChangeOperation::RetireObservation { .. } => Some("active"),
                    ChangeOperation::RestoreObservation { .. } => Some("retired"),
                    _ => None,
                },
            ],
        )?;
    }
    Ok(())
}

fn expected_revision_id(operation: &ChangeOperation) -> Option<String> {
    match operation {
        ChangeOperation::UpdateObservation {
            expected_revision_id,
            ..
        }
        | ChangeOperation::DestroyObservation {
            expected_revision_id,
            ..
        } => expected_revision_id.clone(),
        _ => None,
    }
}

fn load_existing_receipt(
    tx: &Transaction<'_>,
    draft: &ChangeSetDraft,
    request_hash: &str,
) -> MemoryResult<Option<ChangeSetReceipt>> {
    let row: Option<(String, String, i64, i64)> = tx
        .query_row(
            "SELECT id, request_hash, state_version, COALESCE(committed_generation, base_generation)
             FROM change_sets WHERE space_id = ?1 AND idempotency_key = ?2",
            params![draft.space_id.to_string(), draft.idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((id, existing_hash, state_version, generation)) = row else {
        return Ok(None);
    };
    if existing_hash != request_hash {
        return Err(MemoryError::ChangeSetIdempotencyConflict);
    }
    let state: String = tx.query_row(
        "SELECT state FROM change_sets WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    let indexed = current_index_generation(tx)?;
    let repair: i64 = tx.query_row(
        "SELECT index_repair_required FROM domain_state WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    Ok(Some(ChangeSetReceipt {
        id,
        state: ChangeSetState::parse(&state)?,
        state_version: state_version as u64,
        semantic_generation: generation as u64,
        indexed_generation: indexed,
        index_repair_required: repair != 0,
    }))
}

fn verify_proposal_hash(
    tx: &Transaction<'_>,
    id: &str,
    operations: &[ChangeOperation],
) -> MemoryResult<()> {
    let (space_id, actor_kind, actor_id, authority, reason, source, request_hash): (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = tx.query_row(
        "SELECT space_id, actor_kind, actor_id, authority_snapshot, reason, source, request_hash
         FROM change_sets WHERE id = ?1 AND submit_mode = 'propose'",
        params![id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    let draft = ChangeSetDraft {
        idempotency_key: tx.query_row(
            "SELECT idempotency_key FROM change_sets WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?,
        space_id: SpaceId::from_str(&space_id)
            .map_err(|error| MemoryError::Schema(error.to_string()))?,
        actor_kind: serde_json::from_str(&actor_kind)?,
        actor_id,
        authority: AuthoritySnapshot::from_str(&authority)
            .map_err(|error| MemoryError::Schema(error.to_string()))?,
        reason,
        source,
        operations: operations.to_vec(),
    };
    validate_draft(&draft)?;
    if canonical_request_hash(&draft, SubmitMode::Propose) != request_hash {
        return Err(MemoryError::Schema(
            "proposal payload does not match its canonical request hash".into(),
        ));
    }
    Ok(())
}

fn operation_precondition_holds(
    tx: &Transaction<'_>,
    operation: &ChangeOperation,
) -> MemoryResult<bool> {
    match operation {
        ChangeOperation::Remember { .. } => Ok(true),
        ChangeOperation::UpdateObservation {
            observation,
            expected_row_version,
            expected_revision_id,
        } => Ok(load_observation(tx, &observation.id)?.is_some_and(|row| {
            row.row_version == *expected_row_version
                && row.lifecycle == "active"
                && row.observation.entity_id == observation.entity_id
                && !observation.tombstoned
                && expected_revision_id
                    .as_ref()
                    .is_none_or(|id| row.current_revision_id.as_ref() == Some(id))
        })),
        ChangeOperation::RetireObservation {
            observation_id,
            expected_row_version,
        } => Ok(load_observation(tx, observation_id)?.is_some_and(|row| {
            row.row_version == *expected_row_version && row.lifecycle == "active"
        })),
        ChangeOperation::RestoreObservation {
            observation_id,
            expected_row_version,
        } => Ok(load_observation(tx, observation_id)?.is_some_and(|row| {
            row.row_version == *expected_row_version && row.lifecycle == "retired"
        })),
        ChangeOperation::RevertObservation {
            observation_id,
            revision_id,
            expected_row_version,
        } => {
            let head_matches = load_observation(tx, observation_id)?
                .is_some_and(|row| row.row_version == *expected_row_version);
            let revision_exists: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM observation_revisions
                     WHERE revision_id = ?1 AND observation_id = ?2
                 )",
                params![revision_id, observation_id],
                |row| row.get(0),
            )?;
            Ok(head_matches && revision_exists)
        }
        ChangeOperation::DestroyObservation {
            observation_id,
            expected_row_version,
            expected_revision_id,
            ..
        } => Ok(load_observation(tx, observation_id)?.is_some_and(|row| {
            row.row_version == *expected_row_version
                && expected_revision_id
                    .as_ref()
                    .is_none_or(|id| row.current_revision_id.as_ref() == Some(id))
        })),
    }
}

fn proposal_header(tx: &Transaction<'_>, id: &str) -> MemoryResult<(ChangeSetState, u64, SpaceId)> {
    let (state, version, space): (String, i64, String) = tx.query_row(
        "SELECT state, state_version, space_id FROM change_sets WHERE id = ?1",
        params![id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok((
        ChangeSetState::parse(&state)?,
        version as u64,
        SpaceId::from_str(&space)
            .map_err(|_| MemoryError::Schema("invalid changeset space id".into()))?,
    ))
}

fn ensure_distinct_reviewer(
    tx: &Transaction<'_>,
    id: &str,
    review: &ReviewAuthorization,
) -> MemoryResult<()> {
    let submitter: String = tx.query_row(
        "SELECT actor_id FROM change_sets WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    if submitter == review.actor_id {
        return Err(MemoryError::InvalidInput(
            "proposal submitters cannot review their own changeset".into(),
        ));
    }
    Ok(())
}

fn insert_review_event(
    tx: &Transaction<'_>,
    id: &str,
    event: &str,
    review: &ReviewAuthorization,
    reason: &str,
    now: i64,
) -> MemoryResult<()> {
    tx.execute(
        "INSERT INTO change_events(changeset_id, ordinal, event_type, payload_json, created_at)
         SELECT ?1, COALESCE(MAX(ordinal), -1) + 1, ?2, ?3, ?4 FROM change_events WHERE changeset_id = ?1",
        params![
            id,
            event,
            serde_json::json!({
                "actor": review.actor_id,
                "role": review.role,
                "authority": review.authority,
                "reason": reason
            })
            .to_string(),
            now
        ],
    )?;
    Ok(())
}

fn apply_operation_tx(
    tx: &Transaction<'_>,
    changeset_id: openmemory_core::space::ChangeSetId,
    operation: &ChangeOperation,
    generation: u64,
    ordinal: u64,
    now: i64,
    store: &MemoryStore,
) -> MemoryResult<()> {
    match operation {
        ChangeOperation::Remember {
            entity_name,
            entity_type,
            observations,
            relations,
        } => {
            let group = write_group(
                tx,
                entity_name,
                *entity_type,
                observations,
                relations,
                "changeset",
                now,
                store.normalization_params(true),
            )?;
            for (index, observation_id) in group.outcome.observation_ids.iter().enumerate() {
                let row = load_observation(tx, observation_id)?
                    .ok_or_else(|| MemoryError::ObservationNotFound(observation_id.clone()))?;
                let revision = insert_observation_revision(tx, &row, None, changeset_id, now)?;
                tx.execute(
                    "UPDATE observations SET current_revision_id = ?1 WHERE id = ?2",
                    params![revision, observation_id],
                )?;
                insert_outbox(
                    tx,
                    generation,
                    scoped_ordinal(ordinal, index as u64),
                    "upsert",
                    observation_id,
                    Some(&revision),
                    now,
                )?;
                insert_operation_event(
                    tx,
                    changeset_id,
                    scoped_ordinal(ordinal, index as u64),
                    "remembered",
                    observation_id,
                    Some(&revision),
                    now,
                )?;
            }
        }
        ChangeOperation::UpdateObservation {
            observation,
            expected_row_version,
            expected_revision_id,
        } => {
            let current = load_observation(tx, &observation.id)?
                .ok_or_else(|| MemoryError::ObservationNotFound(observation.id.clone()))?;
            check_expected(
                &current,
                *expected_row_version,
                expected_revision_id.as_deref(),
                "active",
            )?;
            if observation.entity_id != current.observation.entity_id || observation.tombstoned {
                return Err(MemoryError::InvalidInput(
                    "an observation update cannot change identity or lifecycle".into(),
                ));
            }
            replace_observation(tx, observation)?;
            let revision = insert_observation_revision(
                tx,
                &load_observation(tx, &observation.id)?
                    .ok_or_else(|| MemoryError::ObservationNotFound(observation.id.clone()))?,
                current.current_revision_id.as_deref(),
                changeset_id,
                now,
            )?;
            let changed = tx.execute(
                "UPDATE observations SET current_revision_id = ?1, row_version = row_version + 1 WHERE id = ?2 AND row_version = ?3",
                params![revision, observation.id, i64::try_from(*expected_row_version).unwrap_or(i64::MAX)],
            )?;
            if changed != 1 {
                return Err(MemoryError::ChangeSetStale);
            }
            insert_outbox(
                tx,
                generation,
                scoped_ordinal(ordinal, 0),
                "upsert",
                &observation.id,
                Some(&revision),
                now,
            )?;
            insert_operation_event(
                tx,
                changeset_id,
                scoped_ordinal(ordinal, 0),
                "updated",
                &observation.id,
                Some(&revision),
                now,
            )?;
        }
        ChangeOperation::RetireObservation {
            observation_id,
            expected_row_version,
        } => {
            apply_lifecycle(
                tx,
                observation_id,
                *expected_row_version,
                "retired",
                true,
                changeset_id,
                generation,
                ordinal,
                now,
            )?;
        }
        ChangeOperation::RestoreObservation {
            observation_id,
            expected_row_version,
        } => {
            apply_lifecycle(
                tx,
                observation_id,
                *expected_row_version,
                "active",
                false,
                changeset_id,
                generation,
                ordinal,
                now,
            )?;
        }
        ChangeOperation::RevertObservation {
            observation_id,
            revision_id,
            expected_row_version,
        } => {
            let current = load_observation(tx, observation_id)?
                .ok_or_else(|| MemoryError::ObservationNotFound(observation_id.clone()))?;
            if current.row_version != *expected_row_version {
                return Err(MemoryError::ChangeSetStale);
            }
            let historical = observation_from_revision(tx, revision_id, observation_id)?;
            replace_observation(tx, &historical)?;
            let refreshed = load_observation(tx, observation_id)?
                .ok_or_else(|| MemoryError::ObservationNotFound(observation_id.clone()))?;
            let revision = insert_observation_revision(
                tx,
                &refreshed,
                current.current_revision_id.as_deref(),
                changeset_id,
                now,
            )?;
            tx.execute("UPDATE observations SET current_revision_id = ?1, row_version = row_version + 1 WHERE id = ?2 AND row_version = ?3", params![revision, observation_id, i64::try_from(*expected_row_version).unwrap_or(i64::MAX)])?;
            insert_outbox(
                tx,
                generation,
                scoped_ordinal(ordinal, 0),
                "upsert",
                observation_id,
                Some(&revision),
                now,
            )?;
            insert_operation_event(
                tx,
                changeset_id,
                scoped_ordinal(ordinal, 0),
                "reverted",
                observation_id,
                Some(&revision),
                now,
            )?;
        }
        ChangeOperation::DestroyObservation {
            observation_id,
            expected_row_version,
            expected_revision_id,
            confirmation_hash,
        } => {
            let current = load_observation(tx, observation_id)?
                .ok_or_else(|| MemoryError::ObservationNotFound(observation_id.clone()))?;
            check_expected(
                &current,
                *expected_row_version,
                expected_revision_id.as_deref(),
                &current.lifecycle,
            )?;
            let confirmation: (
                String,
                String,
                Option<String>,
                i64,
                String,
                String,
                String,
                i64,
                Option<i64>,
            ) = tx.query_row(
                "SELECT space_id, logical_id, expected_revision_id,
                        expected_row_version, preview_hash, actor_id,
                        authority_snapshot, expires_at, consumed_at
                 FROM destruction_confirmations WHERE token_hash = ?1",
                params![confirmation_hash],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )?;
            let changeset: (String, String, String) = tx.query_row(
                "SELECT space_id, actor_id, authority_snapshot
                 FROM change_sets WHERE id = ?1",
                params![changeset_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if confirmation.0 != changeset.0
                || confirmation.1 != *observation_id
                || confirmation.2 != *expected_revision_id
                || confirmation.3 != i64::try_from(*expected_row_version).unwrap_or(i64::MAX)
                || confirmation.5 != changeset.1
                || confirmation.6 != changeset.2
                || confirmation.7 < now
                || confirmation.8.is_some()
            {
                return Err(MemoryError::InvalidInput(
                    "destruction confirmation is stale, consumed, or mismatched".to_owned(),
                ));
            }

            let mut destroyed = current.clone();
            destroyed.observation.tombstoned = true;
            "destroyed".clone_into(&mut destroyed.lifecycle);
            let revision = insert_observation_revision(
                tx,
                &destroyed,
                current.current_revision_id.as_deref(),
                changeset_id,
                now,
            )?;
            insert_outbox(
                tx,
                generation,
                scoped_ordinal(ordinal, 0),
                "delete",
                observation_id,
                Some(&revision),
                now,
            )?;
            insert_operation_event(
                tx,
                changeset_id,
                scoped_ordinal(ordinal, 0),
                "destroyed",
                observation_id,
                Some(&revision),
                now,
            )?;
            let deleted = tx.execute(
                "DELETE FROM observations WHERE id = ?1 AND row_version = ?2",
                params![
                    observation_id,
                    i64::try_from(*expected_row_version).unwrap_or(i64::MAX)
                ],
            )?;
            if deleted != 1 {
                return Err(MemoryError::ChangeSetStale);
            }
            let consumed = tx.execute(
                "UPDATE destruction_confirmations SET consumed_at = ?1
                 WHERE token_hash = ?2 AND consumed_at IS NULL",
                params![now, confirmation_hash],
            )?;
            if consumed != 1 {
                return Err(MemoryError::InvalidInput(
                    "destruction confirmation was already consumed".to_owned(),
                ));
            }
            tx.execute(
                "INSERT INTO destruction_receipts(
                     changeset_id, space_id, object_kind, logical_id,
                     preview_hash, actor_id, destroyed_at
                 ) VALUES(?1, ?2, 'observation', ?3, ?4, ?5, ?6)",
                params![
                    changeset_id.to_string(),
                    confirmation.0,
                    observation_id,
                    confirmation.4,
                    confirmation.5,
                    now,
                ],
            )?;
        }
    }
    Ok(())
}

fn check_expected(
    row: &ObservationRow,
    expected_version: u64,
    expected_revision: Option<&str>,
    lifecycle: &str,
) -> MemoryResult<()> {
    if row.row_version != expected_version
        || row.lifecycle != lifecycle
        || expected_revision.is_some_and(|id| row.current_revision_id.as_deref() != Some(id))
    {
        return Err(MemoryError::ChangeSetStale);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_lifecycle(
    tx: &Transaction<'_>,
    id: &str,
    expected_version: u64,
    lifecycle: &str,
    tombstoned: bool,
    changeset_id: openmemory_core::space::ChangeSetId,
    generation: u64,
    ordinal: u64,
    now: i64,
) -> MemoryResult<()> {
    let current =
        load_observation(tx, id)?.ok_or_else(|| MemoryError::ObservationNotFound(id.to_owned()))?;
    let expected_lifecycle = if tombstoned { "active" } else { "retired" };
    if current.row_version != expected_version || current.lifecycle != expected_lifecycle {
        return Err(MemoryError::ChangeSetStale);
    }
    tx.execute(
        "UPDATE observations SET tombstoned = ?1, lifecycle = ?2
         WHERE id = ?3 AND row_version = ?4",
        params![
            i64::from(tombstoned),
            lifecycle,
            id,
            i64::try_from(expected_version).unwrap_or(i64::MAX)
        ],
    )?;
    let refreshed =
        load_observation(tx, id)?.ok_or_else(|| MemoryError::ObservationNotFound(id.to_owned()))?;
    let revision = insert_observation_revision(
        tx,
        &refreshed,
        current.current_revision_id.as_deref(),
        changeset_id,
        now,
    )?;
    tx.execute("UPDATE observations SET current_revision_id = ?1, row_version = row_version + 1 WHERE id = ?2 AND row_version = ?3", params![revision, id, i64::try_from(expected_version).unwrap_or(i64::MAX)])?;
    insert_outbox(
        tx,
        generation,
        scoped_ordinal(ordinal, 0),
        if tombstoned { "delete" } else { "upsert" },
        id,
        Some(&revision),
        now,
    )?;
    insert_operation_event(
        tx,
        changeset_id,
        scoped_ordinal(ordinal, 0),
        if tombstoned { "retired" } else { "restored" },
        id,
        Some(&revision),
        now,
    )
}

const fn scoped_ordinal(operation_ordinal: u64, item_ordinal: u64) -> u64 {
    operation_ordinal
        .saturating_mul((MAX_LIST_ITEMS as u64) + 1)
        .saturating_add(item_ordinal)
}

fn insert_operation_event(
    tx: &Transaction<'_>,
    changeset_id: openmemory_core::space::ChangeSetId,
    ordinal: u64,
    event_type: &str,
    logical_id: &str,
    revision_id: Option<&str>,
    now: i64,
) -> MemoryResult<()> {
    tx.execute(
        "INSERT INTO change_events(
             changeset_id, ordinal, event_type, object_kind, logical_id,
             revision_id, payload_json, created_at
         ) VALUES(?1, ?2, ?3, 'observation', ?4, ?5, '{}', ?6)",
        params![
            changeset_id.to_string(),
            i64::try_from(ordinal)
                .map_err(|_| MemoryError::InvalidInput("event ordinal overflow".into()))?,
            event_type,
            logical_id,
            revision_id,
            now
        ],
    )?;
    Ok(())
}

fn replace_observation(tx: &Transaction<'_>, observation: &Observation) -> MemoryResult<()> {
    tx.execute("UPDATE observations SET entity_id = ?1, content = ?2, observed_at = ?3, valid_from = ?4, valid_until = ?5, confidence = ?6, source = ?7, tombstoned = ?8, memory_tier = ?9, title = ?10, summary = ?11, importance = ?12, source_kind = ?13, lifecycle = ?14 WHERE id = ?15", params![observation.entity_id, observation.content, observation.observed_at, observation.valid_from, observation.valid_until, observation.confidence, observation.source, i64::from(observation.tombstoned), observation.memory_tier.as_str(), observation.title, observation.summary, observation.importance, observation.source_kind, if observation.tombstoned { "retired" } else { "active" }, observation.id])?;
    tx.execute(
        "DELETE FROM observation_concepts WHERE observation_id = ?1",
        params![observation.id],
    )?;
    tx.execute(
        "DELETE FROM observation_source_files WHERE observation_id = ?1",
        params![observation.id],
    )?;
    for concept in &observation.concepts {
        tx.execute(
            "INSERT INTO observation_concepts(observation_id, concept) VALUES(?1, ?2)",
            params![observation.id, concept],
        )?;
    }
    for file in &observation.source_files {
        tx.execute(
            "INSERT INTO observation_source_files(observation_id, file_path) VALUES(?1, ?2)",
            params![observation.id, file],
        )?;
    }
    Ok(())
}

fn load_observation(conn: &rusqlite::Connection, id: &str) -> MemoryResult<Option<ObservationRow>> {
    let row: Option<(Observation, Option<String>, i64, String)> = conn.query_row("SELECT id, entity_id, content, observed_at, valid_from, valid_until, confidence, source, tombstoned, access_count, memory_tier, title, summary, importance, source_kind, current_revision_id, row_version, lifecycle FROM observations WHERE id = ?1", params![id], |row| {
        let tier: String = row.get(10)?;
        Ok((Observation { id: row.get(0)?, entity_id: row.get(1)?, content: row.get(2)?, observed_at: row.get(3)?, valid_from: row.get(4)?, valid_until: row.get(5)?, confidence: row.get(6)?, source: row.get(7)?, tombstoned: row.get::<_, i64>(8)? != 0, access_count: row.get::<_, i64>(9)? as u32, memory_tier: MemoryTier::parse(&tier).unwrap_or(MemoryTier::Episodic), title: row.get(11)?, summary: row.get(12)?, importance: row.get(13)?, source_kind: row.get(14)?, concepts: Vec::new(), source_files: Vec::new() }, row.get(15)?, row.get(16)?, row.get(17)?))
    }).optional()?;
    let Some((mut observation, current_revision_id, row_version, lifecycle)) = row else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT concept FROM observation_concepts WHERE observation_id = ?1 ORDER BY concept",
    )?;
    observation.concepts = stmt
        .query_map(params![id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut stmt = conn.prepare("SELECT file_path FROM observation_source_files WHERE observation_id = ?1 ORDER BY file_path")?;
    observation.source_files = stmt
        .query_map(params![id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(ObservationRow {
        observation,
        current_revision_id,
        row_version: row_version as u64,
        lifecycle,
    }))
}

#[allow(clippy::type_complexity)]
fn observation_from_revision(
    tx: &Transaction<'_>,
    revision_id: &str,
    observation_id: &str,
) -> MemoryResult<Observation> {
    let (entity_id, content, title, summary, concepts, source_files, observed_at, valid_from, valid_until, confidence, source, source_kind, importance, tier, lifecycle): (String, String, Option<String>, Option<String>, String, String, i64, Option<i64>, Option<i64>, f32, String, Option<String>, Option<f32>, String, String) = tx.query_row("SELECT entity_id, content, title, summary, concepts_json, source_files_json, observed_at, valid_from, valid_until, confidence, source, source_kind, importance, memory_tier, lifecycle FROM observation_revisions WHERE revision_id = ?1 AND observation_id = ?2", params![revision_id, observation_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?, row.get(12)?, row.get(13)?, row.get(14)?)))?;
    Ok(Observation {
        id: observation_id.to_owned(),
        entity_id,
        content,
        observed_at,
        valid_from,
        valid_until,
        confidence,
        source,
        tombstoned: lifecycle != "active",
        access_count: 0,
        memory_tier: MemoryTier::parse(&tier).unwrap_or(MemoryTier::Episodic),
        title,
        summary,
        importance,
        source_kind,
        concepts: serde_json::from_str(&concepts)?,
        source_files: serde_json::from_str(&source_files)?,
    })
}

fn insert_observation_revision(
    tx: &Transaction<'_>,
    row: &ObservationRow,
    parent: Option<&str>,
    changeset_id: openmemory_core::space::ChangeSetId,
    now: i64,
) -> MemoryResult<String> {
    let revision = openmemory_core::space::RevisionId::new();
    let semantic_hash = observation_hash(&row.observation, &row.lifecycle);
    tx.execute("INSERT INTO observation_revisions(revision_id, observation_id, entity_id, parent_revision_id, changeset_id, content, title, summary, concepts_json, source_files_json, observed_at, valid_from, valid_until, confidence, source, source_kind, importance, memory_tier, semantic_hash, created_at, lifecycle) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)", params![revision.to_string(), row.observation.id, row.observation.entity_id, parent, changeset_id.to_string(), row.observation.content, row.observation.title, row.observation.summary, serde_json::to_string(&row.observation.concepts)?, serde_json::to_string(&row.observation.source_files)?, row.observation.observed_at, row.observation.valid_from, row.observation.valid_until, row.observation.confidence, row.observation.source, row.observation.source_kind, row.observation.importance, row.observation.memory_tier.as_str(), semantic_hash, now, row.lifecycle])?;
    Ok(revision.to_string())
}

fn observation_hash(observation: &Observation, lifecycle: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"openmemory/observation/v1");
    frame_str(&mut hasher, &observation.entity_id);
    frame_str(&mut hasher, &observation.content);
    frame_optional_str(&mut hasher, observation.title.as_deref());
    frame_optional_str(&mut hasher, observation.summary.as_deref());
    frame_str(&mut hasher, &observation.source);
    frame_optional_str(&mut hasher, observation.source_kind.as_deref());
    frame_str(&mut hasher, observation.memory_tier.as_str());
    frame_str(&mut hasher, lifecycle);
    frame_i64(&mut hasher, observation.observed_at);
    frame_optional_i64(&mut hasher, observation.valid_from);
    frame_optional_i64(&mut hasher, observation.valid_until);
    hasher.update(&observation.confidence.to_bits().to_le_bytes());
    match observation.importance {
        Some(importance) => {
            frame(&mut hasher, &[1]);
            frame_f32(&mut hasher, importance);
        }
        None => frame(&mut hasher, &[0]),
    }
    let mut concepts = observation.concepts.clone();
    concepts.sort();
    let mut files = observation.source_files.clone();
    files.sort();
    frame_u64(&mut hasher, concepts.len() as u64);
    for value in concepts {
        frame_str(&mut hasher, &value);
    }
    frame_u64(&mut hasher, files.len() as u64);
    for value in files {
        frame_str(&mut hasher, &value);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn insert_outbox(
    tx: &Transaction<'_>,
    generation: u64,
    ordinal: u64,
    operation: &str,
    logical_id: &str,
    revision: Option<&str>,
    now: i64,
) -> MemoryResult<()> {
    tx.execute("INSERT INTO index_outbox(semantic_generation, ordinal, operation, object_kind, logical_id, revision_id, created_at) VALUES(?1, ?2, ?3, 'observation', ?4, ?5, ?6)", params![i64::try_from(generation).unwrap_or(i64::MAX), i64::try_from(ordinal).unwrap_or(i64::MAX), operation, logical_id, revision, now])?;
    Ok(())
}

/// Record a compatibility `remember` inside the caller's existing graph
/// transaction.  This is the bridge that keeps the legacy API on the same
/// audited projection without opening a second transaction or changing its
/// return shape.
pub(crate) fn record_legacy_remembers(
    tx: &Transaction<'_>,
    records: &[(&crate::remember::RememberOutcome, ChangeOperation)],
    change_source: &str,
    now: i64,
) -> MemoryResult<u64> {
    let changeset_id = openmemory_core::space::ChangeSetId::new();
    let space_id: Option<String> = tx
        .query_row(
            "SELECT value FROM memory_meta WHERE key = 'space_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let space_id = if let Some(space_id) = space_id {
        SpaceId::from_str(&space_id).map_err(|error| MemoryError::Schema(error.to_string()))?
    } else {
        let space_id = SpaceId::new();
        tx.execute(
            "INSERT INTO memory_meta(key, value) VALUES('space_id', ?1)",
            params![space_id.to_string()],
        )?;
        space_id
    };
    let authority = AuthoritySnapshot::from_generations(1, &[0])
        .map_err(|error| MemoryError::InvalidInput(error.to_string()))?;
    let draft = ChangeSetDraft {
        idempotency_key: format!("legacy:{changeset_id}"),
        space_id,
        actor_kind: ActorKind::System,
        actor_id: "legacy".to_owned(),
        authority,
        reason: "legacy remember".to_owned(),
        source: if change_source.is_empty() {
            "compatibility".to_owned()
        } else {
            change_source.to_owned()
        },
        operations: records
            .iter()
            .map(|(_, operation)| operation.clone())
            .collect(),
    };
    validate_draft(&draft)?;
    let generation = current_generation(tx)?
        .checked_add(1)
        .ok_or_else(|| MemoryError::InvalidInput("generation overflow".into()))?;
    let request_hash = canonical_request_hash(&draft, SubmitMode::ApplyImmediately);
    tx.execute(
        "INSERT INTO change_sets(
             id, space_id, idempotency_key, request_version, request_hash,
             submit_mode, actor_kind, actor_id, authority_snapshot, reason,
             source, base_generation, committed_generation, state, state_version,
             created_at, updated_at
         ) VALUES(?1, ?2, ?3, 1, ?4, 'apply_immediately', ?5,
                  'legacy', ?6, 'legacy remember', ?7, ?8, ?9,
                  'applied', 1, ?10, ?10)",
        params![
            changeset_id.to_string(),
            space_id.to_string(),
            draft.idempotency_key,
            request_hash,
            serde_json::to_string(&draft.actor_kind)?,
            draft.authority.to_string(),
            draft.source,
            i64::try_from(generation - 1).unwrap_or(i64::MAX),
            i64::try_from(generation).unwrap_or(i64::MAX),
            now,
        ],
    )?;
    insert_requests(tx, changeset_id, &draft.operations)?;
    for (operation_ordinal, (outcome, _)) in records.iter().enumerate() {
        for (item_ordinal, observation_id) in outcome.observation_ids.iter().enumerate() {
            let row = load_observation(tx, observation_id)?
                .ok_or_else(|| MemoryError::ObservationNotFound(observation_id.clone()))?;
            let revision = insert_observation_revision(tx, &row, None, changeset_id, now)?;
            tx.execute(
                "UPDATE observations SET current_revision_id = ?1 WHERE id = ?2",
                params![revision, observation_id],
            )?;
            let ordinal = scoped_ordinal(operation_ordinal as u64, item_ordinal as u64);
            insert_outbox(
                tx,
                generation,
                ordinal,
                "upsert",
                observation_id,
                Some(&revision),
                now,
            )?;
            insert_operation_event(
                tx,
                changeset_id,
                ordinal,
                "remembered",
                observation_id,
                Some(&revision),
                now,
            )?;
        }
    }
    tx.execute(
        "UPDATE domain_state SET semantic_generation = ?1,
             index_repair_required = CASE
                 WHEN EXISTS(SELECT 1 FROM index_outbox WHERE applied_at IS NULL)
                 THEN 1 ELSE index_repair_required END,
             updated_at = ?2 WHERE id = 1",
        params![i64::try_from(generation).unwrap_or(i64::MAX), now],
    )?;
    Ok(generation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_core::config::Config;
    use std::sync::Arc;

    fn authority() -> AuthoritySnapshot {
        AuthoritySnapshot::from_generations(1, &[1, 2, 3]).unwrap()
    }

    fn review() -> ReviewAuthorization {
        ReviewAuthorization::new(
            ActorKind::Human,
            "reviewer",
            SpaceRole::Reviewer,
            authority(),
        )
        .unwrap()
    }

    fn maintainer() -> ReviewAuthorization {
        ReviewAuthorization::new(
            ActorKind::Human,
            "maintainer",
            SpaceRole::Maintainer,
            authority(),
        )
        .unwrap()
    }

    fn store() -> (MemoryStore, SpaceId) {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        let space = SpaceId::new();
        store.bind_space_id(space).unwrap();
        (store, space)
    }

    fn file_store() -> (tempfile::TempDir, MemoryStore, SpaceId) {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.default.jobs = 1;
        let store = MemoryStore::open(&config, temp.path()).unwrap();
        let space = SpaceId::new();
        store.bind_space_id(space).unwrap();
        (temp, store, space)
    }

    #[test]
    fn proposal_is_invisible_until_approval_and_is_idempotent() {
        let (store, space) = store();
        let draft = ChangeSetDraft::new(
            "proposal-1",
            space,
            ActorKind::Agent,
            "proposer",
            authority(),
            "capture fact",
            "test",
            vec![ChangeOperation::Remember {
                entity_name: "Ada".into(),
                entity_type: EntityType::Person,
                observations: vec![ObservationInput::new("writes Rust")],
                relations: vec![],
            }],
        )
        .unwrap();
        let proposed = store
            .submit_changeset(draft.clone(), SubmitMode::Propose)
            .unwrap();
        assert_eq!(proposed.state, ChangeSetState::Proposed);
        assert_eq!(store.status().unwrap().total_observations, 0);

        let approved = store.approve_changeset(&proposed.id, 1, review()).unwrap();
        assert_eq!(approved.state, ChangeSetState::Applied);
        assert_eq!(store.status().unwrap().total_observations, 1);
        let retry = store.submit_changeset(draft, SubmitMode::Propose).unwrap();
        assert_eq!(retry, approved);
        assert_eq!(
            store.approve_changeset(&proposed.id, 1, review()).unwrap(),
            approved
        );

        let conn = store.lock_db();
        let revisions: i64 = conn
            .query_row("SELECT COUNT(*) FROM observation_revisions", [], |row| {
                row.get(0)
            })
            .unwrap();
        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM change_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(revisions, 1);
        assert!(events >= 1);
    }

    #[test]
    fn rejected_proposal_payload_retention_keeps_terminal_receipt() {
        let (store, space) = store();
        let draft = ChangeSetDraft::new(
            "rejected-retention",
            space,
            ActorKind::Agent,
            "proposer",
            authority(),
            "capture fact",
            "test",
            vec![ChangeOperation::Remember {
                entity_name: "Rejected".into(),
                entity_type: EntityType::Fact,
                observations: vec![ObservationInput::new("discard payload")],
                relations: vec![],
            }],
        )
        .unwrap();
        let proposed = store
            .submit_changeset(draft.clone(), SubmitMode::Propose)
            .unwrap();
        let rejected = store
            .reject_changeset(&proposed.id, 1, review(), "not accepted")
            .unwrap();
        let report = store.compact_rejected_proposals(0, 16).unwrap();
        assert_eq!(report.changesets_compacted, 1);
        assert_eq!(report.request_rows_removed, 1);
        assert_eq!(
            store.submit_changeset(draft, SubmitMode::Propose).unwrap(),
            rejected
        );
    }

    #[test]
    fn stale_edit_is_atomic_and_lifecycle_is_reversible() {
        let (store, space) = store();
        let outcome = store
            .remember(
                "Ada",
                EntityType::Person,
                &[ObservationInput::new("old")],
                &[],
                "legacy",
            )
            .unwrap();
        let observation = Observation {
            id: outcome.observation_ids[0].clone(),
            entity_id: outcome.entity_id,
            content: "new".into(),
            observed_at: 10,
            valid_from: Some(10),
            valid_until: None,
            confidence: 1.0,
            source: "edit".into(),
            tombstoned: false,
            access_count: 0,
            memory_tier: MemoryTier::Semantic,
            title: None,
            summary: None,
            importance: None,
            source_kind: None,
            concepts: vec![],
            source_files: vec![],
        };
        let edit = ChangeSetDraft::new(
            "edit-1",
            space,
            ActorKind::Human,
            "editor",
            authority(),
            "correct fact",
            "test",
            vec![ChangeOperation::UpdateObservation {
                observation,
                expected_row_version: 1,
                expected_revision_id: None,
            }],
        )
        .unwrap();
        let receipt = store
            .submit_changeset(edit, SubmitMode::ApplyImmediately)
            .unwrap();
        assert_eq!(receipt.state, ChangeSetState::Applied);
        let stale = ChangeSetDraft::new(
            "edit-stale",
            space,
            ActorKind::Human,
            "editor",
            authority(),
            "stale",
            "test",
            vec![ChangeOperation::RetireObservation {
                observation_id: outcome.observation_ids[0].clone(),
                expected_row_version: 1,
            }],
        )
        .unwrap();
        assert!(store
            .submit_changeset(stale, SubmitMode::ApplyImmediately)
            .is_err());
        let retired = ChangeSetDraft::new(
            "retire-1",
            space,
            ActorKind::Human,
            "editor",
            authority(),
            "retire",
            "test",
            vec![ChangeOperation::RetireObservation {
                observation_id: outcome.observation_ids[0].clone(),
                expected_row_version: 2,
            }],
        )
        .unwrap();
        store
            .submit_changeset(retired, SubmitMode::ApplyImmediately)
            .unwrap();
        let mut filters = crate::RecallFilters::new();
        filters.mode = Some(openmemory_index::SearchMode::KeywordOnly);
        assert!(store.recall("new", 5, &filters).unwrap().is_empty());
        let restored = ChangeSetDraft::new(
            "restore-1",
            space,
            ActorKind::Human,
            "editor",
            authority(),
            "restore",
            "test",
            vec![ChangeOperation::RestoreObservation {
                observation_id: outcome.observation_ids[0].clone(),
                expected_row_version: 3,
            }],
        )
        .unwrap();
        store
            .submit_changeset(restored, SubmitMode::ApplyImmediately)
            .unwrap();
        let live_content: String = store
            .lock_db()
            .query_row(
                "SELECT content FROM observations WHERE id = ?1 AND tombstoned = 0 AND lifecycle = 'active'",
                params![outcome.observation_ids[0]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live_content, "new");
    }

    #[test]
    fn lazy_baseline_is_resumable_and_diff_is_field_deterministic() {
        let (store, space) = store();
        let outcome = store
            .remember(
                "Ada",
                EntityType::Person,
                &[ObservationInput::new("old")],
                &[],
                "legacy",
            )
            .unwrap();
        {
            let conn = store.lock_db();
            conn.execute(
                "UPDATE observations SET current_revision_id = NULL WHERE id = ?1",
                params![outcome.observation_ids[0]],
            )
            .unwrap();
            conn.execute("DELETE FROM observation_revisions", [])
                .unwrap();
        }
        let report = store.backfill_audit(8).unwrap();
        assert_eq!(report.baselines_created, 1);
        assert!(report.complete);
        let revision: String = store
            .lock_db()
            .query_row(
                "SELECT current_revision_id FROM observations WHERE id = ?1",
                params![outcome.observation_ids[0]],
                |row| row.get(0),
            )
            .unwrap();
        let observation = Observation {
            id: outcome.observation_ids[0].clone(),
            entity_id: outcome.entity_id,
            content: "new".into(),
            observed_at: 10,
            valid_from: Some(10),
            valid_until: None,
            confidence: 1.0,
            source: "legacy".into(),
            tombstoned: false,
            access_count: 0,
            memory_tier: MemoryTier::Episodic,
            title: None,
            summary: None,
            importance: None,
            source_kind: None,
            concepts: vec![],
            source_files: vec![],
        };
        let draft = ChangeSetDraft::new(
            "edit",
            space,
            ActorKind::Human,
            "human",
            authority(),
            "edit",
            "test",
            vec![ChangeOperation::UpdateObservation {
                observation,
                expected_row_version: 1,
                expected_revision_id: Some(revision.clone()),
            }],
        )
        .unwrap();
        store
            .submit_changeset(draft, SubmitMode::ApplyImmediately)
            .unwrap();
        let next: String = store
            .lock_db()
            .query_row(
                "SELECT current_revision_id FROM observations WHERE id = ?1",
                params![outcome.observation_ids[0]],
                |row| row.get(0),
            )
            .unwrap();
        let diff = store.diff_observation_revisions(&revision, &next).unwrap();
        assert_eq!(diff, vec!["content", "observed_at", "valid_from"]);
        let typed = store.observation_revision_diff(&revision, &next).unwrap();
        assert_eq!(typed.space_id, space);
        assert_eq!(typed.observation_id, outcome.observation_ids[0]);
        assert_eq!(
            typed.current_head_revision_id.as_deref(),
            Some(next.as_str())
        );
        assert!(!typed.stale);
        assert_eq!(typed.provenance.actor_kind, ActorKind::Human);
        assert_eq!(
            typed
                .changes
                .iter()
                .map(|change| change.field.as_str())
                .collect::<Vec<_>>(),
            ["content", "observed_at", "valid_from"]
        );
        assert!(typed.changes.iter().all(|change| {
            change.before.preview.len() <= 256 && change.after.preview.len() <= 256
        }));
        let first_page = store
            .observation_history(&outcome.observation_ids[0], None, 1)
            .unwrap();
        assert_eq!(first_page.entries.len(), 1);
        let second_page = store
            .observation_history(
                &outcome.observation_ids[0],
                first_page.next_cursor.as_ref(),
                1,
            )
            .unwrap();
        assert_eq!(second_page.entries.len(), 1);
        assert_ne!(
            first_page.entries[0].revision_id,
            second_page.entries[0].revision_id
        );
    }

    #[test]
    fn draft_validation_rejects_non_finite_and_unbounded_nested_values() {
        let mut invalid = ObservationInput::new("invalid");
        invalid.confidence = f32::NAN;
        assert!(ChangeSetDraft::new(
            "invalid",
            SpaceId::new(),
            ActorKind::Agent,
            "agent",
            authority(),
            "reason",
            "test",
            vec![ChangeOperation::Remember {
                entity_name: "Ada".into(),
                entity_type: EntityType::Person,
                observations: vec![invalid],
                relations: vec![],
            }],
        )
        .is_err());

        let too_many = vec!["tag".to_owned(); MAX_LIST_ITEMS + 1];
        assert!(ChangeSetDraft::new(
            "invalid-list",
            SpaceId::new(),
            ActorKind::Agent,
            "agent",
            authority(),
            "reason",
            "test",
            vec![ChangeOperation::Remember {
                entity_name: "Ada".into(),
                entity_type: EntityType::Person,
                observations: vec![ObservationInput::new("fact").with_concepts(too_many)],
                relations: vec![],
            }],
        )
        .is_err());
    }

    #[test]
    fn transaction_abort_leaves_no_partial_audit_or_projection() {
        let (store, space) = store();
        store
            .lock_db()
            .execute_batch(
                "CREATE TRIGGER inject_abort BEFORE INSERT ON observations
                 WHEN NEW.content = 'abort-me'
                 BEGIN SELECT RAISE(ABORT, 'injected transaction abort'); END;",
            )
            .unwrap();
        let draft = ChangeSetDraft::new(
            "abort",
            space,
            ActorKind::Agent,
            "agent",
            authority(),
            "fault test",
            "test",
            vec![ChangeOperation::Remember {
                entity_name: "Atomic".into(),
                entity_type: EntityType::Fact,
                observations: vec![
                    ObservationInput::new("first"),
                    ObservationInput::new("abort-me"),
                ],
                relations: vec![],
            }],
        )
        .unwrap();
        assert!(store
            .submit_changeset(draft, SubmitMode::ApplyImmediately)
            .is_err());
        let conn = store.lock_db();
        for table in [
            "entities",
            "observations",
            "change_sets",
            "change_requests",
            "change_events",
            "observation_revisions",
            "index_outbox",
        ] {
            let count: u64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table} leaked through the aborted transaction");
        }
        let generation: u64 = conn
            .query_row(
                "SELECT semantic_generation FROM domain_state WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation, 0);
    }

    #[test]
    fn sqlite_full_rolls_back_the_complete_changeset() {
        let (_temp, store, space) = file_store();
        {
            let conn = store.lock_db();
            conn.execute_batch("VACUUM").unwrap();
            let pages: u64 = conn
                .pragma_query_value(None, "page_count", |row| row.get(0))
                .unwrap();
            conn.pragma_update(None, "max_page_count", pages).unwrap();
        }
        let draft = ChangeSetDraft::new(
            "disk-full",
            space,
            ActorKind::Agent,
            "agent",
            authority(),
            "disk full",
            "test",
            vec![ChangeOperation::Remember {
                entity_name: "Large".into(),
                entity_type: EntityType::Fact,
                observations: vec![ObservationInput::new("x".repeat(900_000))],
                relations: vec![],
            }],
        )
        .unwrap();
        assert!(store
            .submit_changeset(draft, SubmitMode::ApplyImmediately)
            .is_err());
        let conn = store.lock_db();
        let counts: (u64, u64, u64) = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM observations),
                    (SELECT COUNT(*) FROM change_sets),
                    (SELECT semantic_generation FROM domain_state WHERE id = 1)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (0, 0, 0));
    }

    #[test]
    fn idempotent_receipt_is_stable_across_reopen() {
        let (temp, store, space) = file_store();
        let draft = ChangeSetDraft::new(
            "reopen",
            space,
            ActorKind::Agent,
            "agent",
            authority(),
            "reopen retry",
            "test",
            vec![ChangeOperation::Remember {
                entity_name: "Durable".into(),
                entity_type: EntityType::Fact,
                observations: vec![ObservationInput::new("survives restart")],
                relations: vec![],
            }],
        )
        .unwrap();
        let first = store
            .submit_changeset(draft.clone(), SubmitMode::ApplyImmediately)
            .unwrap();
        drop(store);
        let mut config = Config::default();
        config.default.jobs = 1;
        let reopened = MemoryStore::open(&config, temp.path()).unwrap();
        reopened.bind_space_id(space).unwrap();
        let retry = reopened
            .submit_changeset(draft, SubmitMode::ApplyImmediately)
            .unwrap();
        assert_eq!(retry, first);
        assert_eq!(reopened.status().unwrap().total_observations, 1);
    }

    #[test]
    fn concurrent_approvals_share_one_terminal_receipt() {
        let (store, space) = store();
        let proposed = store
            .submit_changeset(
                ChangeSetDraft::new(
                    "concurrent-review",
                    space,
                    ActorKind::Agent,
                    "proposer",
                    authority(),
                    "review race",
                    "test",
                    vec![ChangeOperation::Remember {
                        entity_name: "Race".into(),
                        entity_type: EntityType::Fact,
                        observations: vec![ObservationInput::new("one commit")],
                        relations: vec![],
                    }],
                )
                .unwrap(),
                SubmitMode::Propose,
            )
            .unwrap();
        let store = Arc::new(store);
        let mut threads = Vec::new();
        for reviewer in ["reviewer-a", "reviewer-b"] {
            let store = Arc::clone(&store);
            let id = proposed.id.clone();
            threads.push(std::thread::spawn(move || {
                store.approve_changeset(
                    &id,
                    1,
                    ReviewAuthorization::new(
                        ActorKind::Human,
                        reviewer,
                        SpaceRole::Reviewer,
                        authority(),
                    )
                    .unwrap(),
                )
            }));
        }
        let left = threads.remove(0).join().unwrap().unwrap();
        let right = threads.remove(0).join().unwrap().unwrap();
        assert_eq!(left, right);
        assert_eq!(left.state, ChangeSetState::Applied);
        assert_eq!(store.status().unwrap().total_observations, 1);
    }

    #[test]
    fn stale_proposal_conflicts_without_mutation_and_self_review_is_denied() {
        let (store, space) = store();
        let outcome = store
            .remember(
                "Head",
                EntityType::Fact,
                &[ObservationInput::new("base")],
                &[],
                "test",
            )
            .unwrap();
        let revision: String = store
            .lock_db()
            .query_row(
                "SELECT current_revision_id FROM observations WHERE id = ?1",
                params![outcome.observation_ids[0]],
                |row| row.get(0),
            )
            .unwrap();
        let proposed_value = Observation {
            id: outcome.observation_ids[0].clone(),
            entity_id: outcome.entity_id.clone(),
            content: "proposed".into(),
            observed_at: 1,
            valid_from: Some(1),
            valid_until: None,
            confidence: 1.0,
            source: "proposal".into(),
            tombstoned: false,
            access_count: 0,
            memory_tier: MemoryTier::Semantic,
            title: None,
            summary: None,
            importance: None,
            source_kind: None,
            concepts: vec![],
            source_files: vec![],
        };
        let proposed = store
            .submit_changeset(
                ChangeSetDraft::new(
                    "stale-proposal",
                    space,
                    ActorKind::Human,
                    "proposer",
                    authority(),
                    "proposal",
                    "test",
                    vec![ChangeOperation::UpdateObservation {
                        observation: proposed_value.clone(),
                        expected_row_version: 1,
                        expected_revision_id: Some(revision.clone()),
                    }],
                )
                .unwrap(),
                SubmitMode::Propose,
            )
            .unwrap();
        assert!(store
            .approve_changeset(
                &proposed.id,
                1,
                ReviewAuthorization::new(
                    ActorKind::Human,
                    "proposer",
                    SpaceRole::Reviewer,
                    authority(),
                )
                .unwrap(),
            )
            .is_err());
        assert!(ReviewAuthorization::new(
            ActorKind::Agent,
            "agent-reviewer",
            SpaceRole::Reviewer,
            authority(),
        )
        .is_err());

        let mut competing = proposed_value;
        competing.content = "winner".into();
        store
            .submit_changeset(
                ChangeSetDraft::new(
                    "competing-edit",
                    space,
                    ActorKind::Human,
                    "other-editor",
                    authority(),
                    "competing edit",
                    "test",
                    vec![ChangeOperation::UpdateObservation {
                        observation: competing,
                        expected_row_version: 1,
                        expected_revision_id: Some(revision),
                    }],
                )
                .unwrap(),
                SubmitMode::ApplyImmediately,
            )
            .unwrap();
        let conflicted = store
            .approve_changeset(
                &proposed.id,
                1,
                ReviewAuthorization::new(
                    ActorKind::Human,
                    "reviewer",
                    SpaceRole::Reviewer,
                    authority(),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(conflicted.state, ChangeSetState::Conflicted);
        let (content, revisions): (String, u64) = store
            .lock_db()
            .query_row(
                "SELECT content,
                        (SELECT COUNT(*) FROM observation_revisions
                         WHERE observation_id = observations.id)
                 FROM observations WHERE id = ?1",
                params![outcome.observation_ids[0]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(content, "winner");
        assert_eq!(revisions, 2);
    }

    #[test]
    fn immutable_revisions_survive_canonical_cleanup() {
        let (store, _) = store();
        let outcome = store
            .remember(
                "Disposable",
                EntityType::Fact,
                &[ObservationInput::new("history remains")],
                &[],
                "test",
            )
            .unwrap();
        assert_eq!(store.forget_entity("Disposable").unwrap(), 1);
        let conn = store.lock_db();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM observations WHERE id = ?1",
                params![outcome.observation_ids[0]],
                |row| row.get::<_, u64>(0),
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM observation_revisions WHERE observation_id = ?1",
                params![outcome.observation_ids[0]],
                |row| row.get::<_, u64>(0),
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn destruction_requires_preview_and_leaves_content_free_receipt() {
        let (store, _) = store();
        let outcome = store
            .remember(
                "Destroyable",
                EntityType::Fact,
                &[ObservationInput::new("sensitive payload")],
                &[],
                "test",
            )
            .unwrap();
        let observation_id = &outcome.observation_ids[0];
        let preview = store
            .preview_observation_destruction(observation_id, &maintainer(), 60)
            .unwrap();
        assert_eq!(preview.revision_count, 1);
        assert!(!preview.confirmation_token().is_empty());

        let first = store
            .destroy_observation(&preview, maintainer(), "destroy-once", "confirmed cleanup")
            .unwrap();
        let retry = store
            .destroy_observation(&preview, maintainer(), "destroy-once", "confirmed cleanup")
            .unwrap();
        assert_eq!(first, retry);
        assert!(store
            .destroy_observation(&preview, maintainer(), "destroy-twice", "confirmed cleanup",)
            .is_err());

        let conn = store.lock_db();
        let canonical: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM observations WHERE id = ?1",
                params![observation_id],
                |row| row.get(0),
            )
            .unwrap();
        let destroyed_revisions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM observation_revisions
                 WHERE observation_id = ?1 AND lifecycle = 'destroyed'",
                params![observation_id],
                |row| row.get(0),
            )
            .unwrap();
        let receipts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM destruction_receipts WHERE logical_id = ?1",
                params![observation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(canonical, 0);
        assert_eq!(destroyed_revisions, 1);
        assert_eq!(receipts, 1);
        let receipt_columns = conn
            .prepare("PRAGMA table_info(destruction_receipts)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!receipt_columns.iter().any(|column| {
            matches!(column.as_str(), "content" | "title" | "summary" | "source")
        }));
    }

    #[cfg(feature = "fts5")]
    #[test]
    fn persistent_index_failure_is_explicit_and_repairable_after_reopen() {
        let (temp, store, space) = file_store();
        rusqlite::Connection::open(temp.path().join(openmemory_index::engine::FULLTEXT_FILE))
            .unwrap()
            .execute_batch("DROP TABLE chunks_fts")
            .unwrap();
        let receipt = store
            .submit_changeset(
                ChangeSetDraft::new(
                    "index-fault",
                    space,
                    ActorKind::Agent,
                    "agent",
                    authority(),
                    "index fault",
                    "test",
                    vec![ChangeOperation::Remember {
                        entity_name: "Repair".into(),
                        entity_type: EntityType::Fact,
                        observations: vec![ObservationInput::new("repairable index")],
                        relations: vec![],
                    }],
                )
                .unwrap(),
                SubmitMode::ApplyImmediately,
            )
            .unwrap();
        assert_eq!(receipt.state, ChangeSetState::Applied);
        assert!(receipt.index_repair_required);
        assert!(store
            .recall("repairable", 5, &crate::RecallFilters::new())
            .is_err());
        drop(store);

        let mut config = Config::default();
        config.default.jobs = 1;
        let reopened = MemoryStore::open(&config, temp.path()).unwrap();
        reopened.bind_space_id(space).unwrap();
        let repaired = reopened.repair_index().unwrap();
        assert_eq!(repaired.remaining, 0);
        assert!(!reopened
            .recall("repairable", 5, &crate::RecallFilters::new())
            .unwrap()
            .is_empty());
    }
}
