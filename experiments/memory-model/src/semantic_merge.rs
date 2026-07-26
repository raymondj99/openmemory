//! Generic, immutable knowledge-space merge planning.
//!
//! Identity resolution and graph mutation are deliberately separate. The
//! planner accepts a complete set of candidate pairs and revision-bound
//! resolution receipts, accounts for every source entity exactly once, rewires
//! relation endpoints, preserves every source contribution, and predicts a
//! content-addressed result without mutating either input snapshot.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::identity::IdentityDecision;
use crate::{PocError, PocResult};

const MAX_ENTITIES: usize = 100_000;
const MAX_RELATION_ASSERTIONS: usize = 1_000_000;
const MAX_TEXT_BYTES: usize = 64 * 1_024;

/// One entity extracted from a single scoped source snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEntity {
    pub logical_id: String,
    pub revision_id: String,
    pub label: String,
    pub aliases: BTreeSet<String>,
    pub kind: String,
    pub identifiers: BTreeMap<String, String>,
    pub description: String,
    pub source_refs: BTreeSet<String>,
}

/// One source-grounded relation extracted from a scoped snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRelation {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub source_ref: String,
}

/// Immutable evidence contributed by one entity revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityContribution {
    pub origin_space: String,
    pub origin_entity: String,
    pub revision_id: String,
    pub label: String,
    pub aliases: BTreeSet<String>,
    pub kind: String,
    pub identifiers: BTreeMap<String, String>,
    pub description: String,
    pub source_refs: BTreeSet<String>,
}

impl EntityContribution {
    fn key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}",
            self.origin_space, self.origin_entity, self.revision_id
        )
    }
}

/// Canonical entity in one materialized space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedEntity {
    pub canonical_id: String,
    pub revision_id: String,
    pub contributions: BTreeMap<String, EntityContribution>,
}

/// Canonical relation address after endpoint rewiring.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelationKey {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

/// Provenance for one assertion of a canonical relation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelationContribution {
    pub origin_space: String,
    pub source_ref: String,
}

/// Immutable, content-addressed graph state for one knowledge space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeSnapshot {
    space_id: String,
    revision_id: String,
    entities: BTreeMap<String, ScopedEntity>,
    relations: BTreeMap<RelationKey, BTreeSet<RelationContribution>>,
    hash: String,
}

#[derive(Serialize)]
struct SnapshotBody<'a> {
    space_id: &'a str,
    revision_id: &'a str,
    entities: &'a BTreeMap<String, ScopedEntity>,
    relations: Vec<SnapshotRelation<'a>>,
}

#[derive(Serialize)]
struct SnapshotRelation<'a> {
    key: &'a RelationKey,
    contributions: &'a BTreeSet<RelationContribution>,
}

impl KnowledgeSnapshot {
    /// Build a canonical snapshot from local entity and relation extraction.
    ///
    /// # Errors
    ///
    /// Rejects malformed or duplicate entities, dangling relation endpoints,
    /// unbound relation provenance, excessive input, and hashing failures.
    pub fn from_local(
        space_id: impl Into<String>,
        revision_id: impl Into<String>,
        entities: impl IntoIterator<Item = LocalEntity>,
        relations: impl IntoIterator<Item = LocalRelation>,
    ) -> PocResult<Self> {
        let space_id = space_id.into();
        let revision_id = revision_id.into();
        validate_text("space ID", &space_id, 512)?;
        validate_text("snapshot revision", &revision_id, 512)?;

        let mut canonical_entities = BTreeMap::new();
        for entity in entities {
            validate_local_entity(&entity)?;
            let contribution = EntityContribution {
                origin_space: space_id.clone(),
                origin_entity: entity.logical_id.clone(),
                revision_id: entity.revision_id.clone(),
                label: entity.label,
                aliases: entity.aliases,
                kind: entity.kind,
                identifiers: entity.identifiers,
                description: entity.description,
                source_refs: entity.source_refs,
            };
            let key = contribution.key();
            let scoped = ScopedEntity {
                canonical_id: entity.logical_id.clone(),
                revision_id: entity.revision_id,
                contributions: BTreeMap::from([(key, contribution)]),
            };
            if canonical_entities
                .insert(entity.logical_id.clone(), scoped)
                .is_some()
            {
                return Err(PocError::Conflict(format!(
                    "duplicate entity {:?} in space {space_id:?}",
                    entity.logical_id
                )));
            }
            if canonical_entities.len() > MAX_ENTITIES {
                return Err(PocError::Invalid(format!(
                    "space exceeds {MAX_ENTITIES} entities"
                )));
            }
        }

        let mut canonical_relations = BTreeMap::<_, BTreeSet<_>>::new();
        let mut assertion_count = 0_usize;
        for relation in relations {
            validate_relation(&relation, &canonical_entities)?;
            let subject = &canonical_entities[&relation.subject];
            if !subject
                .contributions
                .values()
                .any(|contribution| contribution.source_refs.contains(&relation.source_ref))
            {
                return Err(PocError::Invalid(format!(
                    "relation {:?} provenance is not bound to its subject",
                    relation.predicate
                )));
            }
            canonical_relations
                .entry(RelationKey {
                    subject: relation.subject,
                    predicate: relation.predicate,
                    object: relation.object,
                })
                .or_default()
                .insert(RelationContribution {
                    origin_space: space_id.clone(),
                    source_ref: relation.source_ref,
                });
            assertion_count += 1;
            if assertion_count > MAX_RELATION_ASSERTIONS {
                return Err(PocError::Invalid(format!(
                    "space exceeds {MAX_RELATION_ASSERTIONS} relation assertions"
                )));
            }
        }

        Self::from_parts(
            space_id,
            revision_id,
            canonical_entities,
            canonical_relations,
        )
    }

    fn from_parts(
        space_id: String,
        revision_id: String,
        entities: BTreeMap<String, ScopedEntity>,
        relations: BTreeMap<RelationKey, BTreeSet<RelationContribution>>,
    ) -> PocResult<Self> {
        validate_materialized_graph(&entities, &relations)?;
        let relation_body = relations
            .iter()
            .map(|(key, contributions)| SnapshotRelation { key, contributions })
            .collect();
        let hash = hash_serializable(&SnapshotBody {
            space_id: &space_id,
            revision_id: &revision_id,
            entities: &entities,
            relations: relation_body,
        })?;
        Ok(Self {
            space_id,
            revision_id,
            entities,
            relations,
            hash,
        })
    }

    #[must_use]
    pub fn space_id(&self) -> &str {
        &self.space_id
    }

    #[must_use]
    pub fn revision_id(&self) -> &str {
        &self.revision_id
    }

    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    #[must_use]
    pub fn entities(&self) -> &BTreeMap<String, ScopedEntity> {
        &self.entities
    }

    #[must_use]
    pub fn relations(&self) -> &BTreeMap<RelationKey, BTreeSet<RelationContribution>> {
        &self.relations
    }

    #[must_use]
    pub fn relation_assertion_count(&self) -> usize {
        self.relations.values().map(BTreeSet::len).sum()
    }
}

/// Explicit candidate orientation for a directional source-to-target merge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CandidatePair {
    pub target: String,
    pub source: String,
}

impl CandidatePair {
    #[must_use]
    pub fn new(target: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            source: source.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ResolutionAuthority {
    Policy { rule: String },
    HumanReview { reviewer: String },
}

/// Identity result bound to exact entity revisions and an evidence packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityResolution {
    pair: CandidatePair,
    decision: IdentityDecision,
    target_revision: String,
    source_revision: String,
    evidence_packet_hash: String,
    authority: ResolutionAuthority,
}

impl IdentityResolution {
    /// Construct a deterministic policy decision.
    ///
    /// # Errors
    ///
    /// Team policy forbids policy-only `same` decisions; those require an
    /// explicit human review receipt.
    pub fn policy(
        pair: CandidatePair,
        decision: IdentityDecision,
        target_revision: impl Into<String>,
        source_revision: impl Into<String>,
        evidence_packet_hash: impl Into<String>,
        rule: impl Into<String>,
    ) -> PocResult<Self> {
        if decision != IdentityDecision::Different {
            return Err(PocError::Invalid(
                "policy-only identity resolution must be different".to_string(),
            ));
        }
        Self::new(
            pair,
            decision,
            target_revision,
            source_revision,
            evidence_packet_hash,
            ResolutionAuthority::Policy { rule: rule.into() },
        )
    }

    /// Construct an explicit human-reviewed identity decision.
    ///
    /// # Errors
    ///
    /// Rejects unresolved decisions or empty binding fields.
    pub fn reviewed(
        pair: CandidatePair,
        decision: IdentityDecision,
        target_revision: impl Into<String>,
        source_revision: impl Into<String>,
        evidence_packet_hash: impl Into<String>,
        reviewer: impl Into<String>,
    ) -> PocResult<Self> {
        Self::new(
            pair,
            decision,
            target_revision,
            source_revision,
            evidence_packet_hash,
            ResolutionAuthority::HumanReview {
                reviewer: reviewer.into(),
            },
        )
    }

    fn new(
        pair: CandidatePair,
        decision: IdentityDecision,
        target_revision: impl Into<String>,
        source_revision: impl Into<String>,
        evidence_packet_hash: impl Into<String>,
        authority: ResolutionAuthority,
    ) -> PocResult<Self> {
        if decision == IdentityDecision::Undetermined {
            return Err(PocError::Invalid(
                "identity resolution cannot remain undetermined".to_string(),
            ));
        }
        validate_text("target entity ID", &pair.target, 512)?;
        validate_text("source entity ID", &pair.source, 512)?;
        let target_revision = target_revision.into();
        let source_revision = source_revision.into();
        let evidence_packet_hash = evidence_packet_hash.into();
        validate_text("target revision", &target_revision, 512)?;
        validate_text("source revision", &source_revision, 512)?;
        validate_text("evidence packet hash", &evidence_packet_hash, 512)?;
        match &authority {
            ResolutionAuthority::Policy { rule } => validate_text("policy rule", rule, 512)?,
            ResolutionAuthority::HumanReview { reviewer } => {
                validate_text("reviewer", reviewer, 512)?;
            }
        }
        Ok(Self {
            pair,
            decision,
            target_revision,
            source_revision,
            evidence_packet_hash,
            authority,
        })
    }

    fn binding_hash(&self) -> PocResult<String> {
        hash_serializable(self)
    }
}

/// Exactly one disposition for every source entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntityAction {
    Merge {
        source: String,
        target: String,
        resolution_hash: String,
    },
    Add {
        source: String,
        target: String,
    },
}

impl EntityAction {
    #[must_use]
    pub fn source(&self) -> &str {
        match self {
            Self::Merge { source, .. } | Self::Add { source, .. } => source,
        }
    }

    #[must_use]
    pub fn target(&self) -> &str {
        match self {
            Self::Merge { target, .. } | Self::Add { target, .. } => target,
        }
    }
}

/// How one source relation assertion changes the target graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationEffect {
    AddRelation,
    AddProvenance,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationAction {
    pub source_key: RelationKey,
    pub target_key: RelationKey,
    pub contribution: RelationContribution,
    pub effect: RelationEffect,
}

/// Complete, content-addressed source-to-target graph merge plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeMergePlan {
    pub plan_hash: String,
    pub target_space: String,
    pub source_space: String,
    pub target_hash: String,
    pub source_hash: String,
    pub result_revision: String,
    pub result_hash: String,
    pub resolution_hashes: Vec<String>,
    pub entity_actions: Vec<EntityAction>,
    pub relation_actions: Vec<RelationAction>,
    pub separate_candidate_pairs: usize,
}

impl KnowledgeMergePlan {
    #[must_use]
    pub fn merged_entities(&self) -> usize {
        self.entity_actions
            .iter()
            .filter(|action| matches!(action, EntityAction::Merge { .. }))
            .count()
    }

    #[must_use]
    pub fn added_entities(&self) -> usize {
        self.entity_actions
            .iter()
            .filter(|action| matches!(action, EntityAction::Add { .. }))
            .count()
    }
}

#[derive(Serialize)]
struct PlanBody<'a> {
    target_space: &'a str,
    source_space: &'a str,
    target_hash: &'a str,
    source_hash: &'a str,
    result_revision: &'a str,
    result_hash: &'a str,
    resolution_hashes: &'a [String],
    entity_actions: &'a [EntityAction],
    relation_actions: &'a [RelationAction],
    separate_candidate_pairs: usize,
}

struct ResolutionSummary {
    hashes: Vec<String>,
    same_by_source: BTreeMap<String, (String, String)>,
    separate_candidate_pairs: usize,
}

/// Plan a complete directional graph merge without mutating either snapshot.
///
/// # Errors
///
/// Fails closed for missing or extra resolutions, stale receipts, policy-only
/// same decisions, many-to-one coalescence, dangling candidates, generated-ID
/// collisions, incomplete source accounting, or invalid materialization.
pub fn plan_knowledge_merge(
    target: &KnowledgeSnapshot,
    source: &KnowledgeSnapshot,
    candidates: &BTreeSet<CandidatePair>,
    resolutions: &[IdentityResolution],
) -> PocResult<KnowledgeMergePlan> {
    if target.space_id == source.space_id {
        return Err(PocError::Invalid(
            "knowledge merge requires two distinct spaces".to_string(),
        ));
    }

    let resolution_summary = summarize_resolutions(target, source, candidates, resolutions)?;
    let (entity_actions, endpoint_map) =
        plan_entities(target, source, &resolution_summary.same_by_source)?;

    let relation_actions = plan_relations(target, source, &endpoint_map)?;
    let result_revision = format!("merge-{}-{}", &target.hash[..12], &source.hash[..12]);
    let provisional = KnowledgeMergePlan {
        plan_hash: String::new(),
        target_space: target.space_id.clone(),
        source_space: source.space_id.clone(),
        target_hash: target.hash.clone(),
        source_hash: source.hash.clone(),
        result_revision,
        result_hash: String::new(),
        resolution_hashes: resolution_summary.hashes,
        entity_actions,
        relation_actions,
        separate_candidate_pairs: resolution_summary.separate_candidate_pairs,
    };
    let materialized = materialize_unchecked(target, source, &provisional)?;
    validate_result_accounting(target, source, &materialized, &provisional)?;
    let mut plan = KnowledgeMergePlan {
        result_hash: materialized.hash,
        ..provisional
    };
    plan.plan_hash = plan_binding_hash(&plan)?;
    Ok(plan)
}

fn plan_binding_hash(plan: &KnowledgeMergePlan) -> PocResult<String> {
    hash_serializable(&PlanBody {
        target_space: &plan.target_space,
        source_space: &plan.source_space,
        target_hash: &plan.target_hash,
        source_hash: &plan.source_hash,
        result_revision: &plan.result_revision,
        result_hash: &plan.result_hash,
        resolution_hashes: &plan.resolution_hashes,
        entity_actions: &plan.entity_actions,
        relation_actions: &plan.relation_actions,
        separate_candidate_pairs: plan.separate_candidate_pairs,
    })
}

fn summarize_resolutions(
    target: &KnowledgeSnapshot,
    source: &KnowledgeSnapshot,
    candidates: &BTreeSet<CandidatePair>,
    resolutions: &[IdentityResolution],
) -> PocResult<ResolutionSummary> {
    let mut resolution_by_pair = BTreeMap::new();
    let mut hashes = Vec::with_capacity(resolutions.len());
    for resolution in resolutions {
        validate_resolution(target, source, candidates, resolution)?;
        let hash = resolution.binding_hash()?;
        if resolution_by_pair
            .insert(resolution.pair.clone(), (resolution, hash.clone()))
            .is_some()
        {
            return Err(PocError::Conflict(format!(
                "duplicate identity resolution for {:?}",
                resolution.pair
            )));
        }
        hashes.push(hash);
    }
    if resolution_by_pair.len() != candidates.len()
        || candidates
            .iter()
            .any(|candidate| !resolution_by_pair.contains_key(candidate))
    {
        return Err(PocError::Conflict(
            "every candidate requires exactly one current identity resolution".to_string(),
        ));
    }
    hashes.sort();

    let mut same_by_source = BTreeMap::new();
    let mut source_by_target = BTreeMap::<String, String>::new();
    let mut separate_candidate_pairs = 0;
    for (pair, (resolution, hash)) in resolution_by_pair {
        match resolution.decision {
            IdentityDecision::Same => {
                if same_by_source
                    .insert(pair.source.clone(), (pair.target.clone(), hash))
                    .is_some()
                {
                    return Err(PocError::Conflict(format!(
                        "source entity {:?} resolves same more than once",
                        pair.source
                    )));
                }
                if let Some(previous) =
                    source_by_target.insert(pair.target.clone(), pair.source.clone())
                {
                    return Err(PocError::Conflict(format!(
                        "many-to-one coalescence into {:?} requires prior source normalization: {:?}, {:?}",
                        pair.target, previous, pair.source
                    )));
                }
            }
            IdentityDecision::Different => separate_candidate_pairs += 1,
            IdentityDecision::Undetermined => unreachable!("constructor rejects undetermined"),
        }
    }
    Ok(ResolutionSummary {
        hashes,
        same_by_source,
        separate_candidate_pairs,
    })
}

fn plan_entities(
    target: &KnowledgeSnapshot,
    source: &KnowledgeSnapshot,
    same_by_source: &BTreeMap<String, (String, String)>,
) -> PocResult<(Vec<EntityAction>, BTreeMap<String, String>)> {
    let mut actions = Vec::with_capacity(source.entities.len());
    let mut endpoint_map = BTreeMap::new();
    for source_id in source.entities.keys() {
        if let Some((target_id, resolution_hash)) = same_by_source.get(source_id) {
            actions.push(EntityAction::Merge {
                source: source_id.clone(),
                target: target_id.clone(),
                resolution_hash: resolution_hash.clone(),
            });
            endpoint_map.insert(source_id.clone(), target_id.clone());
        } else {
            let target_id = imported_entity_id(&source.space_id, source_id);
            if target.entities.contains_key(&target_id) {
                return Err(PocError::Conflict(format!(
                    "generated imported entity ID already exists: {target_id:?}"
                )));
            }
            actions.push(EntityAction::Add {
                source: source_id.clone(),
                target: target_id.clone(),
            });
            endpoint_map.insert(source_id.clone(), target_id);
        }
    }
    if actions.len() != source.entities.len()
        || actions
            .iter()
            .map(EntityAction::source)
            .collect::<BTreeSet<_>>()
            .len()
            != source.entities.len()
    {
        return Err(PocError::Invalid(
            "source entity accounting is incomplete".to_string(),
        ));
    }
    Ok((actions, endpoint_map))
}

/// Materialize a previously planned graph state in memory.
///
/// # Errors
///
/// Rejects a moved target or source, any invalid action, and a result whose
/// semantic hash differs from the plan. Both inputs remain immutable.
pub fn materialize_knowledge_merge(
    target: &KnowledgeSnapshot,
    source: &KnowledgeSnapshot,
    plan: &KnowledgeMergePlan,
) -> PocResult<KnowledgeSnapshot> {
    let observed_plan_hash = plan_binding_hash(plan)?;
    if observed_plan_hash != plan.plan_hash {
        return Err(PocError::Invalid(format!(
            "knowledge merge plan hash mismatch: expected {}, found {observed_plan_hash}",
            plan.plan_hash
        )));
    }
    if target.hash != plan.target_hash || source.hash != plan.source_hash {
        return Err(PocError::Conflict(
            "knowledge merge input moved after planning".to_string(),
        ));
    }
    let result = materialize_unchecked(target, source, plan)?;
    validate_result_accounting(target, source, &result, plan)?;
    if result.hash != plan.result_hash {
        return Err(PocError::Invalid(format!(
            "knowledge merge result hash mismatch: expected {}, found {}",
            plan.result_hash, result.hash
        )));
    }
    Ok(result)
}

fn validate_result_accounting(
    target: &KnowledgeSnapshot,
    source: &KnowledgeSnapshot,
    result: &KnowledgeSnapshot,
    plan: &KnowledgeMergePlan,
) -> PocResult<()> {
    if result.entities.len() != target.entities.len() + plan.added_entities()
        || plan.entity_actions.len() != source.entities.len()
        || plan.relation_actions.len() != source.relation_assertion_count()
    {
        return Err(PocError::Invalid(
            "knowledge merge source accounting is incomplete".to_string(),
        ));
    }

    let mut expected_contributions = collect_contributions(&target.entities)?;
    for (key, contribution) in collect_contributions(&source.entities)? {
        if expected_contributions
            .insert(key.clone(), contribution)
            .is_some()
        {
            return Err(PocError::Conflict(format!(
                "source contribution {key:?} is already materialized in target"
            )));
        }
    }
    if collect_contributions(&result.entities)? != expected_contributions {
        return Err(PocError::Invalid(
            "materialized entity contributions differ from target plus source".to_string(),
        ));
    }

    for (key, contributions) in &target.relations {
        if !result
            .relations
            .get(key)
            .is_some_and(|actual| contributions.is_subset(actual))
        {
            return Err(PocError::Invalid(
                "materialization lost a target relation assertion".to_string(),
            ));
        }
    }
    for action in &plan.relation_actions {
        if !result
            .relations
            .get(&action.target_key)
            .is_some_and(|actual| actual.contains(&action.contribution))
        {
            return Err(PocError::Invalid(
                "materialization lost a source relation assertion".to_string(),
            ));
        }
    }
    Ok(())
}

fn collect_contributions(
    entities: &BTreeMap<String, ScopedEntity>,
) -> PocResult<BTreeMap<String, EntityContribution>> {
    let mut contributions = BTreeMap::new();
    for entity in entities.values() {
        for (key, contribution) in &entity.contributions {
            if contributions
                .insert(key.clone(), contribution.clone())
                .is_some()
            {
                return Err(PocError::Conflict(format!(
                    "contribution {key:?} appears under multiple canonical entities"
                )));
            }
        }
    }
    Ok(contributions)
}

fn validate_resolution(
    target: &KnowledgeSnapshot,
    source: &KnowledgeSnapshot,
    candidates: &BTreeSet<CandidatePair>,
    resolution: &IdentityResolution,
) -> PocResult<()> {
    if !candidates.contains(&resolution.pair) {
        return Err(PocError::Invalid(format!(
            "identity resolution is not in the candidate set: {:?}",
            resolution.pair
        )));
    }
    let target_entity = target
        .entities
        .get(&resolution.pair.target)
        .ok_or_else(|| PocError::NotFound(format!("target entity {:?}", resolution.pair.target)))?;
    let source_entity = source
        .entities
        .get(&resolution.pair.source)
        .ok_or_else(|| PocError::NotFound(format!("source entity {:?}", resolution.pair.source)))?;
    if target_entity.revision_id != resolution.target_revision
        || source_entity.revision_id != resolution.source_revision
    {
        return Err(PocError::Conflict(format!(
            "stale identity resolution for {:?}",
            resolution.pair
        )));
    }
    if resolution.decision == IdentityDecision::Same
        && !matches!(
            resolution.authority,
            ResolutionAuthority::HumanReview { .. }
        )
    {
        return Err(PocError::Invalid(
            "same identity resolution requires human review".to_string(),
        ));
    }
    Ok(())
}

fn plan_relations(
    target: &KnowledgeSnapshot,
    source: &KnowledgeSnapshot,
    endpoint_map: &BTreeMap<String, String>,
) -> PocResult<Vec<RelationAction>> {
    let mut simulated = target.relations.clone();
    let mut actions = Vec::with_capacity(source.relation_assertion_count());
    for (source_key, contributions) in &source.relations {
        let target_key = RelationKey {
            subject: endpoint_map
                .get(&source_key.subject)
                .ok_or_else(|| PocError::Invalid("unmapped relation subject".to_string()))?
                .clone(),
            predicate: source_key.predicate.clone(),
            object: endpoint_map
                .get(&source_key.object)
                .ok_or_else(|| PocError::Invalid("unmapped relation object".to_string()))?
                .clone(),
        };
        for contribution in contributions {
            let existing = simulated.get(&target_key);
            let effect = match existing {
                Some(values) if values.contains(contribution) => RelationEffect::AlreadyPresent,
                Some(_) => RelationEffect::AddProvenance,
                None => RelationEffect::AddRelation,
            };
            simulated
                .entry(target_key.clone())
                .or_default()
                .insert(contribution.clone());
            actions.push(RelationAction {
                source_key: source_key.clone(),
                target_key: target_key.clone(),
                contribution: contribution.clone(),
                effect,
            });
        }
    }
    Ok(actions)
}

fn materialize_unchecked(
    target: &KnowledgeSnapshot,
    source: &KnowledgeSnapshot,
    plan: &KnowledgeMergePlan,
) -> PocResult<KnowledgeSnapshot> {
    let mut entities = target.entities.clone();
    for action in &plan.entity_actions {
        let source_entity = source
            .entities
            .get(action.source())
            .ok_or_else(|| PocError::NotFound(format!("source entity {:?}", action.source())))?;
        match action {
            EntityAction::Merge { target, .. } => {
                let target_entity = entities
                    .get_mut(target)
                    .ok_or_else(|| PocError::NotFound(format!("merge target entity {target:?}")))?;
                for (key, contribution) in &source_entity.contributions {
                    if let Some(existing) = target_entity.contributions.get(key) {
                        if existing != contribution {
                            return Err(PocError::Conflict(format!(
                                "contribution collision at {key:?}"
                            )));
                        }
                    } else {
                        target_entity
                            .contributions
                            .insert(key.clone(), contribution.clone());
                    }
                }
                target_entity.revision_id = merged_entity_revision(&target_entity.contributions)?;
            }
            EntityAction::Add { target, .. } => {
                let mut added = source_entity.clone();
                added.canonical_id.clone_from(target);
                if entities.insert(target.clone(), added).is_some() {
                    return Err(PocError::Conflict(format!(
                        "imported entity overwrites target {target:?}"
                    )));
                }
            }
        }
    }

    let mut relations = target.relations.clone();
    for action in &plan.relation_actions {
        relations
            .entry(action.target_key.clone())
            .or_default()
            .insert(action.contribution.clone());
    }
    KnowledgeSnapshot::from_parts(
        target.space_id.clone(),
        plan.result_revision.clone(),
        entities,
        relations,
    )
}

fn validate_local_entity(entity: &LocalEntity) -> PocResult<()> {
    validate_text("entity ID", &entity.logical_id, 512)?;
    validate_text("entity revision", &entity.revision_id, 512)?;
    validate_text("entity label", &entity.label, 512)?;
    validate_text("entity kind", &entity.kind, 128)?;
    if entity.description.len() > MAX_TEXT_BYTES
        || entity.source_refs.is_empty()
        || entity.aliases.len() > 64
        || entity.identifiers.len() > 64
        || entity
            .aliases
            .iter()
            .any(|alias| alias.trim().is_empty() || alias.len() > 512)
        || entity
            .identifiers
            .iter()
            .any(|(namespace, value)| namespace.trim().is_empty() || value.trim().is_empty())
        || entity
            .source_refs
            .iter()
            .any(|source| source.trim().is_empty() || source.len() > 2_048)
    {
        return Err(PocError::Invalid(format!(
            "malformed local entity {:?}",
            entity.logical_id
        )));
    }
    Ok(())
}

fn validate_relation(
    relation: &LocalRelation,
    entities: &BTreeMap<String, ScopedEntity>,
) -> PocResult<()> {
    if !entities.contains_key(&relation.subject)
        || !entities.contains_key(&relation.object)
        || relation.predicate.trim().is_empty()
        || relation.predicate.len() > 128
        || relation.source_ref.trim().is_empty()
        || relation.source_ref.len() > 2_048
    {
        return Err(PocError::Invalid(format!(
            "malformed or dangling relation {:?}",
            relation.predicate
        )));
    }
    Ok(())
}

fn validate_materialized_graph(
    entities: &BTreeMap<String, ScopedEntity>,
    relations: &BTreeMap<RelationKey, BTreeSet<RelationContribution>>,
) -> PocResult<()> {
    if entities.len() > MAX_ENTITIES
        || relations.values().map(BTreeSet::len).sum::<usize>() > MAX_RELATION_ASSERTIONS
    {
        return Err(PocError::Invalid(
            "materialized graph exceeds configured bounds".to_string(),
        ));
    }
    for (id, entity) in entities {
        if id != &entity.canonical_id || entity.contributions.is_empty() {
            return Err(PocError::Invalid(format!(
                "invalid canonical entity {id:?}"
            )));
        }
    }
    if relations.iter().any(|(key, assertions)| {
        assertions.is_empty()
            || !entities.contains_key(&key.subject)
            || !entities.contains_key(&key.object)
    }) {
        return Err(PocError::Invalid(
            "materialized graph has dangling or empty relations".to_string(),
        ));
    }
    Ok(())
}

fn merged_entity_revision(
    contributions: &BTreeMap<String, EntityContribution>,
) -> PocResult<String> {
    Ok(format!(
        "merge-{}",
        &hash_serializable(contributions)?[..16]
    ))
}

fn imported_entity_id(source_space: &str, source_id: &str) -> String {
    format!("{source_space}::{source_id}")
}

fn validate_text(name: &str, value: &str, max_len: usize) -> PocResult<()> {
    if value.trim().is_empty() || value.len() > max_len || value.contains(char::is_control) {
        return Err(PocError::Invalid(format!("malformed {name}")));
    }
    Ok(())
}

fn hash_serializable(value: &impl Serialize) -> PocResult<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
