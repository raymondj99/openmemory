//! Packet-bound identity and conservative keep-distinct receipts.

use openmemory_core::space::{DecisionEventId, SpaceRole};
use serde::Serialize;

use crate::canonical::CanonicalHasher;
use crate::discovery::{DiscoveryReceipt, IdentityCandidate};
use crate::evidence::{DeterministicResolution, EntityRevisionRef, IdentityPacket};
use crate::hash::{PacketHash, ReceiptHash};
use crate::{MergeError, MergeErrorCode, MergeResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityDecision {
    Same,
    Different,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolutionAuthority {
    Human {
        decision_event: DecisionEventId,
        role: SpaceRole,
    },
    SystemContradiction,
}

/// Current immutable decision for one exact candidate packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityResolutionReceipt {
    target: EntityRevisionRef,
    source: EntityRevisionRef,
    packet_hash: PacketHash,
    decision: IdentityDecision,
    policy_generation: u64,
    authority: ResolutionAuthority,
    hash: ReceiptHash,
}

impl IdentityResolutionReceipt {
    pub fn reviewed(
        packet: &IdentityPacket,
        decision: IdentityDecision,
        decision_event: DecisionEventId,
        role: SpaceRole,
    ) -> MergeResult<Self> {
        if !role.can_review() {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "identity decision requires Reviewer or Maintainer",
            ));
        }
        if decision == IdentityDecision::Same
            && matches!(
                packet.resolution(),
                DeterministicResolution::ProofDifferent
                    | DeterministicResolution::ConflictingProofs
            )
        {
            return Err(MergeError::new(
                MergeErrorCode::ConflictingResolution,
                "human review cannot override a deterministic contradiction",
            ));
        }
        Self::build(
            packet,
            decision,
            ResolutionAuthority::Human {
                decision_event,
                role,
            },
        )
    }

    pub fn system_different(packet: &IdentityPacket) -> MergeResult<Self> {
        if packet.resolution() != DeterministicResolution::ProofDifferent {
            return Err(MergeError::new(
                MergeErrorCode::ConflictingResolution,
                "system separation requires deterministic proof_different",
            ));
        }
        Self::build(
            packet,
            IdentityDecision::Different,
            ResolutionAuthority::SystemContradiction,
        )
    }

    fn build(
        packet: &IdentityPacket,
        decision: IdentityDecision,
        authority: ResolutionAuthority,
    ) -> MergeResult<Self> {
        let hash = hash_resolution(
            packet.left(),
            packet.right(),
            packet.hash(),
            decision,
            packet.policy_generation(),
            &authority,
        );
        Ok(Self {
            target: packet.left().clone(),
            source: packet.right().clone(),
            packet_hash: packet.hash(),
            decision,
            policy_generation: packet.policy_generation(),
            authority,
            hash,
        })
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
    pub const fn packet_hash(&self) -> PacketHash {
        self.packet_hash
    }

    #[must_use]
    pub const fn decision(&self) -> IdentityDecision {
        self.decision
    }

    #[must_use]
    pub const fn policy_generation(&self) -> u64 {
        self.policy_generation
    }

    #[must_use]
    pub fn authority(&self) -> &ResolutionAuthority {
        &self.authority
    }

    #[must_use]
    pub const fn hash(&self) -> ReceiptHash {
        self.hash
    }

    #[must_use]
    pub fn matches_candidate(&self, candidate: &IdentityCandidate) -> bool {
        self.target == *candidate.target() && self.source == *candidate.source()
    }
}

/// Conservative source-level disposition that consumes exactly one complete
/// discovery page without asserting ontological difference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeepDistinctReceipt {
    source: EntityRevisionRef,
    discovery_hash: ReceiptHash,
    policy_generation: u64,
    candidate_hashes: Vec<crate::hash::CandidateHash>,
    truncated: bool,
    hash: ReceiptHash,
}

impl KeepDistinctReceipt {
    #[must_use]
    pub fn new(discovery: &DiscoveryReceipt) -> Self {
        let candidate_hashes = discovery
            .candidates()
            .iter()
            .map(IdentityCandidate::hash)
            .collect::<Vec<_>>();
        let mut hash = CanonicalHasher::new(b"openmemory/keep-distinct-receipt/v1");
        crate::evidence::encode_revision_ref(&mut hash, discovery.source());
        hash.hash(discovery.hash().as_bytes());
        hash.u64(discovery.policy_generation());
        hash.usize(candidate_hashes.len());
        for candidate in &candidate_hashes {
            hash.hash(candidate.as_bytes());
        }
        hash.bool(discovery.truncated());
        let hash = hash.finish_receipt();
        Self {
            source: discovery.source().clone(),
            discovery_hash: discovery.hash(),
            policy_generation: discovery.policy_generation(),
            candidate_hashes,
            truncated: discovery.truncated(),
            hash,
        }
    }

    #[must_use]
    pub fn source(&self) -> &EntityRevisionRef {
        &self.source
    }

    #[must_use]
    pub const fn discovery_hash(&self) -> ReceiptHash {
        self.discovery_hash
    }

    #[must_use]
    pub const fn policy_generation(&self) -> u64 {
        self.policy_generation
    }

    #[must_use]
    pub fn candidate_hashes(&self) -> &[crate::hash::CandidateHash] {
        &self.candidate_hashes
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub const fn hash(&self) -> ReceiptHash {
        self.hash
    }

    #[must_use]
    pub fn matches_discovery(&self, discovery: &DiscoveryReceipt) -> bool {
        self.source == *discovery.source()
            && self.discovery_hash == discovery.hash()
            && self.policy_generation == discovery.policy_generation()
            && self.truncated == discovery.truncated()
            && self.candidate_hashes
                == discovery
                    .candidates()
                    .iter()
                    .map(IdentityCandidate::hash)
                    .collect::<Vec<_>>()
    }
}

fn hash_resolution(
    target: &EntityRevisionRef,
    source: &EntityRevisionRef,
    packet_hash: PacketHash,
    decision: IdentityDecision,
    policy_generation: u64,
    authority: &ResolutionAuthority,
) -> ReceiptHash {
    let mut hash = CanonicalHasher::new(b"openmemory/identity-receipt/v1");
    crate::evidence::encode_revision_ref(&mut hash, target);
    crate::evidence::encode_revision_ref(&mut hash, source);
    hash.hash(packet_hash.as_bytes());
    hash.u16(decision as u16);
    hash.u64(policy_generation);
    match authority {
        ResolutionAuthority::Human {
            decision_event,
            role,
        } => {
            hash.u16(1);
            hash.text(&decision_event.to_string());
            hash.u16(*role as u16);
        }
        ResolutionAuthority::SystemContradiction => hash.u16(2),
    }
    hash.finish_receipt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{analyze_pair, IdentityPolicy};
    use crate::model::{EntityRecord, LogicalId, ObjectAddress};
    use openmemory_core::space::{RevisionId, SnapshotId, SpaceId};

    fn entity(space: &str, revision: &str, label: &str) -> EntityRecord {
        EntityRecord::new(
            ObjectAddress::new(
                space.parse::<SpaceId>().unwrap(),
                LogicalId::new("entity").unwrap(),
            ),
            revision.parse::<RevisionId>().unwrap(),
            label,
            Some("concept"),
        )
        .unwrap()
    }

    #[test]
    fn agent_or_contributor_cannot_create_review_receipt() {
        let left = entity(
            "018f6b7a-4d3c-7abc-8def-000000000001",
            "018f6b7a-4d3c-7abc-8def-000000000003",
            "same",
        );
        let right = entity(
            "018f6b7a-4d3c-7abc-8def-000000000002",
            "018f6b7a-4d3c-7abc-8def-000000000004",
            "same",
        );
        let packet = analyze_pair(
            &left,
            "018f6b7a-4d3c-7abc-8def-000000000005"
                .parse::<SnapshotId>()
                .unwrap(),
            &right,
            "018f6b7a-4d3c-7abc-8def-000000000006"
                .parse::<SnapshotId>()
                .unwrap(),
            &IdentityPolicy::new(1, 1).unwrap(),
        )
        .unwrap();
        let result = IdentityResolutionReceipt::reviewed(
            &packet,
            IdentityDecision::Same,
            "018f6b7a-4d3c-7abc-8def-000000000007".parse().unwrap(),
            SpaceRole::Contributor,
        );
        assert_eq!(result.unwrap_err().code(), MergeErrorCode::InvalidInput);
    }
}
