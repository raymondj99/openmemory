#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::{DecisionEventId, RevisionId, SnapshotId, SpaceId, SpaceRole};
use openmemory_merge::discovery::{CandidateIndex, DEFAULT_CANDIDATE_CAP};
use openmemory_merge::evidence::{analyze_pair, IdentityPolicy};
use openmemory_merge::hash::{PredictedResultHash, SemanticHash};
use openmemory_merge::model::{
    AssertionTrust, DomainGeneration, EntityRecord, IdentifierAssertion, LogicalId, MemorySnapshot,
    ObjectAddress, ObservationRecord, OriginContribution, QualifiedObjectId, RelationRecord,
    ResultObjectId, SnapshotSource, SpaceVersion,
};
use openmemory_merge::planner::{
    ActionCounts, DistinctReason, EntityDisposition, MergeAccounting, MergeAction, MergePolicy,
    PlanningReceipts,
};
use openmemory_merge::receipt::{IdentityDecision, IdentityResolutionReceipt};
use openmemory_merge::MergeErrorCode;

pub fn uuid7(sequence: u64) -> String {
    format!("018f6b7a-4d3c-7abc-8def-{sequence:012x}")
}

pub fn space(sequence: u64) -> SpaceId {
    uuid7(sequence).parse().unwrap()
}

pub fn snapshot_id(sequence: u64) -> SnapshotId {
    uuid7(sequence).parse().unwrap()
}

pub fn revision(sequence: u64) -> RevisionId {
    uuid7(sequence).parse().unwrap()
}

pub fn version(sequence: u8) -> SpaceVersion {
    SpaceVersion::new(
        vec![DomainGeneration::new(
            0,
            u64::from(sequence),
            u64::from(sequence),
            u64::from(sequence),
        )],
        SemanticHash::from_bytes([sequence; 32]),
    )
    .unwrap()
}

pub fn entity(
    space: SpaceId,
    logical_id: &str,
    revision_sequence: u64,
    label: &str,
    kind: &str,
) -> EntityRecord {
    EntityRecord::new(
        ObjectAddress::new(space, LogicalId::new(logical_id).unwrap()),
        revision(revision_sequence),
        label,
        Some(kind),
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
pub fn verified_entity(
    space: SpaceId,
    logical_id: &str,
    revision_sequence: u64,
    label: &str,
    kind: &str,
    snapshot: SnapshotId,
    namespace: &str,
    value: &str,
    resolver_generation: u64,
) -> EntityRecord {
    entity(space, logical_id, revision_sequence, label, kind)
        .with_identifiers([IdentifierAssertion::source_verified(
            namespace,
            value,
            snapshot,
            "fixture-resolver-v1",
            resolver_generation,
        )
        .unwrap()])
        .unwrap()
}

pub fn observation(
    space: SpaceId,
    logical_id: &str,
    entity_id: &str,
    revision_sequence: u64,
    content: &str,
) -> ObservationRecord {
    ObservationRecord::new(
        ObjectAddress::new(space, LogicalId::new(logical_id).unwrap()),
        ObjectAddress::new(space, LogicalId::new(entity_id).unwrap()),
        revision(revision_sequence),
        content,
    )
    .unwrap()
}

pub fn relation(
    space: SpaceId,
    logical_id: &str,
    subject: &str,
    predicate: &str,
    object: &str,
    revision_sequence: u64,
) -> RelationRecord {
    RelationRecord::new(
        ObjectAddress::new(space, LogicalId::new(logical_id).unwrap()),
        ObjectAddress::new(space, LogicalId::new(subject).unwrap()),
        predicate,
        ObjectAddress::new(space, LogicalId::new(object).unwrap()),
        revision(revision_sequence),
    )
    .unwrap()
}

pub fn memory_snapshot(
    snapshot: SnapshotId,
    space: SpaceId,
    sequence: u8,
    entities: Vec<EntityRecord>,
    observations: Vec<ObservationRecord>,
    relations: Vec<RelationRecord>,
) -> MemorySnapshot {
    MemorySnapshot::new(
        snapshot,
        space,
        version(sequence),
        entities,
        observations,
        relations,
    )
    .unwrap()
}

pub fn planning_receipts(
    target: &MemorySnapshot,
    source: &MemorySnapshot,
    policy: &IdentityPolicy,
    same_pairs: &BTreeSet<(String, String)>,
) -> PlanningReceipts {
    let index = CandidateIndex::build(
        1,
        target.header().snapshot_id(),
        policy.clone(),
        target.entities().iter().cloned(),
    )
    .unwrap();
    let targets = target
        .entities()
        .iter()
        .map(|record| (record.address().clone(), record))
        .collect::<BTreeMap<_, _>>();
    let mut discoveries = Vec::new();
    let mut resolutions = Vec::new();
    let mut event_sequence = 900_u64;
    for source_record in source.entities() {
        let discovery = index
            .discover(
                source_record,
                source.header().snapshot_id(),
                DEFAULT_CANDIDATE_CAP,
            )
            .unwrap();
        for candidate in discovery.candidates() {
            let target_record = targets[candidate.target().address()];
            let packet = analyze_pair(
                target_record,
                target.header().snapshot_id(),
                source_record,
                source.header().snapshot_id(),
                policy,
            )
            .unwrap();
            let pair = (
                target_record.address().logical_id().as_str().to_string(),
                source_record.address().logical_id().as_str().to_string(),
            );
            let decision = if same_pairs.contains(&pair) {
                IdentityDecision::Same
            } else {
                IdentityDecision::Different
            };
            resolutions.push(
                IdentityResolutionReceipt::reviewed(
                    &packet,
                    decision,
                    uuid7(event_sequence).parse::<DecisionEventId>().unwrap(),
                    SpaceRole::Reviewer,
                )
                .unwrap(),
            );
            event_sequence += 1;
        }
        discoveries.push(discovery);
    }
    PlanningReceipts::new(discoveries, resolutions, Vec::new())
}

/// Bounded, test-only materialized reference planner for successful inputs.
/// It intentionally uses maps and full result identity state instead of the
/// production planner's streaming indexes and token accumulator.
pub fn reference_actions(
    target: &MemorySnapshot,
    source: &MemorySnapshot,
    receipts: &PlanningReceipts,
) -> Vec<MergeAction> {
    let same_by_source = receipts
        .resolutions()
        .iter()
        .filter(|receipt| receipt.decision() == IdentityDecision::Same)
        .map(|receipt| (receipt.source().address().logical_id().clone(), receipt))
        .collect::<BTreeMap<_, _>>();
    let discovery_by_source = receipts
        .discoveries()
        .iter()
        .map(|receipt| (receipt.source().address().logical_id().clone(), receipt))
        .collect::<BTreeMap<_, _>>();
    let keep_by_source = receipts
        .keep_distinct()
        .iter()
        .map(|receipt| (receipt.source().address().logical_id().clone(), receipt))
        .collect::<BTreeMap<_, _>>();
    let mut actions = Vec::new();
    let mut entity_results = BTreeMap::<LogicalId, ResultObjectId>::new();

    for record in target.entities() {
        actions.push(MergeAction::KeepTargetEntity {
            target: record.clone(),
        });
    }
    for record in source.entities() {
        let source_id = record.address().logical_id();
        let (disposition, result) = if let Some(receipt) = same_by_source.get(source_id) {
            (
                EntityDisposition::Coalesce {
                    source: record.address().clone(),
                    target: receipt.target().address().clone(),
                    receipt: receipt.hash(),
                },
                ResultObjectId::Target(receipt.target().address().logical_id().clone()),
            )
        } else {
            let discovery = discovery_by_source[source_id];
            let (reason, receipt) = if let Some(keep) = keep_by_source.get(source_id) {
                (DistinctReason::KeepDistinct, Some(keep.hash()))
            } else if discovery.candidates().is_empty() {
                (DistinctReason::NoCandidate, None)
            } else {
                (DistinctReason::ReviewedDifferent, None)
            };
            let qualified = QualifiedObjectId::derive(record.address().space(), source_id);
            (
                EntityDisposition::AddDistinct {
                    source: record.address().clone(),
                    result: qualified.clone(),
                    reason,
                    receipt,
                },
                ResultObjectId::Qualified(qualified),
            )
        };
        entity_results.insert(source_id.clone(), result);
        actions.push(MergeAction::ApplyEntityDisposition {
            source: record.clone(),
            disposition,
        });
    }

    let mut observation_index = BTreeMap::new();
    for record in target.observations() {
        let result = ResultObjectId::Target(record.address().logical_id().clone());
        let result_entity = ResultObjectId::Target(record.entity().logical_id().clone());
        observation_index.insert(
            (
                result_entity.clone(),
                record.lifecycle(),
                record.content().to_string(),
            ),
            result,
        );
        actions.push(MergeAction::KeepTargetObservation {
            target: record.clone(),
            result_entity,
        });
    }
    for record in source.observations() {
        let result_entity = entity_results[record.entity().logical_id()].clone();
        let key = (
            result_entity.clone(),
            record.lifecycle(),
            record.content().to_string(),
        );
        if let Some(target_result) = observation_index.get(&key) {
            actions.push(MergeAction::MergeObservationOrigin {
                source: record.clone(),
                target: target_result.clone(),
                result_entity,
            });
        } else {
            let result =
                QualifiedObjectId::derive(record.address().space(), record.address().logical_id());
            observation_index.insert(key, ResultObjectId::Qualified(result.clone()));
            actions.push(MergeAction::AddObservation {
                source: record.clone(),
                result,
                result_entity,
            });
        }
    }

    let mut relation_index = BTreeMap::new();
    for record in target.relations() {
        let result = ResultObjectId::Target(record.address().logical_id().clone());
        let result_subject = ResultObjectId::Target(record.subject().logical_id().clone());
        let result_object = ResultObjectId::Target(record.object().logical_id().clone());
        relation_index.insert(
            (
                result_subject.clone(),
                record.predicate().to_string(),
                result_object.clone(),
                record.lifecycle(),
            ),
            result,
        );
        actions.push(MergeAction::KeepTargetRelation {
            target: record.clone(),
            result_subject,
            result_object,
        });
    }
    for record in source.relations() {
        let result_subject = entity_results[record.subject().logical_id()].clone();
        let result_object = entity_results[record.object().logical_id()].clone();
        let key = (
            result_subject.clone(),
            record.predicate().to_string(),
            result_object.clone(),
            record.lifecycle(),
        );
        if let Some(target_result) = relation_index.get(&key) {
            actions.push(MergeAction::MergeRelationOrigin {
                source: record.clone(),
                target: target_result.clone(),
                result_subject,
                result_object,
            });
        } else {
            let result =
                QualifiedObjectId::derive(record.address().space(), record.address().logical_id());
            relation_index.insert(key, ResultObjectId::Qualified(result.clone()));
            actions.push(MergeAction::AddRelation {
                source: record.clone(),
                result,
                result_subject,
                result_object,
            });
        }
    }
    actions
}

pub fn reference_action_counts(actions: &[MergeAction]) -> ActionCounts {
    let mut counts = ActionCounts::default();
    for action in actions {
        match action {
            MergeAction::KeepTargetEntity { .. } => counts.target_entities += 1,
            MergeAction::ApplyEntityDisposition { disposition, .. } => {
                counts.source_entities += 1;
                match disposition {
                    EntityDisposition::Coalesce { .. } => counts.coalesced_entities += 1,
                    EntityDisposition::AddDistinct { .. } => counts.added_entities += 1,
                }
            }
            MergeAction::KeepTargetObservation { .. } => counts.target_observations += 1,
            MergeAction::AddObservation { .. } => {
                counts.source_observations += 1;
                counts.added_observations += 1;
            }
            MergeAction::MergeObservationOrigin { .. } => {
                counts.source_observations += 1;
                counts.merged_observations += 1;
            }
            MergeAction::KeepTargetRelation { .. } => counts.target_relations += 1,
            MergeAction::AddRelation { .. } => {
                counts.source_relations += 1;
                counts.added_relations += 1;
            }
            MergeAction::MergeRelationOrigin { .. } => {
                counts.source_relations += 1;
                counts.merged_relations += 1;
            }
        }
    }
    counts
}

pub fn reference_accounting(
    actions: &[MergeAction],
    receipts: &PlanningReceipts,
) -> MergeAccounting {
    let counts = reference_action_counts(actions);
    let mut entity_origins = BTreeMap::<String, BTreeSet<OriginContribution>>::new();
    let mut observation_origins = BTreeMap::<String, BTreeSet<OriginContribution>>::new();
    let mut relation_origins = BTreeMap::<String, BTreeSet<OriginContribution>>::new();
    for action in actions {
        match action {
            MergeAction::KeepTargetEntity { target } => {
                entity_origins.insert(
                    target.address().logical_id().as_str().to_string(),
                    target.origins().clone(),
                );
            }
            MergeAction::ApplyEntityDisposition {
                source,
                disposition,
            } => match disposition {
                EntityDisposition::Coalesce { target, .. } => {
                    entity_origins
                        .get_mut(target.logical_id().as_str())
                        .unwrap()
                        .extend(source.origins().iter().cloned());
                }
                EntityDisposition::AddDistinct { result, .. } => {
                    entity_origins.insert(result.as_str().to_string(), source.origins().clone());
                }
            },
            MergeAction::KeepTargetObservation { target, .. } => {
                observation_origins.insert(
                    target.address().logical_id().as_str().to_string(),
                    target.origins().clone(),
                );
            }
            MergeAction::AddObservation { source, result, .. } => {
                observation_origins.insert(result.as_str().to_string(), source.origins().clone());
            }
            MergeAction::MergeObservationOrigin { source, target, .. } => {
                observation_origins
                    .get_mut(target.as_str())
                    .unwrap()
                    .extend(source.origins().iter().cloned());
            }
            MergeAction::KeepTargetRelation { target, .. } => {
                relation_origins.insert(
                    target.address().logical_id().as_str().to_string(),
                    target.origins().clone(),
                );
            }
            MergeAction::AddRelation { source, result, .. } => {
                relation_origins.insert(result.as_str().to_string(), source.origins().clone());
            }
            MergeAction::MergeRelationOrigin { source, target, .. } => {
                relation_origins
                    .get_mut(target.as_str())
                    .unwrap()
                    .extend(source.origins().iter().cloned());
            }
        }
    }
    let retained_origins = entity_origins
        .values()
        .chain(observation_origins.values())
        .chain(relation_origins.values())
        .map(BTreeSet::len)
        .sum::<usize>();
    MergeAccounting {
        consumed_candidates: receipts
            .discoveries()
            .iter()
            .map(|receipt| receipt.candidates().len() as u64)
            .sum(),
        consumed_discoveries: receipts.discoveries().len() as u64,
        consumed_resolutions: receipts.resolutions().len() as u64,
        consumed_keep_distinct: receipts.keep_distinct().len() as u64,
        result_entities: counts.target_entities + counts.added_entities,
        result_observations: counts.target_observations + counts.added_observations,
        result_relations: counts.target_relations + counts.added_relations,
        retained_origins: retained_origins as u64,
    }
}

/// Independent materialized validation for semantic planner failures exercised
/// by the adversarial corpus.
pub fn reference_merge_error(
    target: &MemorySnapshot,
    source: &MemorySnapshot,
    receipts: &PlanningReceipts,
    policy: &MergePolicy,
) -> Option<MergeErrorCode> {
    let discoveries = receipts
        .discoveries()
        .iter()
        .map(|receipt| (receipt.source().address().logical_id().clone(), receipt))
        .collect::<BTreeMap<_, _>>();
    if discoveries.len() != receipts.discoveries().len()
        || discoveries.len() != source.entities().len()
        || receipts
            .discoveries()
            .iter()
            .any(|receipt| receipt.policy_generation() != policy.identity_policy_generation())
    {
        return Some(MergeErrorCode::DiscoveryIncomplete);
    }
    let resolutions = receipts
        .resolutions()
        .iter()
        .map(|receipt| {
            (
                (
                    receipt.target().address().clone(),
                    receipt.source().address().clone(),
                ),
                receipt,
            )
        })
        .collect::<BTreeMap<_, _>>();
    if resolutions.len() != receipts.resolutions().len() {
        return Some(MergeErrorCode::StaleReceipt);
    }
    let keep_by_source = receipts
        .keep_distinct()
        .iter()
        .map(|receipt| (receipt.source().address().clone(), receipt))
        .collect::<BTreeMap<_, _>>();
    let target_ids = target
        .entities()
        .iter()
        .map(|record| record.address().logical_id().as_str().to_string())
        .collect::<BTreeSet<_>>();
    let mut qualified_ids = BTreeSet::new();
    let mut coalesced_targets = BTreeSet::new();
    let mut entity_results = BTreeMap::new();

    for record in source.entities() {
        let Some(discovery) = discoveries.get(record.address().logical_id()) else {
            return Some(MergeErrorCode::DiscoveryIncomplete);
        };
        let result = if discovery.candidates().is_empty() {
            ResultObjectId::Qualified(QualifiedObjectId::derive(
                record.address().space(),
                record.address().logical_id(),
            ))
        } else if let Some(keep) = keep_by_source.get(record.address()) {
            if !keep.matches_discovery(discovery) {
                return Some(MergeErrorCode::StaleReceipt);
            }
            ResultObjectId::Qualified(QualifiedObjectId::derive(
                record.address().space(),
                record.address().logical_id(),
            ))
        } else {
            if discovery.truncated() {
                return Some(MergeErrorCode::DiscoveryIncomplete);
            }
            let mut same = None;
            for candidate in discovery.candidates() {
                let key = (
                    candidate.target().address().clone(),
                    candidate.source().address().clone(),
                );
                let Some(resolution) = resolutions.get(&key) else {
                    return Some(MergeErrorCode::DiscoveryIncomplete);
                };
                if !resolution.matches_candidate(candidate) {
                    return Some(MergeErrorCode::StaleReceipt);
                }
                if resolution.decision() == IdentityDecision::Same
                    && same.replace(*resolution).is_some()
                {
                    return Some(MergeErrorCode::ConflictingResolution);
                }
            }
            if let Some(resolution) = same {
                if !coalesced_targets.insert(resolution.target().address().clone()) {
                    return Some(MergeErrorCode::ManyToOne);
                }
                ResultObjectId::Target(resolution.target().address().logical_id().clone())
            } else {
                ResultObjectId::Qualified(QualifiedObjectId::derive(
                    record.address().space(),
                    record.address().logical_id(),
                ))
            }
        };
        if let ResultObjectId::Qualified(qualified) = &result {
            if target_ids.contains(qualified.as_str())
                || !qualified_ids.insert(qualified.as_str().to_string())
            {
                return Some(MergeErrorCode::QualifiedIdCollision);
            }
        }
        entity_results.insert(record.address().logical_id().clone(), result);
    }

    let mut target_relations = BTreeSet::new();
    for record in target.relations() {
        let key = (
            record.subject().logical_id().clone(),
            record.predicate().to_string(),
            record.object().logical_id().clone(),
            record.lifecycle(),
        );
        if !target_relations.insert(key) {
            return Some(MergeErrorCode::DuplicateInput);
        }
    }
    for record in source.relations() {
        let Some(subject) = entity_results.get(record.subject().logical_id()) else {
            return Some(MergeErrorCode::DanglingEndpoint);
        };
        let Some(object) = entity_results.get(record.object().logical_id()) else {
            return Some(MergeErrorCode::DanglingEndpoint);
        };
        if subject == object && !policy.allows_self_loop(record.predicate()) {
            return Some(MergeErrorCode::ThreeWayConflict);
        }
    }
    None
}

#[derive(Clone)]
struct ReferenceEntity {
    result: ResultObjectId,
    record: EntityRecord,
    origins: BTreeSet<OriginContribution>,
}

#[derive(Clone)]
struct ReferenceObservation {
    result: ResultObjectId,
    entity: ResultObjectId,
    record: ObservationRecord,
    origins: BTreeSet<OriginContribution>,
}

#[derive(Clone)]
struct ReferenceRelation {
    result: ResultObjectId,
    subject: ResultObjectId,
    object: ResultObjectId,
    record: RelationRecord,
    origins: BTreeSet<OriginContribution>,
}

/// Bounded, test-only materialized oracle independent from the production
/// planner's incremental token state.
pub fn reference_predicted_hash(actions: &[MergeAction]) -> PredictedResultHash {
    let mut entities = BTreeMap::<String, ReferenceEntity>::new();
    let mut observations = BTreeMap::<String, ReferenceObservation>::new();
    let mut relations = BTreeMap::<String, ReferenceRelation>::new();

    for action in actions {
        match action {
            MergeAction::KeepTargetEntity { target } => {
                let result = ResultObjectId::Target(target.address().logical_id().clone());
                entities.insert(
                    result.as_str().to_string(),
                    ReferenceEntity {
                        result,
                        record: target.clone(),
                        origins: target.origins().clone(),
                    },
                );
            }
            MergeAction::ApplyEntityDisposition {
                source,
                disposition,
            } => match disposition {
                openmemory_merge::planner::EntityDisposition::Coalesce { target, .. } => {
                    entities
                        .get_mut(target.logical_id().as_str())
                        .unwrap()
                        .origins
                        .extend(source.origins().iter().cloned());
                }
                openmemory_merge::planner::EntityDisposition::AddDistinct { result, .. } => {
                    let result = ResultObjectId::Qualified(result.clone());
                    entities.insert(
                        result.as_str().to_string(),
                        ReferenceEntity {
                            result,
                            record: source.clone(),
                            origins: source.origins().clone(),
                        },
                    );
                }
            },
            MergeAction::KeepTargetObservation {
                target,
                result_entity,
            } => {
                let result = ResultObjectId::Target(target.address().logical_id().clone());
                observations.insert(
                    result.as_str().to_string(),
                    ReferenceObservation {
                        result,
                        entity: result_entity.clone(),
                        record: target.clone(),
                        origins: target.origins().clone(),
                    },
                );
            }
            MergeAction::AddObservation {
                source,
                result,
                result_entity,
            } => {
                let result = ResultObjectId::Qualified(result.clone());
                observations.insert(
                    result.as_str().to_string(),
                    ReferenceObservation {
                        result,
                        entity: result_entity.clone(),
                        record: source.clone(),
                        origins: source.origins().clone(),
                    },
                );
            }
            MergeAction::MergeObservationOrigin { source, target, .. } => {
                observations
                    .get_mut(target.as_str())
                    .unwrap()
                    .origins
                    .extend(source.origins().iter().cloned());
            }
            MergeAction::KeepTargetRelation {
                target,
                result_subject,
                result_object,
            } => {
                let result = ResultObjectId::Target(target.address().logical_id().clone());
                relations.insert(
                    result.as_str().to_string(),
                    ReferenceRelation {
                        result,
                        subject: result_subject.clone(),
                        object: result_object.clone(),
                        record: target.clone(),
                        origins: target.origins().clone(),
                    },
                );
            }
            MergeAction::AddRelation {
                source,
                result,
                result_subject,
                result_object,
            } => {
                let result = ResultObjectId::Qualified(result.clone());
                relations.insert(
                    result.as_str().to_string(),
                    ReferenceRelation {
                        result,
                        subject: result_subject.clone(),
                        object: result_object.clone(),
                        record: source.clone(),
                        origins: source.origins().clone(),
                    },
                );
            }
            MergeAction::MergeRelationOrigin { source, target, .. } => {
                relations
                    .get_mut(target.as_str())
                    .unwrap()
                    .origins
                    .extend(source.origins().iter().cloned());
            }
        }
    }

    let mut tokens = BTreeSet::new();
    for entity in entities.values() {
        tokens.insert(ReferenceToken::new(
            1,
            reference_entity_projection(&entity.result, &entity.record),
        ));
        add_origin_tokens(&mut tokens, 2, &entity.result, &entity.origins);
    }
    for observation in observations.values() {
        tokens.insert(ReferenceToken::new(
            3,
            reference_observation_projection(
                &observation.result,
                &observation.entity,
                &observation.record,
            ),
        ));
        add_origin_tokens(&mut tokens, 4, &observation.result, &observation.origins);
    }
    for relation in relations.values() {
        tokens.insert(ReferenceToken::new(
            5,
            reference_relation_projection(
                &relation.result,
                &relation.subject,
                &relation.object,
                &relation.record,
            ),
        ));
        add_origin_tokens(&mut tokens, 6, &relation.result, &relation.origins);
    }
    let mut hash = ReferenceHasher::new(b"openmemory/predicted-result/v1");
    hash.usize(tokens.len());
    for token in tokens {
        hash.u16(token.kind);
        hash.hash(&token.value);
    }
    PredictedResultHash::from_bytes(hash.finish())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ReferenceToken {
    kind: u16,
    value: [u8; 32],
}

impl ReferenceToken {
    fn new(kind: u16, value: [u8; 32]) -> Self {
        Self { kind, value }
    }
}

fn add_origin_tokens(
    tokens: &mut BTreeSet<ReferenceToken>,
    kind: u16,
    result: &ResultObjectId,
    origins: &BTreeSet<OriginContribution>,
) {
    for origin in origins {
        let mut hash = ReferenceHasher::new(b"openmemory/result-origin/v1");
        hash.u16(kind);
        encode_result_id(&mut hash, result);
        encode_origin(&mut hash, origin);
        tokens.insert(ReferenceToken::new(kind, hash.finish()));
    }
}

fn reference_entity_projection(result: &ResultObjectId, record: &EntityRecord) -> [u8; 32] {
    let mut hash = ReferenceHasher::new(b"openmemory/result-entity-projection/v1");
    encode_result_id(&mut hash, result);
    hash.text(&record.revision().to_string());
    hash.u16(record.lifecycle() as u16);
    hash.text(record.label());
    hash.optional_text(record.kind());
    hash.optional_text(record.lineage());
    hash.usize(record.aliases().len());
    for alias in record.aliases() {
        hash.text(alias);
    }
    hash.usize(record.identifiers().len());
    for identifier in record.identifiers() {
        hash.text(identifier.namespace());
        hash.text(identifier.value());
        match identifier.trust() {
            AssertionTrust::Claimed => hash.u16(1),
            AssertionTrust::SourceVerified {
                source_snapshot,
                verifier_version,
                resolver_generation,
            } => {
                hash.u16(2);
                hash.text(&source_snapshot.to_string());
                hash.text(verifier_version);
                hash.u64(*resolver_generation);
            }
        }
    }
    hash.usize(record.properties().len());
    for (name, value) in record.properties() {
        hash.text(name);
        hash.text(value);
    }
    hash.finish()
}

fn reference_observation_projection(
    result: &ResultObjectId,
    entity: &ResultObjectId,
    record: &ObservationRecord,
) -> [u8; 32] {
    let mut hash = ReferenceHasher::new(b"openmemory/result-observation-projection/v1");
    encode_result_id(&mut hash, result);
    encode_result_id(&mut hash, entity);
    hash.text(&record.revision().to_string());
    hash.u16(record.lifecycle() as u16);
    hash.text(record.content());
    hash.finish()
}

fn reference_relation_projection(
    result: &ResultObjectId,
    subject: &ResultObjectId,
    object: &ResultObjectId,
    record: &RelationRecord,
) -> [u8; 32] {
    let mut hash = ReferenceHasher::new(b"openmemory/result-relation-projection/v1");
    encode_result_id(&mut hash, result);
    encode_result_id(&mut hash, subject);
    hash.text(record.predicate());
    encode_result_id(&mut hash, object);
    hash.text(&record.revision().to_string());
    hash.u16(record.lifecycle() as u16);
    hash.finish()
}

fn encode_result_id(hash: &mut ReferenceHasher, value: &ResultObjectId) {
    match value {
        ResultObjectId::Target(value) => {
            hash.u16(1);
            hash.text(value.as_str());
        }
        ResultObjectId::Qualified(value) => {
            hash.u16(2);
            hash.text(value.as_str());
        }
    }
}

fn encode_origin(hash: &mut ReferenceHasher, value: &OriginContribution) {
    hash.text(&value.origin().space().to_string());
    hash.text(value.origin().logical_id().as_str());
    hash.text(&value.revision().to_string());
}

struct ReferenceHasher(blake3::Hasher);

impl ReferenceHasher {
    fn new(domain: &[u8]) -> Self {
        let mut value = Self(blake3::Hasher::new());
        value.bytes(b"openmemory/canonical/v1");
        value.bytes(domain);
        value
    }

    fn bytes(&mut self, value: &[u8]) {
        self.0
            .update(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
        self.0.update(value);
    }

    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn u16(&mut self, value: u16) {
        self.0.update(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(&value.to_le_bytes());
    }

    fn usize(&mut self, value: usize) {
        self.u64(u64::try_from(value).unwrap_or(u64::MAX));
    }

    fn optional_text(&mut self, value: Option<&str>) {
        self.0.update(&[u8::from(value.is_some())]);
        if let Some(value) = value {
            self.text(value);
        }
    }

    fn hash(&mut self, value: &[u8; 32]) {
        self.bytes(value);
    }

    fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}
