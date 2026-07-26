//! Pure field-level three-way classification without timestamp winners.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Result class for one logical object's three-way comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreeWayClassification {
    Unchanged,
    SourceOnly,
    TargetOnly,
    SameChange,
    DisjointChange,
    Conflict,
}

/// One conflicting field bound to base, source, and target values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldConflict {
    pub field: String,
    pub base: Option<String>,
    pub source: Option<String>,
    pub target: Option<String>,
}

/// Complete deterministic three-way result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreeWayResult {
    pub classification: ThreeWayClassification,
    pub merged: BTreeMap<String, Option<String>>,
    pub conflicts: Vec<FieldConflict>,
}

/// Classify and merge three field maps.
///
/// Missing fields are explicit deletion. Disjoint source/target edits commute;
/// different edits to the same field and delete-vs-edit are conflicts.
#[must_use]
pub fn classify_fields(
    base: &BTreeMap<String, Option<String>>,
    source: &BTreeMap<String, Option<String>>,
    target: &BTreeMap<String, Option<String>>,
) -> ThreeWayResult {
    let keys = base
        .keys()
        .chain(source.keys())
        .chain(target.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut merged = BTreeMap::new();
    let mut conflicts = Vec::new();
    let mut source_changed = false;
    let mut target_changed = false;
    let mut same_change = false;

    for key in keys {
        let base_value = base.get(&key).cloned().flatten();
        let source_value = source.get(&key).cloned().flatten();
        let target_value = target.get(&key).cloned().flatten();
        let source_differs = source_value != base_value;
        let target_differs = target_value != base_value;
        source_changed |= source_differs;
        target_changed |= target_differs;

        let value = match (source_differs, target_differs) {
            (false, false | true) => target_value,
            (true, false) => source_value,
            (true, true) if source_value == target_value => {
                same_change = true;
                source_value
            }
            (true, true) => {
                conflicts.push(FieldConflict {
                    field: key.clone(),
                    base: base_value,
                    source: source_value,
                    target: target_value.clone(),
                });
                target_value
            }
        };
        merged.insert(key, value);
    }

    let classification = if !conflicts.is_empty() {
        ThreeWayClassification::Conflict
    } else {
        match (source_changed, target_changed, same_change) {
            (false, false, _) => ThreeWayClassification::Unchanged,
            (true, false, _) => ThreeWayClassification::SourceOnly,
            (false, true, _) => ThreeWayClassification::TargetOnly,
            (true, true, true) if source == target => ThreeWayClassification::SameChange,
            (true, true, _) => ThreeWayClassification::DisjointChange,
        }
    };
    ThreeWayResult {
        classification,
        merged,
        conflicts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(values: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.map(str::to_string)))
            .collect()
    }

    #[test]
    fn classifies_all_core_cases() {
        let base = fields(&[("a", Some("1")), ("b", Some("1"))]);
        assert_eq!(
            classify_fields(&base, &base, &base).classification,
            ThreeWayClassification::Unchanged
        );
        assert_eq!(
            classify_fields(&base, &fields(&[("a", Some("2")), ("b", Some("1"))]), &base)
                .classification,
            ThreeWayClassification::SourceOnly
        );
        assert_eq!(
            classify_fields(&base, &base, &fields(&[("a", Some("2")), ("b", Some("1"))]))
                .classification,
            ThreeWayClassification::TargetOnly
        );
        let same = fields(&[("a", Some("2")), ("b", Some("1"))]);
        assert_eq!(
            classify_fields(&base, &same, &same).classification,
            ThreeWayClassification::SameChange
        );
        assert_eq!(
            classify_fields(
                &base,
                &fields(&[("a", Some("2")), ("b", Some("1"))]),
                &fields(&[("a", Some("1")), ("b", Some("2"))])
            )
            .classification,
            ThreeWayClassification::DisjointChange
        );
        assert_eq!(
            classify_fields(
                &base,
                &fields(&[("a", Some("2")), ("b", Some("1"))]),
                &fields(&[("a", Some("3")), ("b", Some("1"))])
            )
            .classification,
            ThreeWayClassification::Conflict
        );
    }
}
