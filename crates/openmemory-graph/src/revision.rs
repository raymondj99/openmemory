//! Immutable observation history queries with keyset pagination.

use openmemory_core::space::{ActorKind, SpaceId};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, MemoryResult};
use crate::store::MemoryStore;

pub use crate::audit::{BackfillReport, ChangeSetReceipt};

/// Stable keyset cursor for observation history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub created_at_unix_secs: i64,
    pub revision_id: String,
}

/// Safe immutable revision metadata. Semantic content remains behind a
/// separately authorized detail read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationHistoryEntry {
    pub space_id: SpaceId,
    pub observation_id: String,
    pub revision_id: String,
    pub parent_revision_id: Option<String>,
    pub changeset_id: String,
    pub lifecycle: String,
    pub semantic_hash: String,
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub reason: String,
    pub source: String,
    pub created_at_unix_secs: i64,
}

/// One bounded history page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationHistoryPage {
    pub entries: Vec<ObservationHistoryEntry>,
    pub next_cursor: Option<HistoryCursor>,
}

impl MemoryStore {
    /// Read one deterministic page of immutable revision history, constrained
    /// to the store's bound space in SQL before rows are materialized.
    pub fn observation_history(
        &self,
        observation_id: &str,
        after: Option<&HistoryCursor>,
        limit: usize,
    ) -> MemoryResult<ObservationHistoryPage> {
        if observation_id.trim().is_empty() {
            return Err(MemoryError::InvalidInput(
                "observation id must not be empty".to_owned(),
            ));
        }
        let limit = limit.clamp(1, 256);
        self.with_reader(|conn| {
            let space_id: String = conn.query_row(
                "SELECT value FROM memory_meta WHERE key = 'space_id'",
                [],
                |row| row.get(0),
            )?;
            let parsed_space: SpaceId =
                space_id
                    .parse()
                    .map_err(|error: openmemory_core::space::SpaceTypeError| {
                        MemoryError::Schema(error.to_string())
                    })?;
            let (after_time, after_id) = after.map_or((i64::MIN, ""), |cursor| {
                (cursor.created_at_unix_secs, cursor.revision_id.as_str())
            });
            let mut stmt = conn.prepare(
                "SELECT revisions.revision_id, revisions.parent_revision_id,
                        revisions.changeset_id, revisions.lifecycle,
                        revisions.semantic_hash, revisions.created_at,
                        changes.actor_kind, changes.actor_id, changes.reason,
                        changes.source
                 FROM observation_revisions AS revisions
                 JOIN change_sets AS changes
                   ON changes.id = revisions.changeset_id
                  AND changes.space_id = ?1
                 WHERE revisions.observation_id = ?2
                   AND (
                       revisions.created_at > ?3
                       OR (revisions.created_at = ?3 AND revisions.revision_id > ?4)
                   )
                 ORDER BY revisions.created_at, revisions.revision_id
                 LIMIT ?5",
            )?;
            let raw = stmt
                .query_map(
                    params![
                        space_id,
                        observation_id,
                        after_time,
                        after_id,
                        i64::try_from(limit + 1).unwrap_or(257),
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, String>(7)?,
                            row.get::<_, String>(8)?,
                            row.get::<_, String>(9)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let has_more = raw.len() > limit;
            let mut entries = raw
                .into_iter()
                .take(limit)
                .map(
                    |(
                        revision_id,
                        parent_revision_id,
                        changeset_id,
                        lifecycle,
                        semantic_hash,
                        created_at_unix_secs,
                        actor_kind,
                        actor_id,
                        reason,
                        source,
                    )| {
                        Ok(ObservationHistoryEntry {
                            space_id: parsed_space,
                            observation_id: observation_id.to_owned(),
                            revision_id,
                            parent_revision_id,
                            changeset_id,
                            lifecycle,
                            semantic_hash,
                            actor_kind: parse_actor_kind(&actor_kind)?,
                            actor_id,
                            reason,
                            source,
                            created_at_unix_secs,
                        })
                    },
                )
                .collect::<MemoryResult<Vec<_>>>()?;
            let next_cursor = if has_more {
                entries.last().map(|entry| HistoryCursor {
                    created_at_unix_secs: entry.created_at_unix_secs,
                    revision_id: entry.revision_id.clone(),
                })
            } else {
                None
            };
            entries.shrink_to_fit();
            Ok(ObservationHistoryPage {
                entries,
                next_cursor,
            })
        })
    }
}

fn parse_actor_kind(value: &str) -> MemoryResult<ActorKind> {
    serde_json::from_str(value)
        .or_else(|_| match value {
            "human" => Ok(ActorKind::Human),
            "agent" => Ok(ActorKind::Agent),
            "system" => Ok(ActorKind::System),
            _ => Err(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unknown actor kind",
            ))),
        })
        .map_err(Into::into)
}
