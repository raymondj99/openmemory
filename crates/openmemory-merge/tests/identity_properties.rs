use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::{RevisionId, SpaceId};
use openmemory_merge::canonical::{AssertionTrust, CanonicalEntity};
use openmemory_merge::identity::{
    analyze_identity, ContextSignal, DeterministicIdentity, EntityAddress, EntityRevisionRef,
    IdentityEvidence, IdentityPacket, KindCompatibility,
};
use proptest::prelude::*;

fn reference(space: SpaceId, id: &str) -> EntityRevisionRef {
    let entity = CanonicalEntity::new(
        space,
        id.to_string(),
        RevisionId::new(),
        id.to_string(),
        BTreeSet::new(),
        "concept".to_string(),
        BTreeSet::new(),
        String::new(),
    )
    .unwrap();
    EntityRevisionRef {
        address: EntityAddress::new(space, id.to_string()).unwrap(),
        revision_id: entity.revision_id,
        semantic_hash: entity.semantic_hash,
        controlled_kind: entity.controlled_kind,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn labels_aliases_embeddings_and_claimed_ids_never_prove_same(
        label in "[A-Za-z0-9 _.-]{1,64}",
        choose_claim in any::<bool>(),
    ) {
        let left = reference(SpaceId::new(), "left");
        let right = reference(SpaceId::new(), "right");
        let evidence = if choose_claim {
            IdentityEvidence::VerifiedIdentifier {
                evidence_id: "claimed-id".to_string(),
                namespace: "registry".to_string(),
                uniqueness_scope: "global".to_string(),
                left_value: label.clone(),
                right_value: label,
                left_trust: AssertionTrust::Claimed,
                right_trust: AssertionTrust::Claimed,
                kind_compatibility: KindCompatibility::Compatible,
            }
        } else {
            IdentityEvidence::Context {
                evidence_id: "context".to_string(),
                signal: ContextSignal::Embedding,
                detail: label,
            }
        };
        let packet = IdentityPacket::new(left, right, 1, 1, BTreeMap::new(), vec![evidence]).unwrap();
        prop_assert_eq!(analyze_identity(&packet).unwrap(), DeterministicIdentity::Undetermined);
    }
}
