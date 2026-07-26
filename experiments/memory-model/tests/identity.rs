use std::sync::{Arc, Barrier};

use openmemory_memory_model_poc::identity::{
    analyze_pair, analyze_pair_with_rules, route_agent_proposal, route_packet,
    validate_agent_proposal, AgentProposal, CandidateIndex, DeterministicResolution, EntityId,
    EntityRecord, IdentifierAssertion, IdentityDecision, IdentityLedger, IdentityPolicy,
    IdentityRules, KindCompatibility, KnownDecision, PolicyRoute, ProposalState, RelationAssertion,
    RelationSuggestion,
};
use openmemory_memory_model_poc::PocError;

fn entity(space: &str, logical_id: &str, revision: &str, label: &str) -> EntityRecord {
    EntityRecord::new(space, logical_id, revision, label)
}

fn add_verified_identifier(
    record: &mut EntityRecord,
    namespace: &str,
    value: &str,
    source_ref: &str,
) {
    record.source_refs.insert(source_ref.to_string());
    record.identifier_assertions.insert(
        namespace.to_string(),
        IdentifierAssertion::source_verified(value, source_ref, "test-verifier"),
    );
}

fn known(
    packet: &openmemory_memory_model_poc::identity::IdentityPacket,
    decision: IdentityDecision,
) -> KnownDecision {
    KnownDecision {
        decision,
        packet_hash: packet.binding_hash().expect("packet hash"),
    }
}

fn proposal(
    packet: &openmemory_memory_model_poc::identity::IdentityPacket,
    recommendation: IdentityDecision,
) -> AgentProposal {
    AgentProposal {
        pair: packet.pair.clone(),
        packet_hash: packet.binding_hash().expect("packet hash"),
        first_revision: packet.first.revision_id.clone(),
        second_revision: packet.second.revision_id.clone(),
        recommendation,
        evidence_ids: packet.evidence.iter().map(|item| item.id).collect(),
        relation_suggestions: Vec::new(),
        rationale: "The cited context supports this recommendation.".to_string(),
        model_id: "fixture-agent-v1".to_string(),
        prompt_version: "identity-v1".to_string(),
    }
}

#[test]
fn same_cerpheus_label_only_requests_agent_and_never_auto_merges() {
    let left = entity("project-a", "a-cerpheus", "a-r1", "Cerpheus");
    let right = entity("project-b", "b-cerpheus", "b-r1", "  cerpheus  ");
    let packet = analyze_pair(&left, &right).expect("analyze");

    assert_eq!(packet.deterministic, DeterministicResolution::NeedsAgent);
    assert_eq!(
        route_packet(&packet, IdentityPolicy::personal_default(), None),
        PolicyRoute::AskAgent
    );
    assert_eq!(
        route_packet(&packet, IdentityPolicy::team_default(), None),
        PolicyRoute::AskAgent
    );
}

#[test]
fn lineage_and_authoritative_ids_are_separate_policy_gates() {
    let mut lineage_left = entity("project-a", "a", "a-r1", "Cerpheus");
    let mut lineage_right = entity("team", "t", "t-r9", "Cerpheus Gateway");
    lineage_left.lineage_id = Some("origin-42".to_string());
    lineage_right.lineage_id = Some("origin-42".to_string());
    let lineage = analyze_pair(&lineage_left, &lineage_right).expect("lineage");
    assert_eq!(
        route_packet(&lineage, IdentityPolicy::team_default(), None),
        PolicyRoute::AutoApply(IdentityDecision::Same)
    );

    let mut id_left = entity("project-a", "a", "a-r1", "Cerpheus");
    let mut id_right = entity("project-b", "b", "b-r1", "Cerpheus");
    add_verified_identifier(
        &mut id_left,
        "repository",
        "github.com/acme/cerpheus",
        "source:left",
    );
    add_verified_identifier(
        &mut id_right,
        "repository",
        "github.com/acme/cerpheus",
        "source:right",
    );
    let authoritative = analyze_pair(&id_left, &id_right).expect("identifier");
    assert!(matches!(
        authoritative.deterministic,
        DeterministicResolution::ProvenSame { .. }
    ));
    assert_eq!(
        route_packet(&authoritative, IdentityPolicy::personal_default(), None),
        PolicyRoute::AutoApply(IdentityDecision::Same)
    );
    assert_eq!(
        route_packet(&authoritative, IdentityPolicy::team_default(), None),
        PolicyRoute::HumanReview(IdentityDecision::Same)
    );
}

#[test]
fn conflicting_unique_ids_keep_same_label_entities_separate() {
    let mut left = entity("project-a", "a", "a-r1", "Cerpheus");
    let mut right = entity("project-b", "b", "b-r1", "Cerpheus");
    add_verified_identifier(
        &mut left,
        "repository",
        "github.com/acme/cerpheus",
        "source:left",
    );
    add_verified_identifier(
        &mut right,
        "repository",
        "github.com/acme/cerpheus-simulator",
        "source:right",
    );
    let packet = analyze_pair(&left, &right).expect("analyze");

    assert!(matches!(
        packet.deterministic,
        DeterministicResolution::ProvenDifferent { .. }
    ));
    assert_eq!(
        route_packet(&packet, IdentityPolicy::team_default(), None),
        PolicyRoute::AutoApply(IdentityDecision::Different)
    );
}

#[test]
fn candidate_index_finds_renames_and_same_ids_without_all_pairs_scan() {
    let mut a = entity("project-a", "same-logical", "a-r1", "Cerpheus");
    a.lineage_id = Some("origin-1".to_string());
    let mut b = entity("project-b", "same-logical", "b-r1", "Cerpheus Gateway");
    b.lineage_id = Some("origin-1".to_string());
    let unrelated = entity("project-b", "unrelated", "u-r1", "Mercury");
    let mut index = CandidateIndex::default();
    index.insert(&a).expect("insert a");
    index.insert(&b).expect("insert b");
    index.insert(&unrelated).expect("insert unrelated");

    let candidates = index
        .candidates_for(&a, "project-b", 16)
        .expect("candidates");
    assert_eq!(candidates.candidates.len(), 1);
    assert!(!candidates.truncated);
    assert!(candidates.candidates.contains(&b.id));
}

#[test]
fn bounded_homonym_candidates_cannot_starve_a_verified_identifier_match() {
    let mut query = entity("project-a", "mercury", "a-r1", "Mercury");
    add_verified_identifier(&mut query, "wikidata", "Q308", "source:query");
    query.source_refs.insert("source:relation".to_string());
    query.relation_assertions.push(
        RelationAssertion::source_verified(
            "related_to",
            "Mercury",
            "astronomical_object",
            "source:relation",
            "test-verifier",
        )
        .expect("valid relation"),
    );
    let mut records = (0..300)
        .map(|index| {
            let mut record = entity(
                "project-b",
                &format!("homonym-{index:03}"),
                "b-r1",
                "Mercury",
            );
            record.kind = Some("astronomical_object".to_string());
            record
                .identifier_assertions
                .insert("wikidata".to_string(), IdentifierAssertion::claimed("Q308"));
            record
        })
        .collect::<Vec<_>>();
    let mut verified = entity("project-b", "zz-verified-planet", "verified-r1", "Mercury");
    verified.kind = Some("astronomical_object".to_string());
    add_verified_identifier(&mut verified, "wikidata", "Q308", "source:verified");
    records.push(verified.clone());

    let mut forward = CandidateIndex::default();
    forward.insert(&query).expect("insert query");
    for record in &records {
        forward.insert(record).expect("insert record");
    }
    let mut reverse = CandidateIndex::default();
    reverse.insert(&query).expect("insert query");
    for record in records.iter().rev() {
        reverse.insert(record).expect("insert record");
    }
    let forward_batch = forward
        .candidates_for(&query, "project-b", 16)
        .expect("bounded candidates");
    let reverse_batch = reverse
        .candidates_for(&query, "project-b", 16)
        .expect("bounded candidates");

    assert!(forward_batch.truncated);
    assert!(forward_batch.candidates.contains(&verified.id));
    assert_eq!(forward_batch, reverse_batch);
    assert!(forward.candidates_for(&query, "project-b", 0).is_err());
}

#[test]
fn verified_relation_target_discovers_a_non_homonymous_entity_by_label_and_kind() {
    let mut language = entity("project-a", "python-language", "a-r1", "Python");
    language.source_refs.insert("source:python".to_string());
    language.relation_assertions.push(
        RelationAssertion::source_verified(
            "named_after",
            "Monty Python",
            "comedy_troupe",
            "source:python",
            "test-verifier",
        )
        .expect("valid relation"),
    );
    let mut troupe = entity("project-b", "monty-python", "b-r1", "Monty Python");
    troupe.kind = Some("comedy_troupe".to_string());
    let mut wrong_kind = entity("project-b", "monty-python-film", "b-r1", "Monty Python");
    wrong_kind.kind = Some("film".to_string());

    let mut index = CandidateIndex::default();
    for record in [&language, &troupe, &wrong_kind] {
        index.insert(record).expect("insert");
    }
    let batch = index
        .candidates_for(&language, "project-b", 16)
        .expect("relation candidates");

    assert_eq!(batch.candidates, [troupe.id].into_iter().collect());
    assert!(!batch.truncated);
}

#[test]
fn claimed_or_unbound_relation_targets_cannot_create_candidates() {
    let mut claimed = entity("project-a", "claimed", "a-r1", "Unrelated source");
    claimed.relation_assertions.push(
        RelationAssertion::claimed("named_after", "Monty Python", "comedy_troupe")
            .expect("valid claim"),
    );
    let mut unbound = entity("project-a", "unbound", "a-r1", "Another source");
    unbound.relation_assertions.push(
        RelationAssertion::source_verified(
            "named_after",
            "Monty Python",
            "comedy_troupe",
            "source:not-bound-to-record",
            "test-verifier",
        )
        .expect("valid assertion"),
    );
    let mut troupe = entity("project-b", "monty-python", "b-r1", "Monty Python");
    troupe.kind = Some("comedy_troupe".to_string());

    let mut index = CandidateIndex::default();
    for record in [&claimed, &unbound, &troupe] {
        index.insert(record).expect("insert");
    }

    assert!(index
        .candidates_for(&claimed, "project-b", 16)
        .expect("claimed candidates")
        .candidates
        .is_empty());
    assert!(index
        .candidates_for(&unbound, "project-b", 16)
        .expect("unbound candidates")
        .candidates
        .is_empty());
}

#[test]
fn aliases_find_real_renames_but_remain_context_only() {
    let mut ceres = entity("project-a", "ceres-old", "a-r1", "Ceres");
    ceres.aliases.insert("1 Ceres".to_string());
    let mut numbered = entity("project-b", "ceres-new", "b-r1", "1 Ceres");
    numbered.aliases.insert("Ceres".to_string());
    let mut index = CandidateIndex::default();
    index.insert(&ceres).expect("insert Ceres");
    index.insert(&numbered).expect("insert numbered Ceres");

    assert!(index
        .candidates_for(&ceres, "project-b", 16)
        .expect("candidates")
        .candidates
        .contains(&numbered.id));
    let packet =
        analyze_pair_with_rules(&ceres, &numbered, &IdentityRules::new()).expect("analyze aliases");
    assert_eq!(packet.deterministic, DeterministicResolution::NeedsAgent);
}

#[test]
fn ontology_rules_allow_historical_classification_without_weakening_identity() {
    let mut historical = entity("project-a", "ceres-old", "a-r1", "Ceres");
    historical.kind = Some("asteroid".to_string());
    add_verified_identifier(&mut historical, "wikidata", "Q596", "source:historical");
    let mut modern = entity("project-b", "ceres-new", "b-r1", "1 Ceres");
    modern.kind = Some("dwarf_planet".to_string());
    add_verified_identifier(&mut modern, "wikidata", "Q596", "source:modern");
    let rules = IdentityRules::new()
        .with_authoritative_namespace("wikidata")
        .with_kind_relation("asteroid", "dwarf_planet", KindCompatibility::Compatible);
    let packet = analyze_pair_with_rules(&historical, &modern, &rules).expect("analyze");

    assert!(matches!(
        packet.deterministic,
        DeterministicResolution::ProvenSame { .. }
    ));
    assert!(!packet.has_contradiction());
}

#[test]
fn conflicting_trusted_proofs_bypass_the_agent_and_force_revalidation() {
    let mut left = entity("project-a", "left", "a-r1", "Cerpheus");
    let mut right = entity("project-b", "right", "b-r1", "Cerpheus renamed");
    left.lineage_id = Some("shared-origin".to_string());
    right.lineage_id = Some("shared-origin".to_string());
    add_verified_identifier(
        &mut left,
        "repository",
        "github.com/acme/cerpheus",
        "source:left",
    );
    add_verified_identifier(
        &mut right,
        "repository",
        "github.com/research/cerpheus",
        "source:right",
    );
    let packet = analyze_pair(&left, &right).expect("analyze");

    assert!(matches!(
        packet.deterministic,
        DeterministicResolution::ConflictingProofs { .. }
    ));
    assert_eq!(
        route_packet(&packet, IdentityPolicy::team_default(), None),
        PolicyRoute::HumanReview(IdentityDecision::Undetermined)
    );
    assert_eq!(
        route_packet(
            &packet,
            IdentityPolicy::team_default(),
            Some(&known(&packet, IdentityDecision::Same))
        ),
        PolicyRoute::Revalidate {
            known: IdentityDecision::Same,
            observed: IdentityDecision::Undetermined,
        }
    );
    assert!(matches!(
        validate_agent_proposal(&packet, &proposal(&packet, IdentityDecision::Different)),
        Err(PocError::Conflict(_))
    ));
}

#[test]
fn agent_cannot_reverse_deterministic_proof_even_without_a_contradiction() {
    let mut left = entity("project-a", "left", "a-r1", "Cerpheus");
    let mut right = entity("project-b", "right", "b-r1", "Renamed gateway");
    left.lineage_id = Some("shared-origin".to_string());
    right.lineage_id = Some("shared-origin".to_string());
    let packet = analyze_pair(&left, &right).expect("analyze");

    assert!(matches!(
        validate_agent_proposal(&packet, &proposal(&packet, IdentityDecision::Different)),
        Err(PocError::Conflict(_))
    ));
}

#[test]
fn proposal_is_bound_to_the_exact_evidence_policy_packet() {
    let left = entity("project-a", "left", "a-r1", "Cerpheus");
    let right = entity("project-b", "right", "b-r1", "Cerpheus");
    let policy_v1 = IdentityRules::new().with_policy_version("identity-policy-v1");
    let policy_v2 = IdentityRules::new().with_policy_version("identity-policy-v2");
    let first_packet = analyze_pair_with_rules(&left, &right, &policy_v1).expect("first packet");
    let second_packet = analyze_pair_with_rules(&left, &right, &policy_v2).expect("second packet");
    let old_proposal = proposal(&first_packet, IdentityDecision::Same);

    assert_ne!(
        first_packet.binding_hash().expect("first hash"),
        second_packet.binding_hash().expect("second hash")
    );
    assert!(matches!(
        validate_agent_proposal(&second_packet, &old_proposal),
        Err(PocError::Conflict(_))
    ));

    let directory = tempfile::tempdir().expect("tempdir");
    let ledger = IdentityLedger::open(directory.path()).expect("open ledger");
    let receipt = ledger
        .submit("policy-bound", &first_packet, &old_proposal)
        .expect("submit");
    let decision = ledger
        .review(
            &receipt.id,
            &first_packet,
            "reviewer",
            IdentityDecision::Same,
            "reviewed under v1",
        )
        .expect("review");
    assert!(ledger
        .proposal_for_revisions(&second_packet)
        .expect("lookup")
        .is_none());
    assert_eq!(
        route_packet(
            &second_packet,
            IdentityPolicy::team_default(),
            Some(&KnownDecision::from(&decision))
        ),
        PolicyRoute::Revalidate {
            known: IdentityDecision::Same,
            observed: IdentityDecision::Undetermined,
        }
    );
}

#[test]
fn unknown_namespaces_and_unknown_kind_pairs_never_become_proof() {
    let mut left = entity("project-a", "left", "a-r1", "Mercury");
    let mut right = entity("project-b", "right", "b-r1", "Mercury");
    left.kind = Some("local_planet_term".to_string());
    right.kind = Some("external_planet_term".to_string());
    left.identifier_assertions.insert(
        "unconfigured_url".to_string(),
        IdentifierAssertion::claimed("same"),
    );
    right.identifier_assertions.insert(
        "unconfigured_url".to_string(),
        IdentifierAssertion::claimed("same"),
    );
    let packet = analyze_pair_with_rules(&left, &right, &IdentityRules::new()).expect("analyze");

    assert_eq!(packet.deterministic, DeterministicResolution::NeedsAgent);
    assert!(!packet.has_contradiction());
    assert!(packet
        .evidence
        .iter()
        .any(|evidence| evidence.kind == "shared_non_authoritative_id"));
    assert!(packet
        .evidence
        .iter()
        .any(|evidence| evidence.kind == "unresolved_kind_relation"));
}

#[test]
fn a_verified_identifier_without_its_bound_source_is_downgraded_to_context() {
    let mut left = entity("project-a", "left", "a-r1", "Mercury");
    let mut right = entity("project-b", "right", "b-r1", "Mercury");
    left.identifier_assertions.insert(
        "wikidata".to_string(),
        IdentifierAssertion::source_verified("Q308", "missing:left", "fixture-verifier"),
    );
    right.identifier_assertions.insert(
        "wikidata".to_string(),
        IdentifierAssertion::source_verified("Q308", "missing:right", "fixture-verifier"),
    );
    let rules = IdentityRules::new().with_authoritative_namespace("wikidata");
    let packet = analyze_pair_with_rules(&left, &right, &rules).expect("analyze");

    assert_eq!(packet.deterministic, DeterministicResolution::NeedsAgent);
    assert!(packet
        .evidence
        .iter()
        .any(|evidence| evidence.kind == "shared_unverified_identifier"));
}

#[test]
fn oversized_packets_and_agent_outputs_fail_closed() {
    let mut oversized = entity("project-a", "left", "a-r1", "Mercury");
    for index in 0..65 {
        oversized.aliases.insert(format!("alias-{index}"));
    }
    let right = entity("project-b", "right", "b-r1", "Mercury");
    assert!(analyze_pair(&oversized, &right).is_err());
    assert!(CandidateIndex::default().insert(&oversized).is_err());

    let packet = analyze_pair(&entity("project-a", "left", "a-r1", "Mercury"), &right)
        .expect("bounded packet");
    let mut duplicate = proposal(&packet, IdentityDecision::Undetermined);
    duplicate.evidence_ids.push(duplicate.evidence_ids[0]);
    assert!(matches!(
        validate_agent_proposal(&packet, &duplicate),
        Err(PocError::Invalid(_))
    ));
}

#[test]
fn agent_output_is_cited_revision_bound_and_cannot_override_contradiction() {
    let mut left = entity("project-a", "a", "a-r1", "Cerpheus");
    let mut right = entity("project-b", "b", "b-r1", "Cerpheus");
    left.kind = Some("service".to_string());
    right.kind = Some("dataset".to_string());
    let packet = analyze_pair(&left, &right).expect("analyze");

    let same = proposal(&packet, IdentityDecision::Same);
    assert!(matches!(
        validate_agent_proposal(&packet, &same),
        Err(PocError::Conflict(_))
    ));

    let different = proposal(&packet, IdentityDecision::Different);
    assert_eq!(
        route_agent_proposal(&packet, &different).expect("valid recommendation"),
        PolicyRoute::HumanReview(IdentityDecision::Different)
    );

    let mut stale = different;
    stale.first_revision = "old-revision".to_string();
    assert!(matches!(
        validate_agent_proposal(&packet, &stale),
        Err(PocError::Conflict(_))
    ));
}

#[test]
fn distinct_but_related_proposals_are_structured_and_cannot_escape_the_pair() {
    let mut planet = entity("project-a", "mercury-planet", "a-r1", "Mercury");
    let mut deity = entity("project-b", "mercury-deity", "b-r1", "Mercury");
    planet.kind = Some("rocky_planet".to_string());
    deity.kind = Some("roman_deity".to_string());
    planet.source_refs.insert("source:wikipedia".to_string());
    planet.relation_assertions.push(
        RelationAssertion::source_verified(
            "named_after",
            "Mercury",
            "roman_deity",
            "source:wikipedia",
            "test-extractor",
        )
        .expect("valid relation assertion"),
    );
    let rules = IdentityRules::new().with_kind_relation(
        "rocky_planet",
        "roman_deity",
        KindCompatibility::Incompatible,
    );
    let packet = analyze_pair_with_rules(&planet, &deity, &rules).expect("analyze");
    let relation_evidence_ids = packet
        .evidence
        .iter()
        .filter(|evidence| evidence.relation.is_some())
        .map(|evidence| evidence.id)
        .collect::<Vec<_>>();
    let mut related = proposal(&packet, IdentityDecision::Different);
    related.relation_suggestions.push(RelationSuggestion {
        relation_type: "named_after".to_string(),
        subject: planet.id.clone(),
        object: deity.id.clone(),
        evidence_ids: relation_evidence_ids,
    });
    assert_eq!(
        route_agent_proposal(&packet, &related).expect("related proposal"),
        PolicyRoute::HumanReview(IdentityDecision::Different)
    );

    let mut escaped = related;
    escaped.relation_suggestions[0].object = EntityId::new("project-c", "unrelated");
    assert!(matches!(
        validate_agent_proposal(&packet, &escaped),
        Err(PocError::Invalid(_))
    ));
}

#[test]
fn agent_cannot_turn_an_unverified_relation_claim_into_a_reviewable_edge() {
    let mut planet = entity("project-a", "mercury-planet", "a-r1", "Mercury");
    let mut deity = entity("project-b", "mercury-deity", "b-r1", "Mercury");
    planet.kind = Some("rocky_planet".to_string());
    deity.kind = Some("roman_deity".to_string());
    planet.relation_assertions.push(
        RelationAssertion::claimed("named_after", "Mercury", "roman_deity").expect("valid claim"),
    );
    let rules = IdentityRules::new().with_kind_relation(
        "rocky_planet",
        "roman_deity",
        KindCompatibility::Incompatible,
    );
    let packet = analyze_pair_with_rules(&planet, &deity, &rules).expect("analyze");
    let mut proposal = proposal(&packet, IdentityDecision::Different);
    proposal.relation_suggestions.push(RelationSuggestion {
        relation_type: "named_after".to_string(),
        subject: planet.id,
        object: deity.id,
        evidence_ids: packet.evidence.iter().map(|evidence| evidence.id).collect(),
    });

    assert!(matches!(
        validate_agent_proposal(&packet, &proposal),
        Err(PocError::Invalid(_))
    ));
}

#[test]
fn an_undetermined_agent_does_not_block_or_mutate_the_merge() {
    let directory = tempfile::tempdir().expect("tempdir");
    let ledger = IdentityLedger::open(directory.path()).expect("open");
    let left = entity("project-a", "a", "a-r1", "Cerpheus");
    let right = entity("project-b", "b", "b-r1", "Cerpheus");
    let packet = analyze_pair(&left, &right).expect("analyze");
    let abstention = proposal(&packet, IdentityDecision::Undetermined);

    assert_eq!(
        route_agent_proposal(&packet, &abstention).expect("abstention"),
        PolicyRoute::KeepSeparate
    );
    let receipt = ledger
        .submit("abstention", &packet, &abstention)
        .expect("submit abstention");
    ledger
        .defer(
            &receipt.id,
            &packet,
            "identity-worker",
            "insufficient evidence",
        )
        .expect("defer");
    assert_eq!(
        ledger
            .proposal_for_revisions(&packet)
            .expect("lookup")
            .expect("cached")
            .state,
        ProposalState::Deferred
    );
    assert!(ledger
        .current_decision(&packet.pair)
        .expect("decision")
        .is_none());

    let mut changed_right = right;
    changed_right.revision_id = "b-r2".to_string();
    let changed_packet = analyze_pair(&left, &changed_right).expect("changed packet");
    assert!(ledger
        .proposal_for_revisions(&changed_packet)
        .expect("new lookup")
        .is_none());
}

#[test]
fn durable_review_is_idempotent_suppresses_future_prompts_and_reopens() {
    let directory = tempfile::tempdir().expect("tempdir");
    let ledger = IdentityLedger::open(directory.path()).expect("open");
    let left = entity("project-a", "a", "a-r1", "Cerpheus");
    let right = entity("project-b", "b", "b-r1", "Cerpheus");
    let packet = analyze_pair(&left, &right).expect("analyze");
    let agent = proposal(&packet, IdentityDecision::Same);

    let first = ledger
        .submit("cerpheus-review", &packet, &agent)
        .expect("submit");
    let retry = ledger
        .submit("cerpheus-review", &packet, &agent)
        .expect("retry");
    assert_eq!(first, retry);
    assert_eq!(first.state, ProposalState::Proposed);

    let decision = ledger
        .review(
            &first.id,
            &packet,
            "alice",
            IdentityDecision::Same,
            "same repository and service context",
        )
        .expect("review");
    let reopened = IdentityLedger::open(directory.path()).expect("reopen");
    let current = reopened
        .current_decision(&packet.pair)
        .expect("current")
        .expect("decision");
    assert_eq!(current, decision);
    let known_current = KnownDecision::from(&current);
    assert_eq!(
        route_packet(
            &packet,
            IdentityPolicy::team_default(),
            Some(&known_current)
        ),
        PolicyRoute::AlreadyDecided(IdentityDecision::Same)
    );
}

#[test]
fn stale_review_cannot_apply_after_an_entity_changes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let ledger = IdentityLedger::open(directory.path()).expect("open");
    let left = entity("project-a", "a", "a-r1", "Cerpheus");
    let right = entity("project-b", "b", "b-r1", "Cerpheus");
    let packet = analyze_pair(&left, &right).expect("analyze");
    let receipt = ledger
        .submit(
            "stale-review",
            &packet,
            &proposal(&packet, IdentityDecision::Same),
        )
        .expect("submit");
    let mut moved_right = right;
    moved_right.revision_id = "b-r2".to_string();
    let moved_packet = analyze_pair(&left, &moved_right).expect("moved");

    assert!(matches!(
        ledger.review(
            &receipt.id,
            &moved_packet,
            "alice",
            IdentityDecision::Same,
            "stale attempt"
        ),
        Err(PocError::Conflict(_))
    ));
    assert!(ledger
        .current_decision(&packet.pair)
        .expect("current")
        .is_none());
}

#[test]
fn decisions_are_reversible_without_erasing_audit_history() {
    let directory = tempfile::tempdir().expect("tempdir");
    let ledger = IdentityLedger::open(directory.path()).expect("open");
    let left = entity("project-a", "a", "a-r1", "Cerpheus");
    let right = entity("project-b", "b", "b-r1", "Cerpheus");
    let packet = analyze_pair(&left, &right).expect("analyze");
    let receipt = ledger
        .submit(
            "reversible",
            &packet,
            &proposal(&packet, IdentityDecision::Same),
        )
        .expect("submit");
    let same = ledger
        .review(
            &receipt.id,
            &packet,
            "alice",
            IdentityDecision::Same,
            "initial interpretation",
        )
        .expect("same");
    let different = ledger
        .revise(
            &packet.pair,
            &same.id,
            "bob",
            IdentityDecision::Different,
            "repositories were discovered to differ",
        )
        .expect("revise");

    assert_eq!(different.supersedes.as_deref(), Some(same.id.as_str()));
    assert_eq!(
        ledger
            .current_decision(&packet.pair)
            .expect("current")
            .expect("decision")
            .decision,
        IdentityDecision::Different
    );
    assert_eq!(
        ledger
            .decision_event_count(&packet.pair)
            .expect("event count"),
        2
    );
}

#[test]
fn new_hard_evidence_reopens_a_persisted_identity_decision() {
    let left = entity("project-a", "a", "a-r1", "Cerpheus");
    let right = entity("project-b", "b", "b-r1", "Cerpheus");
    let original = analyze_pair(&left, &right).expect("original");
    let known_same = known(&original, IdentityDecision::Same);
    assert_eq!(
        route_packet(&original, IdentityPolicy::team_default(), Some(&known_same)),
        PolicyRoute::AlreadyDecided(IdentityDecision::Same)
    );

    let mut changed_left = left;
    let mut changed_right = right;
    changed_left.revision_id = "a-r2".to_string();
    changed_right.revision_id = "b-r2".to_string();
    add_verified_identifier(
        &mut changed_left,
        "repository",
        "github.com/acme/cerpheus",
        "source:left",
    );
    add_verified_identifier(
        &mut changed_right,
        "repository",
        "github.com/research/cerpheus",
        "source:right",
    );
    let changed = analyze_pair(&changed_left, &changed_right).expect("changed");
    assert_eq!(
        route_packet(&changed, IdentityPolicy::team_default(), Some(&known_same)),
        PolicyRoute::Revalidate {
            known: IdentityDecision::Same,
            observed: IdentityDecision::Different,
        }
    );
}

#[test]
fn concurrent_reviewers_produce_one_decision_and_one_conflict() {
    let directory = tempfile::tempdir().expect("tempdir");
    let ledger = IdentityLedger::open(directory.path()).expect("open");
    let left = entity("project-a", "a", "a-r1", "Cerpheus");
    let right = entity("project-b", "b", "b-r1", "Cerpheus");
    let packet = analyze_pair(&left, &right).expect("analyze");
    let receipt = ledger
        .submit(
            "concurrent",
            &packet,
            &proposal(&packet, IdentityDecision::Same),
        )
        .expect("submit");
    let barrier = Arc::new(Barrier::new(3));
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (reviewer, decision) in [
            ("alice", IdentityDecision::Same),
            ("bob", IdentityDecision::Different),
        ] {
            let ledger = ledger.clone();
            let packet = packet.clone();
            let proposal_id = receipt.id.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(scope.spawn(move || {
                barrier.wait();
                ledger.review(
                    &proposal_id,
                    &packet,
                    reviewer,
                    decision,
                    "concurrent review",
                )
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("reviewer did not panic"))
            .collect::<Vec<_>>()
    });

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(PocError::Conflict(_))))
            .count(),
        1
    );
    assert_eq!(ledger.decision_event_count(&packet.pair).expect("count"), 1);
}
