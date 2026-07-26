//! Source-grounded two-project fixtures for identity-resolution evaluation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::identity::{
    analyze_pair, analyze_pair_with_rules, normalize_label, route_agent_proposal, AgentProposal,
    CandidateIndex, EntityRecord, IdentifierAssertion, IdentityDecision, IdentityPacket,
    IdentityRules, KindCompatibility, PolicyRoute, RelationAssertion, RelationSuggestion,
};
use crate::{PocError, PocResult};

const HELIOS_FIXTURE_JSON: &str = include_str!("../fixtures/wikipedia_two_projects.json");
const JAVA_FIXTURE_JSON: &str = include_str!("../fixtures/wikipedia_java_projects.json");
const PYTHON_FIXTURE_JSON: &str = include_str!("../fixtures/wikipedia_python_projects.json");

#[derive(Debug, Deserialize)]
struct WikipediaFixture {
    retrieved_at: String,
    sources: Vec<SourceFixture>,
    spaces: Vec<SpaceFixture>,
    expectations: Vec<ExpectationFixture>,
}

#[derive(Debug, Deserialize)]
struct SourceFixture {
    id: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct SpaceFixture {
    space_id: String,
    project_name: String,
    records: Vec<RecordFixture>,
}

#[derive(Debug, Deserialize)]
struct RecordFixture {
    logical_id: String,
    revision_id: String,
    label: String,
    aliases: Vec<String>,
    kind: String,
    wikidata: Option<String>,
    description: String,
    neighbors: Vec<String>,
    sources: Vec<String>,
    #[serde(default)]
    relations: Vec<RelationFixture>,
}

#[derive(Debug, Deserialize)]
struct RelationFixture {
    relation_type: String,
    target_label: String,
    target_kind: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct ExpectationFixture {
    left: String,
    right: String,
    decision: String,
    relation: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LegacyWikipediaMetrics {
    pub expected_pairs: usize,
    pub label_only_candidates_found: usize,
    pub label_only_candidates_missed: usize,
    pub label_only_correct: usize,
    pub label_only_false_same: usize,
    pub label_only_false_different: usize,
    pub same_identity_blocked_by_string_kind_conflict: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RevisedWikipediaMetrics {
    pub expected_pairs: usize,
    pub indexed_candidates_found: usize,
    pub indexed_candidate_pairs_total: usize,
    pub unexpected_indexed_candidate_pairs: usize,
    pub source_relation_only_candidates_found: usize,
    pub deterministic_correct: usize,
    pub reviewed_correct_recommendations: usize,
    pub wrong: usize,
    pub validated_relation_suggestions: usize,
    pub grouped_relation_review_batches: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct WikipediaEvaluation {
    pub retrieved_at: String,
    pub project_names: Vec<String>,
    pub source_count: usize,
    pub legacy: LegacyWikipediaMetrics,
    pub revised: RevisedWikipediaMetrics,
    pub limitation: String,
}

struct LoadedFixture {
    fixture: WikipediaFixture,
    records: BTreeMap<String, EntityRecord>,
}

/// Evaluate the legacy and revised designs against the embedded Wikipedia
/// two-project corpus.
///
/// # Errors
///
/// Returns malformed fixture, invariant, proposal, or policy failures.
pub fn evaluate_wikipedia_fixture() -> PocResult<WikipediaEvaluation> {
    evaluate_fixture(
        HELIOS_FIXTURE_JSON,
        &wikipedia_rules(),
        "Wikipedia-derived paraphrases and curated decisions validate architecture, not production-model accuracy or reviewer usability.",
    )
}

/// Evaluate the Java language/island/coffee collision corpus.
///
/// # Errors
///
/// Returns malformed fixture, invariant, proposal, or policy failures.
pub fn evaluate_java_fixture() -> PocResult<WikipediaEvaluation> {
    evaluate_fixture(
        JAVA_FIXTURE_JSON,
        &java_rules(),
        "The Java corpus validates a rename, a homonym, and a namesake relation; it is a bounded architecture test, not a universal entity-linking benchmark.",
    )
}

/// Evaluate the Python language/genus/comedy-troupe collision corpus.
///
/// # Errors
///
/// Returns malformed fixture, invariant, proposal, or policy failures.
pub fn evaluate_python_fixture() -> PocResult<WikipediaEvaluation> {
    evaluate_fixture(
        PYTHON_FIXTURE_JSON,
        &python_rules(),
        "The Python corpus validates a rename, a homonym, and a non-homonymous namesake target; it is a bounded architecture test, not a universal entity-linking benchmark.",
    )
}

fn evaluate_fixture(
    fixture_json: &str,
    rules: &IdentityRules,
    limitation: &str,
) -> PocResult<WikipediaEvaluation> {
    let loaded = load_fixture(fixture_json)?;
    let legacy = evaluate_legacy(&loaded)?;
    let revised = evaluate_revised(&loaded, rules)?;
    Ok(WikipediaEvaluation {
        retrieved_at: loaded.fixture.retrieved_at.clone(),
        project_names: loaded
            .fixture
            .spaces
            .iter()
            .map(|space| space.project_name.clone())
            .collect(),
        source_count: loaded.fixture.sources.len(),
        legacy,
        revised,
        limitation: limitation.to_string(),
    })
}

fn load_fixture(fixture_json: &str) -> PocResult<LoadedFixture> {
    let fixture: WikipediaFixture = serde_json::from_str(fixture_json)?;
    if fixture.spaces.len() != 2 || fixture.retrieved_at.trim().is_empty() {
        return Err(PocError::Invalid(
            "Wikipedia fixture must contain two dated spaces".to_string(),
        ));
    }
    let sources = fixture
        .sources
        .iter()
        .map(|source| (source.id.clone(), source.url.clone()))
        .collect::<BTreeMap<_, _>>();
    if sources.len() != fixture.sources.len()
        || sources.values().any(|url| {
            !url.starts_with("https://en.wikipedia.org/wiki/") || url.contains(char::is_whitespace)
        })
    {
        return Err(PocError::Invalid(
            "Wikipedia fixture sources are duplicate or invalid".to_string(),
        ));
    }

    let mut records = BTreeMap::new();
    for space in &fixture.spaces {
        for source in &space.records {
            let mut record = EntityRecord::new(
                &space.space_id,
                &source.logical_id,
                &source.revision_id,
                &source.label,
            );
            record.aliases.extend(source.aliases.iter().cloned());
            record.kind = Some(source.kind.clone());
            record.description.clone_from(&source.description);
            record.neighbors.extend(source.neighbors.iter().cloned());
            for source_id in &source.sources {
                let url = sources.get(source_id).ok_or_else(|| {
                    PocError::Invalid(format!("unknown fixture source {source_id:?}"))
                })?;
                record.source_refs.insert(url.clone());
            }
            if let Some(wikidata) = &source.wikidata {
                let verification_source = record.source_refs.first().ok_or_else(|| {
                    PocError::Invalid("Wikidata assertion has no source".to_string())
                })?;
                record.identifier_assertions.insert(
                    "wikidata".to_string(),
                    IdentifierAssertion::source_verified(
                        wikidata,
                        verification_source,
                        "wikipedia-wikidata-link-fixture-loader-v1",
                    ),
                );
            }
            for relation in &source.relations {
                let source_ref = sources.get(&relation.source).ok_or_else(|| {
                    PocError::Invalid(format!("unknown relation source {:?}", relation.source))
                })?;
                record
                    .relation_assertions
                    .push(RelationAssertion::source_verified(
                        &relation.relation_type,
                        &relation.target_label,
                        &relation.target_kind,
                        source_ref,
                        "wikipedia-relation-fixture-loader-v1",
                    )?);
            }
            if records.insert(source.logical_id.clone(), record).is_some() {
                return Err(PocError::Invalid(format!(
                    "duplicate fixture logical ID {:?}",
                    source.logical_id
                )));
            }
        }
    }
    for expectation in &fixture.expectations {
        let left = records.get(&expectation.left).ok_or_else(|| {
            PocError::Invalid(format!("missing fixture entity {:?}", expectation.left))
        })?;
        let right = records.get(&expectation.right).ok_or_else(|| {
            PocError::Invalid(format!("missing fixture entity {:?}", expectation.right))
        })?;
        if left.id.space_id == right.id.space_id {
            return Err(PocError::Invalid(
                "Wikipedia expectation must cross spaces".to_string(),
            ));
        }
        parse_decision(&expectation.decision)?;
    }
    Ok(LoadedFixture { fixture, records })
}

fn evaluate_legacy(loaded: &LoadedFixture) -> PocResult<LegacyWikipediaMetrics> {
    let mut found = 0;
    let mut correct = 0;
    let mut false_same = 0;
    let mut false_different = 0;
    let mut blocked = 0;
    for expectation in &loaded.fixture.expectations {
        let left = &loaded.records[&expectation.left];
        let right = &loaded.records[&expectation.right];
        let expected = parse_decision(&expectation.decision)?;
        let labels_match = normalize_label(&left.label) == normalize_label(&right.label);
        if labels_match {
            found += 1;
        }
        let label_decision = if labels_match {
            IdentityDecision::Same
        } else {
            IdentityDecision::Different
        };
        if label_decision == expected {
            correct += 1;
        } else if label_decision == IdentityDecision::Same {
            false_same += 1;
        } else {
            false_different += 1;
        }

        if expected == IdentityDecision::Same {
            let packet = analyze_pair(left, right)?;
            let proposal = proposal_for(&packet, expected, None);
            if crate::identity::validate_agent_proposal(&packet, &proposal).is_err()
                && packet
                    .evidence
                    .iter()
                    .any(|evidence| evidence.kind == "incompatible_kind")
            {
                blocked += 1;
            }
        }
    }
    Ok(LegacyWikipediaMetrics {
        expected_pairs: loaded.fixture.expectations.len(),
        label_only_candidates_found: found,
        label_only_candidates_missed: loaded.fixture.expectations.len() - found,
        label_only_correct: correct,
        label_only_false_same: false_same,
        label_only_false_different: false_different,
        same_identity_blocked_by_string_kind_conflict: blocked,
    })
}

fn evaluate_revised(
    loaded: &LoadedFixture,
    rules: &IdentityRules,
) -> PocResult<RevisedWikipediaMetrics> {
    let mut index = CandidateIndex::default();
    for record in loaded.records.values() {
        index.insert(record)?;
    }
    let first_space = &loaded.fixture.spaces[0];
    let second_space = &loaded.fixture.spaces[1];
    let mut discovered_pairs = BTreeSet::new();
    for fixture_record in &first_space.records {
        let record = &loaded.records[&fixture_record.logical_id];
        let batch = index.candidates_for(record, &second_space.space_id, 64)?;
        if batch.truncated {
            return Err(PocError::Invalid(
                "Wikipedia candidate fixture unexpectedly exceeded its bound".to_string(),
            ));
        }
        discovered_pairs.extend(
            batch
                .candidates
                .into_iter()
                .map(|candidate| (record.id.logical_id.clone(), candidate.logical_id)),
        );
    }
    let expected_pairs = loaded
        .fixture
        .expectations
        .iter()
        .map(|expectation| (expectation.left.clone(), expectation.right.clone()))
        .collect::<BTreeSet<_>>();
    let mut found = 0;
    let mut relation_only_found = 0;
    let mut deterministic_correct = 0;
    let mut reviewed_correct = 0;
    let mut wrong = 0;
    let mut relation_suggestions = 0;
    let mut relation_batches = BTreeSet::new();

    for expectation in &loaded.fixture.expectations {
        let left = &loaded.records[&expectation.left];
        let right = &loaded.records[&expectation.right];
        let candidates = index.candidates_for(left, &right.id.space_id, 64)?;
        if candidates.candidates.contains(&right.id) {
            found += 1;
            let mut without_relations = left.clone();
            without_relations.relation_assertions.clear();
            if !index
                .candidates_for(&without_relations, &right.id.space_id, 64)?
                .candidates
                .contains(&right.id)
            {
                relation_only_found += 1;
            }
        }
        let expected = parse_decision(&expectation.decision)?;
        let packet = analyze_pair_with_rules(left, right, rules)?;
        if let Some(decision) = packet.deterministic.decision() {
            if decision == expected {
                deterministic_correct += 1;
            } else {
                wrong += 1;
            }
        } else {
            let proposal = proposal_for(&packet, expected, expectation.relation.as_deref());
            if route_agent_proposal(&packet, &proposal)? == PolicyRoute::HumanReview(expected) {
                reviewed_correct += 1;
            } else {
                wrong += 1;
            }
        }

        if let Some(relation) = &expectation.relation {
            let relation_proposal =
                proposal_for(&packet, IdentityDecision::Different, Some(relation));
            crate::identity::validate_agent_proposal(&packet, &relation_proposal)?;
            relation_suggestions += relation_proposal.relation_suggestions.len();
            relation_batches.insert(relation.clone());
        }
    }

    Ok(RevisedWikipediaMetrics {
        expected_pairs: loaded.fixture.expectations.len(),
        indexed_candidates_found: found,
        indexed_candidate_pairs_total: discovered_pairs.len(),
        unexpected_indexed_candidate_pairs: discovered_pairs.difference(&expected_pairs).count(),
        source_relation_only_candidates_found: relation_only_found,
        deterministic_correct,
        reviewed_correct_recommendations: reviewed_correct,
        wrong,
        validated_relation_suggestions: relation_suggestions,
        grouped_relation_review_batches: relation_batches.len(),
    })
}

fn wikipedia_rules() -> IdentityRules {
    IdentityRules::new()
        .with_policy_version("wikipedia-identity-policy-v2")
        .with_authoritative_namespace("wikidata")
        .with_kind_relation("asteroid", "dwarf_planet", KindCompatibility::Compatible)
        .with_kind_relation("planet", "dwarf_planet", KindCompatibility::Compatible)
        .with_kind_relation(
            "rocky_planet",
            "roman_deity",
            KindCompatibility::Incompatible,
        )
        .with_kind_relation("gas_giant", "roman_deity", KindCompatibility::Incompatible)
        .with_kind_relation("asteroid", "roman_deity", KindCompatibility::Incompatible)
        .with_kind_relation("planet", "classical_deity", KindCompatibility::Incompatible)
}

fn java_rules() -> IdentityRules {
    IdentityRules::new()
        .with_policy_version("java-identity-policy-v1")
        .with_authoritative_namespace("wikidata")
        .with_kind_relation(
            "programming_language",
            "island",
            KindCompatibility::Incompatible,
        )
        .with_kind_relation(
            "programming_language",
            "coffee_product",
            KindCompatibility::Incompatible,
        )
}

fn python_rules() -> IdentityRules {
    IdentityRules::new()
        .with_policy_version("python-identity-policy-v1")
        .with_authoritative_namespace("wikidata")
        .with_kind_relation(
            "programming_language",
            "snake_genus",
            KindCompatibility::Incompatible,
        )
        .with_kind_relation(
            "programming_language",
            "comedy_troupe",
            KindCompatibility::Incompatible,
        )
}

fn proposal_for(
    packet: &IdentityPacket,
    recommendation: IdentityDecision,
    relation: Option<&str>,
) -> AgentProposal {
    let evidence_ids = packet
        .evidence
        .iter()
        .map(|evidence| evidence.id)
        .collect::<Vec<_>>();
    let relation_suggestions = relation
        .into_iter()
        .map(|relation_type| {
            let relation_evidence = packet.evidence.iter().find(|evidence| {
                evidence
                    .relation
                    .as_ref()
                    .is_some_and(|binding| binding.relation_type == relation_type)
            });
            let (subject, object, relation_evidence_ids) = relation_evidence
                .and_then(|evidence| {
                    evidence.relation.as_ref().map(|binding| {
                        (
                            binding.subject.clone(),
                            binding.object.clone(),
                            vec![evidence.id],
                        )
                    })
                })
                .unwrap_or_else(|| {
                    (
                        packet.first.id.clone(),
                        packet.second.id.clone(),
                        evidence_ids.clone(),
                    )
                });
            RelationSuggestion {
                relation_type: relation_type.to_string(),
                subject,
                object,
                evidence_ids: relation_evidence_ids,
            }
        })
        .collect();
    AgentProposal {
        pair: packet.pair.clone(),
        packet_hash: packet.binding_hash().expect("fixture packet serializes"),
        first_revision: packet.first.revision_id.clone(),
        second_revision: packet.second.revision_id.clone(),
        recommendation,
        evidence_ids,
        relation_suggestions,
        rationale: "Curated from the cited Wikipedia context for architecture evaluation."
            .to_string(),
        model_id: "codex-curated-wikipedia-fixture-2026-07-18".to_string(),
        prompt_version: "wikipedia-two-projects-v1".to_string(),
    }
}

fn parse_decision(value: &str) -> PocResult<IdentityDecision> {
    match value {
        "same" => Ok(IdentityDecision::Same),
        "different" => Ok(IdentityDecision::Different),
        "undetermined" => Ok(IdentityDecision::Undetermined),
        other => Err(PocError::Invalid(format!(
            "unknown fixture decision {other:?}"
        ))),
    }
}
