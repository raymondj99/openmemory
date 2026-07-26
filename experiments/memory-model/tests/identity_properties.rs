use openmemory_memory_model_poc::identity::{
    analyze_pair, analyze_pair_with_rules, validate_agent_proposal, AgentProposal, CandidateIndex,
    DeterministicResolution, EntityRecord, IdentifierAssertion, IdentityDecision, IdentityRules,
    PairKey, RelationAssertion,
};
use openmemory_memory_model_poc::PocError;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn a_shared_label_never_proves_identity(label in "[A-Za-z][A-Za-z0-9 ]{0,30}") {
        let left = EntityRecord::new("a", "left", "a-r1", label.clone());
        let right = EntityRecord::new("b", "right", "b-r1", label.to_uppercase());
        let packet = analyze_pair(&left, &right).expect("packet");
        prop_assert_eq!(packet.deterministic, DeterministicResolution::NeedsAgent);
    }

    #[test]
    fn pair_identity_is_symmetric(
        left_id in "[a-z]{1,20}",
        right_id in "[a-z]{1,20}",
    ) {
        let left = openmemory_memory_model_poc::identity::EntityId::new("a", left_id);
        let right = openmemory_memory_model_poc::identity::EntityId::new("b", right_id);
        prop_assert_eq!(
            PairKey::new(&left, &right).expect("forward"),
            PairKey::new(&right, &left).expect("reverse")
        );
    }

    #[test]
    fn complete_pair_analysis_is_symmetric(
        left_label in "[A-Za-z][A-Za-z0-9 ]{0,30}",
        right_label in "[A-Za-z][A-Za-z0-9 ]{0,30}",
    ) {
        let mut left = EntityRecord::new("a", "left", "a-r1", left_label);
        let mut right = EntityRecord::new("b", "right", "b-r1", right_label);
        left.aliases.insert("shared alias".to_string());
        right.aliases.insert("SHARED ALIAS".to_string());
        let rules = IdentityRules::new();
        prop_assert_eq!(
            analyze_pair_with_rules(&left, &right, &rules).expect("forward"),
            analyze_pair_with_rules(&right, &left, &rules).expect("reverse")
        );
    }

    #[test]
    fn an_agent_cannot_merge_across_an_authoritative_contradiction(
        left_value in "[a-z]{1,20}",
        right_value in "[a-z]{1,20}",
    ) {
        prop_assume!(left_value != right_value);
        let mut left = EntityRecord::new("a", "left", "a-r1", "Cerpheus");
        let mut right = EntityRecord::new("b", "right", "b-r1", "Cerpheus");
        left.source_refs.insert("source:left".to_string());
        right.source_refs.insert("source:right".to_string());
        left.identifier_assertions.insert(
            "repository".to_string(),
            IdentifierAssertion::source_verified(left_value, "source:left", "test-verifier"),
        );
        right.identifier_assertions.insert(
            "repository".to_string(),
            IdentifierAssertion::source_verified(right_value, "source:right", "test-verifier"),
        );
        let packet = analyze_pair(&left, &right).expect("packet");
        let proposal = AgentProposal {
            pair: packet.pair.clone(),
            packet_hash: packet.binding_hash().expect("packet hash"),
            first_revision: packet.first.revision_id.clone(),
            second_revision: packet.second.revision_id.clone(),
            recommendation: IdentityDecision::Same,
            evidence_ids: packet.evidence.iter().map(|item| item.id).collect(),
            relation_suggestions: Vec::new(),
            rationale: "same label".to_string(),
            model_id: "adversarial-agent".to_string(),
            prompt_version: "v1".to_string(),
        };
        prop_assert!(matches!(
            validate_agent_proposal(&packet, &proposal),
            Err(PocError::Conflict(_))
        ));
    }

    #[test]
    fn a_shared_alias_discovers_but_never_proves_identity(alias in "[A-Za-z][A-Za-z0-9 ]{0,30}") {
        let mut left = EntityRecord::new("a", "left", "a-r1", "unrelated-left-label");
        let mut right = EntityRecord::new("b", "right", "b-r1", "unrelated-right-label");
        left.aliases.insert(alias.clone());
        right.aliases.insert(alias.to_uppercase());
        let packet = analyze_pair_with_rules(&left, &right, &IdentityRules::new()).expect("packet");
        prop_assert_eq!(&packet.deterministic, &DeterministicResolution::NeedsAgent);
        prop_assert!(!packet.has_contradiction());
    }

    #[test]
    fn unknown_kind_pairs_are_never_hard_contradictions(
        left_kind in "[a-z]{1,20}",
        right_kind in "[a-z]{1,20}",
    ) {
        prop_assume!(left_kind != right_kind);
        let mut left = EntityRecord::new("a", "left", "a-r1", "shared");
        let mut right = EntityRecord::new("b", "right", "b-r1", "shared");
        left.kind = Some(left_kind);
        right.kind = Some(right_kind);
        let packet = analyze_pair_with_rules(&left, &right, &IdentityRules::new()).expect("packet");
        prop_assert!(!packet.has_contradiction());
        prop_assert_eq!(&packet.deterministic, &DeterministicResolution::NeedsAgent);
    }

    #[test]
    fn claimed_configured_identifiers_never_become_proof(
        value in "[A-Za-z0-9:/._-]{1,40}",
    ) {
        let mut left = EntityRecord::new("a", "left", "a-r1", "shared");
        let mut right = EntityRecord::new("b", "right", "b-r1", "shared");
        left.identifier_assertions.insert(
            "wikidata".to_string(),
            IdentifierAssertion::claimed(value.clone()),
        );
        right.identifier_assertions.insert(
            "wikidata".to_string(),
            IdentifierAssertion::claimed(value),
        );
        let rules = IdentityRules::new().with_authoritative_namespace("wikidata");
        let packet = analyze_pair_with_rules(&left, &right, &rules).expect("packet");
        prop_assert_eq!(&packet.deterministic, &DeterministicResolution::NeedsAgent);
        prop_assert!(!packet.has_contradiction());
    }

    #[test]
    fn verified_relation_candidate_discovery_is_normalization_and_order_stable(
        target_label in "[A-Za-z][A-Za-z0-9 ]{0,30}",
    ) {
        let mut query = EntityRecord::new("a", "query", "a-r1", "unrelated source label");
        query.source_refs.insert("source:relation".to_string());
        query.relation_assertions.push(
            RelationAssertion::source_verified(
                "named_after",
                target_label.clone(),
                "target_kind",
                "source:relation",
                "property-verifier",
            ).expect("valid relation"),
        );
        let mut target = EntityRecord::new("b", "target", "b-r1", target_label.to_uppercase());
        target.kind = Some("target_kind".to_string());
        let mut wrong_kind = EntityRecord::new("b", "wrong-kind", "b-r1", target_label);
        wrong_kind.kind = Some("other_kind".to_string());

        let mut forward = CandidateIndex::default();
        for record in [&query, &target, &wrong_kind] {
            forward.insert(record).expect("forward insert");
        }
        let mut reverse = CandidateIndex::default();
        for record in [&wrong_kind, &target, &query] {
            reverse.insert(record).expect("reverse insert");
        }
        let forward_batch = forward.candidates_for(&query, "b", 16).expect("forward query");
        let reverse_batch = reverse.candidates_for(&query, "b", 16).expect("reverse query");

        prop_assert_eq!(&forward_batch, &reverse_batch);
        prop_assert_eq!(forward_batch.candidates.len(), 1);
        prop_assert!(forward_batch.candidates.contains(&target.id));
    }

    #[test]
    fn claimed_relation_candidate_discovery_never_expands_scope(
        target_label in "[A-Za-z][A-Za-z0-9 ]{0,30}",
    ) {
        let mut query = EntityRecord::new("a", "query", "a-r1", "unrelated source label");
        query.relation_assertions.push(
            RelationAssertion::claimed("named_after", target_label.clone(), "target_kind")
                .expect("valid relation"),
        );
        let mut target = EntityRecord::new("b", "target", "b-r1", target_label);
        target.kind = Some("target_kind".to_string());
        let mut index = CandidateIndex::default();
        index.insert(&query).expect("insert query");
        index.insert(&target).expect("insert target");

        prop_assert!(index
            .candidates_for(&query, "b", 16)
            .expect("query")
            .candidates
            .is_empty());
    }
}
