//! Deterministic streaming semantic merge planner and sink protocol.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use openmemory_core::space::{RevisionId, SnapshotId, SpaceId};
use serde::Serialize;

use crate::canonical::CanonicalHasher;
use crate::discovery::DiscoveryReceipt;
use crate::evidence::EntityRevisionRef;
use crate::hash::{
    ActionStreamHash, PlanHash, PredictedResultHash, ReceiptHash, SemanticHash, SnapshotHash,
};
use crate::model::{
    encode_address, encode_entity, encode_observation, encode_origin, encode_relation,
    encode_result_id, encode_space_version, EntityAddress, EntityRecord, LogicalId, ObjectAddress,
    ObservationRecord, OriginContribution, QualifiedObjectId, RelationRecord, ResultObjectId,
    SnapshotHeader, SnapshotSource, SpaceVersion,
};
use crate::receipt::{IdentityDecision, IdentityResolutionReceipt, KeepDistinctReceipt};
use crate::{MergeError, MergeErrorCode, MergeResult};

const MERGE_PLAN_VERSION: u16 = 1;
const MAX_ACTIONS: usize = 3_000_000;

/// Pure merge policy already normalized by the product owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergePolicy {
    version: u16,
    identity_policy_generation: u64,
    allowed_self_loop_predicates: BTreeSet<String>,
}

impl MergePolicy {
    pub fn new(version: u16, identity_policy_generation: u64) -> MergeResult<Self> {
        if version == 0 || identity_policy_generation == 0 {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "merge policy and identity policy generations must be positive",
            ));
        }
        Ok(Self {
            version,
            identity_policy_generation,
            allowed_self_loop_predicates: BTreeSet::new(),
        })
    }

    pub fn with_allowed_self_loops(
        mut self,
        predicates: impl IntoIterator<Item = String>,
    ) -> MergeResult<Self> {
        let predicates = predicates.into_iter().collect::<BTreeSet<_>>();
        if predicates.len() > 64
            || predicates.iter().any(|predicate| {
                predicate.is_empty()
                    || predicate.len() > 128
                    || predicate.chars().any(char::is_control)
            })
        {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "self-loop predicate policy exceeds bounds",
            ));
        }
        self.allowed_self_loop_predicates = predicates;
        Ok(self)
    }

    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    #[must_use]
    pub const fn identity_policy_generation(&self) -> u64 {
        self.identity_policy_generation
    }

    #[must_use]
    pub fn allows_self_loop(&self, predicate: &str) -> bool {
        self.allowed_self_loop_predicates.contains(predicate)
    }
}

/// Exact durable receipts consumed by one planning run.
#[derive(Debug, Clone, Default)]
pub struct PlanningReceipts {
    discoveries: Vec<DiscoveryReceipt>,
    resolutions: Vec<IdentityResolutionReceipt>,
    keep_distinct: Vec<KeepDistinctReceipt>,
}

impl PlanningReceipts {
    #[must_use]
    pub fn new(
        discoveries: Vec<DiscoveryReceipt>,
        resolutions: Vec<IdentityResolutionReceipt>,
        keep_distinct: Vec<KeepDistinctReceipt>,
    ) -> Self {
        Self {
            discoveries,
            resolutions,
            keep_distinct,
        }
    }

    #[must_use]
    pub fn discoveries(&self) -> &[DiscoveryReceipt] {
        &self.discoveries
    }

    #[must_use]
    pub fn resolutions(&self) -> &[IdentityResolutionReceipt] {
        &self.resolutions
    }

    #[must_use]
    pub fn keep_distinct(&self) -> &[KeepDistinctReceipt] {
        &self.keep_distinct
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DistinctReason {
    NoCandidate,
    ReviewedDifferent,
    KeepDistinct,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntityDisposition {
    Coalesce {
        source: EntityAddress,
        target: EntityAddress,
        receipt: ReceiptHash,
    },
    AddDistinct {
        source: EntityAddress,
        result: QualifiedObjectId,
        reason: DistinctReason,
        receipt: Option<ReceiptHash>,
    },
}

/// Semantic action stream. Full source records are present so a later bounded
/// materializer never needs a live source handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeAction {
    KeepTargetEntity {
        target: EntityRecord,
    },
    ApplyEntityDisposition {
        source: EntityRecord,
        disposition: EntityDisposition,
    },
    KeepTargetObservation {
        target: ObservationRecord,
        result_entity: ResultObjectId,
    },
    AddObservation {
        source: ObservationRecord,
        result: QualifiedObjectId,
        result_entity: ResultObjectId,
    },
    MergeObservationOrigin {
        source: ObservationRecord,
        target: ResultObjectId,
        result_entity: ResultObjectId,
    },
    KeepTargetRelation {
        target: RelationRecord,
        result_subject: ResultObjectId,
        result_object: ResultObjectId,
    },
    AddRelation {
        source: RelationRecord,
        result: QualifiedObjectId,
        result_subject: ResultObjectId,
        result_object: ResultObjectId,
    },
    MergeRelationOrigin {
        source: RelationRecord,
        target: ResultObjectId,
        result_subject: ResultObjectId,
        result_object: ResultObjectId,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ActionCounts {
    pub target_entities: u64,
    pub source_entities: u64,
    pub coalesced_entities: u64,
    pub added_entities: u64,
    pub target_observations: u64,
    pub source_observations: u64,
    pub added_observations: u64,
    pub merged_observations: u64,
    pub target_relations: u64,
    pub source_relations: u64,
    pub added_relations: u64,
    pub merged_relations: u64,
}

impl ActionCounts {
    fn total_actions(&self) -> u64 {
        self.target_entities
            + self.source_entities
            + self.target_observations
            + self.source_observations
            + self.target_relations
            + self.source_relations
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MergeAccounting {
    pub consumed_candidates: u64,
    pub consumed_discoveries: u64,
    pub consumed_resolutions: u64,
    pub consumed_keep_distinct: u64,
    pub result_entities: u64,
    pub result_observations: u64,
    pub result_relations: u64,
    pub retained_origins: u64,
}

/// Header available before a sink creates private staging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanPrelude {
    version: u16,
    source_space: SpaceId,
    target_space: SpaceId,
    source_snapshot_hash: SnapshotHash,
    target_snapshot_hash: SnapshotHash,
    expected_target_version: SpaceVersion,
    policy_version: u16,
    identity_policy_generation: u64,
}

impl PlanPrelude {
    #[must_use]
    pub const fn source_space(&self) -> SpaceId {
        self.source_space
    }

    #[must_use]
    pub const fn target_space(&self) -> SpaceId {
        self.target_space
    }
}

/// Finished immutable plan binding exact inputs, action stream, accounting, and
/// predicted semantic result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergePlan {
    prelude: PlanPrelude,
    action_stream_hash: ActionStreamHash,
    action_counts: ActionCounts,
    accounting: MergeAccounting,
    predicted_result_hash: PredictedResultHash,
    plan_hash: PlanHash,
}

impl MergePlan {
    #[must_use]
    pub fn prelude(&self) -> &PlanPrelude {
        &self.prelude
    }

    #[must_use]
    pub const fn action_stream_hash(&self) -> ActionStreamHash {
        self.action_stream_hash
    }

    #[must_use]
    pub fn action_counts(&self) -> &ActionCounts {
        &self.action_counts
    }

    #[must_use]
    pub fn accounting(&self) -> &MergeAccounting {
        &self.accounting
    }

    #[must_use]
    pub const fn predicted_result_hash(&self) -> PredictedResultHash {
        self.predicted_result_hash
    }

    #[must_use]
    pub const fn plan_hash(&self) -> PlanHash {
        self.plan_hash
    }
}

/// Explicit action publication lifecycle. Only `finish` can make a plan
/// consumable; every failure after `begin` is followed by idempotent `abort`.
pub trait ActionSink {
    fn begin(&mut self, prelude: &PlanPrelude) -> MergeResult<()>;
    fn emit(&mut self, action: &MergeAction) -> MergeResult<()>;
    fn finish(&mut self, plan: &MergePlan) -> MergeResult<()>;
    fn abort(&mut self);
}

/// Bounded collecting sink used by tests, previews, and small callers.
#[derive(Debug, Clone)]
pub struct MemoryActionSink {
    max_actions: usize,
    state: MemorySinkState,
    prelude: Option<PlanPrelude>,
    actions: Vec<MergeAction>,
    plan: Option<MergePlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemorySinkState {
    Idle,
    Begun,
    Finished,
    Aborted,
}

impl MemoryActionSink {
    pub fn new(max_actions: usize) -> MergeResult<Self> {
        if max_actions == 0 || max_actions > MAX_ACTIONS {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "sink action bound must be within 1..=3,000,000",
            ));
        }
        Ok(Self {
            max_actions,
            state: MemorySinkState::Idle,
            prelude: None,
            actions: Vec::new(),
            plan: None,
        })
    }

    #[must_use]
    pub fn actions(&self) -> &[MergeAction] {
        &self.actions
    }

    #[must_use]
    pub fn plan(&self) -> Option<&MergePlan> {
        self.plan.as_ref()
    }

    #[must_use]
    pub const fn is_aborted(&self) -> bool {
        matches!(self.state, MemorySinkState::Aborted)
    }
}

impl ActionSink for MemoryActionSink {
    fn begin(&mut self, prelude: &PlanPrelude) -> MergeResult<()> {
        if self.state != MemorySinkState::Idle {
            return Err(MergeError::new(
                MergeErrorCode::ProtocolViolation,
                "sink begin called outside idle state",
            ));
        }
        self.prelude = Some(prelude.clone());
        self.state = MemorySinkState::Begun;
        Ok(())
    }

    fn emit(&mut self, action: &MergeAction) -> MergeResult<()> {
        if self.state != MemorySinkState::Begun {
            return Err(MergeError::new(
                MergeErrorCode::ProtocolViolation,
                "sink emit called outside begun state",
            ));
        }
        if self.actions.len() == self.max_actions {
            return Err(MergeError::new(
                MergeErrorCode::SinkFailure,
                "sink action capacity exceeded",
            ));
        }
        self.actions.push(action.clone());
        Ok(())
    }

    fn finish(&mut self, plan: &MergePlan) -> MergeResult<()> {
        if self.state != MemorySinkState::Begun {
            return Err(MergeError::new(
                MergeErrorCode::ProtocolViolation,
                "sink finish called outside begun state",
            ));
        }
        if self.prelude.as_ref() != Some(plan.prelude()) {
            return Err(MergeError::new(
                MergeErrorCode::ProtocolViolation,
                "finished plan does not match begun prelude",
            ));
        }
        self.plan = Some(plan.clone());
        self.state = MemorySinkState::Finished;
        Ok(())
    }

    fn abort(&mut self) {
        if self.state != MemorySinkState::Finished {
            self.plan = None;
            self.actions.clear();
            self.state = MemorySinkState::Aborted;
        }
    }
}

#[derive(Debug, Clone)]
enum PlannedDisposition {
    Coalesce {
        target: Box<EntityRevisionRef>,
        receipt: ReceiptHash,
    },
    Add {
        reason: DistinctReason,
        receipt: Option<ReceiptHash>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ObservationKey(SemanticHash);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RelationKey(SemanticHash);

fn observation_key(
    entity: &ResultObjectId,
    lifecycle: crate::model::Lifecycle,
    content: &str,
) -> ObservationKey {
    let mut hash = CanonicalHasher::new(b"openmemory/observation-index-key/v1");
    encode_result_id(&mut hash, entity);
    hash.u16(lifecycle as u16);
    hash.text(content);
    ObservationKey(hash.finish_semantic())
}

fn relation_key(
    subject: &ResultObjectId,
    predicate: &str,
    object: &ResultObjectId,
    lifecycle: crate::model::Lifecycle,
) -> RelationKey {
    let mut hash = CanonicalHasher::new(b"openmemory/relation-index-key/v1");
    encode_result_id(&mut hash, subject);
    hash.text(predicate);
    encode_result_id(&mut hash, object);
    hash.u16(lifecycle as u16);
    RelationKey(hash.finish_semantic())
}

fn qualified_collision_key(value: &QualifiedObjectId) -> SemanticHash {
    let mut hash = CanonicalHasher::new(b"openmemory/qualified-id-collision-key/v1");
    hash.text(value.as_str());
    hash.finish_semantic()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResultToken {
    kind: u16,
    value: SemanticHash,
}

struct PlanningState<'a> {
    sink: &'a mut dyn ActionSink,
    action_hash: CanonicalHasher,
    result_tokens: Vec<ResultToken>,
    counts: ActionCounts,
    accounting: MergeAccounting,
}

struct PlannedEntity {
    logical_id: LogicalId,
    revision: RevisionId,
    snapshot: SnapshotId,
    disposition: PlannedDisposition,
}

impl PlanningState<'_> {
    fn emit(&mut self, action: &MergeAction) -> MergeResult<()> {
        if self.counts.total_actions() >= u64::try_from(MAX_ACTIONS).unwrap_or(u64::MAX) {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "merge action count exceeds the hard limit",
            ));
        }
        self.sink.emit(action)?;
        encode_action(&mut self.action_hash, action);
        self.result_tokens.extend(result_tokens(action));
        Ok(())
    }
}

/// Plan one exact immutable source into one exact immutable target.
pub fn plan_merge(
    target: &mut dyn SnapshotSource,
    source: &mut dyn SnapshotSource,
    receipts: &PlanningReceipts,
    policy: &MergePolicy,
    sink: &mut dyn ActionSink,
) -> MergeResult<MergePlan> {
    let target_header = target.header().clone();
    let source_header = source.header().clone();
    validate_headers(&target_header, &source_header)?;
    let prelude = PlanPrelude {
        version: MERGE_PLAN_VERSION,
        source_space: source_header.space_id(),
        target_space: target_header.space_id(),
        source_snapshot_hash: source_header.hash(),
        target_snapshot_hash: target_header.hash(),
        expected_target_version: target_header.space_version().clone(),
        policy_version: policy.version(),
        identity_policy_generation: policy.identity_policy_generation(),
    };
    sink.begin(&prelude)?;

    let result = plan_after_begin(
        target,
        source,
        &target_header,
        &source_header,
        receipts,
        policy,
        prelude,
        sink,
    );
    match result {
        Ok(plan) => {
            if let Err(error) = sink.finish(&plan) {
                sink.abort();
                Err(error)
            } else {
                Ok(plan)
            }
        }
        Err(error) => {
            sink.abort();
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_after_begin(
    target: &mut dyn SnapshotSource,
    source: &mut dyn SnapshotSource,
    target_header: &SnapshotHeader,
    source_header: &SnapshotHeader,
    receipts: &PlanningReceipts,
    policy: &MergePolicy,
    prelude: PlanPrelude,
    sink: &mut dyn ActionSink,
) -> MergeResult<MergePlan> {
    let (planned, mut accounting) = prepare_dispositions(receipts, policy)?;
    let mut state = PlanningState {
        sink,
        action_hash: CanonicalHasher::new(b"openmemory/merge-actions/v1"),
        result_tokens: Vec::new(),
        counts: ActionCounts::default(),
        accounting: std::mem::take(&mut accounting),
    };
    let mut target_verifier = SnapshotVerifier::new(target_header)?;
    let mut source_verifier = SnapshotVerifier::new(source_header)?;
    let mut target_entities = BTreeMap::<LogicalId, RevisionId>::new();
    let mut result_entity_ids = BTreeSet::<String>::new();

    target.visit_entities(&mut |record| {
        target_verifier.entity(&record)?;
        if target_entities
            .insert(record.address().logical_id().clone(), record.revision())
            .is_some()
        {
            return Err(MergeError::new(
                MergeErrorCode::DuplicateInput,
                "target entity is duplicated",
            ));
        }
        result_entity_ids.insert(record.address().logical_id().as_str().to_string());
        state.counts.target_entities += 1;
        state.emit(&MergeAction::KeepTargetEntity { target: record })
    })?;
    target_verifier.end_entities()?;

    let mut source_entity_results = BTreeMap::<LogicalId, ResultObjectId>::new();
    let mut qualified_entity_ids = HashSet::new();
    let mut seen_dispositions = vec![false; planned.len()];
    let mut seen_disposition_count = 0_usize;
    source.visit_entities(&mut |record| {
        source_verifier.entity(&record)?;
        let Ok(disposition_index) = planned
            .binary_search_by(|candidate| candidate.logical_id.cmp(record.address().logical_id()))
        else {
            return Err(MergeError::new(
                MergeErrorCode::DiscoveryIncomplete,
                "source entity has no discovery receipt",
            ));
        };
        let expected = &planned[disposition_index];
        if expected.revision != record.revision()
            || expected.snapshot != source_header.snapshot_id()
        {
            return Err(MergeError::new(
                MergeErrorCode::StaleReceipt,
                "source entity moved after discovery",
            ));
        }
        if std::mem::replace(&mut seen_dispositions[disposition_index], true) {
            return Err(MergeError::new(
                MergeErrorCode::DuplicateInput,
                "source entity consumes one discovery more than once",
            ));
        }
        seen_disposition_count += 1;
        let (public_disposition, result_id) = match &expected.disposition {
            PlannedDisposition::Coalesce { target, receipt } => {
                let Some(target_revision) = target_entities.get(target.address().logical_id())
                else {
                    return Err(MergeError::new(
                        MergeErrorCode::StaleReceipt,
                        "coalescence target is absent",
                    ));
                };
                if *target_revision != target.revision()
                    || target.snapshot() != target_header.snapshot_id()
                {
                    return Err(MergeError::new(
                        MergeErrorCode::StaleReceipt,
                        "coalescence target moved after review",
                    ));
                }
                state.counts.coalesced_entities += 1;
                (
                    EntityDisposition::Coalesce {
                        source: record.address().clone(),
                        target: target.address().clone(),
                        receipt: *receipt,
                    },
                    ResultObjectId::Target(target.address().logical_id().clone()),
                )
            }
            PlannedDisposition::Add { reason, receipt } => {
                let qualified = QualifiedObjectId::derive(
                    record.address().space(),
                    record.address().logical_id(),
                );
                if result_entity_ids.contains(qualified.as_str())
                    || !qualified_entity_ids.insert(qualified_collision_key(&qualified))
                {
                    return Err(MergeError::new(
                        MergeErrorCode::QualifiedIdCollision,
                        "derived entity ID already exists in the target result",
                    ));
                }
                state.counts.added_entities += 1;
                (
                    EntityDisposition::AddDistinct {
                        source: record.address().clone(),
                        result: qualified.clone(),
                        reason: *reason,
                        receipt: *receipt,
                    },
                    ResultObjectId::Qualified(qualified),
                )
            }
        };
        source_entity_results.insert(record.address().logical_id().clone(), result_id);
        state.counts.source_entities += 1;
        state.emit(&MergeAction::ApplyEntityDisposition {
            source: record,
            disposition: public_disposition,
        })
    })?;
    source_verifier.end_entities()?;
    if seen_disposition_count != planned.len() {
        return Err(MergeError::new(
            MergeErrorCode::DiscoveryIncomplete,
            "discovery receipt exists for an absent source entity",
        ));
    }

    let mut observation_index = BTreeMap::<ObservationKey, ResultObjectId>::new();
    let mut result_observation_ids = BTreeSet::<String>::new();
    let mut qualified_observation_ids = HashSet::new();
    target.visit_observations(&mut |record| {
        target_verifier.observation(&record)?;
        if !target_entities.contains_key(record.entity().logical_id()) {
            return Err(MergeError::new(
                MergeErrorCode::DanglingEndpoint,
                "target observation entity is absent",
            ));
        }
        let result = ResultObjectId::Target(record.address().logical_id().clone());
        let result_entity = ResultObjectId::Target(record.entity().logical_id().clone());
        let key = observation_key(&result_entity, record.lifecycle(), record.content());
        observation_index.entry(key).or_insert(result.clone());
        if !result_observation_ids.insert(result.as_str().to_string()) {
            return Err(MergeError::new(
                MergeErrorCode::DuplicateInput,
                "target observation projection is duplicated",
            ));
        }
        state.counts.target_observations += 1;
        state.emit(&MergeAction::KeepTargetObservation {
            target: record,
            result_entity,
        })
    })?;
    target_verifier.end_observations()?;

    source.visit_observations(&mut |record| {
        source_verifier.observation(&record)?;
        let Some(result_entity) = source_entity_results
            .get(record.entity().logical_id())
            .cloned()
        else {
            return Err(MergeError::new(
                MergeErrorCode::DanglingEndpoint,
                "source observation entity has no disposition",
            ));
        };
        let key = observation_key(&result_entity, record.lifecycle(), record.content());
        let action = if let Some(target_result) = observation_index.get(&key).cloned() {
            state.counts.merged_observations += 1;
            MergeAction::MergeObservationOrigin {
                source: record,
                target: target_result,
                result_entity,
            }
        } else {
            let result =
                QualifiedObjectId::derive(record.address().space(), record.address().logical_id());
            if result_observation_ids.contains(result.as_str())
                || !qualified_observation_ids.insert(qualified_collision_key(&result))
            {
                return Err(MergeError::new(
                    MergeErrorCode::QualifiedIdCollision,
                    "derived observation ID already exists",
                ));
            }
            let projected = ResultObjectId::Qualified(result.clone());
            observation_index.insert(key, projected);
            state.counts.added_observations += 1;
            MergeAction::AddObservation {
                source: record,
                result,
                result_entity,
            }
        };
        state.counts.source_observations += 1;
        state.emit(&action)
    })?;
    source_verifier.end_observations()?;

    let mut relation_index = BTreeMap::<RelationKey, ResultObjectId>::new();
    let mut result_relation_ids = BTreeSet::<String>::new();
    let mut qualified_relation_ids = HashSet::new();
    target.visit_relations(&mut |record| {
        target_verifier.relation(&record)?;
        if !target_entities.contains_key(record.subject().logical_id())
            || !target_entities.contains_key(record.object().logical_id())
        {
            return Err(MergeError::new(
                MergeErrorCode::DanglingEndpoint,
                "target relation endpoint is absent",
            ));
        }
        let result_subject = ResultObjectId::Target(record.subject().logical_id().clone());
        let result_object = ResultObjectId::Target(record.object().logical_id().clone());
        let key = relation_key(
            &result_subject,
            record.predicate(),
            &result_object,
            record.lifecycle(),
        );
        let result = ResultObjectId::Target(record.address().logical_id().clone());
        if relation_index.insert(key, result.clone()).is_some()
            || !result_relation_ids.insert(result.as_str().to_string())
        {
            return Err(MergeError::new(
                MergeErrorCode::DuplicateInput,
                "target relation assertion is duplicated",
            ));
        }
        state.counts.target_relations += 1;
        state.emit(&MergeAction::KeepTargetRelation {
            target: record,
            result_subject,
            result_object,
        })
    })?;
    target_verifier.finish()?;

    source.visit_relations(&mut |record| {
        source_verifier.relation(&record)?;
        let Some(result_subject) = source_entity_results
            .get(record.subject().logical_id())
            .cloned()
        else {
            return Err(MergeError::new(
                MergeErrorCode::DanglingEndpoint,
                "source relation subject has no disposition",
            ));
        };
        let Some(result_object) = source_entity_results
            .get(record.object().logical_id())
            .cloned()
        else {
            return Err(MergeError::new(
                MergeErrorCode::DanglingEndpoint,
                "source relation object has no disposition",
            ));
        };
        if result_subject == result_object && !policy.allows_self_loop(record.predicate()) {
            return Err(MergeError::new(
                MergeErrorCode::ThreeWayConflict,
                "coalescence creates a self-loop without explicit predicate policy",
            ));
        }
        let key = relation_key(
            &result_subject,
            record.predicate(),
            &result_object,
            record.lifecycle(),
        );
        let action = if let Some(target_result) = relation_index.get(&key).cloned() {
            state.counts.merged_relations += 1;
            MergeAction::MergeRelationOrigin {
                source: record,
                target: target_result,
                result_subject,
                result_object,
            }
        } else {
            let result =
                QualifiedObjectId::derive(record.address().space(), record.address().logical_id());
            if result_relation_ids.contains(result.as_str())
                || !qualified_relation_ids.insert(qualified_collision_key(&result))
            {
                return Err(MergeError::new(
                    MergeErrorCode::QualifiedIdCollision,
                    "derived relation ID already exists",
                ));
            }
            let projected = ResultObjectId::Qualified(result.clone());
            relation_index.insert(key, projected);
            state.counts.added_relations += 1;
            MergeAction::AddRelation {
                source: record,
                result,
                result_subject,
                result_object,
            }
        };
        state.counts.source_relations += 1;
        state.emit(&action)
    })?;
    source_verifier.finish()?;

    state.result_tokens.sort_unstable();
    state.result_tokens.dedup();
    state.accounting.result_entities = state.counts.target_entities + state.counts.added_entities;
    state.accounting.result_observations =
        state.counts.target_observations + state.counts.added_observations;
    state.accounting.result_relations =
        state.counts.target_relations + state.counts.added_relations;
    state.accounting.retained_origins = u64::try_from(
        state
            .result_tokens
            .iter()
            .filter(|token| matches!(token.kind, 2 | 4 | 6))
            .count(),
    )
    .unwrap_or(u64::MAX);
    validate_accounting(
        &state.counts,
        &state.accounting,
        target_header,
        source_header,
    )?;

    let action_stream_hash = state.action_hash.finish_action_stream();
    let predicted_result_hash = hash_result_tokens(&state.result_tokens);
    let plan_hash = hash_plan(
        &prelude,
        action_stream_hash,
        &state.counts,
        &state.accounting,
        predicted_result_hash,
    );
    Ok(MergePlan {
        prelude,
        action_stream_hash,
        action_counts: state.counts,
        accounting: state.accounting,
        predicted_result_hash,
        plan_hash,
    })
}

fn validate_headers(target: &SnapshotHeader, source: &SnapshotHeader) -> MergeResult<()> {
    if target.format_version() != 1 || source.format_version() != 1 {
        return Err(MergeError::new(
            MergeErrorCode::InvalidInput,
            "unsupported snapshot format version",
        ));
    }
    if target.space_id() == source.space_id() {
        return Err(MergeError::new(
            MergeErrorCode::InvalidInput,
            "material merge source and target must be distinct spaces",
        ));
    }
    Ok(())
}

fn prepare_dispositions(
    receipts: &PlanningReceipts,
    policy: &MergePolicy,
) -> MergeResult<(Vec<PlannedEntity>, MergeAccounting)> {
    let mut discoveries = BTreeMap::new();
    for discovery in &receipts.discoveries {
        if discovery.policy_generation() != policy.identity_policy_generation() {
            return Err(MergeError::new(
                MergeErrorCode::StaleReceipt,
                "discovery policy generation is stale",
            ));
        }
        if discoveries
            .insert(discovery.source().address().clone(), discovery)
            .is_some()
        {
            return Err(MergeError::new(
                MergeErrorCode::DuplicateInput,
                "source discovery receipt is duplicated",
            ));
        }
    }
    let mut resolution_by_pair = BTreeMap::new();
    for resolution in &receipts.resolutions {
        if resolution.policy_generation() != policy.identity_policy_generation()
            || resolution_by_pair
                .insert(
                    (
                        resolution.target().address().clone(),
                        resolution.source().address().clone(),
                    ),
                    resolution,
                )
                .is_some()
        {
            return Err(MergeError::new(
                MergeErrorCode::StaleReceipt,
                "identity resolution is stale or duplicated",
            ));
        }
    }
    let mut keep_by_source = BTreeMap::new();
    for keep in &receipts.keep_distinct {
        if keep.policy_generation() != policy.identity_policy_generation()
            || keep_by_source
                .insert(keep.source().address().clone(), keep)
                .is_some()
        {
            return Err(MergeError::new(
                MergeErrorCode::StaleReceipt,
                "keep-distinct receipt is stale or duplicated",
            ));
        }
    }

    let mut used_resolutions = BTreeSet::new();
    let mut used_keep = BTreeSet::new();
    let mut coalescence_targets = BTreeSet::new();
    let mut planned = Vec::with_capacity(discoveries.len());
    let mut accounting = MergeAccounting {
        consumed_discoveries: u64::try_from(discoveries.len()).unwrap_or(u64::MAX),
        ..MergeAccounting::default()
    };
    for (source_address, discovery) in discoveries {
        let disposition = if discovery.candidates().is_empty() {
            if keep_by_source.contains_key(&source_address) {
                return Err(MergeError::new(
                    MergeErrorCode::ConflictingResolution,
                    "empty discovery must not carry a keep-distinct receipt",
                ));
            }
            PlannedDisposition::Add {
                reason: DistinctReason::NoCandidate,
                receipt: None,
            }
        } else if let Some(keep) = keep_by_source.get(&source_address) {
            if !keep.matches_discovery(discovery) {
                return Err(MergeError::new(
                    MergeErrorCode::StaleReceipt,
                    "keep-distinct receipt does not bind the discovery page",
                ));
            }
            for candidate in discovery.candidates() {
                if resolution_by_pair.contains_key(&(
                    candidate.target().address().clone(),
                    candidate.source().address().clone(),
                )) {
                    return Err(MergeError::new(
                        MergeErrorCode::ConflictingResolution,
                        "candidate is consumed by both resolution and keep-distinct",
                    ));
                }
            }
            used_keep.insert(source_address.clone());
            accounting.consumed_candidates +=
                u64::try_from(discovery.candidates().len()).unwrap_or(u64::MAX);
            PlannedDisposition::Add {
                reason: DistinctReason::KeepDistinct,
                receipt: Some(keep.hash()),
            }
        } else {
            if discovery.truncated() {
                return Err(MergeError::new(
                    MergeErrorCode::DiscoveryIncomplete,
                    "truncated discovery may proceed only through keep-distinct",
                ));
            }
            let mut same = None;
            for candidate in discovery.candidates() {
                let key = (
                    candidate.target().address().clone(),
                    candidate.source().address().clone(),
                );
                let Some(resolution) = resolution_by_pair.get(&key) else {
                    return Err(MergeError::new(
                        MergeErrorCode::DiscoveryIncomplete,
                        "candidate lacks exactly one current resolution",
                    ));
                };
                if !resolution.matches_candidate(candidate) {
                    return Err(MergeError::new(
                        MergeErrorCode::StaleReceipt,
                        "resolution revisions do not match candidate",
                    ));
                }
                used_resolutions.insert(key);
                accounting.consumed_candidates += 1;
                if resolution.decision() == IdentityDecision::Same
                    && same.replace(*resolution).is_some()
                {
                    return Err(MergeError::new(
                        MergeErrorCode::ConflictingResolution,
                        "one source resolves same with multiple targets",
                    ));
                }
            }
            if let Some(resolution) = same {
                if !coalescence_targets.insert(resolution.target().address().clone()) {
                    return Err(MergeError::new(
                        MergeErrorCode::ManyToOne,
                        "multiple source entities cannot coalesce into one target",
                    ));
                }
                PlannedDisposition::Coalesce {
                    target: Box::new(resolution.target().clone()),
                    receipt: resolution.hash(),
                }
            } else {
                PlannedDisposition::Add {
                    reason: DistinctReason::ReviewedDifferent,
                    receipt: None,
                }
            }
        };
        planned.push(PlannedEntity {
            logical_id: source_address.logical_id().clone(),
            revision: discovery.source().revision(),
            snapshot: discovery.source().snapshot(),
            disposition,
        });
    }
    if used_resolutions.len() != resolution_by_pair.len() || used_keep.len() != keep_by_source.len()
    {
        return Err(MergeError::new(
            MergeErrorCode::DiscoveryIncomplete,
            "extra resolution or keep-distinct receipt is outside discovery",
        ));
    }
    accounting.consumed_resolutions = u64::try_from(used_resolutions.len()).unwrap_or(u64::MAX);
    accounting.consumed_keep_distinct = u64::try_from(used_keep.len()).unwrap_or(u64::MAX);
    Ok((planned, accounting))
}

struct SnapshotVerifier {
    expected: SnapshotHeader,
    hash: Option<CanonicalHasher>,
    previous: Option<ObjectAddress>,
    entities: u64,
    observations: u64,
    relations: u64,
    stage: u8,
}

impl SnapshotVerifier {
    fn new(header: &SnapshotHeader) -> MergeResult<Self> {
        if header.format_version() != 1 {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "unsupported snapshot format",
            ));
        }
        let mut hash = CanonicalHasher::new(b"openmemory/space-snapshot/v1");
        hash.u16(header.format_version());
        hash.snapshot_id(header.snapshot_id());
        hash.space_id(header.space_id());
        encode_space_version(&mut hash, header.space_version());
        hash.u64(header.entity_count());
        Ok(Self {
            expected: header.clone(),
            hash: Some(hash),
            previous: None,
            entities: 0,
            observations: 0,
            relations: 0,
            stage: 1,
        })
    }

    fn entity(&mut self, record: &EntityRecord) -> MergeResult<()> {
        self.ensure_record(1, record.address())?;
        encode_entity(self.hash_mut()?, record);
        self.entities += 1;
        Ok(())
    }

    fn end_entities(&mut self) -> MergeResult<()> {
        if self.stage != 1 || self.entities != self.expected.entity_count() {
            return Err(MergeError::new(
                MergeErrorCode::SnapshotMismatch,
                "snapshot entity count differs from its header",
            ));
        }
        let observation_count = self.expected.observation_count();
        self.hash_mut()?.u64(observation_count);
        self.previous = None;
        self.stage = 2;
        Ok(())
    }

    fn observation(&mut self, record: &ObservationRecord) -> MergeResult<()> {
        self.ensure_record(2, record.address())?;
        encode_observation(self.hash_mut()?, record);
        self.observations += 1;
        Ok(())
    }

    fn end_observations(&mut self) -> MergeResult<()> {
        if self.stage != 2 || self.observations != self.expected.observation_count() {
            return Err(MergeError::new(
                MergeErrorCode::SnapshotMismatch,
                "snapshot observation count differs from its header",
            ));
        }
        let relation_count = self.expected.relation_count();
        self.hash_mut()?.u64(relation_count);
        self.previous = None;
        self.stage = 3;
        Ok(())
    }

    fn relation(&mut self, record: &RelationRecord) -> MergeResult<()> {
        self.ensure_record(3, record.address())?;
        encode_relation(self.hash_mut()?, record);
        self.relations += 1;
        Ok(())
    }

    fn finish(&mut self) -> MergeResult<()> {
        if self.stage != 3 || self.relations != self.expected.relation_count() {
            return Err(MergeError::new(
                MergeErrorCode::SnapshotMismatch,
                "snapshot relation count differs from its header",
            ));
        }
        let actual = self
            .hash
            .take()
            .ok_or_else(|| {
                MergeError::new(
                    MergeErrorCode::ProtocolViolation,
                    "snapshot verifier has already finished",
                )
            })?
            .finish_snapshot();
        if actual != self.expected.hash() {
            return Err(MergeError::new(
                MergeErrorCode::SnapshotMismatch,
                "snapshot canonical hash differs from its header",
            ));
        }
        self.stage = 4;
        Ok(())
    }

    fn hash_mut(&mut self) -> MergeResult<&mut CanonicalHasher> {
        self.hash.as_mut().ok_or_else(|| {
            MergeError::new(
                MergeErrorCode::ProtocolViolation,
                "snapshot verifier has already finished",
            )
        })
    }

    fn ensure_record(&mut self, stage: u8, address: &ObjectAddress) -> MergeResult<()> {
        if self.stage != stage {
            return Err(MergeError::new(
                MergeErrorCode::ProtocolViolation,
                "snapshot record section is out of order",
            ));
        }
        if address.space() != self.expected.space_id() {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "snapshot record belongs to another space",
            ));
        }
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| previous >= address)
        {
            return Err(MergeError::new(
                MergeErrorCode::UnsortedInput,
                "snapshot records are not strictly sorted and unique",
            ));
        }
        self.previous = Some(address.clone());
        Ok(())
    }
}

fn validate_accounting(
    counts: &ActionCounts,
    accounting: &MergeAccounting,
    target: &SnapshotHeader,
    source: &SnapshotHeader,
) -> MergeResult<()> {
    if counts.target_entities != target.entity_count()
        || counts.source_entities != source.entity_count()
        || counts.target_observations != target.observation_count()
        || counts.source_observations != source.observation_count()
        || counts.target_relations != target.relation_count()
        || counts.source_relations != source.relation_count()
        || counts.coalesced_entities + counts.added_entities != counts.source_entities
        || counts.added_observations + counts.merged_observations != counts.source_observations
        || counts.added_relations + counts.merged_relations != counts.source_relations
        || accounting.result_entities != counts.target_entities + counts.added_entities
        || accounting.result_observations != counts.target_observations + counts.added_observations
        || accounting.result_relations != counts.target_relations + counts.added_relations
    {
        return Err(MergeError::new(
            MergeErrorCode::AccountingMismatch,
            "merge accounting does not cover every input and result exactly once",
        ));
    }
    Ok(())
}

fn encode_action(hash: &mut CanonicalHasher, action: &MergeAction) {
    match action {
        MergeAction::KeepTargetEntity { target } => {
            hash.u16(1);
            encode_entity(hash, target);
        }
        MergeAction::ApplyEntityDisposition {
            source,
            disposition,
        } => {
            hash.u16(2);
            encode_entity(hash, source);
            match disposition {
                EntityDisposition::Coalesce {
                    source,
                    target,
                    receipt,
                } => {
                    hash.u16(1);
                    encode_address(hash, source);
                    encode_address(hash, target);
                    hash.hash(receipt.as_bytes());
                }
                EntityDisposition::AddDistinct {
                    source,
                    result,
                    reason,
                    receipt,
                } => {
                    hash.u16(2);
                    encode_address(hash, source);
                    hash.text(result.as_str());
                    hash.u16(*reason as u16);
                    hash.bool(receipt.is_some());
                    if let Some(receipt) = receipt {
                        hash.hash(receipt.as_bytes());
                    }
                }
            }
        }
        MergeAction::KeepTargetObservation {
            target,
            result_entity,
        } => {
            hash.u16(3);
            encode_observation(hash, target);
            encode_result_id(hash, result_entity);
        }
        MergeAction::AddObservation {
            source,
            result,
            result_entity,
        } => {
            hash.u16(4);
            encode_observation(hash, source);
            hash.text(result.as_str());
            encode_result_id(hash, result_entity);
        }
        MergeAction::MergeObservationOrigin {
            source,
            target,
            result_entity,
        } => {
            hash.u16(5);
            encode_observation(hash, source);
            encode_result_id(hash, target);
            encode_result_id(hash, result_entity);
        }
        MergeAction::KeepTargetRelation {
            target,
            result_subject,
            result_object,
        } => {
            hash.u16(6);
            encode_relation(hash, target);
            encode_result_id(hash, result_subject);
            encode_result_id(hash, result_object);
        }
        MergeAction::AddRelation {
            source,
            result,
            result_subject,
            result_object,
        } => {
            hash.u16(7);
            encode_relation(hash, source);
            hash.text(result.as_str());
            encode_result_id(hash, result_subject);
            encode_result_id(hash, result_object);
        }
        MergeAction::MergeRelationOrigin {
            source,
            target,
            result_subject,
            result_object,
        } => {
            hash.u16(8);
            encode_relation(hash, source);
            encode_result_id(hash, target);
            encode_result_id(hash, result_subject);
            encode_result_id(hash, result_object);
        }
    }
}

fn result_tokens(action: &MergeAction) -> Vec<ResultToken> {
    match action {
        MergeAction::KeepTargetEntity { target } => {
            let result = ResultObjectId::Target(target.address().logical_id().clone());
            projection_and_origins(
                1,
                2,
                &result,
                entity_projection(&result, target),
                target.origins(),
            )
        }
        MergeAction::ApplyEntityDisposition {
            source,
            disposition,
        } => match disposition {
            EntityDisposition::Coalesce { target, .. } => origins_only(
                2,
                &ResultObjectId::Target(target.logical_id().clone()),
                source.origins(),
            ),
            EntityDisposition::AddDistinct { result, .. } => {
                let result = ResultObjectId::Qualified(result.clone());
                projection_and_origins(
                    1,
                    2,
                    &result,
                    entity_projection(&result, source),
                    source.origins(),
                )
            }
        },
        MergeAction::KeepTargetObservation {
            target,
            result_entity,
        } => {
            let result = ResultObjectId::Target(target.address().logical_id().clone());
            projection_and_origins(
                3,
                4,
                &result,
                observation_projection(&result, result_entity, target),
                target.origins(),
            )
        }
        MergeAction::AddObservation {
            source,
            result,
            result_entity,
        } => {
            let result = ResultObjectId::Qualified(result.clone());
            projection_and_origins(
                3,
                4,
                &result,
                observation_projection(&result, result_entity, source),
                source.origins(),
            )
        }
        MergeAction::MergeObservationOrigin { source, target, .. } => {
            origins_only(4, target, source.origins())
        }
        MergeAction::KeepTargetRelation {
            target,
            result_subject,
            result_object,
        } => {
            let result = ResultObjectId::Target(target.address().logical_id().clone());
            projection_and_origins(
                5,
                6,
                &result,
                relation_projection(&result, result_subject, result_object, target),
                target.origins(),
            )
        }
        MergeAction::AddRelation {
            source,
            result,
            result_subject,
            result_object,
        } => {
            let result = ResultObjectId::Qualified(result.clone());
            projection_and_origins(
                5,
                6,
                &result,
                relation_projection(&result, result_subject, result_object, source),
                source.origins(),
            )
        }
        MergeAction::MergeRelationOrigin { source, target, .. } => {
            origins_only(6, target, source.origins())
        }
    }
}

fn projection_and_origins(
    projection_kind: u16,
    origin_kind: u16,
    result: &ResultObjectId,
    projection: SemanticHash,
    origins: &BTreeSet<OriginContribution>,
) -> Vec<ResultToken> {
    let mut tokens = vec![ResultToken {
        kind: projection_kind,
        value: projection,
    }];
    tokens.extend(origins_only(origin_kind, result, origins));
    tokens
}

fn origins_only(
    kind: u16,
    result: &ResultObjectId,
    origins: &BTreeSet<OriginContribution>,
) -> Vec<ResultToken> {
    origins
        .iter()
        .map(|origin| {
            let mut hash = CanonicalHasher::new(b"openmemory/result-origin/v1");
            hash.u16(kind);
            encode_result_id(&mut hash, result);
            encode_origin(&mut hash, origin);
            ResultToken {
                kind,
                value: hash.finish_semantic(),
            }
        })
        .collect()
}

fn entity_projection(result: &ResultObjectId, record: &EntityRecord) -> SemanticHash {
    let mut hash = CanonicalHasher::new(b"openmemory/result-entity-projection/v1");
    encode_result_id(&mut hash, result);
    hash.revision_id(record.revision());
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
        crate::model::encode_identifier(&mut hash, identifier);
    }
    hash.usize(record.properties().len());
    for (name, value) in record.properties() {
        hash.text(name);
        hash.text(value);
    }
    hash.finish_semantic()
}

fn observation_projection(
    result: &ResultObjectId,
    result_entity: &ResultObjectId,
    record: &ObservationRecord,
) -> SemanticHash {
    let mut hash = CanonicalHasher::new(b"openmemory/result-observation-projection/v1");
    encode_result_id(&mut hash, result);
    encode_result_id(&mut hash, result_entity);
    hash.revision_id(record.revision());
    hash.u16(record.lifecycle() as u16);
    hash.text(record.content());
    hash.finish_semantic()
}

fn relation_projection(
    result: &ResultObjectId,
    subject: &ResultObjectId,
    object: &ResultObjectId,
    record: &RelationRecord,
) -> SemanticHash {
    let mut hash = CanonicalHasher::new(b"openmemory/result-relation-projection/v1");
    encode_result_id(&mut hash, result);
    encode_result_id(&mut hash, subject);
    hash.text(record.predicate());
    encode_result_id(&mut hash, object);
    hash.revision_id(record.revision());
    hash.u16(record.lifecycle() as u16);
    hash.finish_semantic()
}

fn hash_result_tokens(tokens: &[ResultToken]) -> PredictedResultHash {
    let mut hash = CanonicalHasher::new(b"openmemory/predicted-result/v1");
    hash.usize(tokens.len());
    for token in tokens {
        hash.u16(token.kind);
        hash.hash(token.value.as_bytes());
    }
    hash.finish_predicted()
}

fn hash_plan(
    prelude: &PlanPrelude,
    action_stream_hash: ActionStreamHash,
    counts: &ActionCounts,
    accounting: &MergeAccounting,
    predicted: PredictedResultHash,
) -> PlanHash {
    let mut hash = CanonicalHasher::new(b"openmemory/merge-plan/v1");
    hash.u16(prelude.version);
    hash.space_id(prelude.source_space);
    hash.space_id(prelude.target_space);
    hash.hash(prelude.source_snapshot_hash.as_bytes());
    hash.hash(prelude.target_snapshot_hash.as_bytes());
    encode_space_version(&mut hash, &prelude.expected_target_version);
    hash.u16(prelude.policy_version);
    hash.u64(prelude.identity_policy_generation);
    hash.hash(action_stream_hash.as_bytes());
    encode_counts(&mut hash, counts);
    encode_accounting(&mut hash, accounting);
    hash.hash(predicted.as_bytes());
    hash.finish_plan()
}

fn encode_counts(hash: &mut CanonicalHasher, value: &ActionCounts) {
    hash.u64(value.target_entities);
    hash.u64(value.source_entities);
    hash.u64(value.coalesced_entities);
    hash.u64(value.added_entities);
    hash.u64(value.target_observations);
    hash.u64(value.source_observations);
    hash.u64(value.added_observations);
    hash.u64(value.merged_observations);
    hash.u64(value.target_relations);
    hash.u64(value.source_relations);
    hash.u64(value.added_relations);
    hash.u64(value.merged_relations);
}

fn encode_accounting(hash: &mut CanonicalHasher, value: &MergeAccounting) {
    hash.u64(value.consumed_candidates);
    hash.u64(value.consumed_discoveries);
    hash.u64(value.consumed_resolutions);
    hash.u64(value.consumed_keep_distinct);
    hash.u64(value.result_entities);
    hash.u64(value.result_observations);
    hash.u64(value.result_relations);
    hash.u64(value.retained_origins);
}

/// Independently revalidate a completed plan and its semantic action structure.
pub fn verify_plan(actions: &[MergeAction], plan: &MergePlan) -> MergeResult<()> {
    if u64::try_from(actions.len()).unwrap_or(u64::MAX) != plan.action_counts().total_actions() {
        return Err(MergeError::new(
            MergeErrorCode::AccountingMismatch,
            "action length does not match plan counts",
        ));
    }
    let mut action_hash = CanonicalHasher::new(b"openmemory/merge-actions/v1");
    let mut tokens = Vec::new();
    let mut counts = ActionCounts::default();
    for action in actions {
        encode_action(&mut action_hash, action);
        tokens.extend(result_tokens(action));
        count_action(&mut counts, action);
    }
    tokens.sort_unstable();
    tokens.dedup();
    let action_hash = action_hash.finish_action_stream();
    let predicted = hash_result_tokens(&tokens);
    let plan_hash = hash_plan(
        plan.prelude(),
        action_hash,
        &counts,
        plan.accounting(),
        predicted,
    );
    if action_hash != plan.action_stream_hash()
        || predicted != plan.predicted_result_hash()
        || counts != *plan.action_counts()
        || plan_hash != plan.plan_hash()
    {
        return Err(MergeError::new(
            MergeErrorCode::AccountingMismatch,
            "plan hashes or semantic action structure do not verify",
        ));
    }
    Ok(())
}

fn count_action(counts: &mut ActionCounts, action: &MergeAction) {
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
