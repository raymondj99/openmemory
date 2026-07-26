//! Complete, contribution-preserving directional merge planning.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use openmemory_core::space::{RevisionId, SpaceId};
use serde::{Deserialize, Serialize};

use crate::canonical::{CanonicalObservation, CanonicalRelation, CanonicalSpaceSnapshot};
use crate::error::{MergeError, MergeResult};
use crate::hash::{CanonicalHasher, PlanHash, ReceiptHash, SnapshotHash};
use crate::identity::{IdentityCandidate, IdentityDecision, IdentityResolutionReceipt};

pub const MERGE_PLAN_VERSION: u32 = 1;

/// Complete bounded candidate page supplied to planning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSet {
    pub candidates: Vec<IdentityCandidate>,
    pub truncated: bool,
    pub truncation_acknowledged: bool,
}

impl CandidateSet {
    pub fn complete(mut candidates: Vec<IdentityCandidate>) -> MergeResult<Self> {
        candidates.sort_by(|left, right| left.candidate_id.cmp(&right.candidate_id));
        let set = Self {
            candidates,
            truncated: false,
            truncation_acknowledged: false,
        };
        set.validate()?;
        Ok(set)
    }

    fn validate(&self) -> MergeResult<()> {
        if self.truncated && !self.truncation_acknowledged {
            return Err(MergeError::CandidatePageTruncated);
        }
        let mut ids = BTreeSet::new();
        let mut pairs = BTreeSet::new();
        for candidate in &self.candidates {
            if !ids.insert(candidate.candidate_id.as_str())
                || !pairs.insert((
                    &candidate.left.address.logical_id,
                    &candidate.right.address.logical_id,
                ))
            {
                return Err(MergeError::CandidateCoverage);
            }
        }
        Ok(())
    }
}

/// Planner policy generation and conservative collision behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePolicy {
    pub generation: u64,
    pub keep_undetermined_distinct: bool,
}

impl Default for MergePolicy {
    fn default() -> Self {
        Self {
            generation: 1,
            keep_undetermined_distinct: true,
        }
    }
}

/// Full immutable planner request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    pub source: CanonicalSpaceSnapshot,
    pub target: CanonicalSpaceSnapshot,
    pub lineage_base: Option<CanonicalSpaceSnapshot>,
    pub candidates: CandidateSet,
    pub resolutions: Vec<IdentityResolutionReceipt>,
    pub policy: MergePolicy,
}

/// Target movement guard recorded in a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedTargetVersion {
    pub semantic_generation: u64,
    pub snapshot_hash: SnapshotHash,
}

/// Why an unresolved/not-same source identity remains separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistinctReason {
    NoCandidate,
    ProvenDifferent,
    Undetermined,
}

/// Exactly one disposition for every source entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum EntityDisposition {
    Coalesce {
        source_id: String,
        target_id: String,
        receipt_hash: ReceiptHash,
    },
    AddDistinct {
        source_id: String,
        result_id: String,
        reason: DistinctReason,
    },
}

/// One source observation disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum ObservationAction {
    AddDistinct {
        source_id: String,
        result_id: String,
        result_revision_id: RevisionId,
        mapped_entity_id: String,
    },
    ExactNoOp {
        source_id: String,
        target_id: String,
    },
}

/// One source relation assertion disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum RelationAction {
    AddDistinct {
        source_id: String,
        result_id: String,
        result_revision_id: RevisionId,
        mapped_from: String,
        mapped_to: String,
    },
    ExactNoOp {
        source_id: String,
        target_id: String,
    },
}

/// Typed conflict reserved for lineage-aware field resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeConflict {
    pub object_kind: String,
    pub source_id: String,
    pub target_id: String,
    pub field: String,
}

/// Complete source/target/candidate accounting proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeAccounting {
    pub target_entities_retained: usize,
    pub target_observations_retained: usize,
    pub target_relations_retained: usize,
    pub source_entities_accounted: usize,
    pub source_observations_accounted: usize,
    pub source_relations_accounted: usize,
    pub candidates_consumed: usize,
    pub entities_added: usize,
    pub entities_coalesced: usize,
    pub observations_added: usize,
    pub observations_exact: usize,
    pub relations_added: usize,
    pub relations_exact: usize,
    pub contributions_added: usize,
    pub complete: bool,
}

/// Deterministic directional plan. It contains no filesystem paths or authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePlan {
    pub version: u32,
    pub source_space: SpaceId,
    pub target_space: SpaceId,
    pub source_snapshot_hash: SnapshotHash,
    pub target_snapshot_hash: SnapshotHash,
    pub expected_target_version: ExpectedTargetVersion,
    pub policy_generation: u64,
    pub dispositions: Vec<EntityDisposition>,
    pub observation_actions: Vec<ObservationAction>,
    pub relation_actions: Vec<RelationAction>,
    pub resolution_receipts: Vec<ReceiptHash>,
    pub conflicts: Vec<MergeConflict>,
    pub accounting: MergeAccounting,
    pub predicted_result_hash: SnapshotHash,
    pub plan_hash: PlanHash,
}

type EntityResolution = (
    Vec<EntityDisposition>,
    BTreeMap<String, String>,
    Vec<ReceiptHash>,
);

/// Validate all inputs, account every object, and predict the exact result.
pub fn plan_merge(request: &PlanRequest) -> MergeResult<MergePlan> {
    request.source.validate()?;
    request.target.validate()?;
    if request.source.space_id == request.target.space_id {
        return Err(MergeError::SameSpace(request.source.space_id));
    }
    if request.policy.generation == 0 {
        return Err(MergeError::InvalidField {
            field: "merge_policy_generation",
        });
    }
    if let Some(base) = &request.lineage_base {
        base.validate()?;
    }
    request.candidates.validate()?;

    let (dispositions, endpoint_map, receipts) = resolve_entities(request)?;
    let observation_actions = plan_observations(request, &endpoint_map)?;
    let relation_actions = plan_relations(request, &endpoint_map)?;
    let accounting = accounting(
        request,
        &dispositions,
        &observation_actions,
        &relation_actions,
    );
    if !accounting.complete {
        return Err(MergeError::IncompleteAccounting);
    }

    let mut plan = MergePlan {
        version: MERGE_PLAN_VERSION,
        source_space: request.source.space_id,
        target_space: request.target.space_id,
        source_snapshot_hash: request.source.snapshot_hash,
        target_snapshot_hash: request.target.snapshot_hash,
        expected_target_version: ExpectedTargetVersion {
            semantic_generation: request.target.semantic_generation,
            snapshot_hash: request.target.snapshot_hash,
        },
        policy_generation: request.policy.generation,
        dispositions,
        observation_actions,
        relation_actions,
        resolution_receipts: receipts,
        conflicts: Vec::new(),
        accounting,
        predicted_result_hash: SnapshotHash::from_bytes([0; 32]),
        plan_hash: PlanHash::from_bytes([0; 32]),
    };
    let result = materialize_unchecked(&request.target, &request.source, &plan)?;
    plan.predicted_result_hash = result.snapshot_hash;
    plan.plan_hash = hash_plan(&plan);
    Ok(plan)
}

fn resolve_entities(request: &PlanRequest) -> MergeResult<EntityResolution> {
    if request.resolutions.len() != request.candidates.candidates.len() {
        return Err(MergeError::CandidateResolutionCount);
    }
    let candidates = request
        .candidates
        .candidates
        .iter()
        .map(|candidate| (candidate.candidate_id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    let mut decisions = BTreeMap::new();
    let mut receipt_hashes = Vec::with_capacity(request.resolutions.len());
    for receipt in &request.resolutions {
        let candidate = candidates
            .get(receipt.candidate_id.as_str())
            .ok_or(MergeError::CandidateCoverage)?;
        receipt.validate(candidate)?;
        if decisions
            .insert(receipt.candidate_id.as_str(), receipt)
            .is_some()
        {
            return Err(MergeError::CandidateResolutionCount);
        }
        receipt_hashes.push(receipt.receipt_hash);
    }
    receipt_hashes.sort_unstable();

    for candidate in &request.candidates.candidates {
        if candidate.left.address.space_id != request.target.space_id
            || candidate.right.address.space_id != request.source.space_id
        {
            return Err(MergeError::CandidateCoverage);
        }
        let target = request
            .target
            .entity(&candidate.left.address.logical_id)
            .ok_or(MergeError::CandidateCoverage)?;
        let source = request
            .source
            .entity(&candidate.right.address.logical_id)
            .ok_or(MergeError::CandidateCoverage)?;
        if target.revision_id != candidate.left.revision_id
            || target.semantic_hash != candidate.left.semantic_hash
            || source.revision_id != candidate.right.revision_id
            || source.semantic_hash != candidate.right.semantic_hash
        {
            return Err(MergeError::StaleIdentityReceipt);
        }
    }

    let mut by_source: BTreeMap<&str, Vec<(&IdentityCandidate, &IdentityResolutionReceipt)>> =
        BTreeMap::new();
    for candidate in &request.candidates.candidates {
        let receipt = decisions
            .get(candidate.candidate_id.as_str())
            .ok_or(MergeError::CandidateResolutionCount)?;
        by_source
            .entry(&candidate.right.address.logical_id)
            .or_default()
            .push((candidate, *receipt));
    }

    let mut absorbed_targets = BTreeSet::new();
    let mut dispositions = Vec::with_capacity(request.source.entities.len());
    let mut endpoint_map = BTreeMap::new();
    for source in &request.source.entities {
        let related = by_source
            .get(source.logical_id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let same = related
            .iter()
            .filter(|(_, receipt)| receipt.decision == IdentityDecision::Same)
            .collect::<Vec<_>>();
        if same.len() > 1 {
            return Err(MergeError::SourceIdentityAmbiguous);
        }
        if let Some((candidate, receipt)) = same.first().copied() {
            let target_id = candidate.left.address.logical_id.clone();
            if !absorbed_targets.insert(target_id.clone()) {
                return Err(MergeError::ManyToOneCoalescence);
            }
            endpoint_map.insert(source.logical_id.clone(), target_id.clone());
            dispositions.push(EntityDisposition::Coalesce {
                source_id: source.logical_id.clone(),
                target_id,
                receipt_hash: receipt.receipt_hash,
            });
        } else {
            let reason = if related.is_empty() {
                DistinctReason::NoCandidate
            } else if related
                .iter()
                .any(|(_, receipt)| receipt.decision == IdentityDecision::Undetermined)
            {
                if !request.policy.keep_undetermined_distinct {
                    return Err(MergeError::UnresolvedConflicts);
                }
                DistinctReason::Undetermined
            } else {
                DistinctReason::ProvenDifferent
            };
            let result_id = source_qualified_id(
                b"openmemory/imported-entity-id/v1",
                request.source.space_id,
                &source.logical_id,
            );
            if request.target.entity(&result_id).is_some()
                || endpoint_map.values().any(|existing| existing == &result_id)
            {
                return Err(MergeError::GeneratedIdCollision);
            }
            endpoint_map.insert(source.logical_id.clone(), result_id.clone());
            dispositions.push(EntityDisposition::AddDistinct {
                source_id: source.logical_id.clone(),
                result_id,
                reason,
            });
        }
    }
    dispositions.sort_by(|left, right| disposition_source(left).cmp(disposition_source(right)));
    if endpoint_map.len() != request.source.entities.len() {
        return Err(MergeError::IncompleteAccounting);
    }
    Ok((dispositions, endpoint_map, receipt_hashes))
}

fn disposition_source(disposition: &EntityDisposition) -> &str {
    match disposition {
        EntityDisposition::Coalesce { source_id, .. }
        | EntityDisposition::AddDistinct { source_id, .. } => source_id,
    }
}

fn plan_observations(
    request: &PlanRequest,
    endpoint_map: &BTreeMap<String, String>,
) -> MergeResult<Vec<ObservationAction>> {
    let mut actions = Vec::with_capacity(request.source.observations.len());
    let mut generated = BTreeSet::new();
    for source in &request.source.observations {
        let mapped_entity = endpoint_map
            .get(&source.entity_id)
            .ok_or(MergeError::IncompleteAccounting)?;
        if let Some(target) = request.target.observations.iter().find(|target| {
            target.entity_id == *mapped_entity
                && target.content == source.content
                && target.concepts == source.concepts
                && target.source == source.source
                && target.lifecycle == source.lifecycle
        }) {
            actions.push(ObservationAction::ExactNoOp {
                source_id: source.logical_id.clone(),
                target_id: target.logical_id.clone(),
            });
            continue;
        }
        let result_id = source_qualified_id(
            b"openmemory/imported-observation-id/v1",
            request.source.space_id,
            &source.logical_id,
        );
        if request
            .target
            .observations
            .iter()
            .any(|item| item.logical_id == result_id)
            || !generated.insert(result_id.clone())
        {
            return Err(MergeError::GeneratedIdCollision);
        }
        actions.push(ObservationAction::AddDistinct {
            source_id: source.logical_id.clone(),
            result_revision_id: derived_revision(
                b"openmemory/imported-observation-revision/v1",
                request.source.space_id,
                &source.logical_id,
            )?,
            result_id,
            mapped_entity_id: mapped_entity.clone(),
        });
    }
    Ok(actions)
}

fn plan_relations(
    request: &PlanRequest,
    endpoint_map: &BTreeMap<String, String>,
) -> MergeResult<Vec<RelationAction>> {
    let mut actions = Vec::with_capacity(request.source.relations.len());
    let mut generated = BTreeSet::new();
    for source in &request.source.relations {
        let mapped_from = endpoint_map
            .get(&source.from_entity)
            .ok_or(MergeError::IncompleteAccounting)?;
        let mapped_to = endpoint_map
            .get(&source.to_entity)
            .ok_or(MergeError::IncompleteAccounting)?;
        if let Some(target) = request.target.relations.iter().find(|target| {
            target.from_entity == *mapped_from
                && target.to_entity == *mapped_to
                && target.relation_type == source.relation_type
                && target.weight.to_bits() == source.weight.to_bits()
                && target.source == source.source
                && target.evidence == source.evidence
                && target.lifecycle == source.lifecycle
        }) {
            actions.push(RelationAction::ExactNoOp {
                source_id: source.logical_id.clone(),
                target_id: target.logical_id.clone(),
            });
            continue;
        }
        let result_id = source_qualified_id(
            b"openmemory/imported-relation-id/v1",
            request.source.space_id,
            &source.logical_id,
        );
        if request
            .target
            .relations
            .iter()
            .any(|item| item.logical_id == result_id)
            || !generated.insert(result_id.clone())
        {
            return Err(MergeError::GeneratedIdCollision);
        }
        actions.push(RelationAction::AddDistinct {
            source_id: source.logical_id.clone(),
            result_revision_id: derived_revision(
                b"openmemory/imported-relation-revision/v1",
                request.source.space_id,
                &source.logical_id,
            )?,
            result_id,
            mapped_from: mapped_from.clone(),
            mapped_to: mapped_to.clone(),
        });
    }
    Ok(actions)
}

fn accounting(
    request: &PlanRequest,
    dispositions: &[EntityDisposition],
    observations: &[ObservationAction],
    relations: &[RelationAction],
) -> MergeAccounting {
    let entities_added = dispositions
        .iter()
        .filter(|item| matches!(item, EntityDisposition::AddDistinct { .. }))
        .count();
    let entities_coalesced = dispositions.len() - entities_added;
    let observations_added = observations
        .iter()
        .filter(|item| matches!(item, ObservationAction::AddDistinct { .. }))
        .count();
    let observations_exact = observations.len() - observations_added;
    let relations_added = relations
        .iter()
        .filter(|item| matches!(item, RelationAction::AddDistinct { .. }))
        .count();
    let relations_exact = relations.len() - relations_added;
    let complete = dispositions.len() == request.source.entities.len()
        && observations.len() == request.source.observations.len()
        && relations.len() == request.source.relations.len()
        && request.resolutions.len() == request.candidates.candidates.len();
    MergeAccounting {
        target_entities_retained: request.target.entities.len(),
        target_observations_retained: request.target.observations.len(),
        target_relations_retained: request.target.relations.len(),
        source_entities_accounted: dispositions.len(),
        source_observations_accounted: observations.len(),
        source_relations_accounted: relations.len(),
        candidates_consumed: request.resolutions.len(),
        entities_added,
        entities_coalesced,
        observations_added,
        observations_exact,
        relations_added,
        relations_exact,
        contributions_added: request.source.entities.len()
            + request.source.observations.len()
            + request.source.relations.len(),
        complete,
    }
}

/// Revalidate a plan and reproduce the predicted immutable target snapshot.
pub fn materialize_merge(
    target: &CanonicalSpaceSnapshot,
    source: &CanonicalSpaceSnapshot,
    plan: &MergePlan,
) -> MergeResult<CanonicalSpaceSnapshot> {
    target.validate()?;
    source.validate()?;
    if hash_plan(plan) != plan.plan_hash {
        return Err(MergeError::PlanHashMismatch);
    }
    if target.space_id != plan.target_space
        || source.space_id != plan.source_space
        || target.snapshot_hash != plan.target_snapshot_hash
        || source.snapshot_hash != plan.source_snapshot_hash
        || target.semantic_generation != plan.expected_target_version.semantic_generation
    {
        return Err(MergeError::InputMoved);
    }
    let result = materialize_unchecked(target, source, plan)?;
    if result.snapshot_hash != plan.predicted_result_hash {
        return Err(MergeError::PredictedResultMismatch);
    }
    validate_materialized_accounting(target, source, &result, plan)?;
    Ok(result)
}

fn materialize_unchecked(
    target: &CanonicalSpaceSnapshot,
    source: &CanonicalSpaceSnapshot,
    plan: &MergePlan,
) -> MergeResult<CanonicalSpaceSnapshot> {
    let mut entities = target.entities.clone();
    for disposition in &plan.dispositions {
        match disposition {
            EntityDisposition::Coalesce {
                source_id,
                target_id,
                ..
            } => {
                let source_entity = source
                    .entity(source_id)
                    .ok_or(MergeError::IncompleteAccounting)?;
                let target_entity = entities
                    .iter_mut()
                    .find(|entity| entity.logical_id == *target_id)
                    .ok_or(MergeError::IncompleteAccounting)?;
                for (origin, contribution) in &source_entity.contributions {
                    if target_entity
                        .contributions
                        .insert(origin.clone(), contribution.clone())
                        .is_some()
                    {
                        return Err(MergeError::IncompleteAccounting);
                    }
                }
            }
            EntityDisposition::AddDistinct {
                source_id,
                result_id,
                ..
            } => {
                let mut entity = source
                    .entity(source_id)
                    .ok_or(MergeError::IncompleteAccounting)?
                    .clone();
                entity.logical_id.clone_from(result_id);
                entity.add_local_contribution(target.space_id)?;
                entities.push(entity);
            }
        }
    }

    let mut observations = target.observations.clone();
    for action in &plan.observation_actions {
        match action {
            ObservationAction::ExactNoOp {
                source_id,
                target_id,
            } => {
                let source_item = source
                    .observations
                    .iter()
                    .find(|item| item.logical_id == *source_id)
                    .ok_or(MergeError::IncompleteAccounting)?;
                let target_item = observations
                    .iter_mut()
                    .find(|item| item.logical_id == *target_id)
                    .ok_or(MergeError::IncompleteAccounting)?;
                union_origins(&mut target_item.origins, &source_item.origins)?;
            }
            ObservationAction::AddDistinct {
                source_id,
                result_id,
                result_revision_id,
                mapped_entity_id,
            } => {
                let source_item = source
                    .observations
                    .iter()
                    .find(|item| item.logical_id == *source_id)
                    .ok_or(MergeError::IncompleteAccounting)?;
                let mut result = CanonicalObservation::new_complete(
                    target.space_id,
                    result_id.clone(),
                    mapped_entity_id.clone(),
                    *result_revision_id,
                    source_item.content.clone(),
                    source_item.observed_at,
                    source_item.valid_from,
                    source_item.valid_until,
                    source_item.confidence_bits,
                    source_item.concepts.clone(),
                    source_item.source_files.clone(),
                    source_item.source.clone(),
                    source_item.memory_tier.clone(),
                    source_item.title.clone(),
                    source_item.summary.clone(),
                    source_item.importance_bits,
                    source_item.source_kind.clone(),
                    source_item.lifecycle,
                )?;
                result.origins.extend(source_item.origins.clone());
                observations.push(result);
            }
        }
    }

    let mut relations = target.relations.clone();
    for action in &plan.relation_actions {
        match action {
            RelationAction::ExactNoOp {
                source_id,
                target_id,
            } => {
                let source_item = source
                    .relations
                    .iter()
                    .find(|item| item.logical_id == *source_id)
                    .ok_or(MergeError::IncompleteAccounting)?;
                let target_item = relations
                    .iter_mut()
                    .find(|item| item.logical_id == *target_id)
                    .ok_or(MergeError::IncompleteAccounting)?;
                union_origins(&mut target_item.origins, &source_item.origins)?;
            }
            RelationAction::AddDistinct {
                source_id,
                result_id,
                result_revision_id,
                mapped_from,
                mapped_to,
            } => {
                let source_item = source
                    .relations
                    .iter()
                    .find(|item| item.logical_id == *source_id)
                    .ok_or(MergeError::IncompleteAccounting)?;
                let mut result = CanonicalRelation::new_complete(
                    target.space_id,
                    result_id.clone(),
                    *result_revision_id,
                    mapped_from.clone(),
                    mapped_to.clone(),
                    source_item.relation_type.clone(),
                    source_item.weight,
                    source_item.valid_from,
                    source_item.valid_until,
                    source_item.source.clone(),
                    source_item.evidence.clone(),
                    source_item.lifecycle,
                )?;
                result.origins.extend(source_item.origins.clone());
                relations.push(result);
            }
        }
    }
    CanonicalSpaceSnapshot::new(
        target.space_id,
        target.semantic_generation.saturating_add(1),
        entities,
        observations,
        relations,
    )
}

fn union_origins<T: Ord + Clone, V: Clone>(
    target: &mut BTreeMap<T, V>,
    source: &BTreeMap<T, V>,
) -> MergeResult<()> {
    for (key, value) in source {
        target.entry(key.clone()).or_insert_with(|| value.clone());
    }
    Ok(())
}

fn validate_materialized_accounting(
    target: &CanonicalSpaceSnapshot,
    source: &CanonicalSpaceSnapshot,
    result: &CanonicalSpaceSnapshot,
    plan: &MergePlan,
) -> MergeResult<()> {
    let expected_entities = target.entities.len() + plan.accounting.entities_added;
    let expected_observations = target.observations.len() + plan.accounting.observations_added;
    let expected_relations = target.relations.len() + plan.accounting.relations_added;
    if result.entities.len() != expected_entities
        || result.observations.len() != expected_observations
        || result.relations.len() != expected_relations
        || plan.accounting.source_entities_accounted != source.entities.len()
        || plan.accounting.source_observations_accounted != source.observations.len()
        || plan.accounting.source_relations_accounted != source.relations.len()
    {
        return Err(MergeError::IncompleteAccounting);
    }
    for target_entity in &target.entities {
        let result_entity = result
            .entity(&target_entity.logical_id)
            .ok_or(MergeError::IncompleteAccounting)?;
        if !target_entity
            .contributions
            .keys()
            .all(|origin| result_entity.contributions.contains_key(origin))
        {
            return Err(MergeError::IncompleteAccounting);
        }
    }
    for relation in &result.relations {
        if result.entity(&relation.from_entity).is_none()
            || result.entity(&relation.to_entity).is_none()
        {
            return Err(MergeError::DanglingRelationEndpoint);
        }
    }
    Ok(())
}

fn source_qualified_id(domain: &'static [u8], space_id: SpaceId, logical_id: &str) -> String {
    let mut hasher = CanonicalHasher::new(domain);
    hasher.string(&space_id.to_string());
    hasher.string(logical_id);
    let bytes = hasher.finish();
    let mut suffix = String::with_capacity(32);
    for byte in &bytes[..16] {
        use std::fmt::Write as _;
        let _ = write!(suffix, "{byte:02x}");
    }
    format!("src:{suffix}")
}

fn derived_revision(
    domain: &'static [u8],
    space_id: SpaceId,
    logical_id: &str,
) -> MergeResult<RevisionId> {
    let mut hasher = CanonicalHasher::new(domain);
    hasher.string(&space_id.to_string());
    hasher.string(logical_id);
    let mut bytes = hasher.finish()[..16].to_vec();
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let uuid = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    );
    RevisionId::from_str(&uuid).map_err(|_| MergeError::InvalidField {
        field: "derived_revision",
    })
}

fn hash_plan(plan: &MergePlan) -> PlanHash {
    let mut hasher = CanonicalHasher::new(b"openmemory/merge-plan/v1");
    hasher.u32(plan.version);
    hasher.string(&plan.source_space.to_string());
    hasher.string(&plan.target_space.to_string());
    hasher.bytes(plan.source_snapshot_hash.as_bytes());
    hasher.bytes(plan.target_snapshot_hash.as_bytes());
    hasher.u64(plan.expected_target_version.semantic_generation);
    hasher.u64(plan.policy_generation);
    hasher.u64(plan.dispositions.len() as u64);
    for disposition in &plan.dispositions {
        match disposition {
            EntityDisposition::Coalesce {
                source_id,
                target_id,
                receipt_hash,
            } => {
                hasher.tag("coalesce");
                hasher.string(source_id);
                hasher.string(target_id);
                hasher.bytes(receipt_hash.as_bytes());
            }
            EntityDisposition::AddDistinct {
                source_id,
                result_id,
                reason,
            } => {
                hasher.tag("add_distinct");
                hasher.string(source_id);
                hasher.string(result_id);
                hasher.tag(match reason {
                    DistinctReason::NoCandidate => "no_candidate",
                    DistinctReason::ProvenDifferent => "proven_different",
                    DistinctReason::Undetermined => "undetermined",
                });
            }
        }
    }
    hash_observation_actions(&mut hasher, &plan.observation_actions);
    hash_relation_actions(&mut hasher, &plan.relation_actions);
    hasher.u64(plan.resolution_receipts.len() as u64);
    for receipt in &plan.resolution_receipts {
        hasher.bytes(receipt.as_bytes());
    }
    hasher.u64(plan.conflicts.len() as u64);
    for conflict in &plan.conflicts {
        hasher.string(&conflict.object_kind);
        hasher.string(&conflict.source_id);
        hasher.string(&conflict.target_id);
        hasher.string(&conflict.field);
    }
    hash_accounting(&mut hasher, &plan.accounting);
    hasher.bytes(plan.predicted_result_hash.as_bytes());
    PlanHash::from_bytes(hasher.finish())
}

fn hash_observation_actions(hasher: &mut CanonicalHasher, actions: &[ObservationAction]) {
    hasher.u64(actions.len() as u64);
    for action in actions {
        match action {
            ObservationAction::AddDistinct {
                source_id,
                result_id,
                result_revision_id,
                mapped_entity_id,
            } => {
                hasher.tag("observation_add");
                hasher.string(source_id);
                hasher.string(result_id);
                hasher.string(&result_revision_id.to_string());
                hasher.string(mapped_entity_id);
            }
            ObservationAction::ExactNoOp {
                source_id,
                target_id,
            } => {
                hasher.tag("observation_exact");
                hasher.string(source_id);
                hasher.string(target_id);
            }
        }
    }
}

fn hash_relation_actions(hasher: &mut CanonicalHasher, actions: &[RelationAction]) {
    hasher.u64(actions.len() as u64);
    for action in actions {
        match action {
            RelationAction::AddDistinct {
                source_id,
                result_id,
                result_revision_id,
                mapped_from,
                mapped_to,
            } => {
                hasher.tag("relation_add");
                hasher.string(source_id);
                hasher.string(result_id);
                hasher.string(&result_revision_id.to_string());
                hasher.string(mapped_from);
                hasher.string(mapped_to);
            }
            RelationAction::ExactNoOp {
                source_id,
                target_id,
            } => {
                hasher.tag("relation_exact");
                hasher.string(source_id);
                hasher.string(target_id);
            }
        }
    }
}

fn hash_accounting(hasher: &mut CanonicalHasher, accounting: &MergeAccounting) {
    for value in [
        accounting.target_entities_retained,
        accounting.target_observations_retained,
        accounting.target_relations_retained,
        accounting.source_entities_accounted,
        accounting.source_observations_accounted,
        accounting.source_relations_accounted,
        accounting.candidates_consumed,
        accounting.entities_added,
        accounting.entities_coalesced,
        accounting.observations_added,
        accounting.observations_exact,
        accounting.relations_added,
        accounting.relations_exact,
        accounting.contributions_added,
    ] {
        hasher.u64(value as u64);
    }
    hasher.bool(accounting.complete);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use crate::canonical::{CanonicalEntity, Lifecycle};
    use crate::identity::{DecisionSource, EntityAddress, EntityRevisionRef, IdentityPacket};

    fn entity(space: SpaceId, id: &str, label: &str) -> crate::canonical::CanonicalEntity {
        CanonicalEntity::new(
            space,
            id.to_string(),
            RevisionId::new(),
            label.to_string(),
            BTreeSet::new(),
            "concept".to_string(),
            BTreeSet::new(),
            String::new(),
        )
        .unwrap()
    }

    fn snapshots() -> (CanonicalSpaceSnapshot, CanonicalSpaceSnapshot) {
        let target_space = SpaceId::new();
        let source_space = SpaceId::new();
        let target = CanonicalSpaceSnapshot::new(
            target_space,
            1,
            vec![entity(target_space, "cerpheus", "cerpheus")],
            vec![],
            vec![],
        )
        .unwrap();
        let source = CanonicalSpaceSnapshot::new(
            source_space,
            1,
            vec![
                entity(source_space, "cerpheus", "cerpheus"),
                entity(source_space, "runtime", "runtime"),
            ],
            vec![],
            vec![CanonicalRelation::new(
                source_space,
                "implemented-by".to_string(),
                RevisionId::new(),
                "cerpheus".to_string(),
                "runtime".to_string(),
                "implemented_by".to_string(),
                1.0,
                "fixture".to_string(),
                BTreeSet::new(),
                Lifecycle::Active,
            )
            .unwrap()],
        )
        .unwrap();
        (target, source)
    }

    fn candidate(
        target: &CanonicalSpaceSnapshot,
        source: &CanonicalSpaceSnapshot,
    ) -> IdentityCandidate {
        let left = target.entity("cerpheus").unwrap();
        let right = source.entity("cerpheus").unwrap();
        let packet = IdentityPacket::new(
            EntityRevisionRef {
                address: EntityAddress::new(target.space_id, left.logical_id.clone()).unwrap(),
                revision_id: left.revision_id,
                semantic_hash: left.semantic_hash,
                controlled_kind: left.controlled_kind.clone(),
            },
            EntityRevisionRef {
                address: EntityAddress::new(source.space_id, right.logical_id.clone()).unwrap(),
                revision_id: right.revision_id,
                semantic_hash: right.semantic_hash,
                controlled_kind: right.controlled_kind.clone(),
            },
            1,
            1,
            BTreeMap::new(),
            Vec::new(),
        )
        .unwrap();
        IdentityCandidate::from_packet("candidate-1".to_string(), &packet).unwrap()
    }

    fn request(decision: IdentityDecision) -> PlanRequest {
        let (target, source) = snapshots();
        let candidate = candidate(&target, &source);
        let receipt = IdentityResolutionReceipt::new(
            &candidate,
            "decision-1".to_string(),
            decision,
            DecisionSource::Human,
            1,
        )
        .unwrap();
        PlanRequest {
            source,
            target,
            lineage_base: None,
            candidates: CandidateSet::complete(vec![candidate]).unwrap(),
            resolutions: vec![receipt],
            policy: MergePolicy::default(),
        }
    }

    #[test]
    fn reviewed_same_preserves_projection_contribution_and_rewires_relation() {
        let request = request(IdentityDecision::Same);
        let source_hash = request.source.snapshot_hash;
        let target_hash = request.target.snapshot_hash;
        let plan = plan_merge(&request).unwrap();
        let result = materialize_merge(&request.target, &request.source, &plan).unwrap();
        assert_eq!(request.source.snapshot_hash, source_hash);
        assert_eq!(request.target.snapshot_hash, target_hash);
        assert_eq!(plan.accounting.entities_coalesced, 1);
        assert_eq!(plan.accounting.entities_added, 1);
        let merged = result.entity("cerpheus").unwrap();
        assert_eq!(merged.label, "cerpheus");
        assert_eq!(merged.contributions.len(), 2);
        let relation = result.relations.last().unwrap();
        assert_eq!(relation.from_entity, "cerpheus");
        assert!(relation.to_entity.starts_with("src:"));
    }

    #[test]
    fn cerpheus_homonym_stays_distinct_without_reviewed_same() {
        let request = request(IdentityDecision::Different);
        let plan = plan_merge(&request).unwrap();
        let result = materialize_merge(&request.target, &request.source, &plan).unwrap();
        assert_eq!(plan.accounting.entities_coalesced, 0);
        assert_eq!(plan.accounting.entities_added, 2);
        assert_eq!(result.entities.len(), 3);
        assert_eq!(result.entity("cerpheus").unwrap().contributions.len(), 1);
    }

    #[test]
    fn receipt_plan_and_input_tampering_fail_closed() {
        let request = request(IdentityDecision::Same);
        let mut missing = request.clone();
        missing.resolutions.clear();
        assert_eq!(
            plan_merge(&missing),
            Err(MergeError::CandidateResolutionCount)
        );
        let mut plan = plan_merge(&request).unwrap();
        plan.accounting.entities_added += 1;
        assert_eq!(
            materialize_merge(&request.target, &request.source, &plan),
            Err(MergeError::PlanHashMismatch)
        );
        let mut moved = request.target.clone();
        moved.semantic_generation += 1;
        assert_eq!(
            materialize_merge(&moved, &request.source, &plan_merge(&request).unwrap()),
            Err(MergeError::InputMoved)
        );
    }
}
