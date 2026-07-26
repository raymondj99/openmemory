//! Deterministic typed diffs computed from immutable revisions.

use std::collections::BTreeSet;

use openmemory_core::space::{RevisionId, SpaceId};
use serde::{Deserialize, Serialize};

use crate::changeset::{
    load_entity_revision, load_observation_revision, load_relation_revision, EntityValue,
    Lifecycle, ObjectKind, ObjectRef, ObservationValue, OriginRef, RelationValue,
};
use crate::{MemoryError, MemoryResult, MemoryStore};

const INLINE_PREVIEW_CHARS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueChange<T> {
    pub before: T,
    pub after: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextValue {
    pub preview: String,
    pub content_hash: Vec<u8>,
    pub byte_len: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ScalarValue {
    Text(String),
    Integer(i64),
    FloatBits(u32),
    OptionalText(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FieldChange {
    Text {
        field: String,
        before: Option<TextValue>,
        after: Option<TextValue>,
    },
    Scalar {
        field: String,
        before: ScalarValue,
        after: ScalarValue,
    },
    Set {
        field: String,
        added: Vec<String>,
        removed: Vec<String>,
    },
    Relation {
        before: Option<RelationValue>,
        after: Option<RelationValue>,
    },
    Contribution {
        added: Vec<OriginRef>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryDiff {
    pub space_id: SpaceId,
    pub object: ObjectRef,
    pub from_revision: Option<RevisionId>,
    pub to_revision: Option<RevisionId>,
    pub lifecycle: Option<ValueChange<Lifecycle>>,
    pub fields: Vec<FieldChange>,
    pub provenance: Vec<OriginRef>,
    pub expected_current_revision: Option<RevisionId>,
    pub current_revision: Option<RevisionId>,
    pub stale: bool,
}

enum Snapshot {
    Entity(EntityValue),
    Observation(ObservationValue),
    Relation(RelationValue),
}

impl MemoryStore {
    /// Compare two immutable revisions. `None` represents object absence,
    /// useful for create/remove previews; it never silently means "current".
    pub fn diff_revisions(
        &self,
        object: ObjectRef,
        from: Option<RevisionId>,
        to: Option<RevisionId>,
        expected_current_revision: Option<RevisionId>,
    ) -> MemoryResult<MemoryDiff> {
        let current_revision = self
            .object_head(&object)?
            .and_then(|(revision, _, _)| revision);
        let before = from
            .map(|revision| self.load_snapshot(&object, revision))
            .transpose()?;
        let after = to
            .map(|revision| self.load_snapshot(&object, revision))
            .transpose()?;
        let fields = diff_snapshots(before.as_ref(), after.as_ref())?;
        let provenance = self.object_provenance(&object)?;
        let stale =
            expected_current_revision.is_some() && expected_current_revision != current_revision;
        Ok(MemoryDiff {
            space_id: self.space_id(),
            object,
            from_revision: from,
            to_revision: to,
            lifecycle: None,
            fields,
            provenance,
            expected_current_revision,
            current_revision,
            stale,
        })
    }

    fn load_snapshot(&self, object: &ObjectRef, revision: RevisionId) -> MemoryResult<Snapshot> {
        let conn = self.lock_db();
        match object.kind {
            ObjectKind::Entity => {
                load_entity_revision(&conn, &object.logical_id, revision).map(Snapshot::Entity)
            }
            ObjectKind::Observation => {
                load_observation_revision(&conn, &object.logical_id, revision)
                    .map(Snapshot::Observation)
            }
            ObjectKind::Relation => {
                load_relation_revision(&conn, &object.logical_id, revision).map(Snapshot::Relation)
            }
        }
    }

    fn object_provenance(&self, object: &ObjectRef) -> MemoryResult<Vec<OriginRef>> {
        self.with_reader(|conn| {
            let mut statement = conn.prepare(
                "SELECT origin_space_id, origin_logical_id, origin_revision_id
                 FROM origin_contributions
                 WHERE object_kind=?1 AND target_logical_id=?2
                 ORDER BY origin_space_id, origin_logical_id, origin_revision_id",
            )?;
            let rows = statement
                .query_map([object.kind.as_str(), object.logical_id.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|(space, logical_id, revision)| {
                    Ok(OriginRef {
                        space_id: space.parse().map_err(|error| {
                            MemoryError::Schema(format!("invalid provenance space ID: {error}"))
                        })?,
                        logical_id,
                        revision_id: revision.parse().map_err(|error| {
                            MemoryError::Schema(format!("invalid provenance revision ID: {error}"))
                        })?,
                    })
                })
                .collect()
        })
    }
}

fn diff_snapshots(
    before: Option<&Snapshot>,
    after: Option<&Snapshot>,
) -> MemoryResult<Vec<FieldChange>> {
    match (before, after) {
        (Some(Snapshot::Entity(before)), Some(Snapshot::Entity(after))) => {
            Ok(diff_entity(before, after))
        }
        (Some(Snapshot::Observation(before)), Some(Snapshot::Observation(after))) => {
            Ok(diff_observation(before, after))
        }
        (Some(Snapshot::Relation(before)), Some(Snapshot::Relation(after))) => Ok((before
            != after)
            .then(|| FieldChange::Relation {
                before: Some(before.clone()),
                after: Some(after.clone()),
            })
            .into_iter()
            .collect()),
        (None, Some(Snapshot::Entity(after))) => Ok(entity_from_absence(after, true)),
        (Some(Snapshot::Entity(before)), None) => Ok(entity_from_absence(before, false)),
        (None, Some(Snapshot::Observation(after))) => Ok(observation_from_absence(after, true)),
        (Some(Snapshot::Observation(before)), None) => Ok(observation_from_absence(before, false)),
        (None, Some(Snapshot::Relation(after))) => Ok(vec![FieldChange::Relation {
            before: None,
            after: Some(after.clone()),
        }]),
        (Some(Snapshot::Relation(before)), None) => Ok(vec![FieldChange::Relation {
            before: Some(before.clone()),
            after: None,
        }]),
        (None, None) => Ok(Vec::new()),
        _ => Err(MemoryError::InvalidInput(
            "cannot diff revisions from different object kinds".to_string(),
        )),
    }
}

fn diff_entity(before: &EntityValue, after: &EntityValue) -> Vec<FieldChange> {
    let mut fields = Vec::new();
    text_change(&mut fields, "name", Some(&before.name), Some(&after.name));
    scalar_change(
        &mut fields,
        "entity_type",
        ScalarValue::Text(before.entity_type.to_string()),
        ScalarValue::Text(after.entity_type.to_string()),
    );
    scalar_change(
        &mut fields,
        "confidence",
        ScalarValue::FloatBits(before.confidence.to_bits()),
        ScalarValue::FloatBits(after.confidence.to_bits()),
    );
    text_change(
        &mut fields,
        "source",
        Some(&before.source),
        Some(&after.source),
    );
    set_change(&mut fields, "aliases", &before.aliases, &after.aliases);
    let before_identifiers = before
        .identifiers
        .iter()
        .map(stable_json)
        .collect::<Vec<_>>();
    let after_identifiers = after
        .identifiers
        .iter()
        .map(stable_json)
        .collect::<Vec<_>>();
    set_change(
        &mut fields,
        "identifiers",
        &before_identifiers,
        &after_identifiers,
    );
    fields
}

fn diff_observation(before: &ObservationValue, after: &ObservationValue) -> Vec<FieldChange> {
    let mut fields = Vec::new();
    text_change(
        &mut fields,
        "content",
        Some(&before.content),
        Some(&after.content),
    );
    text_change(
        &mut fields,
        "title",
        before.title.as_deref(),
        after.title.as_deref(),
    );
    text_change(
        &mut fields,
        "summary",
        before.summary.as_deref(),
        after.summary.as_deref(),
    );
    text_change(
        &mut fields,
        "source",
        Some(&before.source),
        Some(&after.source),
    );
    scalar_change(
        &mut fields,
        "observed_at",
        ScalarValue::Integer(before.observed_at),
        ScalarValue::Integer(after.observed_at),
    );
    scalar_change(
        &mut fields,
        "confidence",
        ScalarValue::FloatBits(before.confidence.to_bits()),
        ScalarValue::FloatBits(after.confidence.to_bits()),
    );
    scalar_change(
        &mut fields,
        "memory_tier",
        ScalarValue::Text(before.memory_tier.to_string()),
        ScalarValue::Text(after.memory_tier.to_string()),
    );
    scalar_change(
        &mut fields,
        "source_kind",
        ScalarValue::OptionalText(before.source_kind.clone()),
        ScalarValue::OptionalText(after.source_kind.clone()),
    );
    set_change(&mut fields, "concepts", &before.concepts, &after.concepts);
    set_change(
        &mut fields,
        "source_files",
        &before.source_files,
        &after.source_files,
    );
    fields
}

fn entity_from_absence(value: &EntityValue, addition: bool) -> Vec<FieldChange> {
    let (before, after) = if addition {
        (None, Some(value.name.as_str()))
    } else {
        (Some(value.name.as_str()), None)
    };
    let mut fields = Vec::new();
    text_change(&mut fields, "name", before, after);
    fields
}

fn observation_from_absence(value: &ObservationValue, addition: bool) -> Vec<FieldChange> {
    let (before, after) = if addition {
        (None, Some(value.content.as_str()))
    } else {
        (Some(value.content.as_str()), None)
    };
    let mut fields = Vec::new();
    text_change(&mut fields, "content", before, after);
    fields
}

fn text_change(
    fields: &mut Vec<FieldChange>,
    field: &str,
    before: Option<&str>,
    after: Option<&str>,
) {
    if before != after {
        fields.push(FieldChange::Text {
            field: field.to_string(),
            before: before.map(text_value),
            after: after.map(text_value),
        });
    }
}

fn scalar_change(
    fields: &mut Vec<FieldChange>,
    field: &str,
    before: ScalarValue,
    after: ScalarValue,
) {
    if before != after {
        fields.push(FieldChange::Scalar {
            field: field.to_string(),
            before,
            after,
        });
    }
}

fn set_change(fields: &mut Vec<FieldChange>, field: &str, before: &[String], after: &[String]) {
    let before: BTreeSet<_> = before.iter().cloned().collect();
    let after: BTreeSet<_> = after.iter().cloned().collect();
    let added = after.difference(&before).cloned().collect::<Vec<_>>();
    let removed = before.difference(&after).cloned().collect::<Vec<_>>();
    if !added.is_empty() || !removed.is_empty() {
        fields.push(FieldChange::Set {
            field: field.to_string(),
            added,
            removed,
        });
    }
}

fn text_value(value: &str) -> TextValue {
    let preview: String = value.chars().take(INLINE_PREVIEW_CHARS).collect();
    TextValue {
        preview,
        content_hash: blake3::hash(value.as_bytes()).as_bytes().to_vec(),
        byte_len: value.len(),
        truncated: value.chars().count() > INLINE_PREVIEW_CHARS,
    }
}

fn stable_json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"invalid\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changeset::tests::{draft, initial_store};
    use crate::SubmitMode;

    #[test]
    fn diff_is_typed_bounded_and_stale_aware() {
        let (store, observation) = initial_store();
        store
            .submit_changeset(
                &draft(&store, &observation, "diff", &"x".repeat(2_000)),
                SubmitMode::ApplyImmediately,
            )
            .unwrap();
        let object = ObjectRef {
            kind: ObjectKind::Observation,
            logical_id: observation,
        };
        let history = store.object_history(object.clone(), None, 2).unwrap();
        let diff = store
            .diff_revisions(
                object,
                Some(history.revisions[1].revision_id),
                Some(history.revisions[0].revision_id),
                Some(history.revisions[1].revision_id),
            )
            .unwrap();
        assert!(diff.stale);
        assert!(diff.fields.iter().any(|field| {
            matches!(
                field,
                FieldChange::Text {
                    field,
                    after: Some(TextValue { truncated: true, .. }),
                    ..
                } if field == "content"
            )
        }));
    }
}
