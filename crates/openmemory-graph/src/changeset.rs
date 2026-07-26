//! Audited optimistic semantic mutations and immutable revision history.

use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::{ActorKind, ChangeSetId, PrincipalId, RevisionId, SpaceId};
use openmemory_index::IndexEntry;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, MemoryResult};
use crate::remember::RememberOutcome;
use crate::store::MemoryStore;
use crate::types::{new_id, EntityType, MemoryTier};

const MAX_IDEMPOTENCY_BYTES: usize = 128;
const MAX_REASON_SOURCE_BYTES: usize = 1_024;
const MAX_OPERATIONS: usize = 256;
const HARD_MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONTENT_BYTES: usize = HARD_MAX_PAYLOAD_BYTES;
const MAX_LABEL_BYTES: usize = 4 * 1024;
const MAX_SET_ITEMS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Entity,
    Observation,
    Relation,
}

impl ObjectKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Entity => "entity",
            Self::Observation => "observation",
            Self::Relation => "relation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub kind: ObjectKind,
    pub logical_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Active,
    Retired,
    Destroyed,
}

impl Lifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
            Self::Destroyed => "destroyed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationValue {
    pub content: String,
    pub observed_at: i64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub confidence: f32,
    pub source: String,
    pub memory_tier: MemoryTier,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub importance: Option<f32>,
    pub source_kind: Option<String>,
    pub concepts: Vec<String>,
    pub source_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedHead {
    pub revision_id: Option<RevisionId>,
    pub row_version: u64,
    pub lifecycle: Lifecycle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupersedeObservation {
    pub logical_id: String,
    pub expected: ExpectedHead,
    pub value: ObservationValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetObservationTier {
    pub logical_id: String,
    pub expected: ExpectedHead,
    pub memory_tier: MemoryTier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectMutation {
    pub object: ObjectRef,
    pub expected: ExpectedHead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevertToRevision {
    pub object: ObjectRef,
    pub expected: ExpectedHead,
    pub revision_id: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityIdentifier {
    pub namespace: String,
    pub raw_value: String,
    pub canonical_value: Option<String>,
    pub trust: String,
    pub source_snapshot_id: Option<String>,
    pub verifier_version: Option<String>,
    pub resolver_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityValue {
    pub name: String,
    pub entity_type: EntityType,
    pub confidence: f32,
    pub source: String,
    pub aliases: Vec<String>,
    pub identifiers: Vec<EntityIdentifier>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateEntity {
    pub logical_id: String,
    pub expected: ExpectedHead,
    pub value: EntityValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationValue {
    pub from_entity: String,
    pub to_entity: String,
    pub relation_type: String,
    pub weight: f32,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub source: String,
    pub evidence_json: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRelation {
    pub logical_id: String,
    pub expected: ExpectedHead,
    pub value: RelationValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewObservation {
    pub logical_id: Option<String>,
    pub value: ObservationValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewRelation {
    pub logical_id: Option<String>,
    pub to_entity_name: String,
    pub to_entity_type: EntityType,
    pub relation_type: String,
    pub weight: f32,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RememberChange {
    pub entity: EntityValue,
    pub observations: Vec<NewObservation>,
    pub relations: Vec<NewRelation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginRef {
    pub space_id: SpaceId,
    pub logical_id: String,
    pub revision_id: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceChange {
    pub operation: Box<ChangeOperation>,
    pub origins: Vec<OriginRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation", content = "payload")]
pub enum ChangeOperation {
    Remember(RememberChange),
    SupersedeObservation(SupersedeObservation),
    SetObservationTier(SetObservationTier),
    Retire(ObjectMutation),
    Restore(ObjectMutation),
    RevertToRevision(RevertToRevision),
    UpdateEntity(UpdateEntity),
    UpdateRelation(UpdateRelation),
    CherryPick(ProvenanceChange),
    MergeContribution(ProvenanceChange),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSetDraft {
    pub idempotency_key: String,
    pub space_id: SpaceId,
    pub actor_principal: PrincipalId,
    pub actor_kind: ActorKind,
    pub authorization_generation: u64,
    pub reason: String,
    pub source: String,
    pub operations: Vec<ChangeOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitMode {
    ApplyImmediately,
    Propose,
}

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
    pub(crate) fn parse(value: &str) -> MemoryResult<Self> {
        match value {
            "proposed" => Ok(Self::Proposed),
            "applied" => Ok(Self::Applied),
            "rejected" => Ok(Self::Rejected),
            "conflicted" => Ok(Self::Conflicted),
            "reverted" => Ok(Self::Reverted),
            _ => Err(MemoryError::Schema(
                "invalid persisted changeset state".to_string(),
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
            Self::Conflicted => "conflicted",
            Self::Reverted => "reverted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSetReceipt {
    pub id: ChangeSetId,
    pub state: ChangeSetState,
    pub semantic_generation: u64,
    pub indexed_generation: u64,
    pub index_ready: bool,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryBackfillReport {
    pub entities: usize,
    pub observations: usize,
    pub relations: usize,
    pub complete: bool,
}

#[derive(Debug)]
struct ExistingChangeSet {
    id: ChangeSetId,
    request_hash: Vec<u8>,
    state: ChangeSetState,
    committed_generation: Option<u64>,
}

impl MemoryStore {
    /// Persist a proposal or apply one domain-local changeset atomically.
    pub fn submit_changeset(
        &self,
        draft: &ChangeSetDraft,
        mode: SubmitMode,
    ) -> MemoryResult<ChangeSetReceipt> {
        let encoded = self.validate_changeset(draft)?;
        let request_hash = request_hash(&encoded);
        let _barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_existing(&tx, draft.space_id, &draft.idempotency_key)? {
            if existing.request_hash != request_hash {
                return Err(MemoryError::IdempotencyConflict);
            }
            let (semantic_generation, indexed_generation) = generations(&tx)?;
            tx.commit()?;
            return Ok(ChangeSetReceipt {
                id: existing.id,
                state: existing.state,
                semantic_generation: existing.committed_generation.unwrap_or(semantic_generation),
                indexed_generation,
                index_ready: indexed_generation >= semantic_generation,
                idempotent_replay: true,
            });
        }

        let id = ChangeSetId::new();
        let now = self.clock().now_secs();
        let (base_generation, indexed_generation) = generations(&tx)?;
        insert_changeset(&tx, id, draft, &request_hash, base_generation, now)?;
        insert_requests(&tx, id, &draft.operations)?;

        if mode == SubmitMode::Propose {
            tx.commit()?;
            return Ok(ChangeSetReceipt {
                id,
                state: ChangeSetState::Proposed,
                semantic_generation: base_generation,
                indexed_generation,
                index_ready: indexed_generation >= base_generation,
                idempotent_replay: false,
            });
        }

        let generation = base_generation.saturating_add(1);
        apply_operations(&tx, id, draft, generation, now)?;
        mark_applied(&tx, id, generation, now, &draft.actor_principal)?;
        tx.commit()?;
        drop(conn);
        let indexed = self.drain_index_outbox(generation)?;
        Ok(ChangeSetReceipt {
            id,
            state: ChangeSetState::Applied,
            semantic_generation: generation,
            indexed_generation: indexed,
            index_ready: indexed >= generation,
            idempotent_replay: false,
        })
    }

    /// Apply an immutable proposal after human authorization revalidation.
    pub fn approve_changeset(
        &self,
        id: ChangeSetId,
        reviewer: &PrincipalId,
        reviewer_kind: ActorKind,
        authorization_generation: u64,
    ) -> MemoryResult<ChangeSetReceipt> {
        if reviewer_kind != ActorKind::Human {
            return Err(MemoryError::Authorization(
                "only a human reviewer can approve a proposal".to_string(),
            ));
        }
        let _barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (draft, expected_hash, state) = load_proposal(&tx, id)?;
        if state == ChangeSetState::Applied {
            let (semantic, indexed) = generations(&tx)?;
            tx.commit()?;
            return Ok(ChangeSetReceipt {
                id,
                state,
                semantic_generation: semantic,
                indexed_generation: indexed,
                index_ready: indexed >= semantic,
                idempotent_replay: true,
            });
        }
        if state != ChangeSetState::Proposed {
            return Err(MemoryError::ChangeSetStale(format!(
                "proposal is {}",
                state.as_str()
            )));
        }
        if &draft.actor_principal == reviewer {
            return Err(MemoryError::Authorization(
                "team proposals cannot be self-approved".to_string(),
            ));
        }
        if authorization_generation == 0
            || authorization_generation != draft.authorization_generation
        {
            return Err(MemoryError::Authorization(
                "proposal authorization generation is stale".to_string(),
            ));
        }
        let encoded = self.validate_changeset(&draft)?;
        if request_hash(&encoded) != expected_hash {
            return Err(MemoryError::Schema(
                "persisted proposal payload hash mismatch".to_string(),
            ));
        }
        let (base, _) = generations(&tx)?;
        let generation = base.saturating_add(1);
        let now = self.clock().now_secs();
        match apply_operations(&tx, id, &draft, generation, now) {
            Ok(()) => {}
            Err(MemoryError::ChangeSetStale(reason)) => {
                tx.execute(
                    "UPDATE change_sets SET state='conflicted', decided_at=?1,
                         decided_by=?2 WHERE id=?3 AND state='proposed'",
                    params![now, reviewer.to_string(), id.to_string()],
                )?;
                tx.commit()?;
                return Err(MemoryError::ChangeSetStale(reason));
            }
            Err(error) => return Err(error),
        }
        mark_applied(&tx, id, generation, now, reviewer)?;
        tx.commit()?;
        drop(conn);
        let indexed = self.drain_index_outbox(generation)?;
        Ok(ChangeSetReceipt {
            id,
            state: ChangeSetState::Applied,
            semantic_generation: generation,
            indexed_generation: indexed,
            index_ready: indexed >= generation,
            idempotent_replay: false,
        })
    }

    /// Reject a proposal without touching semantic projection or generation.
    pub fn reject_changeset(
        &self,
        id: ChangeSetId,
        reviewer: &PrincipalId,
        reviewer_kind: ActorKind,
        authorization_generation: u64,
    ) -> MemoryResult<ChangeSetReceipt> {
        if reviewer_kind != ActorKind::Human || authorization_generation == 0 {
            return Err(MemoryError::Authorization(
                "current human reviewer authorization is required".to_string(),
            ));
        }
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE change_sets SET state='rejected', decided_at=?1, decided_by=?2
             WHERE id=?3 AND state='proposed' AND authorization_generation=?4",
            params![
                self.clock().now_secs(),
                reviewer.to_string(),
                id.to_string(),
                authorization_generation
            ],
        )?;
        if changed != 1 {
            return Err(MemoryError::ChangeSetStale(
                "proposal is no longer rejectable".to_string(),
            ));
        }
        let (semantic, indexed) = generations(&tx)?;
        tx.commit()?;
        Ok(ChangeSetReceipt {
            id,
            state: ChangeSetState::Rejected,
            semantic_generation: semantic,
            indexed_generation: indexed,
            index_ready: indexed >= semantic,
            idempotent_replay: false,
        })
    }

    /// Revert one applied changeset as a new atomic, auditable changeset.
    ///
    /// The original event rows are collapsed per logical object so multiple
    /// updates to one object inside the original transaction revert directly
    /// from its final after-state to its first before-state. Created objects
    /// are retired rather than destroyed, preserving immutable history.
    pub fn revert_changeset(
        &self,
        original: ChangeSetId,
        actor_principal: &PrincipalId,
        actor_kind: ActorKind,
        authorization_generation: u64,
        idempotency_key: &str,
        reason: &str,
    ) -> MemoryResult<ChangeSetReceipt> {
        if actor_kind != ActorKind::Human {
            return Err(MemoryError::Authorization(
                "only a human can revert an applied changeset".to_string(),
            ));
        }
        bounded(
            "idempotency key",
            idempotency_key,
            1,
            MAX_IDEMPOTENCY_BYTES,
        )?;
        bounded("reason", reason, 1, MAX_REASON_SOURCE_BYTES)?;
        if authorization_generation == 0 {
            return Err(MemoryError::Authorization(
                "authorization generation must be current".to_string(),
            ));
        }
        let source = format!("changeset_revert:{original}");
        bounded("source", &source, 1, MAX_REASON_SOURCE_BYTES)?;

        let _barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_existing(&tx, self.space_id(), idempotency_key)? {
            let (draft, _, _) = load_proposal(&tx, existing.id)?;
            if draft.actor_principal != *actor_principal
                || draft.actor_kind != actor_kind
                || draft.authorization_generation != authorization_generation
                || draft.reason != reason
                || draft.source != source
            {
                return Err(MemoryError::IdempotencyConflict);
            }
            let (semantic_generation, indexed_generation) = generations(&tx)?;
            tx.commit()?;
            return Ok(ChangeSetReceipt {
                id: existing.id,
                state: existing.state,
                semantic_generation: existing.committed_generation.unwrap_or(semantic_generation),
                indexed_generation,
                index_ready: indexed_generation >= semantic_generation,
                idempotent_replay: true,
            });
        }

        let original_state = tx
            .query_row(
                "SELECT state FROM change_sets WHERE id=?1 AND space_id=?2",
                params![original.to_string(), self.space_id().to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                MemoryError::ChangeSetStale("changeset does not exist in this space".to_string())
            })?;
        if original_state != "applied" {
            return Err(MemoryError::ChangeSetStale(format!(
                "changeset is {original_state}, not applied"
            )));
        }

        let operations = inverse_changeset_operations(&tx, original)?;
        if operations.is_empty() {
            return Err(MemoryError::ChangeSetStale(
                "applied changeset has no semantic events".to_string(),
            ));
        }
        let draft = ChangeSetDraft {
            idempotency_key: idempotency_key.to_string(),
            space_id: self.space_id(),
            actor_principal: actor_principal.clone(),
            actor_kind,
            authorization_generation,
            reason: reason.to_string(),
            source,
            operations,
        };
        let encoded = self.validate_changeset(&draft)?;
        let request_hash = request_hash(&encoded);
        let id = ChangeSetId::new();
        let now = self.clock().now_secs();
        let (base_generation, _) = generations(&tx)?;
        insert_changeset(&tx, id, &draft, &request_hash, base_generation, now)?;
        insert_requests(&tx, id, &draft.operations)?;
        let generation = base_generation.saturating_add(1);
        apply_operations(&tx, id, &draft, generation, now)?;
        mark_applied(&tx, id, generation, now, actor_principal)?;
        let changed = tx.execute(
            "UPDATE change_sets SET state='reverted'
             WHERE id=?1 AND space_id=?2 AND state='applied'",
            params![original.to_string(), self.space_id().to_string()],
        )?;
        if changed != 1 {
            return Err(MemoryError::ChangeSetStale(
                "changeset was concurrently changed".to_string(),
            ));
        }
        tx.commit()?;
        drop(conn);
        let indexed = self.drain_index_outbox(generation)?;
        Ok(ChangeSetReceipt {
            id,
            state: ChangeSetState::Applied,
            semantic_generation: generation,
            indexed_generation: indexed,
            index_ready: indexed >= generation,
            idempotent_replay: false,
        })
    }

    fn validate_changeset(&self, draft: &ChangeSetDraft) -> MemoryResult<Vec<u8>> {
        if draft.space_id != self.space_id() {
            return Err(MemoryError::Authorization(
                "changeset targets another semantic space".to_string(),
            ));
        }
        bounded(
            "idempotency key",
            &draft.idempotency_key,
            1,
            MAX_IDEMPOTENCY_BYTES,
        )?;
        bounded("reason", &draft.reason, 1, MAX_REASON_SOURCE_BYTES)?;
        bounded("source", &draft.source, 1, MAX_REASON_SOURCE_BYTES)?;
        if draft.authorization_generation == 0 {
            return Err(MemoryError::Authorization(
                "authorization generation must be current".to_string(),
            ));
        }
        if draft.operations.is_empty() || draft.operations.len() > MAX_OPERATIONS {
            return Err(MemoryError::InvalidInput(
                "changeset must contain 1..=256 operations".to_string(),
            ));
        }
        for operation in &draft.operations {
            validate_operation(operation, false)?;
        }
        let encoded = serde_json::to_vec(draft)?;
        let maximum = self.max_audit_payload_bytes.min(HARD_MAX_PAYLOAD_BYTES);
        if encoded.len() > maximum {
            return Err(MemoryError::InvalidInput(format!(
                "changeset payload exceeds {maximum} bytes"
            )));
        }
        Ok(encoded)
    }

    pub(crate) fn drain_index_outbox(&self, through_generation: u64) -> MemoryResult<u64> {
        loop {
            let row = {
                let conn = self.lock_db();
                conn.query_row(
                    "SELECT generation, logical_id, operation
                     FROM index_outbox
                     WHERE generation<=?1 AND object_kind='observation'
                     ORDER BY generation, logical_id LIMIT 1",
                    [through_generation],
                    |row| {
                        Ok((
                            row.get::<_, u64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?
            };
            let Some((generation, logical_id, operation)) = row else {
                let conn = self.lock_db();
                conn.execute(
                    "UPDATE domain_state SET indexed_generation =
                         MAX(indexed_generation, ?1) WHERE singleton=1",
                    [through_generation],
                )?;
                self.flush_engine();
                return Ok(through_generation);
            };

            let repair = if operation == "delete" {
                self.engine()
                    .engine
                    .delete_by_uri(&format!("memory://observation/{logical_id}"))
                    .map(|_| ())
            } else {
                let entry = self.index_entry_for_observation(&logical_id)?;
                match entry {
                    Some(entry) => self.engine().engine.insert(&[entry]),
                    None => self
                        .engine()
                        .engine
                        .delete_by_uri(&format!("memory://observation/{logical_id}"))
                        .map(|_| ()),
                }
            };
            if let Err(error) = repair {
                let conn = self.lock_db();
                conn.execute(
                    "UPDATE index_outbox SET attempts=attempts+1, last_error=?1
                     WHERE generation=?2 AND object_kind='observation' AND logical_id=?3",
                    params![error.to_string(), generation, logical_id],
                )?;
                return Err(MemoryError::IndexRepairRequired(error.to_string()));
            }
            self.lock_db().execute(
                "DELETE FROM index_outbox
                 WHERE generation=?1 AND object_kind='observation' AND logical_id=?2",
                params![generation, logical_id],
            )?;
        }
    }

    /// A compatibility write already inserted the exact batched index payload
    /// outside SQLite. Acknowledge its durable outbox rows only after that
    /// insert succeeds.
    pub(crate) fn acknowledge_index_outbox(&self, through_generation: u64) -> MemoryResult<()> {
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM index_outbox WHERE generation<=?1",
            [through_generation],
        )?;
        tx.execute(
            "UPDATE domain_state SET indexed_generation =
                 MAX(indexed_generation, ?1) WHERE singleton=1",
            [through_generation],
        )?;
        tx.commit()?;
        self.flush_engine();
        Ok(())
    }

    fn index_entry_for_observation(&self, id: &str) -> MemoryResult<Option<IndexEntry>> {
        let projection = self.with_reader(|conn| {
            conn.query_row(
                "SELECT entity.name, observation.content, observation.title,
                        observation.summary, observation.source_kind,
                        observation.lifecycle, entity.lifecycle
                 FROM observations AS observation
                 JOIN entities AS entity ON entity.id=observation.entity_id
                 WHERE observation.id=?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(Into::into)
        })?;
        let Some((
            entity_name,
            content,
            title,
            summary,
            source_kind,
            observation_lifecycle,
            entity_lifecycle,
        )) = projection
        else {
            return Ok(None);
        };
        if observation_lifecycle != "active" || entity_lifecycle != "active" {
            return Ok(None);
        }
        let (concepts, source_files) = self.with_reader(|conn| {
            Ok((
                strings(
                    conn,
                    "SELECT concept FROM observation_concepts
                     WHERE observation_id=?1 ORDER BY concept",
                    id,
                )?,
                strings(
                    conn,
                    "SELECT file_path FROM observation_source_files
                     WHERE observation_id=?1 ORDER BY file_path",
                    id,
                )?,
            ))
        })?;
        let vector = self.embed_query(&content);
        let mut entry = IndexEntry::new(
            format!("memory://observation/{id}"),
            MemoryStore::search_body_text(&entity_name, &content),
        )
        .with_title(title)
        .with_summary(summary)
        .with_concepts(concepts)
        .with_source_files(source_files)
        .with_source_kind(source_kind)
        .with_entity_name(Some(entity_name));
        if !vector.is_empty() {
            entry = entry.with_vector(vector);
        }
        Ok(Some(entry))
    }

    /// Resumably create immutable baseline heads for legacy projections.
    /// This is maintenance work: semantic generation does not change.
    pub fn backfill_history_batch(&self, limit: usize) -> MemoryResult<HistoryBackfillReport> {
        if limit == 0 || limit > 10_000 {
            return Err(MemoryError::InvalidInput(
                "history backfill batch must be in 1..=10000".to_string(),
            ));
        }
        let _barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (generation, _) = generations(&tx)?;
        let now = self.clock().now_secs();
        let mut report = HistoryBackfillReport::default();

        let entity_ids = ids_without_revision(&tx, "entities", limit)?;
        for id in entity_ids {
            let current = load_entity(&tx, &id)?;
            insert_entity_baseline(&tx, self.space_id(), &id, &current, generation, now)?;
            report.entities += 1;
        }
        let remaining = limit.saturating_sub(report.entities);
        let observation_ids = ids_without_revision(&tx, "observations", remaining)?;
        for id in observation_ids {
            let current = load_observation(&tx, &id)?;
            insert_observation_baseline(&tx, self.space_id(), &id, &current, generation, now)?;
            report.observations += 1;
        }
        let remaining = remaining.saturating_sub(report.observations);
        let relation_ids = ids_without_revision(&tx, "relations", remaining)?;
        for id in relation_ids {
            let current = load_relation(&tx, &id)?;
            insert_relation_baseline(&tx, self.space_id(), &id, &current, generation, now)?;
            report.relations += 1;
        }
        let pending: u64 = tx.query_row(
            "SELECT
                (SELECT COUNT(*) FROM entities WHERE current_revision_id IS NULL) +
                (SELECT COUNT(*) FROM observations WHERE current_revision_id IS NULL) +
                (SELECT COUNT(*) FROM relations WHERE current_revision_id IS NULL)",
            [],
            |row| row.get(0),
        )?;
        report.complete = pending == 0;
        if report.complete {
            tx.execute(
                "UPDATE domain_state SET backfill_state='history_ready' WHERE singleton=1",
                [],
            )?;
        }
        tx.commit()?;
        Ok(report)
    }

    /// Legacy edge descriptor used by the partition facade to pair canonical
    /// and mirror rows deterministically.
    pub fn relation_binding_descriptors(&self) -> MemoryResult<Vec<RelationBindingDescriptor>> {
        self.with_reader(|conn| {
            let mut statement = conn.prepare(
                "SELECT relation.id, relation.canonical_relation_id, relation.mirror_role,
                        source.name, source.source, target.name,
                        relation.relation_type, relation.weight, relation.source
                 FROM relations AS relation
                 JOIN entities AS source ON source.id=relation.from_entity
                 JOIN entities AS target ON target.id=relation.to_entity
                 ORDER BY relation.id",
            )?;
            let descriptors = statement
                .query_map([], |row| {
                    Ok(RelationBindingDescriptor {
                        relation_id: row.get(0)?,
                        canonical_relation_id: row.get(1)?,
                        mirror_role: row.get(2)?,
                        from_name: row.get(3)?,
                        from_is_stub: row.get::<_, String>(4)? == crate::PARTITION_STUB_SOURCE,
                        to_name: row.get(5)?,
                        relation_type: row.get(6)?,
                        weight_bits: row.get::<_, f64>(7)?.to_bits(),
                        source: row.get(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(MemoryError::from)?;
            Ok(descriptors)
        })
    }

    pub fn bind_relation_role(
        &self,
        relation_id: &str,
        canonical_relation_id: &str,
        mirror_role: &str,
    ) -> MemoryResult<()> {
        let conn = self.lock_db();
        let changed = conn.execute(
            "UPDATE relations SET canonical_relation_id=?1, mirror_role=?2 WHERE id=?3",
            params![canonical_relation_id, mirror_role, relation_id],
        )?;
        if changed != 1 {
            return Err(MemoryError::InvalidInput(
                "relation binding target is missing".to_string(),
            ));
        }
        Ok(())
    }

    pub fn mark_mirror_backfill_ready(&self) -> MemoryResult<()> {
        let conn = self.lock_db();
        let pending: u64 = conn.query_row(
            "SELECT COUNT(*) FROM relations WHERE canonical_relation_id IS NULL",
            [],
            |row| row.get(0),
        )?;
        if pending != 0 {
            return Err(MemoryError::InvalidInput(
                "relation bindings remain incomplete".to_string(),
            ));
        }
        conn.execute(
            "UPDATE domain_state SET backfill_state='ready',
                 mirror_generation=semantic_generation WHERE singleton=1",
            [],
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct RevertAggregate {
    object: ObjectRef,
    first_ordinal: u64,
    last_ordinal: u64,
    before_revision: Option<RevisionId>,
    after_revision: Option<RevisionId>,
    before_lifecycle: Option<Lifecycle>,
    after_lifecycle: Option<Lifecycle>,
}

fn inverse_changeset_operations(
    tx: &Transaction<'_>,
    original: ChangeSetId,
) -> MemoryResult<Vec<ChangeOperation>> {
    let mut statement = tx.prepare(
        "SELECT ordinal, object_kind, logical_id, before_revision_id,
                after_revision_id, before_lifecycle, after_lifecycle
         FROM change_events WHERE change_set_id=?1 ORDER BY ordinal",
    )?;
    let rows = statement
        .query_map([original.to_string()], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut grouped = BTreeMap::<(String, String), RevertAggregate>::new();
    for row in rows {
        let kind = parse_object_kind(&row.1)?;
        let key = (row.1, row.2.clone());
        let before_revision = parse_optional_revision(row.3)?;
        let after_revision = parse_optional_revision(row.4)?;
        let before_lifecycle = row.5.as_deref().map(parse_lifecycle).transpose()?;
        let after_lifecycle = row.6.as_deref().map(parse_lifecycle).transpose()?;
        grouped
            .entry(key)
            .and_modify(|aggregate| {
                aggregate.last_ordinal = row.0;
                aggregate.after_revision = after_revision;
                aggregate.after_lifecycle = after_lifecycle;
            })
            .or_insert(RevertAggregate {
                object: ObjectRef {
                    kind,
                    logical_id: row.2,
                },
                first_ordinal: row.0,
                last_ordinal: row.0,
                before_revision,
                after_revision,
                before_lifecycle,
                after_lifecycle,
            });
    }
    let mut aggregates = grouped.into_values().collect::<Vec<_>>();
    aggregates.sort_by(|left, right| {
        right
            .last_ordinal
            .cmp(&left.last_ordinal)
            .then_with(|| right.first_ordinal.cmp(&left.first_ordinal))
    });

    let mut operations = Vec::new();
    for aggregate in aggregates {
        let mut current = object_head(tx, &aggregate.object)?;
        if current.revision_id != aggregate.after_revision
            || Some(current.lifecycle) != aggregate.after_lifecycle
        {
            return Err(MemoryError::ChangeSetStale(format!(
                "{} `{}` advanced after the changeset",
                aggregate.object.kind.as_str(),
                aggregate.object.logical_id
            )));
        }
        if aggregate.before_revision.is_none() {
            if current.lifecycle != Lifecycle::Active {
                return Err(MemoryError::ChangeSetStale(
                    "newly created object is no longer active".to_string(),
                ));
            }
            operations.push(ChangeOperation::Retire(ObjectMutation {
                object: aggregate.object,
                expected: expected_head(&current),
            }));
            continue;
        }

        if aggregate.before_lifecycle != aggregate.after_lifecycle {
            let target = aggregate.before_lifecycle.ok_or_else(|| {
                MemoryError::Schema("event omitted before lifecycle".to_string())
            })?;
            let mutation = ObjectMutation {
                object: aggregate.object.clone(),
                expected: expected_head(&current),
            };
            operations.push(match target {
                Lifecycle::Active => ChangeOperation::Restore(mutation),
                Lifecycle::Retired => ChangeOperation::Retire(mutation),
                Lifecycle::Destroyed => {
                    return Err(MemoryError::Schema(
                        "destroyed lifecycle cannot be restored by changeset revert".to_string(),
                    ))
                }
            });
            current.lifecycle = target;
            current.row_version = current.row_version.saturating_add(1);
        }
        if aggregate.before_revision != aggregate.after_revision {
            operations.push(ChangeOperation::RevertToRevision(RevertToRevision {
                object: aggregate.object,
                expected: expected_head(&current),
                revision_id: aggregate.before_revision.expect("checked above"),
            }));
        }
    }
    Ok(operations)
}

fn expected_head(head: &ProjectionHead) -> ExpectedHead {
    ExpectedHead {
        revision_id: head.revision_id,
        row_version: head.row_version,
        lifecycle: head.lifecycle,
    }
}

fn parse_object_kind(value: &str) -> MemoryResult<ObjectKind> {
    match value {
        "entity" => Ok(ObjectKind::Entity),
        "observation" => Ok(ObjectKind::Observation),
        "relation" => Ok(ObjectKind::Relation),
        _ => Err(MemoryError::Schema(
            "invalid event object kind".to_string(),
        )),
    }
}

/// Attach immutable revisions, events, generation, and durable index work to
/// the legacy remember projection inside the caller's existing transaction.
/// This keeps old API behavior while making the semantic commit auditable.
pub(crate) fn audit_legacy_remember(
    tx: &Transaction<'_>,
    space_id: SpaceId,
    outcomes: &[RememberOutcome],
    source: &str,
    now: i64,
) -> MemoryResult<Option<u64>> {
    let mut entity_ids = BTreeSet::new();
    let mut observation_ids = BTreeSet::new();
    let mut relation_ids = BTreeSet::new();
    for outcome in outcomes {
        entity_ids.insert(outcome.entity_id.clone());
        observation_ids.extend(outcome.observation_ids.iter().cloned());
        relation_ids.extend(outcome.relation_ids.iter().cloned());
    }
    for relation_id in &relation_ids {
        let endpoints = tx.query_row(
            "SELECT from_entity, to_entity FROM relations WHERE id=?1",
            [relation_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        entity_ids.insert(endpoints.0);
        entity_ids.insert(endpoints.1);
    }
    entity_ids.retain(|id| {
        tx.query_row(
            "SELECT current_revision_id IS NULL FROM entities WHERE id=?1",
            [id],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false)
    });
    if entity_ids.is_empty() && observation_ids.is_empty() && relation_ids.is_empty() {
        return Ok(None);
    }

    let (base, _) = generations(tx)?;
    let generation = base.saturating_add(1);
    let change_set = ChangeSetId::new();
    let request_hash = legacy_request_hash(&entity_ids, &observation_ids, &relation_ids, source);
    tx.execute(
        "INSERT INTO change_sets(
            id, space_id, idempotency_key, request_hash, actor_principal,
            actor_kind, authorization_generation, reason, source, state,
            base_generation, committed_generation, created_at, decided_at,
            decided_by)
         VALUES (?1, ?2, ?3, ?4, 'compat:legacy', 'system', 1,
                 'legacy remember compatibility adapter', ?5, 'applied',
                 ?6, ?7, ?8, ?8, 'compat:legacy')",
        params![
            change_set.to_string(),
            space_id.to_string(),
            format!("compat:{}", change_set),
            request_hash.as_bytes().as_slice(),
            source,
            base,
            generation,
            now,
        ],
    )?;

    let mut ordinal = 0_usize;
    for id in entity_ids {
        let current = load_entity(tx, &id)?;
        let revision = insert_entity_revision(tx, change_set, &id, None, &current.value, now)?;
        tx.execute(
            "UPDATE entities SET current_revision_id=?1 WHERE id=?2",
            params![revision.to_string(), id],
        )?;
        insert_legacy_request_event(tx, change_set, ordinal, ObjectKind::Entity, &id, revision)?;
        ordinal += 1;
    }
    for id in observation_ids {
        let current = load_observation(tx, &id)?;
        let revision = insert_observation_revision(tx, change_set, &id, None, &current.value, now)?;
        tx.execute(
            "UPDATE observations SET current_revision_id=?1 WHERE id=?2",
            params![revision.to_string(), id],
        )?;
        insert_legacy_request_event(
            tx,
            change_set,
            ordinal,
            ObjectKind::Observation,
            &id,
            revision,
        )?;
        enqueue_index(tx, generation, &id, "upsert", Some(revision))?;
        ordinal += 1;
    }
    for id in relation_ids {
        let current = load_relation(tx, &id)?;
        let revision = insert_relation_revision(tx, change_set, &id, None, &current.value, now)?;
        let from_source: String = tx.query_row(
            "SELECT entity.source FROM relations AS relation
             JOIN entities AS entity ON entity.id=relation.from_entity
             WHERE relation.id=?1",
            [&id],
            |row| row.get(0),
        )?;
        if from_source == crate::PARTITION_STUB_SOURCE {
            tx.execute(
                "UPDATE relations SET current_revision_id=?1,
                     canonical_relation_id=NULL, mirror_role='unbound_mirror'
                 WHERE id=?2",
                params![revision.to_string(), id],
            )?;
        } else {
            tx.execute(
                "UPDATE relations SET current_revision_id=?1,
                     canonical_relation_id=?2, mirror_role='canonical' WHERE id=?2",
                params![revision.to_string(), id],
            )?;
        }
        insert_legacy_request_event(tx, change_set, ordinal, ObjectKind::Relation, &id, revision)?;
        ordinal += 1;
    }
    tx.execute(
        "UPDATE domain_state SET semantic_generation=?1 WHERE singleton=1",
        [generation],
    )?;
    Ok(Some(generation))
}

impl MemoryStore {
    pub(crate) fn compatibility_set_observation_tier(
        &self,
        logical_id: &str,
        memory_tier: MemoryTier,
    ) -> MemoryResult<bool> {
        let Some(expected) = self.compatibility_observation_head(logical_id)? else {
            return Ok(false);
        };
        if expected.lifecycle != Lifecycle::Active {
            return Ok(false);
        }
        let draft = compatibility_draft(
            self.space_id(),
            "legacy set-tier compatibility adapter",
            ChangeOperation::SetObservationTier(SetObservationTier {
                logical_id: logical_id.to_string(),
                expected,
                memory_tier,
            }),
        );
        self.submit_changeset(&draft, SubmitMode::ApplyImmediately)?;
        Ok(true)
    }

    pub(crate) fn compatibility_retire_observation(&self, logical_id: &str) -> MemoryResult<bool> {
        let Some(expected) = self.compatibility_observation_head(logical_id)? else {
            return Ok(false);
        };
        if expected.lifecycle != Lifecycle::Active {
            return Ok(false);
        }
        let draft = compatibility_draft(
            self.space_id(),
            "legacy forget compatibility adapter",
            ChangeOperation::Retire(ObjectMutation {
                object: ObjectRef {
                    kind: ObjectKind::Observation,
                    logical_id: logical_id.to_string(),
                },
                expected,
            }),
        );
        self.submit_changeset(&draft, SubmitMode::ApplyImmediately)?;
        Ok(true)
    }

    fn compatibility_observation_head(
        &self,
        logical_id: &str,
    ) -> MemoryResult<Option<ExpectedHead>> {
        self.with_reader(|conn| {
            conn.query_row(
                "SELECT current_revision_id, row_version, lifecycle
                 FROM observations WHERE id=?1",
                [logical_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .map(|(revision, row_version, lifecycle)| {
                Ok(ExpectedHead {
                    revision_id: parse_optional_revision(revision)?,
                    row_version,
                    lifecycle: parse_lifecycle(&lifecycle)?,
                })
            })
            .transpose()
        })
    }
}

fn compatibility_draft(
    space_id: SpaceId,
    reason: &str,
    operation: ChangeOperation,
) -> ChangeSetDraft {
    let nonce = ChangeSetId::new();
    ChangeSetDraft {
        idempotency_key: format!("compat:{nonce}"),
        space_id,
        actor_principal: "compat:legacy"
            .parse()
            .expect("static compatibility principal is valid"),
        actor_kind: ActorKind::System,
        authorization_generation: 1,
        reason: reason.to_string(),
        source: "compatibility".to_string(),
        operations: vec![operation],
    }
}

fn legacy_request_hash(
    entities: &BTreeSet<String>,
    observations: &BTreeSet<String>,
    relations: &BTreeSet<String>,
    source: &str,
) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"openmemory/legacy-remember-audit/v1\0");
    for (tag, ids) in [
        (b"entity".as_slice(), entities),
        (b"observation".as_slice(), observations),
        (b"relation".as_slice(), relations),
    ] {
        hasher.update(tag);
        for id in ids {
            hasher.update(&(id.len() as u64).to_le_bytes());
            hasher.update(id.as_bytes());
        }
    }
    hasher.update(source.as_bytes());
    hasher.finalize()
}

fn insert_legacy_request_event(
    tx: &Transaction<'_>,
    change_set: ChangeSetId,
    ordinal: usize,
    kind: ObjectKind,
    logical_id: &str,
    revision: RevisionId,
) -> MemoryResult<()> {
    tx.execute(
        "INSERT INTO change_requests(
            change_set_id, ordinal, payload_version, object_kind, logical_id,
            operation, requested_payload_json)
         VALUES (?1, ?2, 1, ?3, ?4, 'legacy_remember', '{}')",
        params![change_set.to_string(), ordinal, kind.as_str(), logical_id,],
    )?;
    insert_event(
        tx,
        change_set,
        ordinal,
        kind,
        logical_id,
        "legacy_remember",
        None,
        Some(revision),
        None,
        Some(Lifecycle::Active),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationBindingDescriptor {
    pub relation_id: String,
    pub canonical_relation_id: Option<String>,
    pub mirror_role: String,
    pub from_name: String,
    pub from_is_stub: bool,
    pub to_name: String,
    pub relation_type: String,
    pub weight_bits: u64,
    pub source: String,
}

impl MemoryStore {
    /// Whether this physical domain owns the canonical object.
    pub fn contains_object(&self, object: &ObjectRef) -> MemoryResult<bool> {
        let table = match object.kind {
            ObjectKind::Entity => "entities",
            ObjectKind::Observation => "observations",
            ObjectKind::Relation => "relations",
        };
        self.with_reader(|conn| {
            conn.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)"),
                [&object.logical_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
    }

    /// Whether this physical domain stores a changeset/proposal.
    pub fn contains_changeset(&self, id: ChangeSetId) -> MemoryResult<bool> {
        self.with_reader(|conn| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM change_sets WHERE id=?1)",
                [id.to_string()],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
    }
}

fn validate_operation(operation: &ChangeOperation, nested: bool) -> MemoryResult<()> {
    match operation {
        ChangeOperation::Remember(change) => {
            validate_entity_value(&change.entity)?;
            if change.observations.len() > MAX_OPERATIONS || change.relations.len() > MAX_OPERATIONS
            {
                return Err(MemoryError::InvalidInput(
                    "remember child count exceeds 256".to_string(),
                ));
            }
            for observation in &change.observations {
                if let Some(id) = &observation.logical_id {
                    validate_logical_id(id)?;
                }
                validate_observation_value(&observation.value)?;
            }
            for relation in &change.relations {
                if let Some(id) = &relation.logical_id {
                    validate_logical_id(id)?;
                }
                bounded(
                    "relation target",
                    &relation.to_entity_name,
                    1,
                    MAX_LABEL_BYTES,
                )?;
                bounded("relation type", &relation.relation_type, 1, 256)?;
                bounded(
                    "relation source",
                    &relation.source,
                    0,
                    MAX_REASON_SOURCE_BYTES,
                )?;
                finite_unitish(relation.weight, "relation weight")?;
            }
        }
        ChangeOperation::SupersedeObservation(change) => {
            validate_logical_id(&change.logical_id)?;
            validate_observation_value(&change.value)?;
        }
        ChangeOperation::SetObservationTier(change) => validate_logical_id(&change.logical_id)?,
        ChangeOperation::Retire(change) | ChangeOperation::Restore(change) => {
            validate_logical_id(&change.object.logical_id)?;
        }
        ChangeOperation::RevertToRevision(change) => {
            validate_logical_id(&change.object.logical_id)?;
        }
        ChangeOperation::UpdateEntity(change) => {
            validate_logical_id(&change.logical_id)?;
            validate_entity_value(&change.value)?;
        }
        ChangeOperation::UpdateRelation(change) => {
            validate_logical_id(&change.logical_id)?;
            validate_relation_value(&change.value)?;
        }
        ChangeOperation::CherryPick(change) | ChangeOperation::MergeContribution(change) => {
            if nested {
                return Err(MemoryError::InvalidInput(
                    "nested provenance operations are forbidden".to_string(),
                ));
            }
            if change.origins.is_empty() || change.origins.len() > MAX_SET_ITEMS {
                return Err(MemoryError::InvalidInput(
                    "provenance requires 1..=1024 origins".to_string(),
                ));
            }
            let mut unique = BTreeSet::new();
            for origin in &change.origins {
                validate_logical_id(&origin.logical_id)?;
                if !unique.insert((
                    origin.space_id,
                    origin.logical_id.clone(),
                    origin.revision_id,
                )) {
                    return Err(MemoryError::InvalidInput(
                        "duplicate provenance origin".to_string(),
                    ));
                }
            }
            validate_operation(&change.operation, true)?;
        }
    }
    Ok(())
}

fn ids_without_revision(
    tx: &Transaction<'_>,
    table: &str,
    limit: usize,
) -> MemoryResult<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut statement = tx.prepare(&format!(
        "SELECT id FROM {table} WHERE current_revision_id IS NULL ORDER BY id LIMIT ?1"
    ))?;
    let ids = statement
        .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from)?;
    Ok(ids)
}

fn validate_observation_value(value: &ObservationValue) -> MemoryResult<()> {
    bounded("observation content", &value.content, 1, MAX_CONTENT_BYTES)?;
    bounded(
        "observation source",
        &value.source,
        0,
        MAX_REASON_SOURCE_BYTES,
    )?;
    optional_bounded("observation title", value.title.as_deref(), MAX_LABEL_BYTES)?;
    optional_bounded("observation summary", value.summary.as_deref(), 64 * 1024)?;
    optional_bounded("source kind", value.source_kind.as_deref(), 256)?;
    finite_unitish(value.confidence, "observation confidence")?;
    if value
        .importance
        .is_some_and(|importance| !importance.is_finite() || !(0.0..=1.0).contains(&importance))
    {
        return Err(MemoryError::InvalidInput(
            "observation importance must be finite and in [0,1]".to_string(),
        ));
    }
    validate_set(&value.concepts, "concepts", MAX_LABEL_BYTES)?;
    validate_set(&value.source_files, "source files", 16 * 1024)?;
    if value
        .valid_from
        .zip(value.valid_until)
        .is_some_and(|(from, until)| from > until)
    {
        return Err(MemoryError::InvalidInput(
            "observation validity interval is inverted".to_string(),
        ));
    }
    Ok(())
}

fn validate_entity_value(value: &EntityValue) -> MemoryResult<()> {
    bounded("entity name", &value.name, 1, MAX_LABEL_BYTES)?;
    bounded("entity source", &value.source, 0, MAX_REASON_SOURCE_BYTES)?;
    finite_unitish(value.confidence, "entity confidence")?;
    validate_set(&value.aliases, "aliases", MAX_LABEL_BYTES)?;
    if value.identifiers.len() > MAX_SET_ITEMS {
        return Err(MemoryError::InvalidInput(
            "too many entity identifiers".to_string(),
        ));
    }
    let mut identifiers = BTreeSet::new();
    for identifier in &value.identifiers {
        bounded("identifier namespace", &identifier.namespace, 1, 128)?;
        bounded("identifier value", &identifier.raw_value, 1, 1_024)?;
        optional_bounded(
            "canonical identifier",
            identifier.canonical_value.as_deref(),
            1_024,
        )?;
        if !matches!(identifier.trust.as_str(), "claimed" | "source_verified") {
            return Err(MemoryError::InvalidInput(
                "identifier trust is invalid".to_string(),
            ));
        }
        if identifier.trust == "source_verified"
            && (identifier.canonical_value.is_none()
                || identifier
                    .source_snapshot_id
                    .as_deref()
                    .unwrap_or("")
                    .is_empty()
                || identifier
                    .verifier_version
                    .as_deref()
                    .unwrap_or("")
                    .is_empty()
                || identifier.resolver_generation.unwrap_or(0) == 0)
        {
            return Err(MemoryError::InvalidInput(
                "verified identifier lacks bound verification metadata".to_string(),
            ));
        }
        if !identifiers.insert((&identifier.namespace, &identifier.raw_value)) {
            return Err(MemoryError::InvalidInput(
                "duplicate entity identifier".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_relation_value(value: &RelationValue) -> MemoryResult<()> {
    validate_logical_id(&value.from_entity)?;
    validate_logical_id(&value.to_entity)?;
    if value.from_entity == value.to_entity {
        return Err(MemoryError::InvalidInput(
            "self-relations are forbidden".to_string(),
        ));
    }
    bounded("relation type", &value.relation_type, 1, 256)?;
    bounded("relation source", &value.source, 0, MAX_REASON_SOURCE_BYTES)?;
    bounded("relation evidence", &value.evidence_json, 2, 64 * 1024)?;
    let _: serde_json::Value = serde_json::from_str(&value.evidence_json)?;
    finite_unitish(value.weight, "relation weight")?;
    Ok(())
}

fn bounded(field: &str, value: &str, minimum: usize, maximum: usize) -> MemoryResult<()> {
    if value.len() < minimum || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(MemoryError::InvalidInput(format!(
            "{field} must contain {minimum}..={maximum} non-control bytes"
        )));
    }
    Ok(())
}

fn optional_bounded(field: &str, value: Option<&str>, maximum: usize) -> MemoryResult<()> {
    if let Some(value) = value {
        bounded(field, value, 1, maximum)?;
    }
    Ok(())
}

fn validate_logical_id(value: &str) -> MemoryResult<()> {
    bounded("logical ID", value, 1, 256)?;
    if value == "." || value == ".." || value.contains(['/', '\\']) {
        return Err(MemoryError::InvalidInput(
            "logical ID is not safe".to_string(),
        ));
    }
    Ok(())
}

fn validate_set(values: &[String], field: &str, max_item: usize) -> MemoryResult<()> {
    if values.len() > MAX_SET_ITEMS {
        return Err(MemoryError::InvalidInput(format!(
            "{field} has too many items"
        )));
    }
    let mut sorted = BTreeSet::new();
    for value in values {
        bounded(field, value, 1, max_item)?;
        if !sorted.insert(value) {
            return Err(MemoryError::InvalidInput(format!(
                "{field} contains duplicates"
            )));
        }
    }
    Ok(())
}

fn finite_unitish(value: f32, field: &str) -> MemoryResult<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(MemoryError::InvalidInput(format!(
            "{field} must be finite and in [0,1]"
        )));
    }
    Ok(())
}

fn request_hash(encoded: &[u8]) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"openmemory/changeset-request/v1\0");
    hasher.update(&(encoded.len() as u64).to_le_bytes());
    hasher.update(encoded);
    hasher.finalize().as_bytes().to_vec()
}

fn generations(tx: &Transaction<'_>) -> MemoryResult<(u64, u64)> {
    tx.query_row(
        "SELECT semantic_generation, indexed_generation
         FROM domain_state WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(Into::into)
}

fn find_existing(
    tx: &Transaction<'_>,
    space: SpaceId,
    key: &str,
) -> MemoryResult<Option<ExistingChangeSet>> {
    let row = tx
        .query_row(
            "SELECT id, request_hash, state, committed_generation
             FROM change_sets WHERE space_id=?1 AND idempotency_key=?2",
            params![space.to_string(), key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<u64>>(3)?,
                ))
            },
        )
        .optional()?;
    row.map(|(id, request_hash, state, committed_generation)| {
        Ok(ExistingChangeSet {
            id: id
                .parse()
                .map_err(|error| MemoryError::Schema(format!("invalid changeset ID: {error}")))?,
            request_hash,
            state: ChangeSetState::parse(&state)?,
            committed_generation,
        })
    })
    .transpose()
}

fn insert_changeset(
    tx: &Transaction<'_>,
    id: ChangeSetId,
    draft: &ChangeSetDraft,
    request_hash: &[u8],
    base_generation: u64,
    now: i64,
) -> MemoryResult<()> {
    tx.execute(
        "INSERT INTO change_sets(
            id, space_id, idempotency_key, request_hash, actor_principal, actor_kind,
            authorization_generation, reason, source, state, base_generation, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'proposed', ?10, ?11)",
        params![
            id.to_string(),
            draft.space_id.to_string(),
            draft.idempotency_key,
            request_hash,
            draft.actor_principal.to_string(),
            actor_kind(draft.actor_kind),
            draft.authorization_generation,
            draft.reason,
            draft.source,
            base_generation,
            now
        ],
    )?;
    Ok(())
}

fn insert_requests(
    tx: &Transaction<'_>,
    id: ChangeSetId,
    operations: &[ChangeOperation],
) -> MemoryResult<()> {
    for (ordinal, operation) in operations.iter().enumerate() {
        let (kind, logical_id, name) = operation_metadata(operation);
        tx.execute(
            "INSERT INTO change_requests(
                change_set_id, ordinal, payload_version, object_kind, logical_id,
                operation, requested_payload_json)
             VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6)",
            params![
                id.to_string(),
                ordinal as u64,
                kind.as_str(),
                logical_id,
                name,
                serde_json::to_string(operation)?
            ],
        )?;
    }
    Ok(())
}

fn operation_metadata(operation: &ChangeOperation) -> (ObjectKind, String, &'static str) {
    match operation {
        ChangeOperation::Remember(change) => {
            (ObjectKind::Entity, change.entity.name.clone(), "remember")
        }
        ChangeOperation::SupersedeObservation(change) => (
            ObjectKind::Observation,
            change.logical_id.clone(),
            "supersede_observation",
        ),
        ChangeOperation::SetObservationTier(change) => (
            ObjectKind::Observation,
            change.logical_id.clone(),
            "set_observation_tier",
        ),
        ChangeOperation::Retire(change) => (
            change.object.kind,
            change.object.logical_id.clone(),
            "retire",
        ),
        ChangeOperation::Restore(change) => (
            change.object.kind,
            change.object.logical_id.clone(),
            "restore",
        ),
        ChangeOperation::RevertToRevision(change) => (
            change.object.kind,
            change.object.logical_id.clone(),
            "revert_to_revision",
        ),
        ChangeOperation::UpdateEntity(change) => (
            ObjectKind::Entity,
            change.logical_id.clone(),
            "update_entity",
        ),
        ChangeOperation::UpdateRelation(change) => (
            ObjectKind::Relation,
            change.logical_id.clone(),
            "update_relation",
        ),
        ChangeOperation::CherryPick(change) => {
            let (kind, logical, _) = operation_metadata(&change.operation);
            (kind, logical, "cherry_pick")
        }
        ChangeOperation::MergeContribution(change) => {
            let (kind, logical, _) = operation_metadata(&change.operation);
            (kind, logical, "merge_contribution")
        }
    }
}

fn actor_kind(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

fn load_proposal(
    tx: &Transaction<'_>,
    id: ChangeSetId,
) -> MemoryResult<(ChangeSetDraft, Vec<u8>, ChangeSetState)> {
    let metadata = tx
        .query_row(
            "SELECT space_id, idempotency_key, request_hash, actor_principal, actor_kind,
                    authorization_generation, reason, source, state
             FROM change_sets WHERE id=?1",
            [id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, u64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| MemoryError::ChangeSetStale("proposal does not exist".to_string()))?;
    let mut statement = tx.prepare(
        "SELECT requested_payload_json FROM change_requests
         WHERE change_set_id=?1 ORDER BY ordinal",
    )?;
    let operations = statement
        .query_map([id.to_string()], |row| row.get::<_, String>(0))?
        .map(|row| -> MemoryResult<ChangeOperation> { Ok(serde_json::from_str(&row?)?) })
        .collect::<MemoryResult<Vec<_>>>()?;
    let actor_kind = match metadata.4.as_str() {
        "human" => ActorKind::Human,
        "agent" => ActorKind::Agent,
        "system" => ActorKind::System,
        _ => {
            return Err(MemoryError::Schema(
                "invalid changeset actor kind".to_string(),
            ))
        }
    };
    Ok((
        ChangeSetDraft {
            idempotency_key: metadata.1,
            space_id: metadata.0.parse().map_err(|error| {
                MemoryError::Schema(format!("invalid changeset space ID: {error}"))
            })?,
            actor_principal: metadata.3.parse().map_err(|error| {
                MemoryError::Schema(format!("invalid changeset principal: {error}"))
            })?,
            actor_kind,
            authorization_generation: metadata.5,
            reason: metadata.6,
            source: metadata.7,
            operations,
        },
        metadata.2,
        ChangeSetState::parse(&metadata.8)?,
    ))
}

fn mark_applied(
    tx: &Transaction<'_>,
    id: ChangeSetId,
    generation: u64,
    now: i64,
    decided_by: &PrincipalId,
) -> MemoryResult<()> {
    tx.execute(
        "UPDATE domain_state SET semantic_generation=?1 WHERE singleton=1",
        [generation],
    )?;
    let changed = tx.execute(
        "UPDATE change_sets SET state='applied', committed_generation=?1,
             decided_at=?2, decided_by=?3 WHERE id=?4 AND state='proposed'",
        params![generation, now, decided_by.to_string(), id.to_string()],
    )?;
    if changed != 1 {
        return Err(MemoryError::ChangeSetStale(
            "changeset was concurrently decided".to_string(),
        ));
    }
    Ok(())
}

fn apply_operations(
    tx: &Transaction<'_>,
    change_set: ChangeSetId,
    draft: &ChangeSetDraft,
    generation: u64,
    now: i64,
) -> MemoryResult<()> {
    for (ordinal, operation) in draft.operations.iter().enumerate() {
        apply_operation(
            tx,
            draft.space_id,
            change_set,
            ordinal,
            operation,
            generation,
            now,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_operation(
    tx: &Transaction<'_>,
    space_id: SpaceId,
    change_set: ChangeSetId,
    ordinal: usize,
    operation: &ChangeOperation,
    generation: u64,
    now: i64,
) -> MemoryResult<()> {
    match operation {
        ChangeOperation::Remember(change) => {
            apply_remember(tx, space_id, change_set, ordinal, change, generation, now)
        }
        ChangeOperation::SupersedeObservation(change) => apply_observation_update(
            tx,
            space_id,
            change_set,
            ordinal,
            &change.logical_id,
            &change.expected,
            &change.value,
            generation,
            now,
            "supersede_observation",
        ),
        ChangeOperation::SetObservationTier(change) => {
            let current = load_observation(tx, &change.logical_id)?;
            check_expected(&change.expected, &current.head)?;
            let mut value = current.value;
            value.memory_tier = change.memory_tier;
            apply_observation_update(
                tx,
                space_id,
                change_set,
                ordinal,
                &change.logical_id,
                &change.expected,
                &value,
                generation,
                now,
                "set_observation_tier",
            )
        }
        ChangeOperation::Retire(change) => apply_lifecycle(
            tx,
            change_set,
            ordinal,
            change,
            Lifecycle::Retired,
            generation,
            now,
        ),
        ChangeOperation::Restore(change) => apply_lifecycle(
            tx,
            change_set,
            ordinal,
            change,
            Lifecycle::Active,
            generation,
            now,
        ),
        ChangeOperation::RevertToRevision(change) => {
            apply_revert(tx, space_id, change_set, ordinal, change, generation, now)
        }
        ChangeOperation::UpdateEntity(change) => {
            apply_entity_update(tx, space_id, change_set, ordinal, change, generation, now)
        }
        ChangeOperation::UpdateRelation(change) => {
            apply_relation_update(tx, space_id, change_set, ordinal, change, generation, now)
        }
        ChangeOperation::CherryPick(change) | ChangeOperation::MergeContribution(change) => {
            let first_event: u64 = tx.query_row(
                "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM change_events
                 WHERE change_set_id=?1",
                [change_set.to_string()],
                |row| row.get(0),
            )?;
            apply_operation(
                tx,
                space_id,
                change_set,
                ordinal,
                &change.operation,
                generation,
                now,
            )?;
            let mut statement = tx.prepare(
                "SELECT object_kind, logical_id FROM change_events
                 WHERE change_set_id=?1 AND ordinal>=?2 ORDER BY ordinal",
            )?;
            let events = statement
                .query_map(params![change_set.to_string(), first_event], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            let primary_kind = operation_metadata(&change.operation).0;
            let has_exact = events.iter().any(|(_, logical_id)| {
                change
                    .origins
                    .iter()
                    .any(|origin| origin.logical_id == *logical_id)
            });
            for (kind, logical_id) in events {
                let kind = parse_object_kind(&kind)?;
                let exact = change
                    .origins
                    .iter()
                    .filter(|origin| origin.logical_id == logical_id)
                    .collect::<Vec<_>>();
                let selected = if !has_exact && exact.is_empty() && kind == primary_kind {
                    change.origins.iter().collect::<Vec<_>>()
                } else {
                    exact
                };
                if selected.is_empty() {
                    continue;
                }
                let semantic_hash = current_semantic_hash(tx, kind, &logical_id)?;
                for origin in selected {
                    tx.execute(
                        "INSERT OR IGNORE INTO origin_contributions(
                            object_kind, target_logical_id, origin_space_id, origin_logical_id,
                            origin_revision_id, semantic_hash, contribution_json,
                            imported_by_change_set, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            kind.as_str(),
                            logical_id,
                            origin.space_id.to_string(),
                            origin.logical_id,
                            origin.revision_id.to_string(),
                            semantic_hash,
                            serde_json::to_string(origin)?,
                            change_set.to_string(),
                            now
                        ],
                    )?;
                }
            }
            Ok(())
        }
    }
}

#[derive(Debug)]
struct ProjectionHead {
    revision_id: Option<RevisionId>,
    row_version: u64,
    lifecycle: Lifecycle,
}

#[derive(Debug)]
struct ObservationProjection {
    head: ProjectionHead,
    value: ObservationValue,
}

fn load_observation(tx: &Transaction<'_>, id: &str) -> MemoryResult<ObservationProjection> {
    let raw = tx
        .query_row(
            "SELECT content, observed_at, valid_from, valid_until,
                    confidence, source, memory_tier, title, summary, importance,
                    source_kind, current_revision_id, row_version, lifecycle
             FROM observations WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, f32>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<f32>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, u64>(12)?,
                    row.get::<_, String>(13)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| MemoryError::ObservationNotFound(id.to_string()))?;
    let tier = MemoryTier::parse(&raw.6)
        .ok_or_else(|| MemoryError::Schema("invalid observation memory tier".to_string()))?;
    Ok(ObservationProjection {
        value: ObservationValue {
            content: raw.0,
            observed_at: raw.1,
            valid_from: raw.2,
            valid_until: raw.3,
            confidence: raw.4,
            source: raw.5,
            memory_tier: tier,
            title: raw.7,
            summary: raw.8,
            importance: raw.9,
            source_kind: raw.10,
            concepts: strings(
                tx,
                "SELECT concept FROM observation_concepts
                 WHERE observation_id=?1 ORDER BY concept",
                id,
            )?,
            source_files: strings(
                tx,
                "SELECT file_path FROM observation_source_files
                 WHERE observation_id=?1 ORDER BY file_path",
                id,
            )?,
        },
        head: ProjectionHead {
            revision_id: parse_optional_revision(raw.11)?,
            row_version: raw.12,
            lifecycle: parse_lifecycle(&raw.13)?,
        },
    })
}

fn check_expected(expected: &ExpectedHead, actual: &ProjectionHead) -> MemoryResult<()> {
    if expected.revision_id != actual.revision_id
        || expected.row_version != actual.row_version
        || expected.lifecycle != actual.lifecycle
    {
        return Err(MemoryError::ChangeSetStale(format!(
            "expected revision {:?}/version {}/{}, found {:?}/version {}/{}",
            expected.revision_id,
            expected.row_version,
            expected.lifecycle.as_str(),
            actual.revision_id,
            actual.row_version,
            actual.lifecycle.as_str()
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_observation_update(
    tx: &Transaction<'_>,
    space_id: SpaceId,
    change_set: ChangeSetId,
    ordinal: usize,
    id: &str,
    expected: &ExpectedHead,
    value: &ObservationValue,
    generation: u64,
    now: i64,
    operation: &str,
) -> MemoryResult<()> {
    let mut current = load_observation(tx, id)?;
    check_expected(expected, &current.head)?;
    if current.head.lifecycle == Lifecycle::Destroyed {
        return Err(MemoryError::ChangeSetStale(
            "destroyed observations cannot be edited".to_string(),
        ));
    }
    if current.head.revision_id.is_none() {
        current.head.revision_id = Some(insert_observation_baseline(
            tx,
            space_id,
            id,
            &current,
            generation.saturating_sub(1),
            now,
        )?);
    }
    let revision =
        insert_observation_revision(tx, change_set, id, current.head.revision_id, value, now)?;
    tx.execute(
        "UPDATE observations SET content=?1, observed_at=?2, valid_from=?3,
             valid_until=?4, confidence=?5, source=?6, memory_tier=?7,
             title=?8, summary=?9, importance=?10, source_kind=?11,
             current_revision_id=?12, row_version=row_version+1
         WHERE id=?13",
        params![
            value.content,
            value.observed_at,
            value.valid_from,
            value.valid_until,
            value.confidence,
            value.source,
            value.memory_tier.as_str(),
            value.title,
            value.summary,
            value.importance,
            value.source_kind,
            revision.to_string(),
            id
        ],
    )?;
    replace_observation_sets(tx, id, value)?;
    insert_event(
        tx,
        change_set,
        ordinal,
        ObjectKind::Observation,
        id,
        operation,
        current.head.revision_id,
        Some(revision),
        Some(current.head.lifecycle),
        Some(current.head.lifecycle),
    )?;
    enqueue_index(tx, generation, id, "upsert", Some(revision))?;
    Ok(())
}

fn insert_observation_revision(
    tx: &Transaction<'_>,
    change_set: ChangeSetId,
    observation_id: &str,
    parent: Option<RevisionId>,
    value: &ObservationValue,
    now: i64,
) -> MemoryResult<RevisionId> {
    let revision = RevisionId::new();
    let hash = observation_hash(value);
    tx.execute(
        "INSERT INTO observation_revisions(
            id, observation_id, parent_revision_id, semantic_hash, content,
            observed_at, valid_from, valid_until, confidence, source, memory_tier,
            title, summary, importance, source_kind, created_by_change_set, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                 ?13, ?14, ?15, ?16, ?17)",
        params![
            revision.to_string(),
            observation_id,
            parent.map(|id| id.to_string()),
            hash,
            value.content,
            value.observed_at,
            value.valid_from,
            value.valid_until,
            value.confidence,
            value.source,
            value.memory_tier.as_str(),
            value.title,
            value.summary,
            value.importance,
            value.source_kind,
            change_set.to_string(),
            now
        ],
    )?;
    for concept in sorted_unique(&value.concepts) {
        tx.execute(
            "INSERT INTO observation_revision_concepts(revision_id, concept)
             VALUES (?1, ?2)",
            params![revision.to_string(), concept],
        )?;
    }
    for file in sorted_unique(&value.source_files) {
        tx.execute(
            "INSERT INTO observation_revision_source_files(revision_id, file_path)
             VALUES (?1, ?2)",
            params![revision.to_string(), file],
        )?;
    }
    Ok(revision)
}

fn insert_observation_baseline(
    tx: &Transaction<'_>,
    space_id: SpaceId,
    observation_id: &str,
    current: &ObservationProjection,
    generation: u64,
    now: i64,
) -> MemoryResult<RevisionId> {
    let baseline = insert_baseline_changeset(
        tx,
        space_id,
        ObjectKind::Observation,
        observation_id,
        &observation_hash(&current.value),
        generation,
        now,
    )?;
    let revision =
        insert_observation_revision(tx, baseline, observation_id, None, &current.value, now)?;
    tx.execute(
        "UPDATE observations SET current_revision_id=?1 WHERE id=?2",
        params![revision.to_string(), observation_id],
    )?;
    Ok(revision)
}

fn replace_observation_sets(
    tx: &Transaction<'_>,
    id: &str,
    value: &ObservationValue,
) -> MemoryResult<()> {
    tx.execute(
        "DELETE FROM observation_concepts WHERE observation_id=?1",
        [id],
    )?;
    tx.execute(
        "DELETE FROM observation_source_files WHERE observation_id=?1",
        [id],
    )?;
    for concept in sorted_unique(&value.concepts) {
        tx.execute(
            "INSERT INTO observation_concepts(observation_id, concept) VALUES (?1, ?2)",
            params![id, concept],
        )?;
    }
    for file in sorted_unique(&value.source_files) {
        tx.execute(
            "INSERT INTO observation_source_files(observation_id, file_path) VALUES (?1, ?2)",
            params![id, file],
        )?;
    }
    Ok(())
}

fn apply_remember(
    tx: &Transaction<'_>,
    _space_id: SpaceId,
    change_set: ChangeSetId,
    ordinal: usize,
    change: &RememberChange,
    generation: u64,
    now: i64,
) -> MemoryResult<()> {
    let existing = tx
        .query_row(
            "SELECT id FROM entities WHERE name=?1 AND entity_type=?2",
            params![change.entity.name, change.entity.entity_type.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let entity_id = if let Some(id) = existing {
        id
    } else {
        let id = new_id();
        tx.execute(
            "INSERT INTO entities(
                id, name, entity_type, created_at, updated_at, confidence, source,
                row_version, lifecycle)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, 1, 'active')",
            params![
                id,
                change.entity.name,
                change.entity.entity_type.as_str(),
                now,
                change.entity.confidence,
                change.entity.source
            ],
        )?;
        let revision = insert_entity_revision(tx, change_set, &id, None, &change.entity, now)?;
        tx.execute(
            "UPDATE entities SET current_revision_id=?1 WHERE id=?2",
            params![revision.to_string(), id],
        )?;
        insert_event(
            tx,
            change_set,
            ordinal,
            ObjectKind::Entity,
            &id,
            "remember",
            None,
            Some(revision),
            None,
            Some(Lifecycle::Active),
        )?;
        id
    };

    for observation in &change.observations {
        let id = observation.logical_id.clone().unwrap_or_else(new_id);
        let revision =
            insert_observation_revision(tx, change_set, &id, None, &observation.value, now)?;
        tx.execute(
            "INSERT INTO observations(
                id, entity_id, content, observed_at, valid_from, valid_until,
                confidence, source, tombstoned, access_count, memory_tier,
                title, summary, importance, source_kind, current_revision_id,
                row_version, lifecycle)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, ?9, ?10,
                     ?11, ?12, ?13, ?14, 1, 'active')",
            params![
                id,
                entity_id,
                observation.value.content,
                observation.value.observed_at,
                observation.value.valid_from,
                observation.value.valid_until,
                observation.value.confidence,
                observation.value.source,
                observation.value.memory_tier.as_str(),
                observation.value.title,
                observation.value.summary,
                observation.value.importance,
                observation.value.source_kind,
                revision.to_string(),
            ],
        )?;
        replace_observation_sets(tx, &id, &observation.value)?;
        insert_event(
            tx,
            change_set,
            ordinal,
            ObjectKind::Observation,
            &id,
            "remember",
            None,
            Some(revision),
            None,
            Some(Lifecycle::Active),
        )?;
        enqueue_index(tx, generation, &id, "upsert", Some(revision))?;
    }

    for relation in &change.relations {
        let target = tx
            .query_row(
                "SELECT id FROM entities WHERE name=?1 AND entity_type=?2",
                params![relation.to_entity_name, relation.to_entity_type.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let target_id = if let Some(id) = target {
            id
        } else {
            let id = new_id();
            let target_value = EntityValue {
                name: relation.to_entity_name.clone(),
                entity_type: relation.to_entity_type,
                confidence: 1.0,
                source: relation.source.clone(),
                aliases: Vec::new(),
                identifiers: Vec::new(),
            };
            tx.execute(
                "INSERT INTO entities(
                    id, name, entity_type, created_at, updated_at, confidence, source,
                    row_version, lifecycle)
                 VALUES (?1, ?2, ?3, ?4, ?4, 1.0, ?5, 1, 'active')",
                params![
                    id,
                    target_value.name,
                    target_value.entity_type.as_str(),
                    now,
                    target_value.source
                ],
            )?;
            let revision = insert_entity_revision(tx, change_set, &id, None, &target_value, now)?;
            tx.execute(
                "UPDATE entities SET current_revision_id=?1 WHERE id=?2",
                params![revision.to_string(), id],
            )?;
            id
        };
        if target_id == entity_id {
            return Err(MemoryError::InvalidInput(
                "self-relations are forbidden".to_string(),
            ));
        }
        let id = relation.logical_id.clone().unwrap_or_else(new_id);
        let value = RelationValue {
            from_entity: entity_id.clone(),
            to_entity: target_id,
            relation_type: relation.relation_type.clone(),
            weight: relation.weight,
            valid_from: None,
            valid_until: None,
            source: relation.source.clone(),
            evidence_json: "{}".to_string(),
        };
        let revision = insert_relation_revision(tx, change_set, &id, None, &value, now)?;
        tx.execute(
            "INSERT INTO relations(
                id, from_entity, to_entity, relation_type, weight, created_at,
                valid_from, valid_until, source, canonical_relation_id, mirror_role,
                current_revision_id, row_version, lifecycle)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?1, 'canonical',
                     ?8, 1, 'active')",
            params![
                id,
                value.from_entity,
                value.to_entity,
                value.relation_type,
                value.weight,
                now,
                value.source,
                revision.to_string()
            ],
        )?;
        insert_event(
            tx,
            change_set,
            ordinal,
            ObjectKind::Relation,
            &id,
            "remember",
            None,
            Some(revision),
            None,
            Some(Lifecycle::Active),
        )?;
    }
    Ok(())
}

fn apply_entity_update(
    tx: &Transaction<'_>,
    space_id: SpaceId,
    change_set: ChangeSetId,
    ordinal: usize,
    change: &UpdateEntity,
    generation: u64,
    now: i64,
) -> MemoryResult<()> {
    let mut current = load_entity(tx, &change.logical_id)?;
    check_expected(&change.expected, &current.head)?;
    if current.head.revision_id.is_none() {
        current.head.revision_id = Some(insert_entity_baseline(
            tx,
            space_id,
            &change.logical_id,
            &current,
            generation.saturating_sub(1),
            now,
        )?);
    }
    let revision = insert_entity_revision(
        tx,
        change_set,
        &change.logical_id,
        current.head.revision_id,
        &change.value,
        now,
    )?;
    tx.execute(
        "UPDATE entities SET name=?1, entity_type=?2, confidence=?3, source=?4,
             updated_at=?5, current_revision_id=?6, row_version=row_version+1
         WHERE id=?7",
        params![
            change.value.name,
            change.value.entity_type.as_str(),
            change.value.confidence,
            change.value.source,
            now,
            revision.to_string(),
            change.logical_id
        ],
    )?;
    insert_event(
        tx,
        change_set,
        ordinal,
        ObjectKind::Entity,
        &change.logical_id,
        "update_entity",
        current.head.revision_id,
        Some(revision),
        Some(current.head.lifecycle),
        Some(current.head.lifecycle),
    )?;
    enqueue_entity_observations(tx, generation, &change.logical_id)?;
    Ok(())
}

#[derive(Debug)]
struct EntityProjection {
    head: ProjectionHead,
    value: EntityValue,
}

fn load_entity(tx: &Transaction<'_>, id: &str) -> MemoryResult<EntityProjection> {
    let raw = tx
        .query_row(
            "SELECT name, entity_type, confidence, source, current_revision_id,
                    row_version, lifecycle
             FROM entities WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f32>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, u64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| MemoryError::EntityNotFound(id.to_string()))?;
    let entity_type = EntityType::parse(&raw.1)
        .ok_or_else(|| MemoryError::Schema("invalid entity type".to_string()))?;
    let (aliases, identifiers) = if let Some(revision) = raw.4.as_deref() {
        (
            strings(
                tx,
                "SELECT alias FROM entity_revision_aliases
                 WHERE revision_id=?1 ORDER BY alias",
                revision,
            )?,
            load_identifiers(tx, revision)?,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(EntityProjection {
        value: EntityValue {
            name: raw.0,
            entity_type,
            confidence: raw.2,
            source: raw.3,
            aliases,
            identifiers,
        },
        head: ProjectionHead {
            revision_id: parse_optional_revision(raw.4)?,
            row_version: raw.5,
            lifecycle: parse_lifecycle(&raw.6)?,
        },
    })
}

fn insert_entity_revision(
    tx: &Transaction<'_>,
    change_set: ChangeSetId,
    entity_id: &str,
    parent: Option<RevisionId>,
    value: &EntityValue,
    now: i64,
) -> MemoryResult<RevisionId> {
    let revision = RevisionId::new();
    tx.execute(
        "INSERT INTO entity_revisions(
            id, entity_id, parent_revision_id, semantic_hash, name, entity_type,
            confidence, source, created_by_change_set, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            revision.to_string(),
            entity_id,
            parent.map(|id| id.to_string()),
            entity_hash(value),
            value.name,
            value.entity_type.as_str(),
            value.confidence,
            value.source,
            change_set.to_string(),
            now
        ],
    )?;
    for alias in sorted_unique(&value.aliases) {
        tx.execute(
            "INSERT INTO entity_revision_aliases(revision_id, alias) VALUES (?1, ?2)",
            params![revision.to_string(), alias],
        )?;
    }
    let mut identifiers = value.identifiers.clone();
    identifiers.sort();
    for identifier in identifiers {
        tx.execute(
            "INSERT INTO entity_revision_identifiers(
                revision_id, namespace, raw_value, canonical_value, trust,
                source_snapshot_id, verifier_version, resolver_generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                revision.to_string(),
                identifier.namespace,
                identifier.raw_value,
                identifier.canonical_value,
                identifier.trust,
                identifier.source_snapshot_id,
                identifier.verifier_version,
                identifier.resolver_generation
            ],
        )?;
    }
    Ok(revision)
}

fn insert_entity_baseline(
    tx: &Transaction<'_>,
    space_id: SpaceId,
    entity_id: &str,
    current: &EntityProjection,
    generation: u64,
    now: i64,
) -> MemoryResult<RevisionId> {
    let baseline = insert_baseline_changeset(
        tx,
        space_id,
        ObjectKind::Entity,
        entity_id,
        &entity_hash(&current.value),
        generation,
        now,
    )?;
    let revision = insert_entity_revision(tx, baseline, entity_id, None, &current.value, now)?;
    tx.execute(
        "UPDATE entities SET current_revision_id=?1 WHERE id=?2",
        params![revision.to_string(), entity_id],
    )?;
    Ok(revision)
}

#[derive(Debug)]
struct RelationProjection {
    head: ProjectionHead,
    value: RelationValue,
}

fn load_relation(tx: &Transaction<'_>, id: &str) -> MemoryResult<RelationProjection> {
    let raw = tx
        .query_row(
            "SELECT from_entity, to_entity, relation_type, weight, valid_from,
                    valid_until, source, current_revision_id, row_version, lifecycle,
                    mirror_role
             FROM relations WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f32>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, u64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| MemoryError::InvalidInput(format!("relation not found: {id}")))?;
    if raw.10 != "canonical" {
        return Err(MemoryError::InvalidInput(
            "mirror relations cannot be edited directly".to_string(),
        ));
    }
    let evidence_json = raw
        .7
        .as_deref()
        .map(|revision| {
            tx.query_row(
                "SELECT evidence_json FROM relation_revisions WHERE id=?1",
                [revision],
                |row| row.get::<_, String>(0),
            )
        })
        .transpose()?
        .unwrap_or_else(|| "{}".to_string());
    Ok(RelationProjection {
        value: RelationValue {
            from_entity: raw.0,
            to_entity: raw.1,
            relation_type: raw.2,
            weight: raw.3,
            valid_from: raw.4,
            valid_until: raw.5,
            source: raw.6,
            evidence_json,
        },
        head: ProjectionHead {
            revision_id: parse_optional_revision(raw.7)?,
            row_version: raw.8,
            lifecycle: parse_lifecycle(&raw.9)?,
        },
    })
}

fn apply_relation_update(
    tx: &Transaction<'_>,
    space_id: SpaceId,
    change_set: ChangeSetId,
    ordinal: usize,
    change: &UpdateRelation,
    generation: u64,
    now: i64,
) -> MemoryResult<()> {
    let mut current = load_relation(tx, &change.logical_id)?;
    check_expected(&change.expected, &current.head)?;
    if current.value.from_entity != change.value.from_entity
        || current.value.to_entity != change.value.to_entity
    {
        return Err(MemoryError::InvalidInput(
            "relation endpoint changes require retire + add".to_string(),
        ));
    }
    let mirror_ready: String = tx.query_row(
        "SELECT backfill_state FROM domain_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if mirror_ready != "ready" {
        return Err(MemoryError::InvalidInput(
            "relation editing is disabled until mirror backfill is ready".to_string(),
        ));
    }
    if current.head.revision_id.is_none() {
        current.head.revision_id = Some(insert_relation_baseline(
            tx,
            space_id,
            &change.logical_id,
            &current,
            generation.saturating_sub(1),
            now,
        )?);
    }
    let revision = insert_relation_revision(
        tx,
        change_set,
        &change.logical_id,
        current.head.revision_id,
        &change.value,
        now,
    )?;
    tx.execute(
        "UPDATE relations SET relation_type=?1, weight=?2, valid_from=?3,
             valid_until=?4, source=?5, current_revision_id=?6,
             row_version=row_version+1 WHERE id=?7",
        params![
            change.value.relation_type,
            change.value.weight,
            change.value.valid_from,
            change.value.valid_until,
            change.value.source,
            revision.to_string(),
            change.logical_id
        ],
    )?;
    insert_event(
        tx,
        change_set,
        ordinal,
        ObjectKind::Relation,
        &change.logical_id,
        "update_relation",
        current.head.revision_id,
        Some(revision),
        Some(current.head.lifecycle),
        Some(current.head.lifecycle),
    )?;
    enqueue_mirror(tx, generation, &change.logical_id, "upsert", &change.value)?;
    Ok(())
}

fn insert_relation_revision(
    tx: &Transaction<'_>,
    change_set: ChangeSetId,
    relation_id: &str,
    parent: Option<RevisionId>,
    value: &RelationValue,
    now: i64,
) -> MemoryResult<RevisionId> {
    let revision = RevisionId::new();
    tx.execute(
        "INSERT INTO relation_revisions(
            id, relation_id, parent_revision_id, semantic_hash, from_entity,
            to_entity, relation_type, weight, valid_from, valid_until, source,
            evidence_json, created_by_change_set, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            revision.to_string(),
            relation_id,
            parent.map(|id| id.to_string()),
            relation_hash(value),
            value.from_entity,
            value.to_entity,
            value.relation_type,
            value.weight,
            value.valid_from,
            value.valid_until,
            value.source,
            value.evidence_json,
            change_set.to_string(),
            now
        ],
    )?;
    Ok(revision)
}

fn insert_relation_baseline(
    tx: &Transaction<'_>,
    space_id: SpaceId,
    relation_id: &str,
    current: &RelationProjection,
    generation: u64,
    now: i64,
) -> MemoryResult<RevisionId> {
    let baseline = insert_baseline_changeset(
        tx,
        space_id,
        ObjectKind::Relation,
        relation_id,
        &relation_hash(&current.value),
        generation,
        now,
    )?;
    let revision = insert_relation_revision(tx, baseline, relation_id, None, &current.value, now)?;
    tx.execute(
        "UPDATE relations SET current_revision_id=?1, canonical_relation_id=id,
             mirror_role='canonical' WHERE id=?2",
        params![revision.to_string(), relation_id],
    )?;
    Ok(revision)
}

fn apply_lifecycle(
    tx: &Transaction<'_>,
    change_set: ChangeSetId,
    ordinal: usize,
    change: &ObjectMutation,
    target: Lifecycle,
    generation: u64,
    _now: i64,
) -> MemoryResult<()> {
    if target == Lifecycle::Active && change.expected.lifecycle != Lifecycle::Retired {
        return Err(MemoryError::ChangeSetStale(
            "restore requires an expected retired head".to_string(),
        ));
    }
    if target == Lifecycle::Retired && change.expected.lifecycle != Lifecycle::Active {
        return Err(MemoryError::ChangeSetStale(
            "retire requires an expected active head".to_string(),
        ));
    }
    let actual = object_head(tx, &change.object)?;
    check_expected(&change.expected, &actual)?;
    if actual.lifecycle == Lifecycle::Destroyed {
        return Err(MemoryError::ChangeSetStale(
            "destroyed objects cannot transition lifecycle".to_string(),
        ));
    }
    let table = match change.object.kind {
        ObjectKind::Entity => "entities",
        ObjectKind::Observation => "observations",
        ObjectKind::Relation => "relations",
    };
    if change.object.kind == ObjectKind::Observation {
        tx.execute(
            "UPDATE observations SET lifecycle=?1, tombstoned=?2,
                 row_version=row_version+1 WHERE id=?3",
            params![
                target.as_str(),
                target == Lifecycle::Retired,
                change.object.logical_id
            ],
        )?;
    } else {
        tx.execute(
            &format!("UPDATE {table} SET lifecycle=?1, row_version=row_version+1 WHERE id=?2"),
            params![target.as_str(), change.object.logical_id],
        )?;
    }
    insert_event(
        tx,
        change_set,
        ordinal,
        change.object.kind,
        &change.object.logical_id,
        if target == Lifecycle::Active {
            "restore"
        } else {
            "retire"
        },
        actual.revision_id,
        actual.revision_id,
        Some(actual.lifecycle),
        Some(target),
    )?;
    match change.object.kind {
        ObjectKind::Observation => enqueue_index(
            tx,
            generation,
            &change.object.logical_id,
            if target == Lifecycle::Active {
                "upsert"
            } else {
                "delete"
            },
            actual.revision_id,
        )?,
        ObjectKind::Entity => {
            enqueue_entity_observations(tx, generation, &change.object.logical_id)?;
        }
        ObjectKind::Relation => {
            let relation = load_relation(tx, &change.object.logical_id)?;
            enqueue_mirror(
                tx,
                generation,
                &change.object.logical_id,
                if target == Lifecycle::Active {
                    "upsert"
                } else {
                    "delete"
                },
                &relation.value,
            )?;
        }
    }
    Ok(())
}

fn object_head(tx: &Transaction<'_>, object: &ObjectRef) -> MemoryResult<ProjectionHead> {
    match object.kind {
        ObjectKind::Entity => Ok(load_entity(tx, &object.logical_id)?.head),
        ObjectKind::Observation => Ok(load_observation(tx, &object.logical_id)?.head),
        ObjectKind::Relation => Ok(load_relation(tx, &object.logical_id)?.head),
    }
}

fn apply_revert(
    tx: &Transaction<'_>,
    space_id: SpaceId,
    change_set: ChangeSetId,
    ordinal: usize,
    change: &RevertToRevision,
    generation: u64,
    now: i64,
) -> MemoryResult<()> {
    match change.object.kind {
        ObjectKind::Observation => {
            let value =
                load_observation_revision(tx, &change.object.logical_id, change.revision_id)?;
            apply_observation_update(
                tx,
                space_id,
                change_set,
                ordinal,
                &change.object.logical_id,
                &change.expected,
                &value,
                generation,
                now,
                "revert_to_revision",
            )
        }
        ObjectKind::Entity => {
            let value = load_entity_revision(tx, &change.object.logical_id, change.revision_id)?;
            apply_entity_update(
                tx,
                space_id,
                change_set,
                ordinal,
                &UpdateEntity {
                    logical_id: change.object.logical_id.clone(),
                    expected: change.expected.clone(),
                    value,
                },
                generation,
                now,
            )
        }
        ObjectKind::Relation => {
            let value = load_relation_revision(tx, &change.object.logical_id, change.revision_id)?;
            apply_relation_update(
                tx,
                space_id,
                change_set,
                ordinal,
                &UpdateRelation {
                    logical_id: change.object.logical_id.clone(),
                    expected: change.expected.clone(),
                    value,
                },
                generation,
                now,
            )
        }
    }
}

pub(crate) fn load_observation_revision(
    tx: &rusqlite::Connection,
    observation_id: &str,
    revision: RevisionId,
) -> MemoryResult<ObservationValue> {
    let raw = tx
        .query_row(
            "SELECT content, observed_at, valid_from, valid_until, confidence, source,
                    memory_tier, title, summary, importance, source_kind
             FROM observation_revisions WHERE id=?1 AND observation_id=?2",
            params![revision.to_string(), observation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, f32>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<f32>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            MemoryError::InvalidInput("observation revision does not belong to object".to_string())
        })?;
    Ok(ObservationValue {
        content: raw.0,
        observed_at: raw.1,
        valid_from: raw.2,
        valid_until: raw.3,
        confidence: raw.4,
        source: raw.5,
        memory_tier: MemoryTier::parse(&raw.6)
            .ok_or_else(|| MemoryError::Schema("invalid memory tier".to_string()))?,
        title: raw.7,
        summary: raw.8,
        importance: raw.9,
        source_kind: raw.10,
        concepts: strings(
            tx,
            "SELECT concept FROM observation_revision_concepts
             WHERE revision_id=?1 ORDER BY concept",
            &revision.to_string(),
        )?,
        source_files: strings(
            tx,
            "SELECT file_path FROM observation_revision_source_files
             WHERE revision_id=?1 ORDER BY file_path",
            &revision.to_string(),
        )?,
    })
}

pub(crate) fn load_entity_revision(
    tx: &rusqlite::Connection,
    entity_id: &str,
    revision: RevisionId,
) -> MemoryResult<EntityValue> {
    let raw = tx
        .query_row(
            "SELECT name, entity_type, confidence, source
             FROM entity_revisions WHERE id=?1 AND entity_id=?2",
            params![revision.to_string(), entity_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f32>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            MemoryError::InvalidInput("entity revision does not belong to object".to_string())
        })?;
    Ok(EntityValue {
        name: raw.0,
        entity_type: EntityType::parse(&raw.1)
            .ok_or_else(|| MemoryError::Schema("invalid entity type".to_string()))?,
        confidence: raw.2,
        source: raw.3,
        aliases: strings(
            tx,
            "SELECT alias FROM entity_revision_aliases
             WHERE revision_id=?1 ORDER BY alias",
            &revision.to_string(),
        )?,
        identifiers: load_identifiers(tx, &revision.to_string())?,
    })
}

fn load_identifiers(
    tx: &rusqlite::Connection,
    revision: &str,
) -> MemoryResult<Vec<EntityIdentifier>> {
    let mut statement = tx.prepare(
        "SELECT namespace, raw_value, canonical_value, trust, source_snapshot_id,
                verifier_version, resolver_generation
         FROM entity_revision_identifiers
         WHERE revision_id=?1 ORDER BY namespace, raw_value",
    )?;
    let identifiers = statement
        .query_map([revision], |row| {
            Ok(EntityIdentifier {
                namespace: row.get(0)?,
                raw_value: row.get(1)?,
                canonical_value: row.get(2)?,
                trust: row.get(3)?,
                source_snapshot_id: row.get(4)?,
                verifier_version: row.get(5)?,
                resolver_generation: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from)?;
    Ok(identifiers)
}

pub(crate) fn load_relation_revision(
    tx: &rusqlite::Connection,
    relation_id: &str,
    revision: RevisionId,
) -> MemoryResult<RelationValue> {
    tx.query_row(
        "SELECT from_entity, to_entity, relation_type, weight, valid_from,
                valid_until, source, evidence_json
         FROM relation_revisions WHERE id=?1 AND relation_id=?2",
        params![revision.to_string(), relation_id],
        |row| {
            Ok(RelationValue {
                from_entity: row.get(0)?,
                to_entity: row.get(1)?,
                relation_type: row.get(2)?,
                weight: row.get(3)?,
                valid_from: row.get(4)?,
                valid_until: row.get(5)?,
                source: row.get(6)?,
                evidence_json: row.get(7)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| {
        MemoryError::InvalidInput("relation revision does not belong to object".to_string())
    })
}

fn insert_baseline_changeset(
    tx: &Transaction<'_>,
    space: SpaceId,
    kind: ObjectKind,
    logical_id: &str,
    semantic_hash: &[u8],
    generation: u64,
    now: i64,
) -> MemoryResult<ChangeSetId> {
    let key = format!("baseline:{}:{logical_id}", kind.as_str());
    if let Some(id) = tx
        .query_row(
            "SELECT id FROM change_sets WHERE space_id=?1 AND idempotency_key=?2",
            params![space.to_string(), key],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return id.parse().map_err(|error| {
            MemoryError::Schema(format!("invalid baseline changeset ID: {error}"))
        });
    }
    let id = ChangeSetId::new();
    tx.execute(
        "INSERT INTO change_sets(
            id, space_id, idempotency_key, request_hash, actor_principal, actor_kind,
            authorization_generation, reason, source, state, base_generation,
            committed_generation, created_at, decided_at, decided_by)
         VALUES (?1, ?2, ?3, ?4, 'system:legacy-backfill', 'system', 1,
                 'lazy legacy baseline', 'migration:graph-v4', 'applied',
                 ?5, ?5, ?6, ?6, 'system:legacy-backfill')",
        params![
            id.to_string(),
            space.to_string(),
            key,
            semantic_hash,
            generation,
            now
        ],
    )?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
fn insert_event(
    tx: &Transaction<'_>,
    change_set: ChangeSetId,
    ordinal: usize,
    kind: ObjectKind,
    logical_id: &str,
    operation: &str,
    before_revision: Option<RevisionId>,
    after_revision: Option<RevisionId>,
    before_lifecycle: Option<Lifecycle>,
    after_lifecycle: Option<Lifecycle>,
) -> MemoryResult<()> {
    // A remember operation may emit several physical rows for one requested
    // ordinal. Stable sub-ordinals keep the WITHOUT ROWID key collision-free.
    let event_ordinal: u64 = tx.query_row(
        "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM change_events
         WHERE change_set_id=?1",
        [change_set.to_string()],
        |row| row.get(0),
    )?;
    let payload = serde_json::json!({"request_ordinal": ordinal});
    tx.execute(
        "INSERT INTO change_events(
            change_set_id, ordinal, object_kind, logical_id, operation,
            before_revision_id, after_revision_id, before_lifecycle,
            after_lifecycle, compact_payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            change_set.to_string(),
            event_ordinal,
            kind.as_str(),
            logical_id,
            operation,
            before_revision.map(|id| id.to_string()),
            after_revision.map(|id| id.to_string()),
            before_lifecycle.map(Lifecycle::as_str),
            after_lifecycle.map(Lifecycle::as_str),
            serde_json::to_string(&payload)?
        ],
    )?;
    Ok(())
}

fn enqueue_index(
    tx: &Transaction<'_>,
    generation: u64,
    id: &str,
    operation: &str,
    revision: Option<RevisionId>,
) -> MemoryResult<()> {
    tx.execute(
        "INSERT INTO index_outbox(
            generation, object_kind, logical_id, operation, revision_id)
         VALUES (?1, 'observation', ?2, ?3, ?4)
         ON CONFLICT(generation, object_kind, logical_id) DO UPDATE SET
            operation=excluded.operation, revision_id=excluded.revision_id,
            attempts=0, last_error=NULL",
        params![generation, id, operation, revision.map(|id| id.to_string())],
    )?;
    Ok(())
}

fn enqueue_entity_observations(
    tx: &Transaction<'_>,
    generation: u64,
    entity_id: &str,
) -> MemoryResult<()> {
    let lifecycle: String = tx.query_row(
        "SELECT lifecycle FROM entities WHERE id=?1",
        [entity_id],
        |row| row.get(0),
    )?;
    let mut statement = tx.prepare(
        "SELECT id, current_revision_id, lifecycle FROM observations
         WHERE entity_id=?1 ORDER BY id",
    )?;
    let rows = statement
        .query_map([entity_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (id, revision, observation_lifecycle) in rows {
        let operation = if lifecycle == "active" && observation_lifecycle == "active" {
            "upsert"
        } else {
            "delete"
        };
        enqueue_index(
            tx,
            generation,
            &id,
            operation,
            parse_optional_revision(revision)?,
        )?;
    }
    Ok(())
}

fn enqueue_mirror(
    tx: &Transaction<'_>,
    generation: u64,
    relation_id: &str,
    operation: &str,
    value: &RelationValue,
) -> MemoryResult<()> {
    tx.execute(
        "INSERT INTO mirror_outbox(
            generation, canonical_relation_id, operation, target_domain, payload_json)
         VALUES (?1, ?2, ?3, 0, ?4)
         ON CONFLICT(generation, canonical_relation_id, target_domain) DO UPDATE SET
            operation=excluded.operation, payload_json=excluded.payload_json,
            attempts=0, last_error=NULL",
        params![
            generation,
            relation_id,
            operation,
            serde_json::to_string(value)?
        ],
    )?;
    Ok(())
}

fn current_semantic_hash(
    tx: &Transaction<'_>,
    kind: ObjectKind,
    id: &str,
) -> MemoryResult<Vec<u8>> {
    let (projection_table, revision_table) = match kind {
        ObjectKind::Entity => ("entities", "entity_revisions"),
        ObjectKind::Observation => ("observations", "observation_revisions"),
        ObjectKind::Relation => ("relations", "relation_revisions"),
    };
    tx.query_row(
        &format!(
            "SELECT revision.semantic_hash FROM {projection_table} AS projection
             JOIN {revision_table} AS revision
               ON revision.id=projection.current_revision_id
             WHERE projection.id=?1"
        ),
        [id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn parse_optional_revision(value: Option<String>) -> MemoryResult<Option<RevisionId>> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|error| MemoryError::Schema(format!("invalid revision ID: {error}")))
        })
        .transpose()
}

fn parse_lifecycle(value: &str) -> MemoryResult<Lifecycle> {
    match value {
        "active" => Ok(Lifecycle::Active),
        "retired" => Ok(Lifecycle::Retired),
        "destroyed" => Ok(Lifecycle::Destroyed),
        _ => Err(MemoryError::Schema(
            "invalid semantic lifecycle".to_string(),
        )),
    }
}

fn strings(conn: &rusqlite::Connection, sql: &str, parameter: &str) -> MemoryResult<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let values = statement
        .query_map([parameter], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from)?;
    Ok(values)
}

fn sorted_unique(values: &[String]) -> Vec<&str> {
    values
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn observation_hash(value: &ObservationValue) -> Vec<u8> {
    let mut hasher = SemanticHasher::new(b"openmemory/observation/v1");
    hasher.string(&value.content);
    hasher.i64(value.observed_at);
    hasher.optional_i64(value.valid_from);
    hasher.optional_i64(value.valid_until);
    hasher.f32(value.confidence);
    hasher.string(&value.source);
    hasher.string(value.memory_tier.as_str());
    hasher.optional_string(value.title.as_deref());
    hasher.optional_string(value.summary.as_deref());
    hasher.optional_f32(value.importance);
    hasher.optional_string(value.source_kind.as_deref());
    hasher.set(&value.concepts);
    hasher.set(&value.source_files);
    hasher.finish()
}

fn entity_hash(value: &EntityValue) -> Vec<u8> {
    let mut hasher = SemanticHasher::new(b"openmemory/entity/v1");
    hasher.string(&value.name);
    hasher.string(value.entity_type.as_str());
    hasher.f32(value.confidence);
    hasher.string(&value.source);
    hasher.set(&value.aliases);
    let mut identifiers = value.identifiers.clone();
    identifiers.sort();
    hasher.u64(identifiers.len() as u64);
    for identifier in identifiers {
        hasher.string(&identifier.namespace);
        hasher.string(&identifier.raw_value);
        hasher.optional_string(identifier.canonical_value.as_deref());
        hasher.string(&identifier.trust);
        hasher.optional_string(identifier.source_snapshot_id.as_deref());
        hasher.optional_string(identifier.verifier_version.as_deref());
        hasher.u64(identifier.resolver_generation.unwrap_or(0));
    }
    hasher.finish()
}

fn relation_hash(value: &RelationValue) -> Vec<u8> {
    let mut hasher = SemanticHasher::new(b"openmemory/relation/v1");
    hasher.string(&value.from_entity);
    hasher.string(&value.to_entity);
    hasher.string(&value.relation_type);
    hasher.f32(value.weight);
    hasher.optional_i64(value.valid_from);
    hasher.optional_i64(value.valid_until);
    hasher.string(&value.source);
    hasher.string(&value.evidence_json);
    hasher.finish()
}

struct SemanticHasher(blake3::Hasher);

impl SemanticHasher {
    fn new(domain: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(domain.len() as u64).to_le_bytes());
        hasher.update(domain);
        Self(hasher)
    }

    fn bytes(&mut self, value: &[u8]) {
        self.0.update(&(value.len() as u64).to_le_bytes());
        self.0.update(value);
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.0.update(&value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.0.update(&value.to_bits().to_le_bytes());
    }

    fn optional_i64(&mut self, value: Option<i64>) {
        self.0.update(&[u8::from(value.is_some())]);
        if let Some(value) = value {
            self.i64(value);
        }
    }

    fn optional_f32(&mut self, value: Option<f32>) {
        self.0.update(&[u8::from(value.is_some())]);
        if let Some(value) = value {
            self.f32(value);
        }
    }

    fn optional_string(&mut self, value: Option<&str>) {
        self.0.update(&[u8::from(value.is_some())]);
        if let Some(value) = value {
            self.string(value);
        }
    }

    fn set(&mut self, values: &[String]) {
        let values = sorted_unique(values);
        self.u64(values.len() as u64);
        for value in values {
            self.string(value);
        }
    }

    fn finish(self) -> Vec<u8> {
        self.0.finalize().as_bytes().to_vec()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{EntityType, ObservationInput, RecallFilters};
    use openmemory_core::clock::FixedClock;
    use openmemory_core::config::Config;
    use std::sync::Arc;

    fn principal(value: &str) -> PrincipalId {
        value.parse().unwrap()
    }

    pub(crate) fn initial_store() -> (MemoryStore, String) {
        let store = MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_clock(Arc::new(FixedClock::new(100)));
        let outcome = store
            .remember(
                "audit subject",
                EntityType::Fact,
                &[ObservationInput::new("before")],
                &[],
                "test",
            )
            .unwrap();
        (store, outcome.observation_ids[0].clone())
    }

    fn replacement(content: &str) -> ObservationValue {
        ObservationValue {
            content: content.to_string(),
            observed_at: 100,
            valid_from: Some(100),
            valid_until: None,
            confidence: 1.0,
            source: "manual:test".to_string(),
            memory_tier: MemoryTier::Episodic,
            title: None,
            summary: None,
            importance: None,
            source_kind: None,
            concepts: vec!["audit".to_string()],
            source_files: Vec::new(),
        }
    }

    pub(crate) fn draft(store: &MemoryStore, id: &str, key: &str, content: &str) -> ChangeSetDraft {
        let (revision_id, row_version, lifecycle) = store
            .object_head(&ObjectRef {
                kind: ObjectKind::Observation,
                logical_id: id.to_string(),
            })
            .unwrap()
            .unwrap();
        ChangeSetDraft {
            idempotency_key: key.to_string(),
            space_id: store.space_id(),
            actor_principal: principal("local:editor"),
            actor_kind: ActorKind::Human,
            authorization_generation: 1,
            reason: "correct stale memory".to_string(),
            source: "admin:test".to_string(),
            operations: vec![ChangeOperation::SupersedeObservation(
                SupersedeObservation {
                    logical_id: id.to_string(),
                    expected: ExpectedHead {
                        revision_id,
                        row_version,
                        lifecycle: parse_lifecycle(&lifecycle).unwrap(),
                    },
                    value: replacement(content),
                },
            )],
        }
    }

    #[test]
    fn apply_creates_baseline_head_event_generation_and_drains_outbox() {
        let (store, id) = initial_store();
        let receipt = store
            .submit_changeset(
                &draft(&store, &id, "edit-1", "after"),
                SubmitMode::ApplyImmediately,
            )
            .unwrap();
        assert_eq!(receipt.state, ChangeSetState::Applied);
        assert!(receipt.index_ready);
        let conn = store.lock_db();
        let heads: (Option<String>, u64, String) = conn
            .query_row(
                "SELECT current_revision_id, row_version, content
                 FROM observations WHERE id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(heads.0.is_some());
        assert_eq!(heads.1, 2);
        assert_eq!(heads.2, "after");
        let revisions: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM observation_revisions WHERE observation_id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revisions, 2, "lazy baseline plus corrected head");
        let events: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM change_events WHERE change_set_id=?1",
                [receipt.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events, 1);
        let outbox: u64 = conn
            .query_row("SELECT COUNT(*) FROM index_outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outbox, 0);
    }

    #[test]
    fn idempotency_replay_and_conflict_are_exact() {
        let (store, id) = initial_store();
        let request = draft(&store, &id, "same-key", "after");
        let first = store
            .submit_changeset(&request, SubmitMode::ApplyImmediately)
            .unwrap();
        let replay = store
            .submit_changeset(&request, SubmitMode::ApplyImmediately)
            .unwrap();
        assert_eq!(first.id, replay.id);
        assert!(replay.idempotent_replay);
        assert!(matches!(
            store.submit_changeset(
                &draft(&store, &id, "same-key", "different"),
                SubmitMode::ApplyImmediately
            ),
            Err(MemoryError::IdempotencyConflict)
        ));
    }

    #[test]
    fn applied_changeset_revert_is_atomic_audited_and_idempotent() {
        let (store, id) = initial_store();
        let applied = store
            .submit_changeset(
                &draft(&store, &id, "edit-before-revert", "after"),
                SubmitMode::ApplyImmediately,
            )
            .unwrap();
        let reverted = store
            .revert_changeset(
                applied.id,
                &principal("local:reviewer"),
                ActorKind::Human,
                1,
                "revert-edit",
                "undo mistaken edit",
            )
            .unwrap();
        assert_eq!(reverted.state, ChangeSetState::Applied);
        let entity = store.get_entity("audit subject").unwrap().unwrap();
        let observation = store
            .get_entity_observations(&entity.id)
            .unwrap()
            .into_iter()
            .find(|observation| observation.id == id)
            .unwrap();
        assert_eq!(observation.content, "before");
        assert_eq!(
            store.get_changeset(applied.id).unwrap().unwrap().summary.state,
            ChangeSetState::Reverted
        );
        let replay = store
            .revert_changeset(
                applied.id,
                &principal("local:reviewer"),
                ActorKind::Human,
                1,
                "revert-edit",
                "undo mistaken edit",
            )
            .unwrap();
        assert_eq!(replay.id, reverted.id);
        assert!(replay.idempotent_replay);
    }

    #[test]
    fn changeset_revert_fails_closed_after_object_advances() {
        let (store, id) = initial_store();
        let applied = store
            .submit_changeset(
                &draft(&store, &id, "first-edit", "after"),
                SubmitMode::ApplyImmediately,
            )
            .unwrap();
        store
            .submit_changeset(
                &draft(&store, &id, "second-edit", "newer"),
                SubmitMode::ApplyImmediately,
            )
            .unwrap();
        assert!(matches!(
            store.revert_changeset(
                applied.id,
                &principal("local:reviewer"),
                ActorKind::Human,
                1,
                "stale-revert",
                "must not overwrite newer work",
            ),
            Err(MemoryError::ChangeSetStale(_))
        ));
    }

    #[test]
    fn proposal_is_invisible_until_distinct_human_approval() {
        let (store, id) = initial_store();
        let proposed = store
            .submit_changeset(
                &draft(&store, &id, "proposal", "approved needle"),
                SubmitMode::Propose,
            )
            .unwrap();
        assert_eq!(proposed.state, ChangeSetState::Proposed);
        assert!(store
            .recall("approved needle", 10, &RecallFilters::new())
            .unwrap()
            .is_empty());
        assert!(store
            .approve_changeset(proposed.id, &principal("local:editor"), ActorKind::Human, 1)
            .is_err());
        let applied = store
            .approve_changeset(
                proposed.id,
                &principal("local:reviewer"),
                ActorKind::Human,
                1,
            )
            .unwrap();
        assert_eq!(applied.state, ChangeSetState::Applied);
        assert!(!store
            .recall("approved needle", 10, &RecallFilters::new())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn stale_proposal_conflicts_without_overwriting() {
        let (store, id) = initial_store();
        let proposal = store
            .submit_changeset(
                &draft(&store, &id, "proposal-stale", "losing value"),
                SubmitMode::Propose,
            )
            .unwrap();
        store
            .submit_changeset(
                &draft(&store, &id, "winning-edit", "winning value"),
                SubmitMode::ApplyImmediately,
            )
            .unwrap();
        assert!(matches!(
            store.approve_changeset(
                proposal.id,
                &principal("local:reviewer"),
                ActorKind::Human,
                1
            ),
            Err(MemoryError::ChangeSetStale(_))
        ));
        let conn = store.lock_db();
        let state: String = conn
            .query_row(
                "SELECT state FROM change_sets WHERE id=?1",
                [proposal.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "conflicted");
        let content: String = conn
            .query_row(
                "SELECT content FROM observations WHERE id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(content, "winning value");
    }
}
