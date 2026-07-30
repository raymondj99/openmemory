//! Deterministic three-way field and lifecycle classification.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::model::Lifecycle;
use crate::{MergeError, MergeErrorCode, MergeResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreeWaySource {
    Base,
    Source,
    Target,
    IdenticalChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreeWayValue<T> {
    value: T,
    source: ThreeWaySource,
}

impl<T> ThreeWayValue<T> {
    #[must_use]
    pub fn value(&self) -> &T {
        &self.value
    }

    #[must_use]
    pub const fn source(&self) -> ThreeWaySource {
        self.source
    }
}

/// Classify one exact field according to the no-timestamp-winner table.
pub fn merge_value<T: Clone + Eq>(
    base: &T,
    source: &T,
    target: &T,
) -> MergeResult<ThreeWayValue<T>> {
    if source == base && target == base {
        return Ok(ThreeWayValue {
            value: base.clone(),
            source: ThreeWaySource::Base,
        });
    }
    if source != base && target == base {
        return Ok(ThreeWayValue {
            value: source.clone(),
            source: ThreeWaySource::Source,
        });
    }
    if source == base && target != base {
        return Ok(ThreeWayValue {
            value: target.clone(),
            source: ThreeWaySource::Target,
        });
    }
    if source == target {
        return Ok(ThreeWayValue {
            value: source.clone(),
            source: ThreeWaySource::IdenticalChange,
        });
    }
    Err(MergeError::new(
        MergeErrorCode::ThreeWayConflict,
        "source and target changed the same field differently",
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldMerge {
    values: BTreeMap<String, String>,
    sources: BTreeMap<String, ThreeWaySource>,
}

impl FieldMerge {
    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, String> {
        &self.values
    }

    #[must_use]
    pub fn sources(&self) -> &BTreeMap<String, ThreeWaySource> {
        &self.sources
    }
}

/// Merge a closed set of optional fields. Disjoint source/target edits combine;
/// a same-field divergent edit returns an explicit conflict.
pub fn merge_fields(
    base: &BTreeMap<String, String>,
    source: &BTreeMap<String, String>,
    target: &BTreeMap<String, String>,
) -> MergeResult<FieldMerge> {
    let keys = base
        .keys()
        .chain(source.keys())
        .chain(target.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut values = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for key in keys {
        let merged = merge_value(&base.get(&key), &source.get(&key), &target.get(&key))?;
        if let Some(value) = merged.value {
            values.insert(key.clone(), value.clone());
        }
        sources.insert(key, merged.source);
    }
    Ok(FieldMerge { values, sources })
}

/// Lifecycle uses the same exact three-way rule. In particular, a retire or
/// delete racing an edit is a conflict unless both sides selected it.
pub fn merge_lifecycle(
    base: Lifecycle,
    source: Lifecycle,
    target: Lifecycle,
) -> MergeResult<ThreeWayValue<Lifecycle>> {
    merge_value(&base, &source, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_three_way_table_is_enforced() {
        assert_eq!(
            merge_value(&"base", &"base", &"base").unwrap().source(),
            ThreeWaySource::Base
        );
        assert_eq!(
            merge_value(&"base", &"source", &"base").unwrap().source(),
            ThreeWaySource::Source
        );
        assert_eq!(
            merge_value(&"base", &"base", &"target").unwrap().source(),
            ThreeWaySource::Target
        );
        assert_eq!(
            merge_value(&"base", &"same", &"same").unwrap().source(),
            ThreeWaySource::IdenticalChange
        );
        assert_eq!(
            merge_value(&"base", &"source", &"target")
                .unwrap_err()
                .code(),
            MergeErrorCode::ThreeWayConflict
        );
    }

    #[test]
    fn disjoint_field_changes_combine_deterministically() {
        let base = BTreeMap::from([
            ("left".to_string(), "old".to_string()),
            ("right".to_string(), "old".to_string()),
        ]);
        let source = BTreeMap::from([
            ("left".to_string(), "new-source".to_string()),
            ("right".to_string(), "old".to_string()),
        ]);
        let target = BTreeMap::from([
            ("left".to_string(), "old".to_string()),
            ("right".to_string(), "new-target".to_string()),
        ]);
        let merged = merge_fields(&base, &source, &target).unwrap();
        assert_eq!(merged.values()["left"], "new-source");
        assert_eq!(merged.values()["right"], "new-target");
    }

    #[test]
    fn lifecycle_delete_vs_edit_is_an_explicit_conflict() {
        assert_eq!(
            merge_lifecycle(Lifecycle::Active, Lifecycle::Deleted, Lifecycle::Retired)
                .unwrap_err()
                .code(),
            MergeErrorCode::ThreeWayConflict
        );
    }
}
