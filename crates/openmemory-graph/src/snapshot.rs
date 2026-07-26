//! Canonical semantic export records for whole-space snapshot assembly.

use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::{ChangeSetId, RevisionId, SpaceId};
use openmemory_merge::canonical::{
    AssertionTrust, CanonicalEntity, CanonicalObservation, CanonicalRelation, EntityContribution,
    IdentifierAssertion, Lifecycle as CanonicalLifecycle, OriginKey,
};
use openmemory_merge::hash::SemanticHash;

use rusqlite::{params, TransactionBehavior};

use crate::{MemoryError, MemoryResult, MemoryStore, PARTITION_STUB_SOURCE};

/// Derived placeholder needed to materialize a cross-domain relation.
#[derive(Debug, Clone)]
pub struct MaterializedStub {
    pub id: String,
    pub name: String,
    pub entity_type: String,
}

/// One canonical or derived mirror relation row for a staged domain.
#[derive(Debug, Clone)]
pub struct MaterializedRelation {
    pub row_id: String,
    pub canonical_relation_id: String,
    pub mirror_role: String,
    pub from_entity: String,
    pub to_entity: String,
    pub semantic: CanonicalRelation,
}

/// Canonical and derived records assigned to one staged physical domain.
#[derive(Debug, Clone)]
pub struct CanonicalDomainImport {
    pub semantic_generation: u64,
    pub entities: Vec<CanonicalEntity>,
    pub stubs: Vec<MaterializedStub>,
    pub observations: Vec<CanonicalObservation>,
    pub relations: Vec<MaterializedRelation>,
}

#[derive(Debug, Clone)]
pub struct CanonicalEndpoint {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub is_stub: bool,
}

#[derive(Debug, Clone)]
pub struct CanonicalRelationRecord {
    pub logical_id: String,
    pub revision_id: RevisionId,
    pub from: CanonicalEndpoint,
    pub to: CanonicalEndpoint,
    pub relation_type: String,
    pub weight: f64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub source: String,
    pub evidence: BTreeSet<String>,
    pub lifecycle: CanonicalLifecycle,
    pub origins: BTreeMap<OriginKey, SemanticHash>,
}

#[derive(Debug, Clone)]
pub struct CanonicalDomainRecords {
    pub space_id: SpaceId,
    pub semantic_generation: u64,
    pub indexed_generation: u64,
    pub mirror_generation: u64,
    pub entities: Vec<CanonicalEntity>,
    pub observations: Vec<CanonicalObservation>,
    pub relations: Vec<CanonicalRelationRecord>,
}

impl MemoryStore {
    /// Export semantic truth only: no access counts, index files, journals,
    /// mirror rows, or partition stubs.
    pub fn export_canonical_domain_records(&self) -> MemoryResult<CanonicalDomainRecords> {
        self.with_reader(|conn| {
            let (semantic_generation, indexed_generation, mirror_generation): (u64, u64, u64) =
                conn.query_row(
                    "SELECT semantic_generation, indexed_generation, mirror_generation
                     FROM domain_state WHERE singleton=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
            if indexed_generation < semantic_generation {
                return Err(MemoryError::IndexRepairRequired(
                    "canonical snapshot requires indexed generation readiness".to_string(),
                ));
            }

            let mut entities = Vec::new();
            let mut entity_statement = conn.prepare(
                "SELECT id, name, entity_type, confidence, source,
                        current_revision_id, lifecycle
                 FROM entities
                 WHERE source<>?1 AND lifecycle<>'destroyed'
                 ORDER BY id",
            )?;
            let entity_rows = entity_statement
                .query_map([PARTITION_STUB_SOURCE], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f32>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for (id, name, entity_type, confidence, source, revision, _lifecycle) in entity_rows {
                let revision = required_revision(&id, revision)?;
                let aliases = strings(
                    conn,
                    "SELECT alias FROM entity_revision_aliases
                     WHERE revision_id=?1 ORDER BY alias",
                    &revision.to_string(),
                )?
                .into_iter()
                .collect();
                let identifiers = load_identifiers(conn, revision)?;
                let mut entity = CanonicalEntity::new_complete(
                    self.space_id(),
                    id.clone(),
                    revision,
                    name,
                    aliases,
                    entity_type,
                    identifiers,
                    String::new(),
                    confidence.to_bits(),
                    source,
                )
                .map_err(merge_error)?;
                for contribution in entity_contributions(conn, &id)? {
                    entity
                        .contributions
                        .insert(contribution.origin.clone(), contribution);
                }
                entity.validate().map_err(merge_error)?;
                entities.push(entity);
            }

            let mut observations = Vec::new();
            let mut observation_statement = conn.prepare(
                "SELECT observation.id, observation.entity_id, observation.content,
                        observation.observed_at, observation.valid_from,
                        observation.valid_until, observation.confidence,
                        observation.source, observation.memory_tier,
                        observation.title, observation.summary, observation.importance,
                        observation.source_kind, observation.current_revision_id,
                        observation.lifecycle, observation.tombstoned
                 FROM observations AS observation
                 JOIN entities AS entity ON entity.id=observation.entity_id
                 WHERE entity.source<>?1 AND observation.lifecycle<>'destroyed'
                 ORDER BY observation.id",
            )?;
            let observation_rows = observation_statement
                .query_map([PARTITION_STUB_SOURCE], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, f32>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<f32>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, bool>(15)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for row in observation_rows {
                let (
                    id,
                    entity_id,
                    content,
                    observed_at,
                    valid_from,
                    valid_until,
                    confidence,
                    source,
                    memory_tier,
                    title,
                    summary,
                    importance,
                    source_kind,
                    revision,
                    lifecycle,
                    tombstoned,
                ) = row;
                let revision = required_revision(&id, revision)?;
                let concepts = strings(
                    conn,
                    "SELECT concept FROM observation_revision_concepts
                     WHERE revision_id=?1 ORDER BY concept",
                    &revision.to_string(),
                )?
                .into_iter()
                .collect();
                let source_files = strings(
                    conn,
                    "SELECT file_path FROM observation_revision_source_files
                     WHERE revision_id=?1 ORDER BY file_path",
                    &revision.to_string(),
                )?
                .into_iter()
                .collect();
                let lifecycle = if tombstoned || lifecycle == "retired" {
                    CanonicalLifecycle::Retired
                } else {
                    CanonicalLifecycle::Active
                };
                let mut observation = CanonicalObservation::new_complete(
                    self.space_id(),
                    id.clone(),
                    entity_id,
                    revision,
                    content,
                    observed_at,
                    valid_from,
                    valid_until,
                    confidence.to_bits(),
                    concepts,
                    source_files,
                    source,
                    memory_tier,
                    title,
                    summary,
                    importance.map(f32::to_bits),
                    source_kind,
                    lifecycle,
                )
                .map_err(merge_error)?;
                observation
                    .origins
                    .extend(origin_hashes(conn, "observation", &id)?);
                observation.validate().map_err(merge_error)?;
                observations.push(observation);
            }

            let mut relations = Vec::new();
            let mut relation_statement = conn.prepare(
                "SELECT relation.id, relation.current_revision_id,
                        source.id, source.name, source.entity_type, source.source,
                        target.id, target.name, target.entity_type, target.source,
                        relation.relation_type, relation.weight, relation.source,
                        revision.valid_from, revision.valid_until,
                        revision.evidence_json, relation.lifecycle
                 FROM relations AS relation
                 JOIN entities AS source ON source.id=relation.from_entity
                 JOIN entities AS target ON target.id=relation.to_entity
                 JOIN relation_revisions AS revision
                   ON revision.id=relation.current_revision_id
                 WHERE relation.mirror_role='canonical'
                   AND relation.lifecycle<>'destroyed'
                 ORDER BY relation.id",
            )?;
            let relation_rows = relation_statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, f64>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, Option<i64>>(13)?,
                        row.get::<_, Option<i64>>(14)?,
                        row.get::<_, String>(15)?,
                        row.get::<_, String>(16)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for row in relation_rows {
                let revision = required_revision(&row.0, row.1)?;
                let evidence = canonical_evidence(&row.15)?;
                relations.push(CanonicalRelationRecord {
                    logical_id: row.0.clone(),
                    revision_id: revision,
                    from: CanonicalEndpoint {
                        id: row.2,
                        name: row.3,
                        entity_type: row.4,
                        is_stub: row.5 == PARTITION_STUB_SOURCE,
                    },
                    to: CanonicalEndpoint {
                        id: row.6,
                        name: row.7,
                        entity_type: row.8,
                        is_stub: row.9 == PARTITION_STUB_SOURCE,
                    },
                    relation_type: row.10,
                    weight: row.11,
                    source: row.12,
                    evidence,
                    valid_from: row.13,
                    valid_until: row.14,
                    lifecycle: if row.16 == "retired" {
                        CanonicalLifecycle::Retired
                    } else {
                        CanonicalLifecycle::Active
                    },
                    origins: origin_hashes(conn, "relation", &row.0)?,
                });
            }

            Ok(CanonicalDomainRecords {
                space_id: self.space_id(),
                semantic_generation,
                indexed_generation,
                mirror_generation,
                entities,
                observations,
                relations,
            })
        })
    }

    /// Populate a fresh staged domain from canonical records. This is used only
    /// behind whole-root promotion: a non-empty destination fails closed.
    pub fn import_canonical_domain(&self, import: &CanonicalDomainImport) -> MemoryResult<()> {
        validate_import(self.space_id(), import)?;
        let barrier = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let occupied: u64 = tx.query_row(
            "SELECT
                (SELECT COUNT(*) FROM entities) +
                (SELECT COUNT(*) FROM observations) +
                (SELECT COUNT(*) FROM relations)",
            [],
            |row| row.get(0),
        )?;
        if occupied != 0 {
            return Err(MemoryError::InvalidInput(
                "canonical import destination is not empty".to_string(),
            ));
        }

        let change_set = ChangeSetId::new();
        let generation = import.semantic_generation.max(1);
        let request_hash = import_hash(import);
        tx.execute(
            "INSERT INTO change_sets(
                id, space_id, idempotency_key, request_hash, actor_principal,
                actor_kind, authorization_generation, reason, source, state,
                base_generation, committed_generation, created_at, decided_at,
                decided_by)
             VALUES (?1, ?2, ?3, ?4, 'system:materializer', 'system', 1,
                     'staged material merge import', 'material_merge', 'applied',
                     0, ?5, 0, 0, 'system:materializer')",
            params![
                change_set.to_string(),
                self.space_id().to_string(),
                format!("materialize:{}", request_hash.to_hex()),
                request_hash.as_bytes().as_slice(),
                generation,
            ],
        )?;

        let mut ordinal = 0_u64;
        for entity in &import.entities {
            tx.execute(
                "INSERT INTO entities(
                    id, name, entity_type, created_at, updated_at, confidence,
                    source, current_revision_id, row_version, lifecycle)
                 VALUES (?1, ?2, ?3, 0, 0, ?4, ?5, ?6, 1, 'active')",
                params![
                    entity.logical_id,
                    entity.label,
                    entity.controlled_kind,
                    f32::from_bits(entity.confidence_bits),
                    entity.source,
                    entity.revision_id.to_string(),
                ],
            )?;
            tx.execute(
                "INSERT INTO entity_revisions(
                    id, entity_id, parent_revision_id, semantic_hash, name,
                    entity_type, confidence, source, created_by_change_set, created_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
                params![
                    entity.revision_id.to_string(),
                    entity.logical_id,
                    entity.semantic_hash.as_bytes().as_slice(),
                    entity.label,
                    entity.controlled_kind,
                    f32::from_bits(entity.confidence_bits),
                    entity.source,
                    change_set.to_string(),
                ],
            )?;
            for alias in &entity.aliases {
                tx.execute(
                    "INSERT INTO entity_revision_aliases(revision_id, alias)
                     VALUES (?1, ?2)",
                    params![entity.revision_id.to_string(), alias],
                )?;
            }
            for identifier in &entity.identifiers {
                insert_identifier(&tx, entity.revision_id, identifier)?;
            }
            for contribution in entity.contributions.values() {
                if contribution.origin.space_id == self.space_id()
                    && contribution.origin.logical_id == entity.logical_id
                    && contribution.origin.revision_id == entity.revision_id
                {
                    continue;
                }
                insert_contribution(
                    &tx,
                    "entity",
                    &entity.logical_id,
                    &contribution.origin,
                    contribution.semantic_hash,
                    &serde_json::to_string(contribution)?,
                    change_set,
                )?;
            }
            insert_import_audit(
                &tx,
                change_set,
                ordinal,
                "entity",
                &entity.logical_id,
                &entity.revision_id.to_string(),
            )?;
            ordinal = ordinal.saturating_add(1);
        }

        for stub in &import.stubs {
            tx.execute(
                "INSERT INTO entities(
                    id, name, entity_type, created_at, updated_at, confidence,
                    source, current_revision_id, row_version, lifecycle)
                 VALUES (?1, ?2, ?3, 0, 0, 1.0, ?4, NULL, 1, 'active')",
                params![stub.id, stub.name, stub.entity_type, PARTITION_STUB_SOURCE],
            )?;
        }

        for observation in &import.observations {
            let lifecycle = lifecycle_name(observation.lifecycle);
            tx.execute(
                "INSERT INTO observations(
                    id, entity_id, content, observed_at, valid_from, valid_until,
                    confidence, source, tombstoned, access_count, memory_tier,
                    title, summary, importance, source_kind, current_revision_id,
                    row_version, lifecycle)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11,
                         ?12, ?13, ?14, ?15, 1, ?16)",
                params![
                    observation.logical_id,
                    observation.entity_id,
                    observation.content,
                    observation.observed_at,
                    observation.valid_from,
                    observation.valid_until,
                    f32::from_bits(observation.confidence_bits),
                    observation.source,
                    observation.lifecycle == CanonicalLifecycle::Retired,
                    observation.memory_tier,
                    observation.title,
                    observation.summary,
                    observation.importance_bits.map(f32::from_bits),
                    observation.source_kind,
                    observation.revision_id.to_string(),
                    lifecycle,
                ],
            )?;
            tx.execute(
                "INSERT INTO observation_revisions(
                    id, observation_id, parent_revision_id, semantic_hash, content,
                    observed_at, valid_from, valid_until, confidence, source,
                    memory_tier, title, summary, importance, source_kind,
                    created_by_change_set, created_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                         ?11, ?12, ?13, ?14, ?15, 0)",
                params![
                    observation.revision_id.to_string(),
                    observation.logical_id,
                    observation.semantic_hash.as_bytes().as_slice(),
                    observation.content,
                    observation.observed_at,
                    observation.valid_from,
                    observation.valid_until,
                    f32::from_bits(observation.confidence_bits),
                    observation.source,
                    observation.memory_tier,
                    observation.title,
                    observation.summary,
                    observation.importance_bits.map(f32::from_bits),
                    observation.source_kind,
                    change_set.to_string(),
                ],
            )?;
            for concept in &observation.concepts {
                tx.execute(
                    "INSERT INTO observation_concepts(observation_id, concept)
                     VALUES (?1, ?2)",
                    params![observation.logical_id, concept],
                )?;
                tx.execute(
                    "INSERT INTO observation_revision_concepts(revision_id, concept)
                     VALUES (?1, ?2)",
                    params![observation.revision_id.to_string(), concept],
                )?;
            }
            for path in &observation.source_files {
                tx.execute(
                    "INSERT INTO observation_source_files(observation_id, file_path)
                     VALUES (?1, ?2)",
                    params![observation.logical_id, path],
                )?;
                tx.execute(
                    "INSERT INTO observation_revision_source_files(revision_id, file_path)
                     VALUES (?1, ?2)",
                    params![observation.revision_id.to_string(), path],
                )?;
            }
            for (origin, hash) in &observation.origins {
                if origin.space_id == self.space_id()
                    && origin.logical_id == observation.logical_id
                    && origin.revision_id == observation.revision_id
                {
                    continue;
                }
                insert_contribution(
                    &tx,
                    "observation",
                    &observation.logical_id,
                    origin,
                    *hash,
                    "{}",
                    change_set,
                )?;
            }
            tx.execute(
                "INSERT INTO index_outbox(
                    generation, object_kind, logical_id, operation, revision_id)
                 VALUES (?1, 'observation', ?2, ?3, ?4)",
                params![
                    generation,
                    observation.logical_id,
                    if observation.lifecycle == CanonicalLifecycle::Active {
                        "upsert"
                    } else {
                        "delete"
                    },
                    observation.revision_id.to_string(),
                ],
            )?;
            insert_import_audit(
                &tx,
                change_set,
                ordinal,
                "observation",
                &observation.logical_id,
                &observation.revision_id.to_string(),
            )?;
            ordinal = ordinal.saturating_add(1);
        }

        for relation in &import.relations {
            let lifecycle = lifecycle_name(relation.semantic.lifecycle);
            let evidence = encode_evidence(&relation.semantic.evidence)?;
            tx.execute(
                "INSERT INTO relations(
                    id, from_entity, to_entity, relation_type, weight, created_at,
                    valid_from, valid_until, source, canonical_relation_id,
                    mirror_role, current_revision_id, row_version, lifecycle)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?10, ?11,
                         1, ?12)",
                params![
                    relation.row_id,
                    relation.from_entity,
                    relation.to_entity,
                    relation.semantic.relation_type,
                    relation.semantic.weight,
                    relation.semantic.valid_from,
                    relation.semantic.valid_until,
                    relation.semantic.source,
                    relation.canonical_relation_id,
                    relation.mirror_role,
                    relation.semantic.revision_id.to_string(),
                    lifecycle,
                ],
            )?;
            tx.execute(
                "INSERT INTO relation_revisions(
                    id, relation_id, parent_revision_id, semantic_hash, from_entity,
                    to_entity, relation_type, weight, valid_from, valid_until,
                    source, evidence_json, created_by_change_set, created_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                         ?11, ?12, 0)",
                params![
                    relation.semantic.revision_id.to_string(),
                    relation.row_id,
                    relation.semantic.semantic_hash.as_bytes().as_slice(),
                    relation.from_entity,
                    relation.to_entity,
                    relation.semantic.relation_type,
                    relation.semantic.weight,
                    relation.semantic.valid_from,
                    relation.semantic.valid_until,
                    relation.semantic.source,
                    evidence,
                    change_set.to_string(),
                ],
            )?;
            if relation.mirror_role == "canonical" {
                for (origin, hash) in &relation.semantic.origins {
                    if origin.space_id == self.space_id()
                        && origin.logical_id == relation.semantic.logical_id
                        && origin.revision_id == relation.semantic.revision_id
                    {
                        continue;
                    }
                    insert_contribution(
                        &tx,
                        "relation",
                        &relation.semantic.logical_id,
                        origin,
                        *hash,
                        "{}",
                        change_set,
                    )?;
                }
                insert_import_audit(
                    &tx,
                    change_set,
                    ordinal,
                    "relation",
                    &relation.semantic.logical_id,
                    &relation.semantic.revision_id.to_string(),
                )?;
                ordinal = ordinal.saturating_add(1);
            }
        }

        tx.execute(
            "UPDATE domain_state SET semantic_generation=?1,
                 indexed_generation=0, mirror_generation=?1,
                 backfill_state='ready' WHERE singleton=1",
            [generation],
        )?;
        tx.commit()?;
        drop(conn);
        drop(barrier);
        self.repair_index_outbox()?;
        Ok(())
    }
}

fn validate_import(space_id: SpaceId, import: &CanonicalDomainImport) -> MemoryResult<()> {
    let mut ids = BTreeSet::new();
    for entity in &import.entities {
        entity.validate().map_err(merge_error)?;
        if !ids.insert(entity.logical_id.as_str()) {
            return Err(MemoryError::InvalidInput(
                "canonical import has duplicate entity IDs".to_string(),
            ));
        }
    }
    for stub in &import.stubs {
        if !openmemory_merge::canonical::valid_logical_id(&stub.id)
            || stub.name.is_empty()
            || stub.entity_type.is_empty()
            || !ids.insert(stub.id.as_str())
        {
            return Err(MemoryError::InvalidInput(
                "canonical import has an invalid or duplicate stub".to_string(),
            ));
        }
    }
    for observation in &import.observations {
        observation.validate().map_err(merge_error)?;
        if !ids.contains(observation.entity_id.as_str()) {
            return Err(MemoryError::InvalidInput(
                "canonical import observation endpoint is missing".to_string(),
            ));
        }
    }
    let mut row_ids = BTreeSet::new();
    let mut roles = BTreeSet::new();
    for relation in &import.relations {
        relation.semantic.validate().map_err(merge_error)?;
        if !ids.contains(relation.from_entity.as_str())
            || !ids.contains(relation.to_entity.as_str())
            || !row_ids.insert(relation.row_id.as_str())
            || !roles.insert((
                relation.canonical_relation_id.as_str(),
                relation.mirror_role.as_str(),
            ))
        {
            return Err(MemoryError::InvalidInput(
                "canonical import relation is invalid or duplicated".to_string(),
            ));
        }
    }
    if import.entities.iter().any(|entity| {
        entity
            .contributions
            .keys()
            .any(|origin| origin.space_id == space_id && origin.logical_id.is_empty())
    }) {
        return Err(MemoryError::InvalidInput(
            "canonical import contains an invalid local origin".to_string(),
        ));
    }
    Ok(())
}

fn import_hash(import: &CanonicalDomainImport) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"openmemory/canonical-domain-import/v1\0");
    hasher.update(&import.semantic_generation.to_le_bytes());
    for entity in &import.entities {
        hasher.update(entity.logical_id.as_bytes());
        hasher.update(entity.semantic_hash.as_bytes());
    }
    for observation in &import.observations {
        hasher.update(observation.logical_id.as_bytes());
        hasher.update(observation.semantic_hash.as_bytes());
    }
    for relation in &import.relations {
        hasher.update(relation.row_id.as_bytes());
        hasher.update(relation.semantic.semantic_hash.as_bytes());
    }
    hasher.finalize()
}

fn insert_identifier(
    tx: &rusqlite::Transaction<'_>,
    revision: RevisionId,
    identifier: &IdentifierAssertion,
) -> MemoryResult<()> {
    let (trust, snapshot, verifier, generation) = match &identifier.trust {
        AssertionTrust::Claimed => ("claimed", None, None, None),
        AssertionTrust::SourceVerified {
            source_snapshot_id,
            verifier_version,
            resolver_generation,
        } => (
            "source_verified",
            Some(source_snapshot_id.as_str()),
            Some(verifier_version.as_str()),
            Some(*resolver_generation),
        ),
    };
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
            trust,
            snapshot,
            verifier,
            generation,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_contribution(
    tx: &rusqlite::Transaction<'_>,
    kind: &str,
    target: &str,
    origin: &OriginKey,
    hash: SemanticHash,
    encoded: &str,
    change_set: ChangeSetId,
) -> MemoryResult<()> {
    tx.execute(
        "INSERT INTO origin_contributions(
            object_kind, target_logical_id, origin_space_id, origin_logical_id,
            origin_revision_id, semantic_hash, contribution_json,
            imported_by_change_set, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
        params![
            kind,
            target,
            origin.space_id.to_string(),
            origin.logical_id,
            origin.revision_id.to_string(),
            hash.as_bytes().as_slice(),
            encoded,
            change_set.to_string(),
        ],
    )?;
    Ok(())
}

fn insert_import_audit(
    tx: &rusqlite::Transaction<'_>,
    change_set: ChangeSetId,
    ordinal: u64,
    kind: &str,
    logical_id: &str,
    revision_id: &str,
) -> MemoryResult<()> {
    tx.execute(
        "INSERT INTO change_requests(
            change_set_id, ordinal, payload_version, object_kind, logical_id,
            operation, requested_payload_json)
         VALUES (?1, ?2, 1, ?3, ?4, 'materialize', '{}')",
        params![change_set.to_string(), ordinal, kind, logical_id],
    )?;
    tx.execute(
        "INSERT INTO change_events(
            change_set_id, ordinal, object_kind, logical_id, operation,
            after_revision_id, after_lifecycle)
         VALUES (?1, ?2, ?3, ?4, 'materialize', ?5, 'active')",
        params![
            change_set.to_string(),
            ordinal,
            kind,
            logical_id,
            revision_id,
        ],
    )?;
    Ok(())
}

fn lifecycle_name(lifecycle: CanonicalLifecycle) -> &'static str {
    match lifecycle {
        CanonicalLifecycle::Active => "active",
        CanonicalLifecycle::Retired => "retired",
    }
}

fn encode_evidence(evidence: &BTreeSet<String>) -> MemoryResult<String> {
    if evidence.is_empty() {
        return Ok("{}".to_string());
    }
    if evidence.len() == 1 {
        let value: serde_json::Value = serde_json::from_str(
            evidence
                .first()
                .expect("non-empty evidence has a first value"),
        )?;
        return Ok(canonical_json(&value));
    }
    let values = evidence
        .iter()
        .map(|encoded| serde_json::from_str(encoded))
        .collect::<Result<Vec<serde_json::Value>, _>>()?;
    Ok(canonical_json(&serde_json::Value::Array(values)))
}

fn load_identifiers(
    conn: &rusqlite::Connection,
    revision: RevisionId,
) -> MemoryResult<BTreeSet<IdentifierAssertion>> {
    let mut statement = conn.prepare(
        "SELECT namespace, raw_value, canonical_value, trust, source_snapshot_id,
                verifier_version, resolver_generation
         FROM entity_revision_identifiers
         WHERE revision_id=?1 ORDER BY namespace, raw_value",
    )?;
    let rows = statement
        .query_map([revision.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<u64>>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|row| {
            let trust = match row.3.as_str() {
                "claimed" => AssertionTrust::Claimed,
                "source_verified" => AssertionTrust::SourceVerified {
                    source_snapshot_id: row.4.ok_or_else(|| {
                        MemoryError::Schema("verified identifier lacks source snapshot".to_string())
                    })?,
                    verifier_version: row.5.ok_or_else(|| {
                        MemoryError::Schema(
                            "verified identifier lacks verifier version".to_string(),
                        )
                    })?,
                    resolver_generation: row.6.ok_or_else(|| {
                        MemoryError::Schema(
                            "verified identifier lacks resolver generation".to_string(),
                        )
                    })?,
                },
                _ => return Err(MemoryError::Schema("invalid identifier trust".to_string())),
            };
            Ok(IdentifierAssertion {
                namespace: row.0,
                raw_value: row.1,
                canonical_value: row.2,
                trust,
            })
        })
        .collect()
}

fn entity_contributions(
    conn: &rusqlite::Connection,
    id: &str,
) -> MemoryResult<Vec<EntityContribution>> {
    let mut statement = conn.prepare(
        "SELECT contribution_json FROM origin_contributions
         WHERE object_kind='entity' AND target_logical_id=?1
         ORDER BY origin_space_id, origin_logical_id, origin_revision_id",
    )?;
    let rows = statement
        .query_map([id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|encoded| {
            serde_json::from_str(&encoded).map_err(|error| {
                MemoryError::Schema(format!("entity contribution is not canonical: {error}"))
            })
        })
        .collect()
}

fn origin_hashes(
    conn: &rusqlite::Connection,
    kind: &str,
    id: &str,
) -> MemoryResult<BTreeMap<OriginKey, SemanticHash>> {
    let mut statement = conn.prepare(
        "SELECT origin_space_id, origin_logical_id, origin_revision_id, semantic_hash
         FROM origin_contributions
         WHERE object_kind=?1 AND target_logical_id=?2
         ORDER BY origin_space_id, origin_logical_id, origin_revision_id",
    )?;
    let rows = statement
        .query_map([kind, id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(space, logical_id, revision, hash)| {
            Ok((
                OriginKey {
                    space_id: space.parse().map_err(|error| {
                        MemoryError::Schema(format!("invalid origin space ID: {error}"))
                    })?,
                    logical_id,
                    revision_id: revision.parse().map_err(|error| {
                        MemoryError::Schema(format!("invalid origin revision ID: {error}"))
                    })?,
                },
                semantic_hash(&hash)?,
            ))
        })
        .collect()
}

fn semantic_hash(bytes: &[u8]) -> MemoryResult<SemanticHash> {
    if bytes.len() != 32 {
        return Err(MemoryError::Schema(
            "semantic hash is not 32 bytes".to_string(),
        ));
    }
    let text = format!(
        "blake3:{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    text.parse()
        .map_err(|error| MemoryError::Schema(format!("invalid semantic hash: {error}")))
}

fn canonical_evidence(encoded: &str) -> MemoryResult<BTreeSet<String>> {
    let value: serde_json::Value = serde_json::from_str(encoded)?;
    if value == serde_json::json!({}) {
        Ok(BTreeSet::new())
    } else {
        Ok(BTreeSet::from([canonical_json(&value)]))
    }
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => {
            serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
        }
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        serde_json::Value::Object(values) => {
            let sorted = values
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        canonical_json(value)
                    )
                })
                .collect::<BTreeSet<_>>();
            format!("{{{}}}", sorted.into_iter().collect::<Vec<_>>().join(","))
        }
    }
}

fn required_revision(id: &str, revision: Option<String>) -> MemoryResult<RevisionId> {
    revision
        .ok_or_else(|| {
            MemoryError::InvalidInput(format!(
                "object {id} has no immutable revision; run history backfill"
            ))
        })?
        .parse()
        .map_err(|error| MemoryError::Schema(format!("invalid revision ID: {error}")))
}

fn strings(conn: &rusqlite::Connection, sql: &str, parameter: &str) -> MemoryResult<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let values = statement
        .query_map([parameter], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from)?;
    Ok(values)
}

fn merge_error(error: openmemory_merge::MergeError) -> MemoryError {
    MemoryError::InvalidInput(format!("canonical snapshot export failed: {error}"))
}
