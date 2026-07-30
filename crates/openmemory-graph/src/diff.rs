//! Typed, deterministic observation revision diffs.
//!
//! Diffs are derived from immutable revisions. Large values are represented
//! by a hash, byte length, and bounded preview so callers cannot accidentally
//! turn a list/diff surface into an unrestricted content export.

use openmemory_core::space::{ActorKind, SpaceId};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, MemoryResult};
use crate::store::MemoryStore;

const MAX_PREVIEW_BYTES: usize = 256;

/// Bounded representation of one side of a field change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffValueSummary {
    pub hash: String,
    pub byte_len: u64,
    pub preview: String,
    pub truncated: bool,
}

/// One deterministic field change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationFieldChange {
    pub field: String,
    pub before: DiffValueSummary,
    pub after: DiffValueSummary,
}

/// Safe provenance attached to the revision that produced the `to` side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffProvenance {
    pub changeset_id: String,
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub reason: String,
    pub source: String,
}

/// Complete typed diff between two revisions of one logical observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationRevisionDiff {
    pub space_id: SpaceId,
    pub observation_id: String,
    pub from_revision_id: String,
    pub to_revision_id: String,
    pub from_lifecycle: String,
    pub to_lifecycle: String,
    pub changes: Vec<ObservationFieldChange>,
    pub provenance: DiffProvenance,
    pub expected_head_revision_id: String,
    pub current_head_revision_id: Option<String>,
    pub stale: bool,
}

#[derive(Debug, PartialEq)]
struct RevisionFields {
    observation_id: String,
    entity_id: String,
    content: String,
    title: Option<String>,
    summary: Option<String>,
    concepts: Vec<String>,
    source_files: Vec<String>,
    observed_at: i64,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    confidence: f32,
    source: String,
    source_kind: Option<String>,
    importance: Option<f32>,
    memory_tier: String,
    lifecycle: String,
    changeset_id: String,
}

struct RevisionFieldsRaw {
    observation_id: String,
    entity_id: String,
    content: String,
    title: Option<String>,
    summary: Option<String>,
    concepts: String,
    source_files: String,
    observed_at: i64,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    confidence: f32,
    source: String,
    source_kind: Option<String>,
    importance: Option<f32>,
    memory_tier: String,
    lifecycle: String,
    changeset_id: String,
}

fn revision_fields(conn: &rusqlite::Connection, revision_id: &str) -> MemoryResult<RevisionFields> {
    let raw = conn.query_row(
        "SELECT observation_id, entity_id, content, title, summary,
                concepts_json, source_files_json, observed_at, valid_from,
                valid_until, confidence, source, source_kind, importance,
                memory_tier, lifecycle, changeset_id
         FROM observation_revisions WHERE revision_id = ?1",
        params![revision_id],
        |row| {
            Ok(RevisionFieldsRaw {
                observation_id: row.get(0)?,
                entity_id: row.get(1)?,
                content: row.get(2)?,
                title: row.get(3)?,
                summary: row.get(4)?,
                concepts: row.get(5)?,
                source_files: row.get(6)?,
                observed_at: row.get(7)?,
                valid_from: row.get(8)?,
                valid_until: row.get(9)?,
                confidence: row.get(10)?,
                source: row.get(11)?,
                source_kind: row.get(12)?,
                importance: row.get(13)?,
                memory_tier: row.get(14)?,
                lifecycle: row.get(15)?,
                changeset_id: row.get(16)?,
            })
        },
    )?;
    let mut concepts: Vec<String> = serde_json::from_str(&raw.concepts)?;
    let mut source_files: Vec<String> = serde_json::from_str(&raw.source_files)?;
    concepts.sort();
    concepts.dedup();
    source_files.sort();
    source_files.dedup();
    Ok(RevisionFields {
        observation_id: raw.observation_id,
        entity_id: raw.entity_id,
        content: raw.content,
        title: raw.title,
        summary: raw.summary,
        concepts,
        source_files,
        observed_at: raw.observed_at,
        valid_from: raw.valid_from,
        valid_until: raw.valid_until,
        confidence: raw.confidence,
        source: raw.source,
        source_kind: raw.source_kind,
        importance: raw.importance,
        memory_tier: raw.memory_tier,
        lifecycle: raw.lifecycle,
        changeset_id: raw.changeset_id,
    })
}

impl MemoryStore {
    /// Produce a complete typed diff between two immutable observation
    /// revisions. Revisions for different logical observations are rejected.
    pub fn observation_revision_diff(
        &self,
        from_revision: &str,
        to_revision: &str,
    ) -> MemoryResult<ObservationRevisionDiff> {
        self.with_reader(|conn| {
            let from = revision_fields(conn, from_revision)?;
            let to = revision_fields(conn, to_revision)?;
            if from.observation_id != to.observation_id {
                return Err(MemoryError::InvalidInput(
                    "revision diff requires one logical observation".to_owned(),
                ));
            }

            let mut changes = Vec::new();
            push_change(&mut changes, "entity_id", &from.entity_id, &to.entity_id)?;
            push_change(&mut changes, "content", &from.content, &to.content)?;
            push_change(&mut changes, "title", &from.title, &to.title)?;
            push_change(&mut changes, "summary", &from.summary, &to.summary)?;
            push_change(&mut changes, "concepts", &from.concepts, &to.concepts)?;
            push_change(
                &mut changes,
                "source_files",
                &from.source_files,
                &to.source_files,
            )?;
            push_change(
                &mut changes,
                "observed_at",
                &from.observed_at,
                &to.observed_at,
            )?;
            push_change(&mut changes, "valid_from", &from.valid_from, &to.valid_from)?;
            push_change(
                &mut changes,
                "valid_until",
                &from.valid_until,
                &to.valid_until,
            )?;
            if from.confidence.to_bits() != to.confidence.to_bits() {
                push_changed(&mut changes, "confidence", &from.confidence, &to.confidence)?;
            }
            push_change(&mut changes, "source", &from.source, &to.source)?;
            push_change(
                &mut changes,
                "source_kind",
                &from.source_kind,
                &to.source_kind,
            )?;
            if from.importance.map(f32::to_bits) != to.importance.map(f32::to_bits) {
                push_changed(&mut changes, "importance", &from.importance, &to.importance)?;
            }
            push_change(
                &mut changes,
                "memory_tier",
                &from.memory_tier,
                &to.memory_tier,
            )?;
            push_change(&mut changes, "lifecycle", &from.lifecycle, &to.lifecycle)?;

            let provenance = conn.query_row(
                "SELECT actor_kind, actor_id, reason, source
                 FROM change_sets WHERE id = ?1",
                params![to.changeset_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )?;
            let actor_kind = parse_actor_kind(&provenance.0)?;
            let space_id: String = conn.query_row(
                "SELECT space_id FROM change_sets WHERE id = ?1",
                params![to.changeset_id],
                |row| row.get(0),
            )?;
            let space_id =
                space_id
                    .parse()
                    .map_err(|error: openmemory_core::space::SpaceTypeError| {
                        MemoryError::Schema(error.to_string())
                    })?;
            let current_head_revision_id: Option<String> = conn
                .query_row(
                    "SELECT current_revision_id FROM observations WHERE id = ?1",
                    params![to.observation_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();

            Ok(ObservationRevisionDiff {
                space_id,
                observation_id: to.observation_id,
                from_revision_id: from_revision.to_owned(),
                to_revision_id: to_revision.to_owned(),
                from_lifecycle: from.lifecycle,
                to_lifecycle: to.lifecycle,
                changes,
                provenance: DiffProvenance {
                    changeset_id: to.changeset_id,
                    actor_kind,
                    actor_id: provenance.1,
                    reason: provenance.2,
                    source: provenance.3,
                },
                expected_head_revision_id: to_revision.to_owned(),
                stale: current_head_revision_id.as_deref() != Some(to_revision),
                current_head_revision_id,
            })
        })
    }

    /// Compatibility helper returning only deterministic changed field names.
    pub fn diff_observation_revisions(
        &self,
        from_revision: &str,
        to_revision: &str,
    ) -> MemoryResult<Vec<String>> {
        Ok(self
            .observation_revision_diff(from_revision, to_revision)?
            .changes
            .into_iter()
            .map(|change| change.field)
            .collect())
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

fn push_change<T>(
    changes: &mut Vec<ObservationFieldChange>,
    field: &str,
    before: &T,
    after: &T,
) -> MemoryResult<()>
where
    T: PartialEq + Serialize,
{
    if before != after {
        push_changed(changes, field, before, after)?;
    }
    Ok(())
}

fn push_changed<T>(
    changes: &mut Vec<ObservationFieldChange>,
    field: &str,
    before: &T,
    after: &T,
) -> MemoryResult<()>
where
    T: Serialize,
{
    changes.push(ObservationFieldChange {
        field: field.to_owned(),
        before: summarize(before)?,
        after: summarize(after)?,
    });
    Ok(())
}

fn summarize<T: Serialize>(value: &T) -> MemoryResult<DiffValueSummary> {
    let encoded = serde_json::to_string(value)?;
    let preview_end = encoded
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= MAX_PREVIEW_BYTES)
        .last()
        .unwrap_or(0);
    let truncated = encoded.len() > MAX_PREVIEW_BYTES;
    let preview = if truncated {
        encoded[..preview_end].to_owned()
    } else {
        encoded.clone()
    };
    Ok(DiffValueSummary {
        hash: format!("blake3:{}", blake3::hash(encoded.as_bytes()).to_hex()),
        byte_len: encoded.len() as u64,
        preview,
        truncated,
    })
}
