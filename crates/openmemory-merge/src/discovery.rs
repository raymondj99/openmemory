//! Deterministic, bounded, target-scoped candidate discovery.

use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::SnapshotId;
use serde::Serialize;

use crate::canonical::CanonicalHasher;
use crate::evidence::{normalize_label, EntityRevisionRef, IdentityPolicy};
use crate::hash::{CandidateHash, ReceiptHash};
use crate::model::{AssertionTrust, EntityAddress, EntityRecord};
use crate::{MergeError, MergeErrorCode, MergeResult};

pub const DEFAULT_CANDIDATE_CAP: usize = 32;
pub const MAX_CANDIDATE_CAP: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSignal {
    Lineage,
    VerifiedIdentifier,
    ExactLabel,
    Alias,
}

/// Exact candidate pair and the conservative signals that discovered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityCandidate {
    target: EntityRevisionRef,
    source: EntityRevisionRef,
    signals: BTreeSet<CandidateSignal>,
    hash: CandidateHash,
}

impl IdentityCandidate {
    fn new(
        target: EntityRevisionRef,
        source: EntityRevisionRef,
        signals: BTreeSet<CandidateSignal>,
    ) -> Self {
        let mut hash = CanonicalHasher::new(b"openmemory/identity-candidate/v1");
        crate::evidence::encode_revision_ref(&mut hash, &target);
        crate::evidence::encode_revision_ref(&mut hash, &source);
        hash.usize(signals.len());
        for signal in &signals {
            hash.u16(*signal as u16);
        }
        let hash = hash.finish_candidate();
        Self {
            target,
            source,
            signals,
            hash,
        }
    }

    #[must_use]
    pub fn target(&self) -> &EntityRevisionRef {
        &self.target
    }

    #[must_use]
    pub fn source(&self) -> &EntityRevisionRef {
        &self.source
    }

    #[must_use]
    pub fn signals(&self) -> &BTreeSet<CandidateSignal> {
        &self.signals
    }

    #[must_use]
    pub const fn hash(&self) -> CandidateHash {
        self.hash
    }

    fn priority(&self) -> CandidateSignal {
        self.signals
            .iter()
            .next()
            .copied()
            .unwrap_or(CandidateSignal::Alias)
    }
}

/// Complete bounded discovery result for one source entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveryReceipt {
    discovery_generation: u64,
    policy_generation: u64,
    source: EntityRevisionRef,
    candidates: Vec<IdentityCandidate>,
    total_lower_bound: u64,
    cap: u16,
    truncated: bool,
    hash: ReceiptHash,
}

impl DiscoveryReceipt {
    #[must_use]
    pub const fn discovery_generation(&self) -> u64 {
        self.discovery_generation
    }

    #[must_use]
    pub const fn policy_generation(&self) -> u64 {
        self.policy_generation
    }

    #[must_use]
    pub fn source(&self) -> &EntityRevisionRef {
        &self.source
    }

    #[must_use]
    pub fn candidates(&self) -> &[IdentityCandidate] {
        &self.candidates
    }

    #[must_use]
    pub const fn total_lower_bound(&self) -> u64 {
        self.total_lower_bound
    }

    #[must_use]
    pub const fn cap(&self) -> u16 {
        self.cap
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub const fn hash(&self) -> ReceiptHash {
        self.hash
    }
}

/// Pure in-memory reference index. Phase 6 may specialize its storage shape,
/// but must preserve this receipt contract and forced-path equivalence.
#[derive(Debug, Clone)]
pub struct CandidateIndex {
    discovery_generation: u64,
    target_snapshot: SnapshotId,
    policy: IdentityPolicy,
    entities: BTreeMap<EntityAddress, EntityRecord>,
    lineage: BTreeMap<String, BTreeSet<EntityAddress>>,
    verified: BTreeMap<(String, String), BTreeSet<EntityAddress>>,
    labels: BTreeMap<(String, Option<String>), BTreeSet<EntityAddress>>,
    aliases: BTreeMap<(String, Option<String>), BTreeSet<EntityAddress>>,
}

impl CandidateIndex {
    pub fn build(
        discovery_generation: u64,
        target_snapshot: SnapshotId,
        policy: IdentityPolicy,
        entities: impl IntoIterator<Item = EntityRecord>,
    ) -> MergeResult<Self> {
        if discovery_generation == 0 {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "discovery generation must be positive",
            ));
        }
        let mut index = Self {
            discovery_generation,
            target_snapshot,
            policy,
            entities: BTreeMap::new(),
            lineage: BTreeMap::new(),
            verified: BTreeMap::new(),
            labels: BTreeMap::new(),
            aliases: BTreeMap::new(),
        };
        for entity in entities {
            let address = entity.address().clone();
            if index.entities.contains_key(&address) {
                return Err(MergeError::new(
                    MergeErrorCode::DuplicateInput,
                    "candidate index entity is duplicated",
                ));
            }
            if let Some(lineage) = entity.lineage() {
                index
                    .lineage
                    .entry(lineage.to_string())
                    .or_default()
                    .insert(address.clone());
            }
            for identifier in entity.identifiers() {
                if is_current_verified(
                    identifier.trust(),
                    target_snapshot,
                    index.policy.resolver_generation(identifier.namespace()),
                ) {
                    index
                        .verified
                        .entry((
                            identifier.namespace().to_string(),
                            identifier.value().to_string(),
                        ))
                        .or_default()
                        .insert(address.clone());
                }
            }
            let kind = entity.kind().map(str::to_string);
            index
                .labels
                .entry((normalize_label(entity.label()), kind.clone()))
                .or_default()
                .insert(address.clone());
            for alias in entity.aliases() {
                index
                    .aliases
                    .entry((normalize_label(alias), kind.clone()))
                    .or_default()
                    .insert(address.clone());
            }
            index.entities.insert(address, entity);
        }
        Ok(index)
    }

    pub fn discover(
        &self,
        source: &EntityRecord,
        source_snapshot: SnapshotId,
        cap: usize,
    ) -> MergeResult<DiscoveryReceipt> {
        if cap == 0 || cap > MAX_CANDIDATE_CAP {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "candidate cap must be within 1..=128",
            ));
        }
        let mut matches = BTreeMap::<EntityAddress, BTreeSet<CandidateSignal>>::new();
        if let Some(lineage) = source.lineage() {
            add_matches(
                &mut matches,
                self.lineage.get(lineage),
                CandidateSignal::Lineage,
            );
        }
        for identifier in source.identifiers() {
            if is_current_verified(
                identifier.trust(),
                source_snapshot,
                self.policy.resolver_generation(identifier.namespace()),
            ) {
                add_matches(
                    &mut matches,
                    self.verified.get(&(
                        identifier.namespace().to_string(),
                        identifier.value().to_string(),
                    )),
                    CandidateSignal::VerifiedIdentifier,
                );
            }
        }
        let kind = source.kind().map(str::to_string);
        add_matches(
            &mut matches,
            self.labels
                .get(&(normalize_label(source.label()), kind.clone())),
            CandidateSignal::ExactLabel,
        );
        for alias in source.aliases() {
            add_matches(
                &mut matches,
                self.labels.get(&(normalize_label(alias), kind.clone())),
                CandidateSignal::Alias,
            );
            add_matches(
                &mut matches,
                self.aliases.get(&(normalize_label(alias), kind.clone())),
                CandidateSignal::Alias,
            );
        }

        let source_ref =
            EntityRevisionRef::new(source.address().clone(), source.revision(), source_snapshot);
        let total_lower_bound = u64::try_from(matches.len()).unwrap_or(u64::MAX);
        let mut candidates = matches
            .into_iter()
            .filter_map(|(address, signals)| {
                let target = self.entities.get(&address)?;
                Some(IdentityCandidate::new(
                    EntityRevisionRef::new(
                        target.address().clone(),
                        target.revision(),
                        self.target_snapshot,
                    ),
                    source_ref.clone(),
                    signals,
                ))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.priority()
                .cmp(&right.priority())
                .then_with(|| left.target().address().cmp(right.target().address()))
        });
        let truncated = candidates.len() > cap;
        candidates.truncate(cap);
        let cap = u16::try_from(cap).unwrap_or(u16::MAX);
        let hash = hash_discovery(
            self.discovery_generation,
            self.policy.policy_generation(),
            &source_ref,
            &candidates,
            total_lower_bound,
            cap,
            truncated,
        );
        Ok(DiscoveryReceipt {
            discovery_generation: self.discovery_generation,
            policy_generation: self.policy.policy_generation(),
            source: source_ref,
            candidates,
            total_lower_bound,
            cap,
            truncated,
            hash,
        })
    }

    #[must_use]
    pub fn target(&self, address: &EntityAddress) -> Option<&EntityRecord> {
        self.entities.get(address)
    }
}

fn add_matches(
    output: &mut BTreeMap<EntityAddress, BTreeSet<CandidateSignal>>,
    matches: Option<&BTreeSet<EntityAddress>>,
    signal: CandidateSignal,
) {
    if let Some(matches) = matches {
        for address in matches {
            output.entry(address.clone()).or_default().insert(signal);
        }
    }
}

fn is_current_verified(
    trust: &AssertionTrust,
    snapshot: SnapshotId,
    required_generation: Option<u64>,
) -> bool {
    matches!(
        (trust, required_generation),
        (
            AssertionTrust::SourceVerified {
                source_snapshot,
                resolver_generation,
                ..
            },
            Some(required)
        ) if *source_snapshot == snapshot && *resolver_generation == required
    )
}

fn hash_discovery(
    discovery_generation: u64,
    policy_generation: u64,
    source: &EntityRevisionRef,
    candidates: &[IdentityCandidate],
    total_lower_bound: u64,
    cap: u16,
    truncated: bool,
) -> ReceiptHash {
    let mut hash = CanonicalHasher::new(b"openmemory/discovery-receipt/v1");
    hash.u64(discovery_generation);
    hash.u64(policy_generation);
    crate::evidence::encode_revision_ref(&mut hash, source);
    hash.usize(candidates.len());
    for candidate in candidates {
        hash.hash(candidate.hash().as_bytes());
    }
    hash.u64(total_lower_bound);
    hash.u16(cap);
    hash.bool(truncated);
    hash.finish_receipt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{IdentifierAssertion, LogicalId, ObjectAddress};
    use openmemory_core::space::{RevisionId, SpaceId};

    const TARGET_SPACE: &str = "018f6b7a-4d3c-7abc-8def-000000000001";
    const SOURCE_SPACE: &str = "018f6b7a-4d3c-7abc-8def-000000000002";
    const TARGET_SNAPSHOT: &str = "018f6b7a-4d3c-7abc-8def-000000000003";
    const SOURCE_SNAPSHOT: &str = "018f6b7a-4d3c-7abc-8def-000000000004";

    fn id(sequence: u64) -> RevisionId {
        format!("018f6b7a-4d3c-7abc-8def-{sequence:012x}")
            .parse()
            .unwrap()
    }

    fn entity(
        space: &str,
        logical: &str,
        revision: u64,
        label: &str,
        verified_id: Option<&str>,
        snapshot: &str,
    ) -> EntityRecord {
        let mut record = EntityRecord::new(
            ObjectAddress::new(
                space.parse::<SpaceId>().unwrap(),
                LogicalId::new(logical).unwrap(),
            ),
            id(revision),
            label,
            Some("project"),
        )
        .unwrap();
        if let Some(value) = verified_id {
            record = record
                .with_identifiers([IdentifierAssertion::source_verified(
                    "repo",
                    value,
                    snapshot.parse().unwrap(),
                    "resolver-v1",
                    9,
                )
                .unwrap()])
                .unwrap();
        }
        record
    }

    #[test]
    fn strong_evidence_precedes_homonyms_and_truncation_is_explicit() {
        let policy = IdentityPolicy::new(1, 1)
            .unwrap()
            .with_authoritative_namespace("repo", 9)
            .unwrap();
        let mut targets = (10..20)
            .map(|sequence| {
                entity(
                    TARGET_SPACE,
                    &format!("homonym-{sequence}"),
                    sequence,
                    "shared",
                    None,
                    TARGET_SNAPSHOT,
                )
            })
            .collect::<Vec<_>>();
        targets.push(entity(
            TARGET_SPACE,
            "verified",
            20,
            "other",
            Some("canonical"),
            TARGET_SNAPSHOT,
        ));
        let index =
            CandidateIndex::build(1, TARGET_SNAPSHOT.parse().unwrap(), policy, targets).unwrap();
        let source = entity(
            SOURCE_SPACE,
            "source",
            21,
            "shared",
            Some("canonical"),
            SOURCE_SNAPSHOT,
        );
        let receipt = index
            .discover(&source, SOURCE_SNAPSHOT.parse().unwrap(), 3)
            .unwrap();
        assert!(receipt.truncated());
        assert_eq!(
            receipt.candidates()[0].signals().iter().next(),
            Some(&CandidateSignal::VerifiedIdentifier)
        );
        assert_eq!(receipt.total_lower_bound(), 11);
    }

    #[test]
    fn duplicate_target_insertion_fails_atomically() {
        let policy = IdentityPolicy::new(1, 1).unwrap();
        let target = entity(TARGET_SPACE, "same", 5, "same", None, TARGET_SNAPSHOT);
        let result = CandidateIndex::build(
            1,
            TARGET_SNAPSHOT.parse().unwrap(),
            policy,
            [target.clone(), target],
        );
        assert_eq!(result.unwrap_err().code(), MergeErrorCode::DuplicateInput);
    }
}
