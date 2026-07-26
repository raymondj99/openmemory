//! Guarded, explicitly scoped legal destruction.
//!
//! Ordinary deletion is lifecycle retirement. This module is the narrow human
//! administrative exception: a short-lived preview binds an exact head,
//! generation, scope, and inventory; execution purges semantic payload while
//! retaining only a content-free receipt.

use openmemory_core::space::{ActorKind, PrincipalId, RevisionId};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::changeset::{ObjectKind, ObjectRef};
use crate::{MemoryError, MemoryResult, MemoryStore};

const MAX_PREVIEW_IDS: usize = 256;
const CONFIRMATION_TTL_SECS: i64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestroyInventory {
    pub canonical_rows: u64,
    pub revisions: u64,
    pub contributions: u64,
    pub related_change_requests: u64,
    pub affected_ids: Vec<String>,
    pub affected_ids_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestroyPreview {
    pub object: ObjectRef,
    pub scope: String,
    pub inventory: DestroyInventory,
    pub confirmation: String,
    pub expires_at: i64,
    pub expected_revision_id: Option<RevisionId>,
    pub expected_row_version: u64,
    pub expected_lifecycle: String,
    pub expected_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestroyReceipt {
    pub id: String,
    pub object_kind: ObjectKind,
    pub scope: String,
    pub semantic_generation: u64,
    pub indexed_generation: u64,
    pub index_ready: bool,
    pub destroyed_at: i64,
}

impl MemoryStore {
    /// Persist a five-minute, one-use confirmation bound to the current
    /// semantic head and exact destruction inventory.
    pub fn preview_destroy(
        &self,
        object: ObjectRef,
        scope: &str,
    ) -> MemoryResult<DestroyPreview> {
        validate_scope(&object, scope)?;
        let (revision, row_version, lifecycle) = self.object_head(&object)?.ok_or_else(|| {
            MemoryError::InvalidInput("destruction target does not exist".to_string())
        })?;
        if lifecycle == "destroyed" {
            return Err(MemoryError::ChangeSetStale(
                "destruction target is already destroyed".to_string(),
            ));
        }
        let revision = revision.ok_or_else(|| {
            MemoryError::InvalidInput(
                "destruction requires immutable history readiness".to_string(),
            )
        })?;
        let now = self.clock().now_secs();
        let expires_at = now.saturating_add(CONFIRMATION_TTL_SECS);
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let generation: u64 = tx.query_row(
            "SELECT semantic_generation FROM domain_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        let inventory = inventory(&tx, &object, scope)?;
        let inventory_json = serde_json::to_string(&inventory)?;
        let mut token_hasher = blake3::Hasher::new();
        token_hasher.update(b"openmemory/destroy-confirmation/v1\0");
        token_hasher.update(crate::new_id().as_bytes());
        token_hasher.update(object.kind.as_str().as_bytes());
        token_hasher.update(object.logical_id.as_bytes());
        token_hasher.update(scope.as_bytes());
        token_hasher.update(revision.to_string().as_bytes());
        token_hasher.update(&row_version.to_le_bytes());
        token_hasher.update(&generation.to_le_bytes());
        token_hasher.update(inventory_json.as_bytes());
        let confirmation = token_hasher.finalize().to_hex().to_string();
        let confirmation_hash = blake3::hash(confirmation.as_bytes());
        tx.execute(
            "DELETE FROM destruction_previews
             WHERE expires_at<?1 OR consumed_at IS NOT NULL",
            [now],
        )?;
        tx.execute(
            "INSERT INTO destruction_previews(
                confirmation_hash, object_kind, logical_id, scope,
                expected_revision_id, expected_row_version, expected_lifecycle,
                expected_generation, inventory_json, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                confirmation_hash.as_bytes().as_slice(),
                object.kind.as_str(),
                object.logical_id,
                scope,
                revision.to_string(),
                row_version,
                lifecycle,
                generation,
                inventory_json,
                expires_at
            ],
        )?;
        tx.commit()?;
        Ok(DestroyPreview {
            object,
            scope: scope.to_string(),
            inventory,
            confirmation,
            expires_at,
            expected_revision_id: Some(revision),
            expected_row_version: row_version,
            expected_lifecycle: lifecycle,
            expected_generation: generation,
        })
    }

    /// Consume a current preview and irreversibly purge its exact scope.
    pub fn destroy_previewed(
        &self,
        object: ObjectRef,
        scope: &str,
        confirmation: &str,
        actor_principal: &PrincipalId,
        actor_kind: ActorKind,
        reason: &str,
    ) -> MemoryResult<DestroyReceipt> {
        if actor_kind != ActorKind::Human {
            return Err(MemoryError::Authorization(
                "only a human maintainer can destroy memory".to_string(),
            ));
        }
        validate_scope(&object, scope)?;
        if confirmation.len() != 64 || reason.is_empty() || reason.len() > 2_048 {
            return Err(MemoryError::InvalidInput(
                "current confirmation and a bounded reason are required".to_string(),
            ));
        }
        let confirmation_hash = blake3::hash(confirmation.as_bytes());
        let now = self.clock().now_secs();
        let _barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let preview = tx
            .query_row(
                "SELECT object_kind, logical_id, scope, expected_revision_id,
                        expected_row_version, expected_lifecycle,
                        expected_generation, inventory_json, expires_at,
                        consumed_at
                 FROM destruction_previews WHERE confirmation_hash=?1",
                [confirmation_hash.as_bytes().as_slice()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, u64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, u64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| MemoryError::Authorization("confirmation is invalid".to_string()))?;
        if preview.0 != object.kind.as_str()
            || preview.1 != object.logical_id
            || preview.2 != scope
            || preview.8 < now
            || preview.9.is_some()
        {
            return Err(MemoryError::Authorization(
                "confirmation is expired, consumed, or targets another scope".to_string(),
            ));
        }
        let expected_revision = preview
            .3
            .as_deref()
            .map(str::parse::<RevisionId>)
            .transpose()
            .map_err(|error| MemoryError::Schema(format!("invalid preview revision: {error}")))?;
        let (current_revision, row_version, lifecycle) =
            tx_object_head(&tx, &object)?.ok_or_else(|| {
                MemoryError::ChangeSetStale("destruction target disappeared".to_string())
            })?;
        let generation: u64 = tx.query_row(
            "SELECT semantic_generation FROM domain_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        if current_revision != expected_revision
            || row_version != preview.4
            || lifecycle != preview.5
            || generation != preview.6
        {
            return Err(MemoryError::ChangeSetStale(
                "destruction target or space generation advanced after preview".to_string(),
            ));
        }
        let current_inventory = inventory(&tx, &object, scope)?;
        let current_inventory_json = serde_json::to_string(&current_inventory)?;
        if !constant_time_eq(current_inventory_json.as_bytes(), preview.7.as_bytes()) {
            return Err(MemoryError::ChangeSetStale(
                "destruction inventory changed after preview".to_string(),
            ));
        }

        let next_generation = generation.saturating_add(1);
        enqueue_destroyed_derivations(&tx, &object, scope, next_generation)?;
        purge_semantic_payload(&tx, &object, scope)?;
        let receipt_id = format!("destroy:{}", crate::new_id());
        let logical_id_hash = blake3::hash(object.logical_id.as_bytes());
        let inventory_hash = blake3::hash(current_inventory_json.as_bytes());
        let reason_hash = blake3::hash(reason.as_bytes());
        tx.execute(
            "INSERT INTO destruction_receipts(
                id, object_kind, logical_id_hash, scope, inventory_hash,
                reason_hash, actor_principal, semantic_generation, destroyed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                receipt_id,
                object.kind.as_str(),
                logical_id_hash.as_bytes().as_slice(),
                scope,
                inventory_hash.as_bytes().as_slice(),
                reason_hash.as_bytes().as_slice(),
                actor_principal.to_string(),
                next_generation,
                now
            ],
        )?;
        tx.execute(
            "UPDATE destruction_previews SET consumed_at=?1
             WHERE confirmation_hash=?2 AND consumed_at IS NULL",
            params![now, confirmation_hash.as_bytes().as_slice()],
        )?;
        tx.execute(
            "UPDATE domain_state SET semantic_generation=?1 WHERE singleton=1",
            [next_generation],
        )?;
        tx.commit()?;
        drop(conn);
        let indexed_generation = self.drain_index_outbox(next_generation)?;
        Ok(DestroyReceipt {
            id: receipt_id,
            object_kind: object.kind,
            scope: scope.to_string(),
            semantic_generation: next_generation,
            indexed_generation,
            index_ready: indexed_generation >= next_generation,
            destroyed_at: now,
        })
    }
}

fn validate_scope(object: &ObjectRef, scope: &str) -> MemoryResult<()> {
    let valid = match object.kind {
        ObjectKind::Entity => scope == "entity_cascade",
        ObjectKind::Observation | ObjectKind::Relation => scope == "object",
    };
    if !valid {
        return Err(MemoryError::InvalidInput(match object.kind {
            ObjectKind::Entity => {
                "entity destruction requires explicit scope `entity_cascade`".to_string()
            }
            ObjectKind::Observation | ObjectKind::Relation => {
                "observation/relation destruction requires scope `object`".to_string()
            }
        }));
    }
    Ok(())
}

fn inventory(
    tx: &rusqlite::Transaction<'_>,
    object: &ObjectRef,
    scope: &str,
) -> MemoryResult<DestroyInventory> {
    validate_scope(object, scope)?;
    let (canonical_rows, revisions, contributions, related_change_requests) = match object.kind {
        ObjectKind::Observation => (
            count(tx, "SELECT COUNT(*) FROM observations WHERE id=?1", &object.logical_id)?,
            count(
                tx,
                "SELECT COUNT(*) FROM observation_revisions WHERE observation_id=?1",
                &object.logical_id,
            )?,
            contribution_count(tx, "observation", &object.logical_id)?,
            related_request_count(tx, "observation", &object.logical_id)?,
        ),
        ObjectKind::Relation => (
            count(tx, "SELECT COUNT(*) FROM relations WHERE id=?1", &object.logical_id)?,
            count(
                tx,
                "SELECT COUNT(*) FROM relation_revisions WHERE relation_id=?1",
                &object.logical_id,
            )?,
            contribution_count(tx, "relation", &object.logical_id)?,
            related_request_count(tx, "relation", &object.logical_id)?,
        ),
        ObjectKind::Entity => entity_inventory(tx, &object.logical_id)?,
    };
    if canonical_rows == 0 {
        return Err(MemoryError::InvalidInput(
            "destruction target does not exist".to_string(),
        ));
    }
    let (affected_ids, affected_ids_truncated) = affected_ids(tx, object)?;
    Ok(DestroyInventory {
        canonical_rows,
        revisions,
        contributions,
        related_change_requests,
        affected_ids,
        affected_ids_truncated,
    })
}

fn entity_inventory(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
) -> MemoryResult<(u64, u64, u64, u64)> {
    let entity = count(tx, "SELECT COUNT(*) FROM entities WHERE id=?1", id)?;
    let observations = count(
        tx,
        "SELECT COUNT(*) FROM observations WHERE entity_id=?1",
        id,
    )?;
    let relations = count(
        tx,
        "SELECT COUNT(*) FROM relations WHERE from_entity=?1 OR to_entity=?1",
        id,
    )?;
    let revisions: u64 = tx.query_row(
        "SELECT
            (SELECT COUNT(*) FROM entity_revisions WHERE entity_id=?1) +
            (SELECT COUNT(*) FROM observation_revisions WHERE observation_id IN
                (SELECT id FROM observations WHERE entity_id=?1)) +
            (SELECT COUNT(*) FROM relation_revisions WHERE relation_id IN
                (SELECT id FROM relations WHERE from_entity=?1 OR to_entity=?1))",
        [id],
        |row| row.get(0),
    )?;
    let contributions: u64 = tx.query_row(
        "SELECT COUNT(*) FROM origin_contributions
         WHERE (object_kind='entity' AND target_logical_id=?1)
            OR (object_kind='observation' AND target_logical_id IN
                (SELECT id FROM observations WHERE entity_id=?1))
            OR (object_kind='relation' AND target_logical_id IN
                (SELECT id FROM relations WHERE from_entity=?1 OR to_entity=?1))",
        [id],
        |row| row.get(0),
    )?;
    let requests: u64 = tx.query_row(
        "SELECT COUNT(*) FROM change_requests WHERE change_set_id IN (
            SELECT DISTINCT change_set_id FROM change_events
            WHERE (object_kind='entity' AND logical_id=?1)
               OR (object_kind='observation' AND logical_id IN
                   (SELECT id FROM observations WHERE entity_id=?1))
               OR (object_kind='relation' AND logical_id IN
                   (SELECT id FROM relations WHERE from_entity=?1 OR to_entity=?1)))",
        [id],
        |row| row.get(0),
    )?;
    Ok((
        entity.saturating_add(observations).saturating_add(relations),
        revisions,
        contributions,
        requests,
    ))
}

fn affected_ids(
    tx: &rusqlite::Transaction<'_>,
    object: &ObjectRef,
) -> MemoryResult<(Vec<String>, bool)> {
    if object.kind != ObjectKind::Entity {
        return Ok((vec![object.logical_id.clone()], false));
    }
    let mut statement = tx.prepare(
        "SELECT id FROM (
            SELECT id, 0 AS priority FROM entities WHERE id=?1
            UNION ALL SELECT id, 1 FROM observations WHERE entity_id=?1
            UNION ALL SELECT id, 2 FROM relations
                WHERE from_entity=?1 OR to_entity=?1)
         ORDER BY priority, id LIMIT ?2",
    )?;
    let mut ids = statement
        .query_map(
            params![object.logical_id, MAX_PREVIEW_IDS.saturating_add(1)],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let truncated = ids.len() > MAX_PREVIEW_IDS;
    ids.truncate(MAX_PREVIEW_IDS);
    Ok((ids, truncated))
}

fn enqueue_destroyed_derivations(
    tx: &rusqlite::Transaction<'_>,
    object: &ObjectRef,
    scope: &str,
    generation: u64,
) -> MemoryResult<()> {
    validate_scope(object, scope)?;
    match object.kind {
        ObjectKind::Observation => {
            tx.execute(
                "DELETE FROM index_outbox
                 WHERE object_kind='observation' AND logical_id=?1",
                [&object.logical_id],
            )?;
            tx.execute(
                "INSERT INTO index_outbox(
                    generation, object_kind, logical_id, operation, revision_id)
                 VALUES (?1, 'observation', ?2, 'delete', NULL)",
                params![generation, object.logical_id],
            )?;
        }
        ObjectKind::Relation => enqueue_relation_deletes(tx, object, generation)?,
        ObjectKind::Entity => {
            tx.execute(
                "DELETE FROM index_outbox WHERE object_kind='observation'
                 AND logical_id IN
                    (SELECT id FROM observations WHERE entity_id=?1)",
                [&object.logical_id],
            )?;
            tx.execute(
                "INSERT INTO index_outbox(
                    generation, object_kind, logical_id, operation, revision_id)
                 SELECT ?1, 'observation', id, 'delete', NULL
                 FROM observations WHERE entity_id=?2",
                params![generation, object.logical_id],
            )?;
            enqueue_relation_deletes(tx, object, generation)?;
        }
    }
    Ok(())
}

fn enqueue_relation_deletes(
    tx: &rusqlite::Transaction<'_>,
    object: &ObjectRef,
    generation: u64,
) -> MemoryResult<()> {
    let predicate = if object.kind == ObjectKind::Entity {
        "from_entity=?1 OR to_entity=?1"
    } else {
        "id=?1"
    };
    tx.execute(
        &format!(
            "DELETE FROM mirror_outbox WHERE canonical_relation_id IN
                (SELECT COALESCE(canonical_relation_id, id)
                 FROM relations WHERE {predicate})"
        ),
        [&object.logical_id],
    )?;
    let insert_predicate = predicate.replace("?1", "?2");
    tx.execute(
        &format!(
            "INSERT INTO mirror_outbox(
                generation, canonical_relation_id, operation, target_domain, payload_json)
             SELECT ?1, COALESCE(canonical_relation_id, id), 'delete', 0, '{{}}'
             FROM relations WHERE {insert_predicate}
             ON CONFLICT(generation, canonical_relation_id, target_domain)
             DO UPDATE SET operation='delete', payload_json='{{}}',
                           attempts=0, last_error=NULL"
        ),
        params![generation, object.logical_id],
    )?;
    Ok(())
}

fn purge_semantic_payload(
    tx: &rusqlite::Transaction<'_>,
    object: &ObjectRef,
    scope: &str,
) -> MemoryResult<()> {
    validate_scope(object, scope)?;
    match object.kind {
        ObjectKind::Observation => purge_observation(tx, &object.logical_id),
        ObjectKind::Relation => purge_relation(tx, &object.logical_id),
        ObjectKind::Entity => purge_entity_cascade(tx, &object.logical_id),
    }
}

fn purge_observation(tx: &rusqlite::Transaction<'_>, id: &str) -> MemoryResult<()> {
    redact_change_requests(tx, "observation", id)?;
    tx.execute(
        "DELETE FROM origin_contributions
         WHERE object_kind='observation' AND target_logical_id=?1",
        [id],
    )?;
    tx.execute(
        "DELETE FROM observation_revision_concepts WHERE revision_id IN
            (SELECT id FROM observation_revisions WHERE observation_id=?1)",
        [id],
    )?;
    tx.execute(
        "DELETE FROM observation_revision_source_files WHERE revision_id IN
            (SELECT id FROM observation_revisions WHERE observation_id=?1)",
        [id],
    )?;
    tx.execute(
        "DELETE FROM observation_revisions WHERE observation_id=?1",
        [id],
    )?;
    tx.execute("DELETE FROM observations WHERE id=?1", [id])?;
    Ok(())
}

fn purge_relation(tx: &rusqlite::Transaction<'_>, id: &str) -> MemoryResult<()> {
    redact_change_requests(tx, "relation", id)?;
    tx.execute(
        "DELETE FROM origin_contributions
         WHERE object_kind='relation' AND target_logical_id=?1",
        [id],
    )?;
    tx.execute("DELETE FROM relation_revisions WHERE relation_id=?1", [id])?;
    tx.execute("DELETE FROM relations WHERE id=?1", [id])?;
    Ok(())
}

fn purge_entity_cascade(tx: &rusqlite::Transaction<'_>, id: &str) -> MemoryResult<()> {
    tx.execute(
        "DELETE FROM change_requests WHERE change_set_id IN (
            SELECT DISTINCT change_set_id FROM change_events
            WHERE (object_kind='entity' AND logical_id=?1)
               OR (object_kind='observation' AND logical_id IN
                   (SELECT id FROM observations WHERE entity_id=?1))
               OR (object_kind='relation' AND logical_id IN
                   (SELECT id FROM relations WHERE from_entity=?1 OR to_entity=?1)))",
        [id],
    )?;
    tx.execute(
        "DELETE FROM origin_contributions
         WHERE (object_kind='entity' AND target_logical_id=?1)
            OR (object_kind='observation' AND target_logical_id IN
                (SELECT id FROM observations WHERE entity_id=?1))
            OR (object_kind='relation' AND target_logical_id IN
                (SELECT id FROM relations WHERE from_entity=?1 OR to_entity=?1))",
        [id],
    )?;
    tx.execute(
        "DELETE FROM observation_revision_concepts WHERE revision_id IN
            (SELECT revision.id FROM observation_revisions AS revision
             JOIN observations AS observation ON observation.id=revision.observation_id
             WHERE observation.entity_id=?1)",
        [id],
    )?;
    tx.execute(
        "DELETE FROM observation_revision_source_files WHERE revision_id IN
            (SELECT revision.id FROM observation_revisions AS revision
             JOIN observations AS observation ON observation.id=revision.observation_id
             WHERE observation.entity_id=?1)",
        [id],
    )?;
    tx.execute(
        "DELETE FROM observation_revisions WHERE observation_id IN
            (SELECT id FROM observations WHERE entity_id=?1)",
        [id],
    )?;
    tx.execute(
        "DELETE FROM relation_revisions WHERE relation_id IN
            (SELECT id FROM relations WHERE from_entity=?1 OR to_entity=?1)",
        [id],
    )?;
    tx.execute(
        "DELETE FROM entity_revision_aliases WHERE revision_id IN
            (SELECT id FROM entity_revisions WHERE entity_id=?1)",
        [id],
    )?;
    tx.execute(
        "DELETE FROM entity_revision_identifiers WHERE revision_id IN
            (SELECT id FROM entity_revisions WHERE entity_id=?1)",
        [id],
    )?;
    tx.execute("DELETE FROM entity_revisions WHERE entity_id=?1", [id])?;
    tx.execute("DELETE FROM relations WHERE from_entity=?1 OR to_entity=?1", [id])?;
    tx.execute("DELETE FROM observations WHERE entity_id=?1", [id])?;
    tx.execute("DELETE FROM entities WHERE id=?1", [id])?;
    Ok(())
}

fn redact_change_requests(
    tx: &rusqlite::Transaction<'_>,
    kind: &str,
    id: &str,
) -> MemoryResult<()> {
    tx.execute(
        "DELETE FROM change_requests WHERE change_set_id IN (
            SELECT DISTINCT change_set_id FROM change_events
            WHERE object_kind=?1 AND logical_id=?2)",
        params![kind, id],
    )?;
    Ok(())
}

fn tx_object_head(
    tx: &rusqlite::Transaction<'_>,
    object: &ObjectRef,
) -> MemoryResult<Option<(Option<RevisionId>, u64, String)>> {
    let table = match object.kind {
        ObjectKind::Entity => "entities",
        ObjectKind::Observation => "observations",
        ObjectKind::Relation => "relations",
    };
    let row = tx
        .query_row(
            &format!(
                "SELECT current_revision_id, row_version, lifecycle
                 FROM {table} WHERE id=?1"
            ),
            [&object.logical_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(revision, version, lifecycle)| {
        Ok((
            revision
                .map(|value| {
                    value.parse().map_err(|error| {
                        MemoryError::Schema(format!("invalid current revision: {error}"))
                    })
                })
                .transpose()?,
            version,
            lifecycle,
        ))
    })
    .transpose()
}

fn contribution_count(
    tx: &rusqlite::Transaction<'_>,
    kind: &str,
    id: &str,
) -> MemoryResult<u64> {
    tx.query_row(
        "SELECT COUNT(*) FROM origin_contributions
         WHERE object_kind=?1 AND target_logical_id=?2",
        params![kind, id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn related_request_count(
    tx: &rusqlite::Transaction<'_>,
    kind: &str,
    id: &str,
) -> MemoryResult<u64> {
    tx.query_row(
        "SELECT COUNT(*) FROM change_requests WHERE change_set_id IN (
            SELECT DISTINCT change_set_id FROM change_events
            WHERE object_kind=?1 AND logical_id=?2)",
        params![kind, id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn count(tx: &rusqlite::Transaction<'_>, sql: &str, id: &str) -> MemoryResult<u64> {
    tx.query_row(sql, [id], |row| row.get(0))
        .map_err(Into::into)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use openmemory_core::clock::{Clock, FixedClock};
    use openmemory_core::config::Config;

    use super::*;
    use crate::{EntityType, ObservationInput};

    fn principal() -> PrincipalId {
        "local:maintainer".parse().unwrap()
    }

    fn store_with_observation() -> (MemoryStore, Arc<FixedClock>, ObjectRef) {
        let clock = Arc::new(FixedClock::new(100));
        let store = MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_clock(Arc::clone(&clock) as Arc<dyn Clock>);
        let remembered = store
            .remember(
                "private subject",
                EntityType::Fact,
                &[ObservationInput::new("private payload")],
                &[],
                "test",
            )
            .unwrap();
        store.backfill_history_batch(100).unwrap();
        let object = ObjectRef {
            kind: ObjectKind::Observation,
            logical_id: remembered.observation_ids[0].clone(),
        };
        (store, clock, object)
    }

    #[test]
    fn preview_binds_head_and_destroy_leaves_content_free_receipt() {
        let (store, _, object) = store_with_observation();
        let preview = store.preview_destroy(object.clone(), "object").unwrap();
        assert_eq!(preview.inventory.canonical_rows, 1);
        let receipt = store
            .destroy_previewed(
                object.clone(),
                "object",
                &preview.confirmation,
                &principal(),
                ActorKind::Human,
                "legal erasure request",
            )
            .unwrap();
        assert!(receipt.index_ready);
        assert!(store.object_head(&object).unwrap().is_none());
        let conn = store.lock_db();
        let receipts: u64 = conn
            .query_row("SELECT COUNT(*) FROM destruction_receipts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(receipts, 1);
        let leaked: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM change_requests
                 WHERE requested_payload_json LIKE '%private payload%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
    }

    #[test]
    fn destroy_rejects_agent_and_expired_confirmation() {
        let (store, clock, object) = store_with_observation();
        let preview = store.preview_destroy(object.clone(), "object").unwrap();
        assert!(matches!(
            store.destroy_previewed(
                object.clone(),
                "object",
                &preview.confirmation,
                &principal(),
                ActorKind::Agent,
                "not authorized",
            ),
            Err(MemoryError::Authorization(_))
        ));
        clock.set(1_000);
        assert!(matches!(
            store.destroy_previewed(
                object,
                "object",
                &preview.confirmation,
                &principal(),
                ActorKind::Human,
                "expired",
            ),
            Err(MemoryError::Authorization(_))
        ));
    }
}
