use std::collections::BTreeSet;
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

use openmemory_memory_model_poc::identity::{
    analyze_pair, analyze_pair_with_rules, normalize_label, route_agent_proposal, route_packet,
    AgentProposal, CandidateIndex, EntityRecord, IdentifierAssertion, IdentityDecision,
    IdentityLedger, IdentityPacket, IdentityPolicy, IdentityRules, KindCompatibility,
    KnownDecision, PolicyRoute, RelationAssertion,
};
use openmemory_memory_model_poc::{PocError, PocResult};
use serde::Serialize;

const FIXTURES_PER_CLASS: usize = 8;
const INDEX_ENTITIES_PER_SPACE: usize = 40_000;
const LEDGER_SAMPLES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Truth {
    Same,
    Different,
    Ambiguous,
}

struct Scenario {
    packet: IdentityPacket,
    truth: Truth,
}

#[derive(Debug, Default, Serialize)]
struct StrategyMetrics {
    auto_correct: usize,
    auto_wrong: usize,
    human_review_correct_recommendations: usize,
    human_review_wrong_recommendations: usize,
    kept_separate_or_abstained: usize,
    agent_calls: usize,
    unsafe_ambiguous_merges: usize,
}

#[derive(Debug, Serialize)]
struct CorpusReport {
    total_pairs: usize,
    known_same: usize,
    known_different: usize,
    ambiguous: usize,
    name_only_control: StrategyMetrics,
    proof_gated_personal_default: StrategyMetrics,
    proof_gated_team_default: StrategyMetrics,
    second_pass_review_cards_after_persisting_decisions: usize,
    caveat: &'static str,
}

#[derive(Debug, Serialize)]
struct TimingStats {
    samples: usize,
    mean: u128,
    p50: u128,
    p95: u128,
    p99: u128,
    max: u128,
    unit: &'static str,
}

#[derive(Debug, Serialize)]
struct PerformanceReport {
    indexed_entities: usize,
    aliases_per_entity: usize,
    possible_all_pairs: u64,
    actual_candidates: usize,
    index_build_us: u128,
    indexed_candidate_lookup: TimingStats,
    deterministic_pair_analysis: TimingStats,
    durable_proposal_write: TimingStats,
    durable_review_write: TimingStats,
    ledger_bytes_after_samples: u64,
}

#[derive(Debug, Serialize)]
struct Report {
    corpus: CorpusReport,
    performance: PerformanceReport,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("identity evaluation failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> PocResult<()> {
    let scenarios = scenarios()?;
    let report = Report {
        corpus: evaluate_corpus(&scenarios)?,
        performance: measure_performance()?,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn evaluate_corpus(scenarios: &[Scenario]) -> PocResult<CorpusReport> {
    Ok(CorpusReport {
        total_pairs: scenarios.len(),
        known_same: count_truth(scenarios, Truth::Same),
        known_different: count_truth(scenarios, Truth::Different),
        ambiguous: count_truth(scenarios, Truth::Ambiguous),
        name_only_control: evaluate_name_only(scenarios),
        proof_gated_personal_default: evaluate_proof_gated(
            scenarios,
            IdentityPolicy::personal_default(),
        ),
        proof_gated_team_default: evaluate_proof_gated(scenarios, IdentityPolicy::team_default()),
        second_pass_review_cards_after_persisting_decisions: persist_and_count_second_pass(
            scenarios,
        )?,
        caveat: "Synthetic fixtures validate routing and safety, not real-model semantic accuracy.",
    })
}

fn persist_and_count_second_pass(scenarios: &[Scenario]) -> PocResult<usize> {
    let directory = tempfile::tempdir()?;
    let ledger = IdentityLedger::open(directory.path())?;
    let policy = IdentityPolicy::team_default();
    for (index, scenario) in scenarios.iter().enumerate() {
        match route_packet(&scenario.packet, policy, None) {
            PolicyRoute::HumanReview(decision) => {
                persist_review(
                    &ledger,
                    index,
                    &scenario.packet,
                    decision,
                    "deterministic recommendation reviewed",
                )?;
            }
            PolicyRoute::AskAgent => {
                let proposal = contextual_control_proposal(&scenario.packet);
                match route_agent_proposal(&scenario.packet, &proposal)? {
                    PolicyRoute::HumanReview(decision) => {
                        persist_review(
                            &ledger,
                            index,
                            &scenario.packet,
                            decision,
                            "agent recommendation reviewed",
                        )?;
                    }
                    PolicyRoute::KeepSeparate => {
                        let receipt = ledger.submit(
                            &format!("corpus-defer-{index}"),
                            &scenario.packet,
                            &proposal,
                        )?;
                        ledger.defer(
                            &receipt.id,
                            &scenario.packet,
                            "evaluation-worker",
                            "agent abstained",
                        )?;
                    }
                    other => panic!("unexpected persisted agent route {other:?}"),
                }
            }
            PolicyRoute::AutoApply(_)
            | PolicyRoute::KeepSeparate
            | PolicyRoute::AlreadyDecided(_)
            | PolicyRoute::Revalidate { .. } => {}
        }
    }

    let mut review_cards = 0;
    for scenario in scenarios {
        let current = ledger.current_decision(&scenario.packet.pair)?;
        let known = current.as_ref().map(KnownDecision::from);
        if matches!(
            route_packet(&scenario.packet, policy, known.as_ref()),
            PolicyRoute::Revalidate { .. }
        ) {
            review_cards += 1;
            continue;
        }
        if current.is_some() || ledger.proposal_for_revisions(&scenario.packet)?.is_some() {
            continue;
        }
        if matches!(
            route_packet(&scenario.packet, policy, None),
            PolicyRoute::HumanReview(_) | PolicyRoute::AskAgent | PolicyRoute::Revalidate { .. }
        ) {
            review_cards += 1;
        }
    }
    Ok(review_cards)
}

fn persist_review(
    ledger: &IdentityLedger,
    index: usize,
    packet: &IdentityPacket,
    decision: IdentityDecision,
    reason: &str,
) -> PocResult<()> {
    let proposal = proposal_with_decision(packet, decision);
    let receipt = ledger.submit(&format!("corpus-review-{index}"), packet, &proposal)?;
    ledger.review(&receipt.id, packet, "evaluation-reviewer", decision, reason)?;
    Ok(())
}

fn proposal_with_decision(
    packet: &IdentityPacket,
    recommendation: IdentityDecision,
) -> AgentProposal {
    AgentProposal {
        pair: packet.pair.clone(),
        packet_hash: packet.binding_hash().expect("packet serializes"),
        first_revision: packet.first.revision_id.clone(),
        second_revision: packet.second.revision_id.clone(),
        recommendation,
        evidence_ids: packet.evidence.iter().map(|evidence| evidence.id).collect(),
        relation_suggestions: Vec::new(),
        rationale: "Structured recommendation persisted for review.".to_string(),
        model_id: "identity-evaluation-router-v1".to_string(),
        prompt_version: "identity-contract-v1".to_string(),
    }
}

fn count_truth(scenarios: &[Scenario], truth: Truth) -> usize {
    scenarios
        .iter()
        .filter(|scenario| scenario.truth == truth)
        .count()
}

fn evaluate_name_only(scenarios: &[Scenario]) -> StrategyMetrics {
    let mut metrics = StrategyMetrics::default();
    for scenario in scenarios {
        let decision = if normalize_label(&scenario.packet.first.label)
            == normalize_label(&scenario.packet.second.label)
        {
            IdentityDecision::Same
        } else {
            IdentityDecision::Different
        };
        record_automatic(&mut metrics, decision, scenario.truth);
    }
    metrics
}

fn evaluate_proof_gated(scenarios: &[Scenario], policy: IdentityPolicy) -> StrategyMetrics {
    let mut metrics = StrategyMetrics::default();
    for scenario in scenarios {
        match route_packet(&scenario.packet, policy, None) {
            PolicyRoute::AutoApply(decision) => {
                record_automatic(&mut metrics, decision, scenario.truth);
            }
            PolicyRoute::HumanReview(decision) => {
                record_review(&mut metrics, decision, scenario.truth);
            }
            PolicyRoute::AskAgent => {
                metrics.agent_calls += 1;
                let proposal = contextual_control_proposal(&scenario.packet);
                let route = route_agent_proposal(&scenario.packet, &proposal)
                    .expect("control proposal must validate");
                match route {
                    PolicyRoute::HumanReview(decision) => {
                        record_review(&mut metrics, decision, scenario.truth);
                    }
                    PolicyRoute::KeepSeparate => metrics.kept_separate_or_abstained += 1,
                    other => panic!("unexpected agent route {other:?}"),
                }
            }
            PolicyRoute::KeepSeparate => metrics.kept_separate_or_abstained += 1,
            PolicyRoute::AlreadyDecided(_) | PolicyRoute::Revalidate { .. } => {
                unreachable!("first pass has no decisions")
            }
        }
    }
    metrics
}

fn matches_truth(decision: IdentityDecision, truth: Truth) -> Option<bool> {
    match truth {
        Truth::Same => Some(decision == IdentityDecision::Same),
        Truth::Different => Some(decision == IdentityDecision::Different),
        Truth::Ambiguous => None,
    }
}

fn record_automatic(metrics: &mut StrategyMetrics, decision: IdentityDecision, truth: Truth) {
    match matches_truth(decision, truth) {
        Some(true) => metrics.auto_correct += 1,
        Some(false) => metrics.auto_wrong += 1,
        None if decision == IdentityDecision::Same => metrics.unsafe_ambiguous_merges += 1,
        None => metrics.kept_separate_or_abstained += 1,
    }
}

fn record_review(metrics: &mut StrategyMetrics, decision: IdentityDecision, truth: Truth) {
    match matches_truth(decision, truth) {
        Some(true) => metrics.human_review_correct_recommendations += 1,
        Some(false) => metrics.human_review_wrong_recommendations += 1,
        None => metrics.kept_separate_or_abstained += 1,
    }
}

/// This deterministic contextual heuristic is a reproducible contract control,
/// not a stand-in accuracy claim for an LLM.
fn contextual_control_proposal(packet: &IdentityPacket) -> AgentProposal {
    let has_kind_conflict = has_evidence(packet, "incompatible_kind");
    let has_context = has_evidence(packet, "shared_neighbors")
        && has_evidence(packet, "description_token_overlap");
    let recommendation = if has_kind_conflict {
        IdentityDecision::Different
    } else if has_context {
        IdentityDecision::Same
    } else {
        IdentityDecision::Undetermined
    };
    AgentProposal {
        pair: packet.pair.clone(),
        packet_hash: packet.binding_hash().expect("packet serializes"),
        first_revision: packet.first.revision_id.clone(),
        second_revision: packet.second.revision_id.clone(),
        recommendation,
        evidence_ids: packet.evidence.iter().map(|evidence| evidence.id).collect(),
        relation_suggestions: Vec::new(),
        rationale: "Reproducible contextual control for contract evaluation.".to_string(),
        model_id: "not-a-model-context-control-v1".to_string(),
        prompt_version: "identity-contract-v1".to_string(),
    }
}

fn has_evidence(packet: &IdentityPacket, kind: &str) -> bool {
    packet.evidence.iter().any(|evidence| evidence.kind == kind)
}

fn scenarios() -> PocResult<Vec<Scenario>> {
    let mut scenarios = Vec::new();
    for index in 0..FIXTURES_PER_CLASS {
        scenarios.push(lineage_scenario(index)?);
        scenarios.push(authoritative_same_scenario(index)?);
        scenarios.push(contextual_same_scenario(index)?);
        scenarios.push(authoritative_different_scenario(index)?);
        scenarios.push(kind_different_scenario(index)?);
        scenarios.push(ambiguous_scenario(index)?);
    }
    Ok(scenarios)
}

fn lineage_scenario(index: usize) -> PocResult<Scenario> {
    let mut left = fixture_entity("a", index, "Cerpheus");
    let mut right = fixture_entity("b", index, "Cerpheus Gateway");
    left.lineage_id = Some(format!("origin-{index}"));
    right.lineage_id = Some(format!("origin-{index}"));
    scenario(&left, &right, Truth::Same)
}

fn authoritative_same_scenario(index: usize) -> PocResult<Scenario> {
    let mut left = fixture_entity("a", 100 + index, "Cerpheus");
    let mut right = fixture_entity("b", 100 + index, "Cerpheus");
    let repository = format!("github.com/acme/cerpheus-{index}");
    add_verified_repository(&mut left, repository.clone(), "fixture:left");
    add_verified_repository(&mut right, repository, "fixture:right");
    scenario(&left, &right, Truth::Same)
}

fn contextual_same_scenario(index: usize) -> PocResult<Scenario> {
    let mut left = fixture_entity("a", 200 + index, "Cerpheus");
    let mut right = fixture_entity("b", 200 + index, "Cerpheus");
    left.kind = Some("service".to_string());
    right.kind = Some("service".to_string());
    left.description = "authentication gateway for customer requests".to_string();
    right.description = "customer authentication gateway deployment".to_string();
    left.neighbors.insert("team-platform".to_string());
    right.neighbors.insert("team-platform".to_string());
    scenario(&left, &right, Truth::Same)
}

fn authoritative_different_scenario(index: usize) -> PocResult<Scenario> {
    let mut left = fixture_entity("a", 300 + index, "Cerpheus");
    let mut right = fixture_entity("b", 300 + index, "Cerpheus");
    add_verified_repository(
        &mut left,
        format!("github.com/acme/cerpheus-{index}"),
        "fixture:left",
    );
    add_verified_repository(
        &mut right,
        format!("github.com/research/cerpheus-{index}"),
        "fixture:right",
    );
    scenario(&left, &right, Truth::Different)
}

fn kind_different_scenario(index: usize) -> PocResult<Scenario> {
    let mut left = fixture_entity("a", 400 + index, "Cerpheus");
    let mut right = fixture_entity("b", 400 + index, "Cerpheus");
    left.kind = Some("service".to_string());
    right.kind = Some("dataset".to_string());
    scenario(&left, &right, Truth::Different)
}

fn ambiguous_scenario(index: usize) -> PocResult<Scenario> {
    let left = fixture_entity("a", 500 + index, "Cerpheus");
    let right = fixture_entity("b", 500 + index, "Cerpheus");
    scenario(&left, &right, Truth::Ambiguous)
}

fn fixture_entity(space: &str, index: usize, label: &str) -> EntityRecord {
    EntityRecord::new(
        format!("project-{space}"),
        format!("{space}-entity-{index}"),
        format!("{space}-revision-{index}"),
        label,
    )
}

fn add_verified_repository(record: &mut EntityRecord, value: String, source_ref: &str) {
    record.source_refs.insert(source_ref.to_string());
    record.identifier_assertions.insert(
        "repository".to_string(),
        IdentifierAssertion::source_verified(value, source_ref, "synthetic-fixture-loader"),
    );
}

fn scenario(left: &EntityRecord, right: &EntityRecord, truth: Truth) -> PocResult<Scenario> {
    Ok(Scenario {
        packet: analyze_pair(left, right)?,
        truth,
    })
}

fn measure_performance() -> PocResult<PerformanceReport> {
    let (left_records, right_records) = scale_records()?;

    let mut index = CandidateIndex::default();
    let build_started = Instant::now();
    for record in left_records.iter().chain(&right_records) {
        index.insert(record)?;
    }
    let index_build_us = build_started.elapsed().as_micros();

    let mut query_durations = Vec::with_capacity(left_records.len());
    let mut actual_candidates = BTreeSet::new();
    for record in &left_records {
        let started = Instant::now();
        let candidates = index.candidates_for(record, "project-b", 16)?;
        query_durations.push(started.elapsed());
        if candidates.truncated {
            return Err(PocError::Invalid(
                "scale fixture unexpectedly truncated candidates".to_string(),
            ));
        }
        for candidate in candidates.candidates {
            actual_candidates.insert((record.id.clone(), candidate));
        }
    }
    black_box(&actual_candidates);

    let mut analysis_durations = Vec::with_capacity(actual_candidates.len());
    let rules = IdentityRules::new()
        .with_authoritative_namespace("wikidata")
        .with_kind_relation(
            "rocky_planet",
            "roman_deity",
            KindCompatibility::Incompatible,
        );
    for pair_index in (0..INDEX_ENTITIES_PER_SPACE).step_by(20) {
        let started = Instant::now();
        black_box(analyze_pair_with_rules(
            &left_records[pair_index],
            &right_records[pair_index],
            &rules,
        )?);
        analysis_durations.push(started.elapsed());
    }

    let ledger_directory = tempfile::tempdir()?;
    let ledger = IdentityLedger::open(ledger_directory.path())?;
    let mut proposal_durations = Vec::with_capacity(LEDGER_SAMPLES);
    let mut review_durations = Vec::with_capacity(LEDGER_SAMPLES);
    for sample in 0..LEDGER_SAMPLES {
        let packet = benchmark_packet(sample)?;
        let agent = contextual_control_proposal(&packet);
        let started = Instant::now();
        let receipt = ledger.submit(&format!("ledger-{sample}"), &packet, &agent)?;
        proposal_durations.push(started.elapsed());
        let started = Instant::now();
        ledger.review(
            &receipt.id,
            &packet,
            "benchmark-reviewer",
            IdentityDecision::Different,
            "benchmark decision",
        )?;
        review_durations.push(started.elapsed());
    }

    let per_space = u64::try_from(INDEX_ENTITIES_PER_SPACE)
        .map_err(|_| PocError::Invalid("benchmark size overflow".to_string()))?;
    Ok(PerformanceReport {
        indexed_entities: INDEX_ENTITIES_PER_SPACE * 2,
        aliases_per_entity: 2,
        possible_all_pairs: per_space.saturating_mul(per_space),
        actual_candidates: actual_candidates.len(),
        index_build_us,
        indexed_candidate_lookup: timing_stats(&mut query_durations, "ns"),
        deterministic_pair_analysis: timing_stats(&mut analysis_durations, "ns"),
        durable_proposal_write: timing_stats(&mut proposal_durations, "us"),
        durable_review_write: timing_stats(&mut review_durations, "us"),
        ledger_bytes_after_samples: directory_bytes(ledger_directory.path())?,
    })
}

fn scale_records() -> PocResult<(Vec<EntityRecord>, Vec<EntityRecord>)> {
    let mut left_records = Vec::with_capacity(INDEX_ENTITIES_PER_SPACE);
    let mut right_records = Vec::with_capacity(INDEX_ENTITIES_PER_SPACE);
    for index in 0..INDEX_ENTITIES_PER_SPACE {
        let (left, right) = scale_record_pair(index)?;
        left_records.push(left);
        right_records.push(right);
    }
    Ok((left_records, right_records))
}

fn scale_record_pair(index: usize) -> PocResult<(EntityRecord, EntityRecord)> {
    let mut left = EntityRecord::new(
        "project-a",
        format!("a-{index}"),
        format!("a-r-{index}"),
        format!("Entity {index}"),
    );
    left.aliases.insert(format!("Helios object {index}"));
    left.aliases.insert(format!("Catalog number H{index}"));
    let candidate = index % 20 == 0;
    let right_label = if candidate {
        format!("Entity {index}")
    } else {
        format!("Other {index}")
    };
    let mut right = EntityRecord::new(
        "project-b",
        format!("b-{index}"),
        format!("b-r-{index}"),
        right_label,
    );
    if candidate {
        enrich_scale_candidate(index, &mut left, &mut right)?;
    } else {
        right.aliases.insert(format!("Pantheon object {index}"));
        right.aliases.insert(format!("Catalog number P{index}"));
    }
    Ok((left, right))
}

fn enrich_scale_candidate(
    index: usize,
    left: &mut EntityRecord,
    right: &mut EntityRecord,
) -> PocResult<()> {
    right.aliases.insert(format!("Helios object {index}"));
    right.aliases.insert(format!("Catalog number H{index}"));
    left.kind = Some("rocky_planet".to_string());
    right.kind = Some("roman_deity".to_string());
    let left_source = format!("source:left:{index}");
    let right_source = format!("source:right:{index}");
    left.source_refs.insert(left_source.clone());
    right.source_refs.insert(right_source.clone());
    left.identifier_assertions.insert(
        "wikidata".to_string(),
        IdentifierAssertion::source_verified(
            format!("Q-left-{index}"),
            &left_source,
            "scale-fixture-loader",
        ),
    );
    right.identifier_assertions.insert(
        "wikidata".to_string(),
        IdentifierAssertion::source_verified(
            format!("Q-right-{index}"),
            &right_source,
            "scale-fixture-loader",
        ),
    );
    left.relation_assertions
        .push(RelationAssertion::source_verified(
            "named_after",
            format!("Entity {index}"),
            "roman_deity",
            &left_source,
            "scale-fixture-loader",
        )?);
    Ok(())
}

fn benchmark_packet(sample: usize) -> PocResult<IdentityPacket> {
    let left = EntityRecord::new(
        "project-a",
        format!("ledger-a-{sample}"),
        format!("ledger-a-r-{sample}"),
        format!("Cerpheus {sample}"),
    );
    let right = EntityRecord::new(
        "project-b",
        format!("ledger-b-{sample}"),
        format!("ledger-b-r-{sample}"),
        format!("Cerpheus {sample}"),
    );
    analyze_pair(&left, &right)
}

fn timing_stats(durations: &mut [Duration], unit: &'static str) -> TimingStats {
    durations.sort_unstable();
    let convert: fn(&Duration) -> u128 = if unit == "ns" {
        Duration::as_nanos
    } else {
        Duration::as_micros
    };
    TimingStats {
        samples: durations.len(),
        mean: durations.iter().map(convert).sum::<u128>()
            / u128::try_from(durations.len()).expect("duration count fits u128"),
        p50: convert(&percentile(durations, 50)),
        p95: convert(&percentile(durations, 95)),
        p99: convert(&percentile(durations, 99)),
        max: convert(durations.last().expect("non-empty durations")),
        unit,
    }
}

fn percentile(durations: &[Duration], percent: usize) -> Duration {
    let index = durations
        .len()
        .saturating_mul(percent)
        .div_ceil(100)
        .saturating_sub(1);
    durations[index.min(durations.len() - 1)]
}

fn directory_bytes(path: &Path) -> PocResult<u64> {
    let mut bytes = 0_u64;
    for entry in std::fs::read_dir(path)? {
        let metadata = entry?.metadata()?;
        if metadata.is_file() {
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    Ok(bytes)
}
