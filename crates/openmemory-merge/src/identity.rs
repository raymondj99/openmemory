//! Bounded evidence packets, deterministic proof analysis, and reviewed receipts.

use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::{PrincipalId, RevisionId, SpaceId};
use serde::{Deserialize, Serialize};

use crate::canonical::{AssertionTrust, OriginKey};
use crate::error::{MergeError, MergeResult};
use crate::hash::{CanonicalHasher, PacketHash, ReceiptHash, SemanticHash};

pub const MAX_IDENTITY_EVIDENCE: usize = 128;
pub const MAX_AGENT_RATIONALE_BYTES: usize = 2_048;

fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

/// A logical entity address is only unique together with its space.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityAddress {
    pub space_id: SpaceId,
    pub logical_id: String,
}

impl EntityAddress {
    pub fn new(space_id: SpaceId, logical_id: String) -> MergeResult<Self> {
        if !crate::canonical::valid_logical_id(&logical_id) {
            return Err(MergeError::InvalidField {
                field: "entity_address",
            });
        }
        Ok(Self {
            space_id,
            logical_id,
        })
    }
}

/// Immutable entity revision bound into an evidence packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityRevisionRef {
    pub address: EntityAddress,
    pub revision_id: RevisionId,
    pub semantic_hash: SemanticHash,
    pub controlled_kind: String,
}

impl EntityRevisionRef {
    fn validate(&self) -> MergeResult<()> {
        if !bounded(&self.controlled_kind, 128)
            || !crate::canonical::valid_logical_id(&self.address.logical_id)
        {
            return Err(MergeError::InvalidField {
                field: "entity_revision_ref",
            });
        }
        Ok(())
    }
}

/// Versioned controlled-kind compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindCompatibility {
    Compatible,
    Incompatible,
    Unknown,
}

/// Context-only signal which can rank a candidate but never prove identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSignal {
    Label,
    Alias,
    Description,
    Embedding,
    GraphNeighborhood,
    ClaimedIdentifier,
    DirectionalRelation,
    Timestamp,
    AgentAssertion,
}

/// Typed evidence. Its proof class is computed, never caller supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IdentityEvidence {
    Lineage {
        evidence_id: String,
        copied_from: OriginKey,
    },
    VerifiedIdentifier {
        evidence_id: String,
        namespace: String,
        uniqueness_scope: String,
        left_value: String,
        right_value: String,
        left_trust: AssertionTrust,
        right_trust: AssertionTrust,
        kind_compatibility: KindCompatibility,
    },
    ControlledKind {
        evidence_id: String,
        compatibility: KindCompatibility,
    },
    Context {
        evidence_id: String,
        signal: ContextSignal,
        detail: String,
    },
}

impl IdentityEvidence {
    #[must_use]
    pub fn evidence_id(&self) -> &str {
        match self {
            Self::Lineage { evidence_id, .. }
            | Self::VerifiedIdentifier { evidence_id, .. }
            | Self::ControlledKind { evidence_id, .. }
            | Self::Context { evidence_id, .. } => evidence_id,
        }
    }

    fn validate(&self, resolver_generations: &BTreeMap<String, u64>) -> MergeResult<()> {
        if !bounded(self.evidence_id(), 128) {
            return Err(MergeError::InvalidEvidence {
                reason: "invalid evidence identifier",
            });
        }
        match self {
            Self::Lineage { copied_from, .. } => copied_from.validate(),
            Self::VerifiedIdentifier {
                namespace,
                uniqueness_scope,
                left_value,
                right_value,
                left_trust,
                right_trust,
                ..
            } => {
                if !bounded(namespace, 128)
                    || !bounded(uniqueness_scope, 128)
                    || !bounded(left_value, 1_024)
                    || !bounded(right_value, 1_024)
                {
                    return Err(MergeError::InvalidEvidence {
                        reason: "identifier evidence field exceeds bounds",
                    });
                }
                for trust in [left_trust, right_trust] {
                    if let AssertionTrust::SourceVerified {
                        source_snapshot_id,
                        verifier_version,
                        resolver_generation,
                    } = trust
                    {
                        if !bounded(source_snapshot_id, 512)
                            || !bounded(verifier_version, 128)
                            || resolver_generations.get(namespace) != Some(resolver_generation)
                        {
                            return Err(MergeError::InvalidEvidence {
                                reason: "source verification is stale or unbound",
                            });
                        }
                    }
                }
                Ok(())
            }
            Self::ControlledKind { .. } => Ok(()),
            Self::Context { detail, .. } => {
                if detail.len() > 2_048 || detail.chars().any(char::is_control) {
                    return Err(MergeError::InvalidEvidence {
                        reason: "context detail exceeds bounds",
                    });
                }
                Ok(())
            }
        }
    }

    fn proof_class(&self) -> EvidenceClass {
        match self {
            Self::Lineage { .. } => EvidenceClass::ProofSame,
            Self::VerifiedIdentifier {
                left_value,
                right_value,
                left_trust,
                right_trust,
                kind_compatibility,
                ..
            } => {
                let both_verified = matches!(left_trust, AssertionTrust::SourceVerified { .. })
                    && matches!(right_trust, AssertionTrust::SourceVerified { .. });
                if !both_verified {
                    EvidenceClass::Context
                } else if left_value == right_value
                    && *kind_compatibility == KindCompatibility::Compatible
                {
                    EvidenceClass::ProofSame
                } else if left_value != right_value {
                    EvidenceClass::ProofDifferent
                } else {
                    EvidenceClass::Context
                }
            }
            Self::ControlledKind {
                compatibility: KindCompatibility::Incompatible,
                ..
            } => EvidenceClass::ProofDifferent,
            Self::ControlledKind { .. } | Self::Context { .. } => EvidenceClass::Context,
        }
    }
}

/// Derived trust class shown to reviewers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    ProofSame,
    ProofDifferent,
    Context,
}

/// Complete immutable packet sent to deterministic analysis or an optional agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityPacket {
    pub left: EntityRevisionRef,
    pub right: EntityRevisionRef,
    pub policy_generation: u64,
    pub ontology_generation: u64,
    pub resolver_generations: BTreeMap<String, u64>,
    pub evidence: Vec<IdentityEvidence>,
    pub binding_hash: PacketHash,
}

impl IdentityPacket {
    pub fn new(
        left: EntityRevisionRef,
        right: EntityRevisionRef,
        policy_generation: u64,
        ontology_generation: u64,
        resolver_generations: BTreeMap<String, u64>,
        mut evidence: Vec<IdentityEvidence>,
    ) -> MergeResult<Self> {
        evidence.sort_by(|a, b| a.evidence_id().cmp(b.evidence_id()));
        let mut packet = Self {
            left,
            right,
            policy_generation,
            ontology_generation,
            resolver_generations,
            evidence,
            binding_hash: PacketHash::from_bytes([0; 32]),
        };
        packet.validate_fields()?;
        packet.binding_hash = hash_packet(&packet);
        Ok(packet)
    }

    pub fn validate(&self) -> MergeResult<()> {
        self.validate_fields()?;
        if hash_packet(self) != self.binding_hash {
            return Err(MergeError::PacketHashMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> MergeResult<()> {
        self.left.validate()?;
        self.right.validate()?;
        if self.left.address.space_id == self.right.address.space_id {
            return Err(MergeError::SameSpace(self.left.address.space_id));
        }
        if self.policy_generation == 0
            || self.ontology_generation == 0
            || self.evidence.len() > MAX_IDENTITY_EVIDENCE
            || self.resolver_generations.values().any(|value| *value == 0)
        {
            return Err(MergeError::InvalidEvidence {
                reason: "packet generation or cardinality is invalid",
            });
        }
        let mut seen = BTreeSet::new();
        for item in &self.evidence {
            item.validate(&self.resolver_generations)?;
            if !seen.insert(item.evidence_id()) {
                return Err(MergeError::InvalidEvidence {
                    reason: "duplicate evidence identifier",
                });
            }
        }
        Ok(())
    }
}

fn hash_packet(packet: &IdentityPacket) -> PacketHash {
    let mut hasher = CanonicalHasher::new(b"openmemory/identity-packet/v1");
    hash_revision_ref(&mut hasher, &packet.left);
    hash_revision_ref(&mut hasher, &packet.right);
    hasher.u64(packet.policy_generation);
    hasher.u64(packet.ontology_generation);
    hasher.u64(packet.resolver_generations.len() as u64);
    for (namespace, generation) in &packet.resolver_generations {
        hasher.string(namespace);
        hasher.u64(*generation);
    }
    hasher.u64(packet.evidence.len() as u64);
    for evidence in &packet.evidence {
        hash_evidence(&mut hasher, evidence);
    }
    PacketHash::from_bytes(hasher.finish())
}

fn hash_revision_ref(hasher: &mut CanonicalHasher, reference: &EntityRevisionRef) {
    hasher.string(&reference.address.space_id.to_string());
    hasher.string(&reference.address.logical_id);
    hasher.string(&reference.revision_id.to_string());
    hasher.bytes(reference.semantic_hash.as_bytes());
    hasher.string(&reference.controlled_kind);
}

fn hash_trust(hasher: &mut CanonicalHasher, trust: &AssertionTrust) {
    match trust {
        AssertionTrust::Claimed => hasher.tag("claimed"),
        AssertionTrust::SourceVerified {
            source_snapshot_id,
            verifier_version,
            resolver_generation,
        } => {
            hasher.tag("source_verified");
            hasher.string(source_snapshot_id);
            hasher.string(verifier_version);
            hasher.u64(*resolver_generation);
        }
    }
}

fn hash_evidence(hasher: &mut CanonicalHasher, evidence: &IdentityEvidence) {
    hasher.string(evidence.evidence_id());
    match evidence {
        IdentityEvidence::Lineage { copied_from, .. } => {
            hasher.tag("lineage");
            hasher.string(&copied_from.space_id.to_string());
            hasher.string(&copied_from.logical_id);
            hasher.string(&copied_from.revision_id.to_string());
        }
        IdentityEvidence::VerifiedIdentifier {
            namespace,
            uniqueness_scope,
            left_value,
            right_value,
            left_trust,
            right_trust,
            kind_compatibility,
            ..
        } => {
            hasher.tag("verified_identifier");
            hasher.string(namespace);
            hasher.string(uniqueness_scope);
            hasher.string(left_value);
            hasher.string(right_value);
            hash_trust(hasher, left_trust);
            hash_trust(hasher, right_trust);
            hash_kind_compatibility(hasher, *kind_compatibility);
        }
        IdentityEvidence::ControlledKind { compatibility, .. } => {
            hasher.tag("controlled_kind");
            hash_kind_compatibility(hasher, *compatibility);
        }
        IdentityEvidence::Context { signal, detail, .. } => {
            hasher.tag("context");
            hasher.tag(match signal {
                ContextSignal::Label => "label",
                ContextSignal::Alias => "alias",
                ContextSignal::Description => "description",
                ContextSignal::Embedding => "embedding",
                ContextSignal::GraphNeighborhood => "graph_neighborhood",
                ContextSignal::ClaimedIdentifier => "claimed_identifier",
                ContextSignal::DirectionalRelation => "directional_relation",
                ContextSignal::Timestamp => "timestamp",
                ContextSignal::AgentAssertion => "agent_assertion",
            });
            hasher.string(detail);
        }
    }
}

fn hash_kind_compatibility(hasher: &mut CanonicalHasher, value: KindCompatibility) {
    hasher.tag(match value {
        KindCompatibility::Compatible => "compatible",
        KindCompatibility::Incompatible => "incompatible",
        KindCompatibility::Unknown => "unknown",
    });
}

/// Deterministic result before policy/human review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeterministicIdentity {
    Same,
    Different,
    Undetermined,
    ConflictingProofs,
}

/// Analyze current trusted evidence. Context-only packets never prove same.
pub fn analyze_identity(packet: &IdentityPacket) -> MergeResult<DeterministicIdentity> {
    packet.validate()?;
    let mut same = false;
    let mut different = false;
    for item in &packet.evidence {
        match item.proof_class() {
            EvidenceClass::ProofSame => same = true,
            EvidenceClass::ProofDifferent => different = true,
            EvidenceClass::Context => {}
        }
    }
    Ok(match (same, different) {
        (true, true) => DeterministicIdentity::ConflictingProofs,
        (true, false) => DeterministicIdentity::Same,
        (false, true) => DeterministicIdentity::Different,
        (false, false) => DeterministicIdentity::Undetermined,
    })
}

/// Candidate pair discovered by bounded retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityCandidate {
    pub candidate_id: String,
    pub left: EntityRevisionRef,
    pub right: EntityRevisionRef,
    pub packet_hash: PacketHash,
    pub policy_generation: u64,
}

impl IdentityCandidate {
    pub fn from_packet(candidate_id: String, packet: &IdentityPacket) -> MergeResult<Self> {
        if !bounded(&candidate_id, 128) {
            return Err(MergeError::InvalidField {
                field: "candidate_id",
            });
        }
        packet.validate()?;
        Ok(Self {
            candidate_id,
            left: packet.left.clone(),
            right: packet.right.clone(),
            packet_hash: packet.binding_hash,
            policy_generation: packet.policy_generation,
        })
    }
}

/// Authoritative planner decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityDecision {
    Same,
    Different,
    Undetermined,
}

/// Source of an authoritative receipt. Agent proposals are deliberately absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    Lineage,
    VerifiedIdentifier,
    Human,
}

/// Current revision- and packet-bound identity decision consumed by planning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityResolutionReceipt {
    pub candidate_id: String,
    pub left_revision: RevisionId,
    pub right_revision: RevisionId,
    pub packet_hash: PacketHash,
    pub decision_event_id: String,
    pub decision: IdentityDecision,
    pub decision_source: DecisionSource,
    pub policy_generation: u64,
    pub receipt_hash: ReceiptHash,
}

impl IdentityResolutionReceipt {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        candidate: &IdentityCandidate,
        decision_event_id: String,
        decision: IdentityDecision,
        decision_source: DecisionSource,
        policy_generation: u64,
    ) -> MergeResult<Self> {
        if !bounded(&decision_event_id, 128) || policy_generation == 0 {
            return Err(MergeError::InvalidField {
                field: "identity_receipt",
            });
        }
        let mut receipt = Self {
            candidate_id: candidate.candidate_id.clone(),
            left_revision: candidate.left.revision_id,
            right_revision: candidate.right.revision_id,
            packet_hash: candidate.packet_hash,
            decision_event_id,
            decision,
            decision_source,
            policy_generation,
            receipt_hash: ReceiptHash::from_bytes([0; 32]),
        };
        receipt.receipt_hash = hash_receipt(&receipt);
        receipt.validate(candidate)?;
        Ok(receipt)
    }

    pub fn validate(&self, candidate: &IdentityCandidate) -> MergeResult<()> {
        if self.candidate_id != candidate.candidate_id
            || self.left_revision != candidate.left.revision_id
            || self.right_revision != candidate.right.revision_id
            || self.packet_hash != candidate.packet_hash
            || self.policy_generation != candidate.policy_generation
        {
            return Err(MergeError::StaleIdentityReceipt);
        }
        if hash_receipt(self) != self.receipt_hash {
            return Err(MergeError::ReceiptHashMismatch);
        }
        Ok(())
    }
}

fn hash_receipt(receipt: &IdentityResolutionReceipt) -> ReceiptHash {
    let mut hasher = CanonicalHasher::new(b"openmemory/identity-receipt/v1");
    hasher.string(&receipt.candidate_id);
    hasher.string(&receipt.left_revision.to_string());
    hasher.string(&receipt.right_revision.to_string());
    hasher.bytes(receipt.packet_hash.as_bytes());
    hasher.string(&receipt.decision_event_id);
    hasher.tag(match receipt.decision {
        IdentityDecision::Same => "same",
        IdentityDecision::Different => "different",
        IdentityDecision::Undetermined => "undetermined",
    });
    hasher.tag(match receipt.decision_source {
        DecisionSource::Lineage => "lineage",
        DecisionSource::VerifiedIdentifier => "verified_identifier",
        DecisionSource::Human => "human",
    });
    hasher.u64(receipt.policy_generation);
    ReceiptHash::from_bytes(hasher.finish())
}

/// Strict, non-authoritative optional agent output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIdentityProposal {
    pub packet_hash: PacketHash,
    pub decision: IdentityDecision,
    pub cited_evidence_ids: BTreeSet<String>,
    pub rationale: String,
    pub model: String,
    pub prompt_version: String,
}

impl AgentIdentityProposal {
    pub fn validate(
        &self,
        packet: &IdentityPacket,
        deterministic: DeterministicIdentity,
    ) -> MergeResult<()> {
        packet.validate()?;
        if self.packet_hash != packet.binding_hash {
            return Err(MergeError::InvalidAgentProposal {
                reason: "wrong packet hash",
            });
        }
        if self.cited_evidence_ids.len() > MAX_IDENTITY_EVIDENCE
            || self
                .cited_evidence_ids
                .iter()
                .any(|id| !packet.evidence.iter().any(|item| item.evidence_id() == id))
        {
            return Err(MergeError::InvalidAgentProposal {
                reason: "unknown evidence citation",
            });
        }
        if !bounded(&self.rationale, MAX_AGENT_RATIONALE_BYTES)
            || !bounded(&self.model, 256)
            || !bounded(&self.prompt_version, 128)
        {
            return Err(MergeError::InvalidAgentProposal {
                reason: "proposal field exceeds bounds",
            });
        }
        let overrides_proof = matches!(
            (deterministic, self.decision),
            (
                DeterministicIdentity::Same | DeterministicIdentity::ConflictingProofs,
                IdentityDecision::Different
            ) | (
                DeterministicIdentity::Different | DeterministicIdentity::ConflictingProofs,
                IdentityDecision::Same
            )
        );
        if overrides_proof {
            return Err(MergeError::InvalidAgentProposal {
                reason: "agent cannot override trusted proof",
            });
        }
        Ok(())
    }
}

/// Human reviewer metadata is control-plane state, not part of the receipt hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanReview {
    pub reviewer: PrincipalId,
    pub authority_generation: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::CanonicalEntity;

    fn refs() -> (EntityRevisionRef, EntityRevisionRef) {
        let left_space = SpaceId::new();
        let right_space = SpaceId::new();
        let left = CanonicalEntity::new(
            left_space,
            "cerpheus".to_string(),
            RevisionId::new(),
            "cerpheus".to_string(),
            BTreeSet::new(),
            "concept".to_string(),
            BTreeSet::new(),
            String::new(),
        )
        .unwrap();
        let right = CanonicalEntity::new(
            right_space,
            "cerpheus".to_string(),
            RevisionId::new(),
            "cerpheus".to_string(),
            BTreeSet::new(),
            "concept".to_string(),
            BTreeSet::new(),
            String::new(),
        )
        .unwrap();
        (
            EntityRevisionRef {
                address: EntityAddress::new(left_space, left.logical_id.clone()).unwrap(),
                revision_id: left.revision_id,
                semantic_hash: left.semantic_hash,
                controlled_kind: left.controlled_kind,
            },
            EntityRevisionRef {
                address: EntityAddress::new(right_space, right.logical_id.clone()).unwrap(),
                revision_id: right.revision_id,
                semantic_hash: right.semantic_hash,
                controlled_kind: right.controlled_kind,
            },
        )
    }

    fn packet(evidence: Vec<IdentityEvidence>) -> IdentityPacket {
        let (left, right) = refs();
        IdentityPacket::new(left, right, 1, 1, BTreeMap::new(), evidence).unwrap()
    }

    #[test]
    fn context_only_homonym_never_proves_same() {
        let packet = packet(vec![IdentityEvidence::Context {
            evidence_id: "label".to_string(),
            signal: ContextSignal::Label,
            detail: "exact normalized label".to_string(),
        }]);
        assert_eq!(
            analyze_identity(&packet).unwrap(),
            DeterministicIdentity::Undetermined
        );
    }

    #[test]
    fn simultaneous_same_and_different_proof_conflicts() {
        let (left, right) = refs();
        let lineage = IdentityEvidence::Lineage {
            evidence_id: "lineage".to_string(),
            copied_from: OriginKey {
                space_id: left.address.space_id,
                logical_id: left.address.logical_id.clone(),
                revision_id: left.revision_id,
            },
        };
        let kind = IdentityEvidence::ControlledKind {
            evidence_id: "kind".to_string(),
            compatibility: KindCompatibility::Incompatible,
        };
        let packet =
            IdentityPacket::new(left, right, 1, 1, BTreeMap::new(), vec![lineage, kind]).unwrap();
        assert_eq!(
            analyze_identity(&packet).unwrap(),
            DeterministicIdentity::ConflictingProofs
        );
    }

    #[test]
    fn claimed_identifier_is_context_and_stale_verified_id_fails() {
        let claimed = IdentityEvidence::VerifiedIdentifier {
            evidence_id: "claimed".to_string(),
            namespace: "registry".to_string(),
            uniqueness_scope: "global".to_string(),
            left_value: "same".to_string(),
            right_value: "same".to_string(),
            left_trust: AssertionTrust::Claimed,
            right_trust: AssertionTrust::Claimed,
            kind_compatibility: KindCompatibility::Compatible,
        };
        assert_eq!(
            analyze_identity(&packet(vec![claimed])).unwrap(),
            DeterministicIdentity::Undetermined
        );

        let verified = AssertionTrust::SourceVerified {
            source_snapshot_id: "snapshot".to_string(),
            verifier_version: "v1".to_string(),
            resolver_generation: 2,
        };
        let evidence = IdentityEvidence::VerifiedIdentifier {
            evidence_id: "verified".to_string(),
            namespace: "registry".to_string(),
            uniqueness_scope: "global".to_string(),
            left_value: "same".to_string(),
            right_value: "same".to_string(),
            left_trust: verified.clone(),
            right_trust: verified,
            kind_compatibility: KindCompatibility::Compatible,
        };
        let (left, right) = refs();
        assert!(IdentityPacket::new(
            left,
            right,
            1,
            1,
            BTreeMap::from([("registry".to_string(), 1)]),
            vec![evidence]
        )
        .is_err());
    }
}
