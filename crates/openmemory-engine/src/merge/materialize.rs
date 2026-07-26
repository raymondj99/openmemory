//! Fresh staged `DomainStore` construction from a pure merge result.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use openmemory_core::config::Config;
use openmemory_graph::{
    CanonicalDomainImport, MaterializedRelation, MaterializedStub, MemoryError, MemoryResult,
};
use openmemory_merge::canonical::{CanonicalEntity, CanonicalSpaceSnapshot};
use serde::{Deserialize, Serialize};

use crate::partition::DomainStore;
use crate::space::{capture_space_snapshot, SpaceSnapshot};

/// Verification facts retained by the durable merge job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationReport {
    pub staging_root: PathBuf,
    pub entities: usize,
    pub observations: usize,
    pub relations: usize,
    pub domains: usize,
    pub predicted_hash: String,
    pub verified_hash: String,
}

/// Build a fresh staged root, derive partition stubs/mirrors, rebuild indexes,
/// and verify the canonical result hash before returning.
pub fn materialize_snapshot(
    config: &Config,
    staging_root: &Path,
    domains: usize,
    result: &CanonicalSpaceSnapshot,
) -> MemoryResult<(SpaceSnapshot, MaterializationReport)> {
    result.validate().map_err(merge_error)?;
    if domains == 0 || domains > 1_024 {
        return Err(MemoryError::InvalidInput(
            "materialization domain count must be in 1..=1024".to_string(),
        ));
    }
    if staging_root.exists() {
        if staging_root.read_dir()?.next().is_some() {
            return Err(MemoryError::InvalidInput(
                "merge staging root is not empty".to_string(),
            ));
        }
    } else {
        std::fs::create_dir_all(staging_root)?;
    }
    let store = DomainStore::open_scoped(config, staging_root, domains, result.space_id)?;
    let entity_map = result
        .entities
        .iter()
        .map(|entity| (entity.logical_id.as_str(), entity))
        .collect::<BTreeMap<_, _>>();
    let entity_domains = result
        .entities
        .iter()
        .map(|entity| (entity.logical_id.clone(), store.domain_for(&entity.label)))
        .collect::<BTreeMap<_, _>>();
    let mut imports = (0..domains)
        .map(|_| CanonicalDomainImport {
            semantic_generation: result.semantic_generation,
            entities: Vec::new(),
            stubs: Vec::new(),
            observations: Vec::new(),
            relations: Vec::new(),
        })
        .collect::<Vec<_>>();

    for entity in &result.entities {
        imports[entity_domains[&entity.logical_id]]
            .entities
            .push(entity.clone());
    }
    for observation in &result.observations {
        let domain = *entity_domains.get(&observation.entity_id).ok_or_else(|| {
            MemoryError::InvalidInput(
                "materialized observation has no canonical entity".to_string(),
            )
        })?;
        imports[domain].observations.push(observation.clone());
    }

    let mut stubs = BTreeMap::<(usize, String), MaterializedStub>::new();
    for relation in &result.relations {
        let from_domain = *entity_domains.get(&relation.from_entity).ok_or_else(|| {
            MemoryError::InvalidInput("relation source endpoint is missing".to_string())
        })?;
        let to_domain = *entity_domains.get(&relation.to_entity).ok_or_else(|| {
            MemoryError::InvalidInput("relation target endpoint is missing".to_string())
        })?;
        let from = entity_map[relation.from_entity.as_str()];
        let to = entity_map[relation.to_entity.as_str()];
        if from_domain == to_domain {
            imports[from_domain].relations.push(MaterializedRelation {
                row_id: relation.logical_id.clone(),
                canonical_relation_id: relation.logical_id.clone(),
                mirror_role: "canonical".to_string(),
                from_entity: relation.from_entity.clone(),
                to_entity: relation.to_entity.clone(),
                semantic: relation.clone(),
            });
            continue;
        }

        let target_stub = materialized_stub(result.space_id, to, from_domain);
        let source_stub = materialized_stub(result.space_id, from, to_domain);
        stubs.insert((from_domain, target_stub.id.clone()), target_stub.clone());
        stubs.insert((to_domain, source_stub.id.clone()), source_stub.clone());
        imports[from_domain].relations.push(MaterializedRelation {
            row_id: relation.logical_id.clone(),
            canonical_relation_id: relation.logical_id.clone(),
            mirror_role: "canonical".to_string(),
            from_entity: relation.from_entity.clone(),
            to_entity: target_stub.id,
            semantic: relation.clone(),
        });
        imports[to_domain].relations.push(MaterializedRelation {
            row_id: derived_row_id(
                b"openmemory/relation-mirror/v1",
                result.space_id,
                &relation.logical_id,
                to_domain,
            ),
            canonical_relation_id: relation.logical_id.clone(),
            mirror_role: format!("mirror:{to_domain}"),
            from_entity: source_stub.id,
            to_entity: relation.to_entity.clone(),
            semantic: relation.clone(),
        });
    }
    for ((domain, _), stub) in stubs {
        imports[domain].stubs.push(stub);
    }
    for import in &mut imports {
        import
            .entities
            .sort_by(|left, right| left.logical_id.cmp(&right.logical_id));
        import.stubs.sort_by(|left, right| left.id.cmp(&right.id));
        import
            .observations
            .sort_by(|left, right| left.logical_id.cmp(&right.logical_id));
        import
            .relations
            .sort_by(|left, right| left.row_id.cmp(&right.row_id));
    }

    for (graph, import) in store.stores().iter().zip(&imports) {
        graph.import_canonical_domain(import)?;
        let checkpoint = graph.wal_checkpoint()?;
        if !checkpoint.complete {
            return Err(MemoryError::InvalidInput(
                "staged graph WAL checkpoint did not complete".to_string(),
            ));
        }
    }
    let verified = capture_space_snapshot(&store)?;
    if verified.canonical.snapshot_hash != result.snapshot_hash {
        return Err(MemoryError::InvalidInput(format!(
            "staged semantic hash mismatch: predicted {}, verified {}",
            result.snapshot_hash, verified.canonical.snapshot_hash
        )));
    }
    let report = MaterializationReport {
        staging_root: staging_root.to_path_buf(),
        entities: result.entities.len(),
        observations: result.observations.len(),
        relations: result.relations.len(),
        domains,
        predicted_hash: result.snapshot_hash.to_string(),
        verified_hash: verified.canonical.snapshot_hash.to_string(),
    };
    Ok((verified, report))
}

fn materialized_stub(
    space_id: openmemory_core::space::SpaceId,
    entity: &CanonicalEntity,
    domain: usize,
) -> MaterializedStub {
    MaterializedStub {
        id: derived_stub_id(space_id, &entity.logical_id, domain),
        name: entity.label.clone(),
        entity_type: entity.controlled_kind.clone(),
    }
}

pub(crate) fn derived_stub_id(
    space_id: openmemory_core::space::SpaceId,
    logical_id: &str,
    domain: usize,
) -> String {
    derived_row_id(
        b"openmemory/partition-stub/v1",
        space_id,
        logical_id,
        domain,
    )
}

fn derived_row_id(
    separator: &'static [u8],
    space_id: openmemory_core::space::SpaceId,
    logical_id: &str,
    domain: usize,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(separator);
    hasher.update(space_id.to_string().as_bytes());
    hasher.update(&(logical_id.len() as u64).to_le_bytes());
    hasher.update(logical_id.as_bytes());
    hasher.update(&(domain as u64).to_le_bytes());
    format!("derived:{}", &hasher.finalize().to_hex()[..32])
}

fn merge_error(error: openmemory_merge::MergeError) -> MemoryError {
    MemoryError::InvalidInput(format!("materialized snapshot is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_core::space::{RevisionId, SpaceId};
    use openmemory_merge::canonical::{
        CanonicalEntity, CanonicalObservation, CanonicalRelation, Lifecycle,
    };
    use std::collections::BTreeSet;

    #[test]
    fn fresh_materialization_verifies_across_domains() {
        let space = SpaceId::new();
        let mut entities = vec![
            CanonicalEntity::new(
                space,
                "one".to_string(),
                RevisionId::new(),
                "same-name".to_string(),
                BTreeSet::new(),
                "concept".to_string(),
                BTreeSet::new(),
                String::new(),
            )
            .unwrap(),
            CanonicalEntity::new(
                space,
                "two".to_string(),
                RevisionId::new(),
                "different-name".to_string(),
                BTreeSet::new(),
                "concept".to_string(),
                BTreeSet::new(),
                String::new(),
            )
            .unwrap(),
        ];
        let probe = tempfile::tempdir().unwrap();
        let config = Config::default();
        let routing = DomainStore::open_scoped(&config, probe.path(), 2, space).unwrap();
        while routing.domain_for(&entities[0].label) == routing.domain_for(&entities[1].label) {
            entities[1].label.push('x');
        }
        entities[1] = CanonicalEntity::new(
            space,
            "two".to_string(),
            entities[1].revision_id,
            entities[1].label.clone(),
            BTreeSet::new(),
            "concept".to_string(),
            BTreeSet::new(),
            String::new(),
        )
        .unwrap();
        drop(routing);
        let observation = CanonicalObservation::new(
            space,
            "observation".to_string(),
            "one".to_string(),
            RevisionId::new(),
            "materialized text".to_string(),
            BTreeSet::new(),
            "test".to_string(),
            Lifecycle::Active,
        )
        .unwrap();
        let relation = CanonicalRelation::new(
            space,
            "relation".to_string(),
            RevisionId::new(),
            "one".to_string(),
            "two".to_string(),
            "references".to_string(),
            1.0,
            "test".to_string(),
            BTreeSet::new(),
            Lifecycle::Active,
        )
        .unwrap();
        let result =
            CanonicalSpaceSnapshot::new(space, 1, entities, vec![observation], vec![relation])
                .unwrap();
        let stage = tempfile::tempdir().unwrap();
        let (_verified, report) = materialize_snapshot(&config, stage.path(), 2, &result).unwrap();
        assert_eq!(report.predicted_hash, report.verified_hash);
    }
}
