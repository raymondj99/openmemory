//! Indexed, bounded, deterministic identity candidate discovery.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::canonical::{AssertionTrust, CanonicalEntity, CanonicalSpaceSnapshot};
use crate::error::{MergeError, MergeResult};
use crate::identity::{
    ContextSignal, EntityAddress, EntityRevisionRef, IdentityCandidate, IdentityEvidence,
    IdentityPacket, KindCompatibility,
};

pub const DEFAULT_CANDIDATE_CAP: usize = 32;
pub const HARD_CANDIDATE_CAP: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespacePolicy {
    pub namespace: String,
    pub uniqueness_scope: String,
    pub resolver_generation: u64,
    pub compatible_kinds: BTreeSet<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRecord {
    pub candidate: IdentityCandidate,
    pub packet: IdentityPacket,
    pub strongest_signal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidatePage {
    pub source_entity_id: String,
    pub candidates: Vec<CandidateRecord>,
    pub total_lower_bound: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateDiscovery {
    pub source_space_id: openmemory_core::space::SpaceId,
    pub target_space_id: openmemory_core::space::SpaceId,
    pub pages: Vec<CandidatePage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SignalRank {
    Semantic = 5,
    Alias = 4,
    Label = 3,
    VerifiedIdentifier = 2,
    Lineage = 1,
}

#[derive(Debug, Clone)]
struct Match {
    rank: SignalRank,
    evidence: Vec<IdentityEvidence>,
}

/// Discover candidates through bounded indexes. No source/target Cartesian
/// scan is performed, and lineage/verified identifiers sort ahead of labels.
pub fn discover_candidates(
    source: &CanonicalSpaceSnapshot,
    target: &CanonicalSpaceSnapshot,
    namespace_policies: &[NamespacePolicy],
    policy_generation: u64,
    ontology_generation: u64,
    cap: usize,
) -> MergeResult<CandidateDiscovery> {
    source.validate()?;
    target.validate()?;
    if source.space_id == target.space_id {
        return Err(MergeError::SameSpace(source.space_id));
    }
    if policy_generation == 0 || ontology_generation == 0 {
        return Err(MergeError::InvalidField {
            field: "candidate_generation",
        });
    }
    let cap = cap.clamp(1, HARD_CANDIDATE_CAP);
    let policies = validate_policies(namespace_policies)?;
    let indexes = TargetIndexes::new(target, &policies);
    let resolver_generations = policies
        .iter()
        .map(|(namespace, policy)| (namespace.clone(), policy.resolver_generation))
        .collect::<BTreeMap<_, _>>();
    let mut pages = Vec::with_capacity(source.entities.len());
    for source_entity in &source.entities {
        let mut matches = indexes.matches(source_entity, source.space_id, &policies);
        let total_lower_bound = matches.len();
        let truncated = total_lower_bound > cap;
        let selected = matches
            .iter_mut()
            .take(cap)
            .map(|(target_id, found)| {
                found
                    .evidence
                    .sort_by(|left, right| left.evidence_id().cmp(right.evidence_id()));
                found
                    .evidence
                    .dedup_by(|left, right| left.evidence_id() == right.evidence_id());
                let target_entity = target
                    .entity(target_id)
                    .ok_or(MergeError::CandidateCoverage)?;
                let packet = IdentityPacket::new(
                    revision_ref(target.space_id, target_entity)?,
                    revision_ref(source.space_id, source_entity)?,
                    policy_generation,
                    ontology_generation,
                    resolver_generations.clone(),
                    found.evidence.clone(),
                )?;
                let candidate_id = candidate_id(&packet);
                let candidate = IdentityCandidate::from_packet(candidate_id, &packet)?;
                Ok(CandidateRecord {
                    candidate,
                    packet,
                    strongest_signal: signal_name(found.rank).to_string(),
                })
            })
            .collect::<MergeResult<Vec<_>>>()?;
        pages.push(CandidatePage {
            source_entity_id: source_entity.logical_id.clone(),
            candidates: selected,
            total_lower_bound,
            truncated,
        });
    }
    Ok(CandidateDiscovery {
        source_space_id: source.space_id,
        target_space_id: target.space_id,
        pages,
    })
}

fn validate_policies(
    policies: &[NamespacePolicy],
) -> MergeResult<BTreeMap<String, NamespacePolicy>> {
    let mut result = BTreeMap::new();
    for policy in policies {
        if policy.namespace.is_empty()
            || policy.namespace.len() > 128
            || policy.uniqueness_scope.is_empty()
            || policy.uniqueness_scope.len() > 128
            || policy.resolver_generation == 0
            || result
                .insert(policy.namespace.clone(), policy.clone())
                .is_some()
        {
            return Err(MergeError::InvalidField {
                field: "namespace_policy",
            });
        }
    }
    Ok(result)
}

struct TargetIndexes {
    lineage: BTreeMap<
        (openmemory_core::space::SpaceId, String),
        BTreeMap<String, crate::canonical::OriginKey>,
    >,
    verified: BTreeMap<(String, String), BTreeSet<String>>,
    labels: BTreeMap<String, BTreeSet<String>>,
    aliases: BTreeMap<String, BTreeSet<String>>,
    tokens: BTreeMap<String, BTreeSet<String>>,
    entities: BTreeMap<String, CanonicalEntity>,
}

impl TargetIndexes {
    fn new(target: &CanonicalSpaceSnapshot, policies: &BTreeMap<String, NamespacePolicy>) -> Self {
        let mut indexes = Self {
            lineage: BTreeMap::new(),
            verified: BTreeMap::new(),
            labels: BTreeMap::new(),
            aliases: BTreeMap::new(),
            tokens: BTreeMap::new(),
            entities: BTreeMap::new(),
        };
        for entity in &target.entities {
            for origin in entity.contributions.keys() {
                indexes
                    .lineage
                    .entry((origin.space_id, origin.logical_id.clone()))
                    .or_default()
                    .insert(entity.logical_id.clone(), origin.clone());
            }
            for identifier in &entity.identifiers {
                if policies.contains_key(&identifier.namespace)
                    && matches!(identifier.trust, AssertionTrust::SourceVerified { .. })
                {
                    if let Some(canonical) = &identifier.canonical_value {
                        indexes
                            .verified
                            .entry((identifier.namespace.clone(), canonical.clone()))
                            .or_default()
                            .insert(entity.logical_id.clone());
                    }
                }
            }
            indexes
                .labels
                .entry(normalize(&entity.label))
                .or_default()
                .insert(entity.logical_id.clone());
            for alias in &entity.aliases {
                indexes
                    .aliases
                    .entry(normalize(alias))
                    .or_default()
                    .insert(entity.logical_id.clone());
            }
            for token in tokens(&entity.description) {
                indexes
                    .tokens
                    .entry(token)
                    .or_default()
                    .insert(entity.logical_id.clone());
            }
            indexes
                .entities
                .insert(entity.logical_id.clone(), entity.clone());
        }
        indexes
    }

    fn matches(
        &self,
        source: &CanonicalEntity,
        source_space: openmemory_core::space::SpaceId,
        policies: &BTreeMap<String, NamespacePolicy>,
    ) -> Vec<(String, Match)> {
        let mut found: BTreeMap<String, Match> = BTreeMap::new();
        if let Some(targets) = self.lineage.get(&(source_space, source.logical_id.clone())) {
            for (target, origin) in targets {
                add_match(
                    &mut found,
                    target,
                    SignalRank::Lineage,
                    IdentityEvidence::Lineage {
                        evidence_id: "lineage-index".to_string(),
                        copied_from: origin.clone(),
                    },
                );
            }
        }
        for identifier in &source.identifiers {
            let Some(policy) = policies.get(&identifier.namespace) else {
                continue;
            };
            let Some(canonical) = &identifier.canonical_value else {
                continue;
            };
            if !matches!(identifier.trust, AssertionTrust::SourceVerified { .. }) {
                continue;
            }
            if let Some(targets) = self
                .verified
                .get(&(identifier.namespace.clone(), canonical.clone()))
            {
                for target in targets {
                    let target_entity = self
                        .entities
                        .get(target)
                        .expect("candidate index refers to one target entity");
                    let target_identifier = target_entity
                        .identifiers
                        .iter()
                        .find(|candidate| {
                            candidate.namespace == identifier.namespace
                                && candidate.canonical_value.as_ref() == Some(canonical)
                                && matches!(candidate.trust, AssertionTrust::SourceVerified { .. })
                        })
                        .expect("verified index refers to matching assertion");
                    let compatibility = kind_compatibility(
                        &source.controlled_kind,
                        &target_entity.controlled_kind,
                        policy,
                    );
                    add_match(
                        &mut found,
                        target,
                        SignalRank::VerifiedIdentifier,
                        IdentityEvidence::VerifiedIdentifier {
                            evidence_id: format!("verified-index:{}", identifier.namespace),
                            namespace: identifier.namespace.clone(),
                            uniqueness_scope: policy.uniqueness_scope.clone(),
                            left_value: target_identifier
                                .canonical_value
                                .clone()
                                .expect("verified canonical value"),
                            right_value: canonical.clone(),
                            left_trust: target_identifier.trust.clone(),
                            right_trust: identifier.trust.clone(),
                            kind_compatibility: compatibility,
                        },
                    );
                }
            }
        }
        if let Some(targets) = self.labels.get(&normalize(&source.label)) {
            for target in targets {
                add_match(
                    &mut found,
                    target,
                    SignalRank::Label,
                    IdentityEvidence::Context {
                        evidence_id: "label".to_string(),
                        signal: ContextSignal::Label,
                        detail: "exact normalized label".to_string(),
                    },
                );
            }
        }
        for alias in &source.aliases {
            if let Some(targets) = self.aliases.get(&normalize(alias)) {
                for target in targets {
                    add_match(
                        &mut found,
                        target,
                        SignalRank::Alias,
                        IdentityEvidence::Context {
                            evidence_id: format!("alias:{}", normalize(alias)),
                            signal: ContextSignal::Alias,
                            detail: "exact normalized alias".to_string(),
                        },
                    );
                }
            }
        }
        let source_tokens = tokens(&source.description);
        let mut token_counts = BTreeMap::<String, usize>::new();
        for token in source_tokens.iter().take(64) {
            if let Some(targets) = self.tokens.get(token) {
                for target in targets.iter().take(HARD_CANDIDATE_CAP * 2) {
                    *token_counts.entry(target.clone()).or_default() += 1;
                }
            }
        }
        for (target, overlap) in token_counts {
            if overlap >= 2 {
                add_match(
                    &mut found,
                    &target,
                    SignalRank::Semantic,
                    IdentityEvidence::Context {
                        evidence_id: "description-tokens".to_string(),
                        signal: ContextSignal::Description,
                        detail: format!("{overlap} shared normalized description tokens"),
                    },
                );
            }
        }
        let mut found = found.into_iter().collect::<Vec<_>>();
        found.sort_by(|left, right| {
            left.1
                .rank
                .cmp(&right.1.rank)
                .then_with(|| left.0.cmp(&right.0))
        });
        found
    }
}

fn kind_compatibility(source: &str, target: &str, policy: &NamespacePolicy) -> KindCompatibility {
    if source == target
        || policy
            .compatible_kinds
            .contains(&(source.to_string(), target.to_string()))
        || policy
            .compatible_kinds
            .contains(&(target.to_string(), source.to_string()))
    {
        KindCompatibility::Compatible
    } else {
        KindCompatibility::Unknown
    }
}

fn add_match(
    matches: &mut BTreeMap<String, Match>,
    target: &str,
    rank: SignalRank,
    evidence: IdentityEvidence,
) {
    let entry = matches.entry(target.to_string()).or_insert_with(|| Match {
        rank,
        evidence: Vec::new(),
    });
    entry.rank = entry.rank.min(rank);
    entry.evidence.push(evidence);
}

fn revision_ref(
    space_id: openmemory_core::space::SpaceId,
    entity: &CanonicalEntity,
) -> MergeResult<EntityRevisionRef> {
    Ok(EntityRevisionRef {
        address: EntityAddress::new(space_id, entity.logical_id.clone())?,
        revision_id: entity.revision_id,
        semantic_hash: entity.semantic_hash,
        controlled_kind: entity.controlled_kind.clone(),
    })
}

fn candidate_id(packet: &IdentityPacket) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"openmemory/identity-candidate/v1\0");
    hasher.update(packet.binding_hash.as_bytes());
    format!("candidate:{}", hasher.finalize().to_hex())
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() >= 3)
        .take(256)
        .map(str::to_lowercase)
        .collect()
}

fn signal_name(rank: SignalRank) -> &'static str {
    match rank {
        SignalRank::Lineage => "lineage",
        SignalRank::VerifiedIdentifier => "verified_identifier",
        SignalRank::Label => "label",
        SignalRank::Alias => "alias",
        SignalRank::Semantic => "semantic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{CanonicalEntity, CanonicalSpaceSnapshot};
    use openmemory_core::space::{RevisionId, SpaceId};

    fn snapshot(space: SpaceId, labels: &[&str]) -> CanonicalSpaceSnapshot {
        CanonicalSpaceSnapshot::new(
            space,
            1,
            labels
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    CanonicalEntity::new(
                        space,
                        format!("entity-{index}"),
                        RevisionId::new(),
                        (*label).to_string(),
                        BTreeSet::new(),
                        "concept".to_string(),
                        BTreeSet::new(),
                        String::new(),
                    )
                    .unwrap()
                })
                .collect(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn repeated_homonyms_are_bounded_and_deterministic() {
        let source_space = SpaceId::new();
        let target_space = SpaceId::new();
        let source = snapshot(source_space, &["cerpheus"]);
        let target = snapshot(target_space, &vec!["cerpheus"; 200]);
        let first = discover_candidates(&source, &target, &[], 1, 1, 32).unwrap();
        let second = discover_candidates(&source, &target, &[], 1, 1, 32).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.pages[0].candidates.len(), 32);
        assert_eq!(first.pages[0].total_lower_bound, 200);
        assert!(first.pages[0].truncated);
    }
}
