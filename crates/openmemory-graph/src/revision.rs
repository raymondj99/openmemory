//! Cursor-paginated immutable history and compact changeset audit queries.

use openmemory_core::space::{ChangeSetId, RevisionId, SpaceId};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::changeset::{ChangeOperation, ChangeSetState, ObjectKind, ObjectRef};
use crate::{MemoryError, MemoryResult, MemoryStore};

const MAX_PAGE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionSummary {
    pub revision_id: RevisionId,
    pub parent_revision_id: Option<RevisionId>,
    pub semantic_hash: Vec<u8>,
    pub created_by_change_set: ChangeSetId,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryPage {
    pub space_id: SpaceId,
    pub object: ObjectRef,
    pub revisions: Vec<RevisionSummary>,
    pub next_cursor: Option<(i64, RevisionId)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSetAuditRow {
    pub id: ChangeSetId,
    pub actor_principal: String,
    pub actor_kind: String,
    pub reason: String,
    pub source: String,
    pub state: ChangeSetState,
    pub base_generation: u64,
    pub committed_generation: Option<u64>,
    pub created_at: i64,
    pub decided_at: Option<i64>,
    pub decided_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSetDetail {
    pub summary: ChangeSetAuditRow,
    pub operations: Vec<ChangeOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainGeneration {
    pub semantic: u64,
    pub indexed: u64,
    pub mirrors: u64,
    pub index_outbox_rows: u64,
    pub mirror_outbox_rows: u64,
}

/// Runtime-derived semantic feature readiness for one graph domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticReadiness {
    pub history_ready: bool,
    pub index_ready: bool,
    pub mirrors_ready: bool,
    pub semantic_generation: u64,
    pub indexed_generation: u64,
    pub mirror_generation: u64,
}

impl MemoryStore {
    /// Read readiness from durable generation/backfill state, never the binary
    /// version. Callers must require all domains to be ready.
    pub fn semantic_readiness(&self) -> MemoryResult<SemanticReadiness> {
        self.with_reader(|conn| {
            let (
                semantic_generation,
                indexed_generation,
                mirror_generation,
                backfill_state,
                index_pending,
                mirror_pending,
            ): (u64, u64, u64, String, u64, u64) = conn.query_row(
                "SELECT semantic_generation, indexed_generation, mirror_generation,
                        backfill_state,
                        (SELECT COUNT(*) FROM index_outbox),
                        (SELECT COUNT(*) FROM mirror_outbox)
                 FROM domain_state WHERE singleton=1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?;
            Ok(SemanticReadiness {
                history_ready: matches!(backfill_state.as_str(), "history_ready" | "ready"),
                index_ready: index_pending == 0 && indexed_generation >= semantic_generation,
                mirrors_ready: mirror_pending == 0
                    && mirror_generation >= semantic_generation
                    && backfill_state == "ready",
                semantic_generation,
                indexed_generation,
                mirror_generation,
            })
        })
    }

    /// Immutable revision summaries ordered newest-first using a stable cursor.
    pub fn object_history(
        &self,
        object: ObjectRef,
        before: Option<(i64, RevisionId)>,
        limit: usize,
    ) -> MemoryResult<HistoryPage> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(MemoryError::InvalidInput(
                "history page size must be in 1..=256".to_string(),
            ));
        }
        if !self.contains_object(&object)? {
            return Err(match object.kind {
                ObjectKind::Entity => MemoryError::EntityNotFound(object.logical_id.clone()),
                ObjectKind::Observation => {
                    MemoryError::ObservationNotFound(object.logical_id.clone())
                }
                ObjectKind::Relation => {
                    MemoryError::InvalidInput(format!("relation not found: {}", object.logical_id))
                }
            });
        }
        let (table, id_column) = revision_table(object.kind);
        let fetch = limit.saturating_add(1);
        let rows = self.with_reader(|conn| {
            let sql = format!(
                "SELECT id, parent_revision_id, semantic_hash, created_by_change_set, created_at
                 FROM {table}
                 WHERE {id_column}=?1
                   AND (?2 IS NULL OR created_at<?2 OR (created_at=?2 AND id<?3))
                 ORDER BY created_at DESC, id DESC LIMIT ?4"
            );
            let (cursor_time, cursor_id) = before.map_or((None, None), |(time, id)| {
                (Some(time), Some(id.to_string()))
            });
            let mut statement = conn.prepare(&sql)?;
            let mapped = statement.query_map(
                params![
                    object.logical_id,
                    cursor_time,
                    cursor_id,
                    i64::try_from(fetch).unwrap_or(i64::MAX)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )?;
            mapped.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })?;
        let mut revisions = rows
            .into_iter()
            .map(
                |(id, parent, semantic_hash, changeset, created_at)| -> MemoryResult<_> {
                    Ok(RevisionSummary {
                        revision_id: parse_revision(&id)?,
                        parent_revision_id: parent.map(|id| parse_revision(&id)).transpose()?,
                        semantic_hash,
                        created_by_change_set: changeset.parse().map_err(|error| {
                            MemoryError::Schema(format!("invalid revision changeset ID: {error}"))
                        })?,
                        created_at,
                    })
                },
            )
            .collect::<MemoryResult<Vec<_>>>()?;
        let next_cursor = if revisions.len() > limit {
            revisions.truncate(limit);
            revisions
                .last()
                .map(|revision| (revision.created_at, revision.revision_id))
        } else {
            None
        };
        Ok(HistoryPage {
            space_id: self.space_id(),
            object,
            revisions,
            next_cursor,
        })
    }

    /// Compact audit metadata; semantic request content remains a separate
    /// authorized detail concern.
    pub fn list_changesets(
        &self,
        before: Option<(i64, ChangeSetId)>,
        limit: usize,
    ) -> MemoryResult<Vec<ChangeSetAuditRow>> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(MemoryError::InvalidInput(
                "changeset page size must be in 1..=256".to_string(),
            ));
        }
        self.with_reader(|conn| {
            let (cursor_time, cursor_id) = before.map_or((None, None), |(time, id)| {
                (Some(time), Some(id.to_string()))
            });
            let mut statement = conn.prepare(
                "SELECT id, actor_principal, actor_kind, reason, source, state,
                        base_generation, committed_generation, created_at,
                        decided_at, decided_by
                 FROM change_sets
                 WHERE space_id=?1
                   AND (?2 IS NULL OR created_at<?2 OR (created_at=?2 AND id<?3))
                 ORDER BY created_at DESC, id DESC LIMIT ?4",
            )?;
            let rows = statement
                .query_map(
                    params![
                        self.space_id().to_string(),
                        cursor_time,
                        cursor_id,
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, u64>(6)?,
                            row.get::<_, Option<u64>>(7)?,
                            row.get::<_, i64>(8)?,
                            row.get::<_, Option<i64>>(9)?,
                            row.get::<_, Option<String>>(10)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|row| -> MemoryResult<ChangeSetAuditRow> {
                    Ok(ChangeSetAuditRow {
                        id: row.0.parse().map_err(|error| {
                            MemoryError::Schema(format!("invalid changeset ID: {error}"))
                        })?,
                        actor_principal: row.1,
                        actor_kind: row.2,
                        reason: row.3,
                        source: row.4,
                        state: ChangeSetState::parse(&row.5)?,
                        base_generation: row.6,
                        committed_generation: row.7,
                        created_at: row.8,
                        decided_at: row.9,
                        decided_by: row.10,
                    })
                })
                .collect()
        })
    }

    /// Load one authorized changeset plus its bounded immutable request body.
    pub fn get_changeset(&self, id: ChangeSetId) -> MemoryResult<Option<ChangeSetDetail>> {
        self.with_reader(|conn| {
            let summary = conn
                .query_row(
                    "SELECT id, actor_principal, actor_kind, reason, source, state,
                            base_generation, committed_generation, created_at,
                            decided_at, decided_by
                     FROM change_sets WHERE id=?1 AND space_id=?2",
                    params![id.to_string(), self.space_id().to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, u64>(6)?,
                            row.get::<_, Option<u64>>(7)?,
                            row.get::<_, i64>(8)?,
                            row.get::<_, Option<i64>>(9)?,
                            row.get::<_, Option<String>>(10)?,
                        ))
                    },
                )
                .optional()?;
            let Some(row) = summary else {
                return Ok(None);
            };
            let summary = ChangeSetAuditRow {
                id: row.0.parse().map_err(|error| {
                    MemoryError::Schema(format!("invalid changeset ID: {error}"))
                })?,
                actor_principal: row.1,
                actor_kind: row.2,
                reason: row.3,
                source: row.4,
                state: ChangeSetState::parse(&row.5)?,
                base_generation: row.6,
                committed_generation: row.7,
                created_at: row.8,
                decided_at: row.9,
                decided_by: row.10,
            };
            let mut statement = conn.prepare(
                "SELECT requested_payload_json FROM change_requests
                 WHERE change_set_id=?1 ORDER BY ordinal",
            )?;
            let encoded = statement
                .query_map([id.to_string()], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if encoded.len() > 256 {
                return Err(MemoryError::Schema(
                    "changeset operation count exceeds hard bound".to_string(),
                ));
            }
            let operations = encoded
                .into_iter()
                .map(|value| serde_json::from_str(&value).map_err(Into::into))
                .collect::<MemoryResult<Vec<_>>>()?;
            Ok(Some(ChangeSetDetail {
                summary,
                operations,
            }))
        })
    }

    pub fn domain_generation(&self) -> MemoryResult<DomainGeneration> {
        self.with_reader(|conn| {
            let (semantic, indexed, mirrors) = conn.query_row(
                "SELECT semantic_generation, indexed_generation, mirror_generation
                 FROM domain_state WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let index_outbox_rows =
                conn.query_row("SELECT COUNT(*) FROM index_outbox", [], |row| row.get(0))?;
            let mirror_outbox_rows =
                conn.query_row("SELECT COUNT(*) FROM mirror_outbox", [], |row| row.get(0))?;
            Ok(DomainGeneration {
                semantic,
                indexed,
                mirrors,
                index_outbox_rows,
                mirror_outbox_rows,
            })
        })
    }

    /// Retry committed derived-index work before a store is declared ready.
    pub fn repair_index_outbox(&self) -> MemoryResult<DomainGeneration> {
        let semantic = self.domain_generation()?.semantic;
        let _barrier = self.write_rebuild();
        self.drain_index_outbox(semantic)?;
        self.domain_generation()
    }

    /// Current immutable revision ID for optimistic clients.
    pub fn object_head(
        &self,
        object: &ObjectRef,
    ) -> MemoryResult<Option<(Option<RevisionId>, u64, String)>> {
        let table = match object.kind {
            ObjectKind::Entity => "entities",
            ObjectKind::Observation => "observations",
            ObjectKind::Relation => "relations",
        };
        self.with_reader(|conn| {
            conn.query_row(
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
            .optional()?
            .map(|(revision, row_version, lifecycle)| {
                Ok((
                    revision.map(|id| parse_revision(&id)).transpose()?,
                    row_version,
                    lifecycle,
                ))
            })
            .transpose()
        })
    }
}

fn revision_table(kind: ObjectKind) -> (&'static str, &'static str) {
    match kind {
        ObjectKind::Entity => ("entity_revisions", "entity_id"),
        ObjectKind::Observation => ("observation_revisions", "observation_id"),
        ObjectKind::Relation => ("relation_revisions", "relation_id"),
    }
}

fn parse_revision(value: &str) -> MemoryResult<RevisionId> {
    value
        .parse()
        .map_err(|error| MemoryError::Schema(format!("invalid revision ID: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changeset::tests::{draft, initial_store};
    use crate::SubmitMode;

    #[test]
    fn history_and_audit_are_cursor_stable() {
        let (store, observation) = initial_store();
        let receipt = store
            .submit_changeset(
                &draft(&store, &observation, "history", "after"),
                SubmitMode::ApplyImmediately,
            )
            .unwrap();
        let object = ObjectRef {
            kind: ObjectKind::Observation,
            logical_id: observation,
        };
        let first = store.object_history(object.clone(), None, 1).unwrap();
        assert_eq!(first.revisions.len(), 1);
        assert!(first.next_cursor.is_some());
        let second = store.object_history(object, first.next_cursor, 1).unwrap();
        assert_eq!(second.revisions.len(), 1);
        assert_ne!(
            first.revisions[0].revision_id,
            second.revisions[0].revision_id
        );
        assert!(store
            .list_changesets(None, 10)
            .unwrap()
            .iter()
            .any(|row| row.id == receipt.id));
    }
}
