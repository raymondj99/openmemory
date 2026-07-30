//! Conservative deterministic identity evidence.

use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::{RevisionId, SnapshotId};
use serde::Serialize;

use crate::canonical::CanonicalHasher;
use crate::hash::PacketHash;
use crate::model::{
    AssertionTrust, EntityAddress, EntityRecord, IdentifierAssertion, MAX_KIND_BYTES,
};
use crate::{MergeError, MergeErrorCode, MergeResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KindCompatibility {
    Compatible,
    Incompatible,
    Unknown,
}

/// Versioned ontology and unique-identifier rules supplied by product policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityPolicy {
    policy_generation: u64,
    ontology_generation: u64,
    authoritative_namespaces: BTreeMap<String, u64>,
    kind_relations: BTreeMap<(String, String), KindCompatibility>,
}

impl IdentityPolicy {
    pub fn new(policy_generation: u64, ontology_generation: u64) -> MergeResult<Self> {
        if policy_generation == 0 || ontology_generation == 0 {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "identity policy and ontology generations must be positive",
            ));
        }
        Ok(Self {
            policy_generation,
            ontology_generation,
            authoritative_namespaces: BTreeMap::new(),
            kind_relations: BTreeMap::new(),
        })
    }

    pub fn with_authoritative_namespace(
        mut self,
        namespace: impl Into<String>,
        resolver_generation: u64,
    ) -> MergeResult<Self> {
        let namespace = namespace.into();
        if !valid_policy_token(&namespace) || resolver_generation == 0 {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "authoritative namespace and resolver generation are invalid",
            ));
        }
        if self
            .authoritative_namespaces
            .insert(namespace, resolver_generation)
            .is_some()
        {
            return Err(MergeError::new(
                MergeErrorCode::DuplicateInput,
                "authoritative namespace is duplicated",
            ));
        }
        Ok(self)
    }

    pub fn with_kind_relation(
        mut self,
        left: impl Into<String>,
        right: impl Into<String>,
        relation: KindCompatibility,
    ) -> MergeResult<Self> {
        let left = left.into();
        let right = right.into();
        if !valid_policy_token(&left) || !valid_policy_token(&right) {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "kind relation contains invalid tokens",
            ));
        }
        let pair = canonical_kind_pair(&left, &right);
        if self.kind_relations.insert(pair, relation).is_some() {
            return Err(MergeError::new(
                MergeErrorCode::DuplicateInput,
                "kind relation is duplicated",
            ));
        }
        Ok(self)
    }

    #[must_use]
    pub const fn policy_generation(&self) -> u64 {
        self.policy_generation
    }

    #[must_use]
    pub const fn ontology_generation(&self) -> u64 {
        self.ontology_generation
    }

    #[must_use]
    pub fn resolver_generation(&self, namespace: &str) -> Option<u64> {
        self.authoritative_namespaces.get(namespace).copied()
    }

    #[must_use]
    pub fn kind_compatibility(&self, left: Option<&str>, right: Option<&str>) -> KindCompatibility {
        match (left, right) {
            (Some(left), Some(right)) if left == right => KindCompatibility::Compatible,
            (Some(left), Some(right)) => self
                .kind_relations
                .get(&canonical_kind_pair(left, right))
                .copied()
                .unwrap_or(KindCompatibility::Unknown),
            _ => KindCompatibility::Unknown,
        }
    }
}

fn valid_policy_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KIND_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn canonical_kind_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_string(), right.to_string())
    } else {
        (right.to_string(), left.to_string())
    }
}

/// Exact immutable entity revision participating in an identity packet.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct EntityRevisionRef {
    address: EntityAddress,
    revision: RevisionId,
    snapshot: SnapshotId,
}

impl EntityRevisionRef {
    #[must_use]
    pub const fn new(address: EntityAddress, revision: RevisionId, snapshot: SnapshotId) -> Self {
        Self {
            address,
            revision,
            snapshot,
        }
    }

    #[must_use]
    pub fn address(&self) -> &EntityAddress {
        &self.address
    }

    #[must_use]
    pub const fn revision(&self) -> RevisionId {
        self.revision
    }

    #[must_use]
    pub const fn snapshot(&self) -> SnapshotId {
        self.snapshot
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Lineage,
    VerifiedIdentifierMatch,
    VerifiedIdentifierConflict,
    ExactLabel,
    Alias,
    ClaimedIdentifier,
    KindIncompatible,
}

/// Canonically ordered evidence reference. Values are bounded by validated
/// entity records and never interpreted as authority by this structure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct EvidenceItem {
    kind: EvidenceKind,
    namespace: Option<String>,
    left: String,
    right: String,
}

impl EvidenceItem {
    fn new(
        kind: EvidenceKind,
        namespace: Option<String>,
        left: impl Into<String>,
        right: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            namespace,
            left: left.into(),
            right: right.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> EvidenceKind {
        self.kind
    }

    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeterministicResolution {
    ProofSame,
    ProofDifferent,
    ConflictingProofs,
    ReviewOrSeparate,
}

/// Immutable, generation-bound deterministic identity analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityPacket {
    left: EntityRevisionRef,
    right: EntityRevisionRef,
    policy_generation: u64,
    ontology_generation: u64,
    resolver_generations: BTreeMap<String, u64>,
    evidence: Vec<EvidenceItem>,
    resolution: DeterministicResolution,
    hash: PacketHash,
}

impl IdentityPacket {
    #[must_use]
    pub fn left(&self) -> &EntityRevisionRef {
        &self.left
    }

    #[must_use]
    pub fn right(&self) -> &EntityRevisionRef {
        &self.right
    }

    #[must_use]
    pub const fn policy_generation(&self) -> u64 {
        self.policy_generation
    }

    #[must_use]
    pub const fn ontology_generation(&self) -> u64 {
        self.ontology_generation
    }

    #[must_use]
    pub fn resolver_generations(&self) -> &BTreeMap<String, u64> {
        &self.resolver_generations
    }

    #[must_use]
    pub fn evidence(&self) -> &[EvidenceItem] {
        &self.evidence
    }

    #[must_use]
    pub const fn resolution(&self) -> DeterministicResolution {
        self.resolution
    }

    #[must_use]
    pub const fn hash(&self) -> PacketHash {
        self.hash
    }
}

/// Analyze an exact target/source revision pair using only conservative facts.
pub fn analyze_pair(
    left: &EntityRecord,
    left_snapshot: SnapshotId,
    right: &EntityRecord,
    right_snapshot: SnapshotId,
    policy: &IdentityPolicy,
) -> MergeResult<IdentityPacket> {
    if left.address() == right.address() {
        return Err(MergeError::new(
            MergeErrorCode::InvalidInput,
            "identity packet requires two distinct entity addresses",
        ));
    }

    let left_ref = EntityRevisionRef::new(left.address().clone(), left.revision(), left_snapshot);
    let right_ref =
        EntityRevisionRef::new(right.address().clone(), right.revision(), right_snapshot);
    let mut evidence = BTreeSet::new();
    let mut proves_same = false;
    let mut proves_different = false;
    let mut resolver_generations = BTreeMap::new();

    if let (Some(left_lineage), Some(right_lineage)) = (left.lineage(), right.lineage()) {
        if left_lineage == right_lineage {
            evidence.insert(EvidenceItem::new(
                EvidenceKind::Lineage,
                None,
                left_lineage,
                right_lineage,
            ));
            proves_same = true;
        }
    }

    let kind_compatibility = policy.kind_compatibility(left.kind(), right.kind());
    if kind_compatibility == KindCompatibility::Incompatible {
        evidence.insert(EvidenceItem::new(
            EvidenceKind::KindIncompatible,
            None,
            left.kind().unwrap_or_default(),
            right.kind().unwrap_or_default(),
        ));
    }

    for left_id in left.identifiers() {
        for right_id in right.identifiers() {
            if left_id.namespace() != right_id.namespace() {
                continue;
            }
            let namespace = left_id.namespace();
            let Some(required_generation) = policy.resolver_generation(namespace) else {
                if left_id.value() == right_id.value() {
                    evidence.insert(EvidenceItem::new(
                        EvidenceKind::ClaimedIdentifier,
                        Some(namespace.to_string()),
                        left_id.value(),
                        right_id.value(),
                    ));
                }
                continue;
            };
            resolver_generations.insert(namespace.to_string(), required_generation);
            let left_verified = is_current_verified(left_id, left_snapshot, required_generation);
            let right_verified = is_current_verified(right_id, right_snapshot, required_generation);
            if left_verified && right_verified {
                if left_id.value() == right_id.value()
                    && kind_compatibility == KindCompatibility::Compatible
                {
                    evidence.insert(EvidenceItem::new(
                        EvidenceKind::VerifiedIdentifierMatch,
                        Some(namespace.to_string()),
                        left_id.value(),
                        right_id.value(),
                    ));
                    proves_same = true;
                } else if left_id.value() != right_id.value() {
                    evidence.insert(EvidenceItem::new(
                        EvidenceKind::VerifiedIdentifierConflict,
                        Some(namespace.to_string()),
                        left_id.value(),
                        right_id.value(),
                    ));
                    proves_different = true;
                }
            } else if left_id.value() == right_id.value() {
                evidence.insert(EvidenceItem::new(
                    EvidenceKind::ClaimedIdentifier,
                    Some(namespace.to_string()),
                    left_id.value(),
                    right_id.value(),
                ));
            }
        }
    }

    if normalize_label(left.label()) == normalize_label(right.label()) {
        evidence.insert(EvidenceItem::new(
            EvidenceKind::ExactLabel,
            None,
            normalize_label(left.label()),
            normalize_label(right.label()),
        ));
    }
    let left_names = normalized_names(left);
    let right_names = normalized_names(right);
    if left_names
        .intersection(&right_names)
        .any(|name| name != &normalize_label(left.label()))
    {
        evidence.insert(EvidenceItem::new(
            EvidenceKind::Alias,
            None,
            left.label(),
            right.label(),
        ));
    }

    let resolution = match (proves_same, proves_different) {
        (true, true) => DeterministicResolution::ConflictingProofs,
        (true, false) => DeterministicResolution::ProofSame,
        (false, true) => DeterministicResolution::ProofDifferent,
        (false, false) => DeterministicResolution::ReviewOrSeparate,
    };
    let evidence = evidence.into_iter().collect::<Vec<_>>();
    let hash = hash_packet(
        &left_ref,
        &right_ref,
        policy,
        &resolver_generations,
        &evidence,
        resolution,
    );
    Ok(IdentityPacket {
        left: left_ref,
        right: right_ref,
        policy_generation: policy.policy_generation(),
        ontology_generation: policy.ontology_generation(),
        resolver_generations,
        evidence,
        resolution,
        hash,
    })
}

fn is_current_verified(
    assertion: &IdentifierAssertion,
    expected_snapshot: SnapshotId,
    required_generation: u64,
) -> bool {
    matches!(
        assertion.trust(),
        AssertionTrust::SourceVerified {
            source_snapshot,
            resolver_generation,
            ..
        } if *source_snapshot == expected_snapshot && *resolver_generation == required_generation
    )
}

#[must_use]
pub fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn normalized_names(entity: &EntityRecord) -> BTreeSet<String> {
    std::iter::once(normalize_label(entity.label()))
        .chain(entity.aliases().iter().map(|alias| normalize_label(alias)))
        .collect()
}

fn hash_packet(
    left: &EntityRevisionRef,
    right: &EntityRevisionRef,
    policy: &IdentityPolicy,
    resolver_generations: &BTreeMap<String, u64>,
    evidence: &[EvidenceItem],
    resolution: DeterministicResolution,
) -> PacketHash {
    let mut hash = CanonicalHasher::new(b"openmemory/identity-packet/v1");
    encode_revision_ref(&mut hash, left);
    encode_revision_ref(&mut hash, right);
    hash.u64(policy.policy_generation());
    hash.u64(policy.ontology_generation());
    hash.usize(resolver_generations.len());
    for (namespace, generation) in resolver_generations {
        hash.text(namespace);
        hash.u64(*generation);
    }
    hash.usize(evidence.len());
    for item in evidence {
        hash.u16(item.kind as u16);
        hash.optional_text(item.namespace.as_deref());
        hash.text(&item.left);
        hash.text(&item.right);
    }
    hash.u16(resolution as u16);
    hash.finish_packet()
}

pub(crate) fn encode_revision_ref(hash: &mut CanonicalHasher, value: &EntityRevisionRef) {
    crate::model::encode_address(hash, value.address());
    hash.revision_id(value.revision());
    hash.snapshot_id(value.snapshot());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::SemanticHash;
    use crate::model::{DomainGeneration, LogicalId, ObjectAddress, SpaceVersion};
    use openmemory_core::space::SpaceId;

    const LEFT_SPACE: &str = "018f6b7a-4d3c-7abc-8def-000000000001";
    const RIGHT_SPACE: &str = "018f6b7a-4d3c-7abc-8def-000000000002";
    const LEFT_SNAPSHOT: &str = "018f6b7a-4d3c-7abc-8def-000000000003";
    const RIGHT_SNAPSHOT: &str = "018f6b7a-4d3c-7abc-8def-000000000004";
    const LEFT_REVISION: &str = "018f6b7a-4d3c-7abc-8def-000000000005";
    const RIGHT_REVISION: &str = "018f6b7a-4d3c-7abc-8def-000000000006";

    fn record(
        space: &str,
        revision: &str,
        label: &str,
        snapshot: &str,
        value: &str,
        verified: bool,
    ) -> EntityRecord {
        let assertion = if verified {
            IdentifierAssertion::source_verified(
                "repo",
                value,
                snapshot.parse().unwrap(),
                "resolver-v1",
                7,
            )
            .unwrap()
        } else {
            IdentifierAssertion::claimed("repo", value).unwrap()
        };
        EntityRecord::new(
            ObjectAddress::new(
                space.parse::<SpaceId>().unwrap(),
                LogicalId::new("entity").unwrap(),
            ),
            revision.parse().unwrap(),
            label,
            Some("project"),
        )
        .unwrap()
        .with_identifiers([assertion])
        .unwrap()
    }

    fn policy() -> IdentityPolicy {
        IdentityPolicy::new(3, 4)
            .unwrap()
            .with_authoritative_namespace("repo", 7)
            .unwrap()
    }

    #[test]
    fn claimed_identifier_and_label_are_context_only() {
        let left = record(
            LEFT_SPACE,
            LEFT_REVISION,
            "same",
            LEFT_SNAPSHOT,
            "shared",
            false,
        );
        let right = record(
            RIGHT_SPACE,
            RIGHT_REVISION,
            "same",
            RIGHT_SNAPSHOT,
            "shared",
            false,
        );
        let packet = analyze_pair(
            &left,
            LEFT_SNAPSHOT.parse().unwrap(),
            &right,
            RIGHT_SNAPSHOT.parse().unwrap(),
            &policy(),
        )
        .unwrap();
        assert_eq!(
            packet.resolution(),
            DeterministicResolution::ReviewOrSeparate
        );
    }

    #[test]
    fn current_source_bound_unique_identifier_proves_same() {
        let left = record(
            LEFT_SPACE,
            LEFT_REVISION,
            "left",
            LEFT_SNAPSHOT,
            "shared",
            true,
        );
        let right = record(
            RIGHT_SPACE,
            RIGHT_REVISION,
            "right",
            RIGHT_SNAPSHOT,
            "shared",
            true,
        );
        let packet = analyze_pair(
            &left,
            LEFT_SNAPSHOT.parse().unwrap(),
            &right,
            RIGHT_SNAPSHOT.parse().unwrap(),
            &policy(),
        )
        .unwrap();
        assert_eq!(packet.resolution(), DeterministicResolution::ProofSame);
    }

    #[test]
    fn conflicting_current_unique_identifiers_prove_different() {
        let left = record(
            LEFT_SPACE,
            LEFT_REVISION,
            "same",
            LEFT_SNAPSHOT,
            "left-id",
            true,
        );
        let right = record(
            RIGHT_SPACE,
            RIGHT_REVISION,
            "same",
            RIGHT_SNAPSHOT,
            "right-id",
            true,
        );
        let packet = analyze_pair(
            &left,
            LEFT_SNAPSHOT.parse().unwrap(),
            &right,
            RIGHT_SNAPSHOT.parse().unwrap(),
            &policy(),
        )
        .unwrap();
        assert_eq!(packet.resolution(), DeterministicResolution::ProofDifferent);
    }

    #[test]
    fn stale_resolver_or_wrong_snapshot_never_proves_identity() {
        let left = record(
            LEFT_SPACE,
            LEFT_REVISION,
            "left",
            LEFT_SNAPSHOT,
            "shared",
            true,
        );
        let right = record(
            RIGHT_SPACE,
            RIGHT_REVISION,
            "right",
            RIGHT_SNAPSHOT,
            "shared",
            true,
        );
        let packet = analyze_pair(
            &left,
            RIGHT_SNAPSHOT.parse().unwrap(),
            &right,
            LEFT_SNAPSHOT.parse().unwrap(),
            &policy(),
        )
        .unwrap();
        assert_eq!(
            packet.resolution(),
            DeterministicResolution::ReviewOrSeparate
        );
    }

    #[test]
    fn policy_scaffolding_types_remain_constructible() {
        let _ = SpaceVersion::new(
            vec![DomainGeneration::new(0, 1, 1, 1)],
            SemanticHash::from_bytes([1; 32]),
        )
        .unwrap();
    }
}
