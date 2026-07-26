//! Consistent canonical snapshots across a space's performance domains.

use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::SpaceId;
use openmemory_graph::{MemoryError, MemoryResult};
use openmemory_merge::canonical::{CanonicalRelation, CanonicalSpaceSnapshot};
use serde::{Deserialize, Serialize};

use crate::partition::DomainStore;

/// Ordered readiness vector for one physical domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainVersion {
    pub domain: usize,
    pub semantic_generation: u64,
    pub indexed_generation: u64,
    pub mirror_generation: u64,
}

/// Whole-space immutable semantic input plus its operational freshness vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceSnapshot {
    pub space_id: SpaceId,
    pub domains: Vec<DomainVersion>,
    pub canonical: CanonicalSpaceSnapshot,
}

/// Drain derived index work, checkpoint every WAL, and export a deterministic
/// whole-space snapshot. Callers that own a [`crate::ContextEngine`] pause its
/// admissions before invoking this function.
pub fn capture_space_snapshot(store: &DomainStore) -> MemoryResult<SpaceSnapshot> {
    let mut domain_versions = Vec::with_capacity(store.domains());
    let mut entities = Vec::new();
    let mut observations = Vec::new();
    let mut relation_records = Vec::new();
    for (domain, graph) in store.stores().iter().enumerate() {
        let generation = graph.repair_index_outbox()?;
        if generation.index_outbox_rows != 0 {
            return Err(MemoryError::IndexRepairRequired(
                "canonical snapshot has pending index work".to_string(),
            ));
        }
        if generation.mirror_outbox_rows != 0 {
            return Err(MemoryError::InvalidInput(format!(
                "domain {domain} has pending relation mirror work"
            )));
        }
        let checkpoint = graph.wal_checkpoint()?;
        if !checkpoint.complete {
            return Err(MemoryError::InvalidInput(format!(
                "domain {domain} WAL checkpoint did not complete"
            )));
        }
        let records = graph.export_canonical_domain_records()?;
        if records.space_id != store.space_id() {
            return Err(MemoryError::Authorization(
                "snapshot domain has a mismatched space binding".to_string(),
            ));
        }
        domain_versions.push(DomainVersion {
            domain,
            semantic_generation: records.semantic_generation,
            indexed_generation: records.indexed_generation,
            mirror_generation: records.mirror_generation,
        });
        entities.extend(records.entities);
        observations.extend(records.observations);
        relation_records.extend(
            records
                .relations
                .into_iter()
                .map(|relation| (domain, relation)),
        );
    }

    for (version, graph) in domain_versions.iter().zip(store.stores()) {
        let current = graph.domain_generation()?;
        if current.semantic != version.semantic_generation
            || current.indexed != version.indexed_generation
            || current.mirrors != version.mirror_generation
        {
            return Err(MemoryError::ChangeSetStale(
                "space changed while its canonical snapshot was captured".to_string(),
            ));
        }
    }

    let entity_by_id = entities
        .iter()
        .map(|entity| (entity.logical_id.clone(), entity.logical_id.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut canonical_by_name = BTreeMap::<(String, String), Vec<String>>::new();
    for entity in &entities {
        canonical_by_name
            .entry((entity.label.to_lowercase(), entity.controlled_kind.clone()))
            .or_default()
            .push(entity.logical_id.clone());
    }

    let mut relations = Vec::with_capacity(relation_records.len());
    let mut relation_ids = BTreeSet::new();
    for (domain, record) in relation_records {
        if !relation_ids.insert(record.logical_id.clone()) {
            return Err(MemoryError::InvalidInput(
                "duplicate canonical relation ID across domains".to_string(),
            ));
        }
        let from = resolve_endpoint(
            store.space_id(),
            domain,
            &record.from,
            &entity_by_id,
            &canonical_by_name,
        )?;
        let to = resolve_endpoint(
            store.space_id(),
            domain,
            &record.to,
            &entity_by_id,
            &canonical_by_name,
        )?;
        let mut relation = CanonicalRelation::new_complete(
            store.space_id(),
            record.logical_id,
            record.revision_id,
            from,
            to,
            record.relation_type,
            record.weight,
            record.valid_from,
            record.valid_until,
            record.source,
            record.evidence,
            record.lifecycle,
        )
        .map_err(merge_error)?;
        relation.origins.extend(record.origins);
        relation.validate().map_err(merge_error)?;
        relations.push(relation);
    }

    let semantic_generation = domain_versions.iter().fold(0_u64, |accumulator, version| {
        accumulator
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(version.semantic_generation)
            .wrapping_add(version.domain as u64)
    });
    let canonical = CanonicalSpaceSnapshot::new(
        store.space_id(),
        semantic_generation,
        entities,
        observations,
        relations,
    )
    .map_err(merge_error)?;
    Ok(SpaceSnapshot {
        space_id: store.space_id(),
        domains: domain_versions,
        canonical,
    })
}

fn resolve_endpoint(
    space_id: SpaceId,
    domain: usize,
    endpoint: &openmemory_graph::CanonicalEndpoint,
    by_id: &BTreeMap<String, String>,
    by_name: &BTreeMap<(String, String), Vec<String>>,
) -> MemoryResult<String> {
    if !endpoint.is_stub {
        return by_id.get(&endpoint.id).cloned().ok_or_else(|| {
            MemoryError::InvalidInput("canonical relation endpoint is missing".to_string())
        });
    }
    if let Some(entity) = by_id.values().find(|logical_id| {
        crate::merge::derived_stub_id(space_id, logical_id, domain) == endpoint.id
    }) {
        return Ok(entity.clone());
    }
    let candidates = by_name
        .get(&(endpoint.name.to_lowercase(), endpoint.entity_type.clone()))
        .map(Vec::as_slice)
        .unwrap_or_default();
    if let [only] = candidates {
        Ok(only.clone())
    } else {
        Err(MemoryError::InvalidInput(
            "partition stub has no unique canonical endpoint".to_string(),
        ))
    }
}

fn merge_error(error: openmemory_merge::MergeError) -> MemoryError {
    MemoryError::InvalidInput(format!("canonical snapshot failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_core::config::Config;
    use openmemory_graph::{EntityType, ObservationInput, RelationInput};

    #[test]
    fn snapshot_rewrites_cross_domain_stubs_and_excludes_mirrors() {
        let root = tempfile::tempdir().unwrap();
        let id = SpaceId::new();
        let store = DomainStore::open_scoped(&Config::default(), root.path(), 2, id).unwrap();
        let mut names = ("alpha".to_string(), "bravo".to_string());
        while store.domain_for(&names.0) == store.domain_for(&names.1) {
            names.1.push('x');
        }
        store
            .remember(
                &names.0,
                EntityType::Concept,
                &[ObservationInput::new("source")],
                &[RelationInput::new(
                    "references",
                    names.1.clone(),
                    EntityType::Concept,
                )],
                "test",
            )
            .unwrap();
        store.backfill_history_and_mirrors(100).unwrap();
        let snapshot = capture_space_snapshot(&store).unwrap();
        assert_eq!(snapshot.canonical.entities.len(), 2);
        assert_eq!(snapshot.canonical.relations.len(), 1);
        let relation = &snapshot.canonical.relations[0];
        assert!(snapshot.canonical.entity(&relation.from_entity).is_some());
        assert!(snapshot.canonical.entity(&relation.to_entity).is_some());
    }
}
