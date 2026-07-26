//! Source-grounded merge evaluation for two pinned GitHub repository spaces.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::identity::{
    analyze_pair_with_rules, route_agent_proposal, route_packet, AgentProposal, CandidateIndex,
    DeterministicResolution, EntityRecord, EvidenceStrength, IdentifierAssertion, IdentityDecision,
    IdentityPacket, IdentityPolicy, IdentityRules, PolicyRoute,
};
use crate::semantic_merge::{
    materialize_knowledge_merge, plan_knowledge_merge, CandidatePair, EntityAction,
    IdentityResolution, KnowledgeMergePlan, KnowledgeSnapshot, LocalEntity, LocalRelation,
};
use crate::{PocError, PocResult};

const FIXTURE_JSON: &str = include_str!("../fixtures/github_codex_homebrew_tools.json");
const FRAMEWORK_FIXTURE_JSON: &str = include_str!("../fixtures/github_axum_actix_web.json");
const MATH_CODE_FIXTURE_JSON: &str = include_str!("../fixtures/github_mathlib_lean4.json");
const FIXTURE_VERIFIER: &str = "pinned-github-repository-loader-v1";

#[derive(Debug, Deserialize)]
struct GithubFixture {
    schema_version: u32,
    retrieved_at: String,
    spaces: Vec<SpaceFixture>,
    expectations: Vec<ExpectationFixture>,
    negative_checks: Vec<NegativeCheckFixture>,
    guarded_non_edges: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SpaceFixture {
    space_id: String,
    project_name: String,
    repository: RepositoryFixture,
    sources: Vec<SourceFixture>,
    records: Vec<RecordFixture>,
}

#[derive(Debug, Deserialize)]
struct RepositoryFixture {
    url: String,
    commit: String,
    tracked_files: usize,
}

#[derive(Debug, Deserialize)]
struct SourceFixture {
    id: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct RecordFixture {
    logical_id: String,
    label: String,
    aliases: Vec<String>,
    kind: String,
    identifiers: BTreeMap<String, String>,
    description: String,
    sources: Vec<String>,
    relations: Vec<GraphRelationFixture>,
}

#[derive(Debug, Deserialize)]
struct GraphRelationFixture {
    relation_type: String,
    target: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct ExpectationFixture {
    left: String,
    right: String,
    decision: String,
}

#[derive(Debug, Deserialize)]
struct NegativeCheckFixture {
    space_id: String,
    query: String,
    match_count: usize,
    scope: String,
}

struct LoadedFixture {
    fixture: GithubFixture,
    records: BTreeMap<String, EntityRecord>,
}

struct IdentityEvaluation {
    metrics: GithubIdentityMetrics,
    outcomes: Vec<GithubIdentityOutcome>,
    discovered_pairs: BTreeSet<(String, String)>,
}

/// Coverage of one immutable repository snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryCoverage {
    pub space_id: String,
    pub project_name: String,
    pub repository_url: String,
    pub commit: String,
    pub tracked_files: usize,
    pub source_documents: usize,
    pub entity_records: usize,
    pub graph_relations: usize,
}

/// Result of one cross-space identity decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GithubIdentityOutcome {
    pub left: String,
    pub right: String,
    pub expected: IdentityDecision,
    pub deterministic_resolution: String,
    pub team_route: String,
    pub packet_hash: String,
}

/// Candidate and decision metrics for the repository merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GithubIdentityMetrics {
    pub expected_pairs: usize,
    pub indexed_candidates_found: usize,
    pub indexed_candidate_pairs_total: usize,
    pub unexpected_indexed_candidate_pairs: usize,
    pub deterministic_correct: usize,
    pub agent_recommendations_routed_to_review: usize,
    pub wrong: usize,
    pub team_reviews_required: usize,
}

/// Counts and a git-like rendering of the proposed target-graph changeset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticMergePreview {
    pub target_space: String,
    pub source_space: String,
    pub merged_entities: usize,
    pub kept_separate_candidate_pairs: usize,
    pub added_entities: usize,
    pub added_relations: usize,
    pub deleted_entities: usize,
    pub diff: String,
}

/// Complete evaluation report for one pinned two-repository corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GithubMergeEvaluation {
    pub retrieved_at: String,
    pub coverage: Vec<RepositoryCoverage>,
    pub identity: GithubIdentityMetrics,
    pub outcomes: Vec<GithubIdentityOutcome>,
    pub preview: SemanticMergePreview,
    pub negative_checks: Vec<String>,
    pub limitation: String,
}

/// Evaluate the pinned `openai/codex` and `openai/homebrew-tools` spaces.
///
/// # Errors
///
/// Rejects malformed snapshots, unpinned provenance, invalid graph endpoints,
/// missed candidates, or an identity result that disagrees with the curated
/// source-grounded expectation.
pub fn evaluate_github_repository_merge() -> PocResult<GithubMergeEvaluation> {
    evaluate_repository_fixture(FIXTURE_JSON)
}

/// Evaluate the pinned `tokio-rs/axum` and `actix/actix-web` spaces.
///
/// # Errors
///
/// Rejects malformed snapshots, unpinned provenance, invalid graph endpoints,
/// missed candidates, or an identity result that disagrees with the curated
/// source-grounded expectation.
pub fn evaluate_framework_repository_merge() -> PocResult<GithubMergeEvaluation> {
    evaluate_repository_fixture(FRAMEWORK_FIXTURE_JSON)
}

/// Evaluate the pinned `leanprover-community/mathlib4` and `leanprover/lean4` spaces.
///
/// # Errors
///
/// Rejects malformed snapshots, unpinned provenance, invalid graph endpoints,
/// missed candidates, or an identity result that disagrees with the curated
/// source-grounded expectation.
pub fn evaluate_math_code_repository_merge() -> PocResult<GithubMergeEvaluation> {
    evaluate_repository_fixture(MATH_CODE_FIXTURE_JSON)
}

fn evaluate_repository_fixture(fixture_json: &str) -> PocResult<GithubMergeEvaluation> {
    let loaded = load_fixture(fixture_json)?;
    let identity_evaluation = evaluate_identity(&loaded)?;
    let identity = identity_evaluation.metrics;
    let outcomes = identity_evaluation.outcomes;
    if identity.indexed_candidates_found != identity.expected_pairs {
        return Err(PocError::Invalid(
            "repository fixture failed to discover every expected pair".to_string(),
        ));
    }
    if identity.unexpected_indexed_candidate_pairs != 0 || identity.wrong != 0 {
        return Err(PocError::Invalid(
            "repository fixture produced an unexpected identity result".to_string(),
        ));
    }
    let coverage = loaded
        .fixture
        .spaces
        .iter()
        .map(|space| RepositoryCoverage {
            space_id: space.space_id.clone(),
            project_name: space.project_name.clone(),
            repository_url: space.repository.url.clone(),
            commit: space.repository.commit.clone(),
            tracked_files: space.repository.tracked_files,
            source_documents: space.sources.len(),
            entity_records: space.records.len(),
            graph_relations: space
                .records
                .iter()
                .map(|record| record.relations.len())
                .sum(),
        })
        .collect();
    let negative_checks = loaded
        .fixture
        .negative_checks
        .iter()
        .map(|check| {
            format!(
                "{}: /{}/ matched {} files across {}",
                check.space_id, check.query, check.match_count, check.scope
            )
        })
        .collect();
    let merge_plan = plan_fixture_merge(&loaded, &outcomes, &identity_evaluation.discovered_pairs)?;
    let preview = render_preview(&loaded, &outcomes, &merge_plan)?;
    Ok(GithubMergeEvaluation {
        retrieved_at: loaded.fixture.retrieved_at.clone(),
        coverage,
        identity,
        outcomes,
        preview,
        negative_checks,
        limitation: "This is a pinned source corpus and curated identity evaluation. It validates isolation, provenance, candidate routing, and changeset shape; it is not a held-out model-accuracy or reviewer-usability study.".to_string(),
    })
}

fn load_fixture(fixture_json: &str) -> PocResult<LoadedFixture> {
    let fixture: GithubFixture = serde_json::from_str(fixture_json)?;
    if fixture.schema_version != 1
        || fixture.retrieved_at.trim().is_empty()
        || fixture.spaces.len() != 2
    {
        return Err(PocError::Invalid(
            "GitHub fixture must contain two dated schema-v1 spaces".to_string(),
        ));
    }

    let (records, space_ids) = load_space_records(&fixture.spaces)?;
    validate_expectations(&fixture, &records, &space_ids)?;
    Ok(LoadedFixture { fixture, records })
}

fn load_space_records(
    spaces: &[SpaceFixture],
) -> PocResult<(BTreeMap<String, EntityRecord>, BTreeSet<String>)> {
    let mut records = BTreeMap::new();
    let mut space_ids = BTreeSet::new();
    for space in spaces {
        validate_space(space)?;
        if !space_ids.insert(space.space_id.clone()) {
            return Err(PocError::Invalid(format!(
                "duplicate fixture space {:?}",
                space.space_id
            )));
        }
        let sources = space
            .sources
            .iter()
            .map(|source| (source.id.clone(), source.url.clone()))
            .collect::<BTreeMap<_, _>>();
        if sources.len() != space.sources.len() {
            return Err(PocError::Invalid(format!(
                "duplicate source ID in {:?}",
                space.space_id
            )));
        }
        let local_ids = space
            .records
            .iter()
            .map(|record| record.logical_id.as_str())
            .collect::<BTreeSet<_>>();
        if local_ids.len() != space.records.len() {
            return Err(PocError::Invalid(format!(
                "duplicate logical ID in {:?}",
                space.space_id
            )));
        }
        for source in &space.records {
            validate_record_fixture(source, &sources, &local_ids)?;
            let mut record = EntityRecord::new(
                &space.space_id,
                &source.logical_id,
                format!("{}:{}", space.repository.commit, source.logical_id),
                &source.label,
            );
            record.aliases.extend(source.aliases.iter().cloned());
            record.kind = Some(source.kind.clone());
            record.description.clone_from(&source.description);
            for source_id in &source.sources {
                let source_ref = sources
                    .get(source_id)
                    .ok_or_else(|| PocError::Invalid(format!("unknown source ID {source_id:?}")))?;
                record.source_refs.insert(source_ref.clone());
            }
            let verification_source = record.source_refs.first().ok_or_else(|| {
                PocError::Invalid(format!("record {:?} has no source", source.logical_id))
            })?;
            for (namespace, value) in &source.identifiers {
                record.identifier_assertions.insert(
                    namespace.clone(),
                    IdentifierAssertion::source_verified(
                        value,
                        verification_source,
                        FIXTURE_VERIFIER,
                    ),
                );
            }
            if records.insert(source.logical_id.clone(), record).is_some() {
                return Err(PocError::Invalid(format!(
                    "duplicate cross-space logical ID {:?}",
                    source.logical_id
                )));
            }
        }
    }
    Ok((records, space_ids))
}

fn validate_expectations(
    fixture: &GithubFixture,
    records: &BTreeMap<String, EntityRecord>,
    space_ids: &BTreeSet<String>,
) -> PocResult<()> {
    for expectation in &fixture.expectations {
        let left = records.get(&expectation.left).ok_or_else(|| {
            PocError::Invalid(format!("missing expectation entity {:?}", expectation.left))
        })?;
        let right = records.get(&expectation.right).ok_or_else(|| {
            PocError::Invalid(format!(
                "missing expectation entity {:?}",
                expectation.right
            ))
        })?;
        if left.id.space_id == right.id.space_id {
            return Err(PocError::Invalid(
                "GitHub fixture expectations must cross spaces".to_string(),
            ));
        }
        parse_decision(&expectation.decision)?;
    }
    let expectation_pairs = fixture
        .expectations
        .iter()
        .map(|expectation| (&expectation.left, &expectation.right))
        .collect::<BTreeSet<_>>();
    if expectation_pairs.len() != fixture.expectations.len() {
        return Err(PocError::Invalid(
            "duplicate GitHub fixture expectation".to_string(),
        ));
    }
    for check in &fixture.negative_checks {
        if !space_ids.contains(&check.space_id)
            || check.query.trim().is_empty()
            || check.scope.trim().is_empty()
            || check.match_count != 0
        {
            return Err(PocError::Invalid(
                "negative search evidence is malformed or not clean".to_string(),
            ));
        }
    }
    if fixture.guarded_non_edges.is_empty()
        || fixture
            .guarded_non_edges
            .iter()
            .any(|non_edge| non_edge.trim().is_empty())
    {
        return Err(PocError::Invalid(
            "guarded non-edges must be explicit and non-empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_space(space: &SpaceFixture) -> PocResult<()> {
    if space.space_id.trim().is_empty()
        || space.project_name.trim().is_empty()
        || space.repository.tracked_files == 0
        || !is_full_lower_hex_sha(&space.repository.commit)
        || !space.repository.url.starts_with("https://github.com/")
        || space.sources.is_empty()
        || space.records.is_empty()
    {
        return Err(PocError::Invalid(format!(
            "malformed repository snapshot {:?}",
            space.space_id
        )));
    }
    let required_prefix = format!("{}/blob/{}/", space.repository.url, space.repository.commit);
    if space
        .sources
        .iter()
        .any(|source| source.id.trim().is_empty() || !source.url.starts_with(&required_prefix))
    {
        return Err(PocError::Invalid(format!(
            "source provenance is not pinned to {}",
            space.repository.commit
        )));
    }
    Ok(())
}

fn validate_record_fixture(
    record: &RecordFixture,
    sources: &BTreeMap<String, String>,
    local_ids: &BTreeSet<&str>,
) -> PocResult<()> {
    if record.logical_id.trim().is_empty()
        || record.label.trim().is_empty()
        || record.kind.trim().is_empty()
        || record.description.trim().is_empty()
        || record.sources.is_empty()
        || record
            .sources
            .iter()
            .any(|source| !sources.contains_key(source))
        || record
            .identifiers
            .iter()
            .any(|(namespace, value)| namespace.trim().is_empty() || value.trim().is_empty())
        || record.relations.iter().any(|relation| {
            relation.relation_type.trim().is_empty()
                || !local_ids.contains(relation.target.as_str())
                || !sources.contains_key(&relation.source)
        })
    {
        return Err(PocError::Invalid(format!(
            "malformed repository entity {:?}",
            record.logical_id
        )));
    }
    Ok(())
}

fn evaluate_identity(loaded: &LoadedFixture) -> PocResult<IdentityEvaluation> {
    let mut index = CandidateIndex::default();
    for record in loaded.records.values() {
        index.insert(record)?;
    }
    let target = &loaded.fixture.spaces[0];
    let source = &loaded.fixture.spaces[1];
    let mut discovered_pairs = BTreeSet::new();
    for fixture_record in &target.records {
        let record = &loaded.records[&fixture_record.logical_id];
        let batch = index.candidates_for(record, &source.space_id, 64)?;
        if batch.truncated {
            return Err(PocError::Invalid(
                "repository fixture unexpectedly exceeded candidate bound".to_string(),
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

    let rules = github_rules();
    let mut found = 0;
    let mut deterministic_correct = 0;
    let mut reviewed = 0;
    let mut wrong = 0;
    let mut team_reviews = 0;
    let mut outcomes = Vec::new();
    for expectation in &loaded.fixture.expectations {
        let left = &loaded.records[&expectation.left];
        let right = &loaded.records[&expectation.right];
        if discovered_pairs.contains(&(expectation.left.clone(), expectation.right.clone())) {
            found += 1;
        }
        let expected = parse_decision(&expectation.decision)?;
        let packet = analyze_pair_with_rules(left, right, &rules)?;
        let route = if let Some(decision) = packet.deterministic.decision() {
            if decision == expected {
                deterministic_correct += 1;
            } else {
                wrong += 1;
            }
            route_packet(&packet, IdentityPolicy::team_default(), None)
        } else {
            let proposal = proposal_for(&packet, expected);
            let proposal_route = route_agent_proposal(&packet, &proposal)?;
            if proposal_route == PolicyRoute::HumanReview(expected) {
                reviewed += 1;
            } else {
                wrong += 1;
            }
            proposal_route
        };
        if matches!(route, PolicyRoute::HumanReview(_)) {
            team_reviews += 1;
        }
        outcomes.push(GithubIdentityOutcome {
            left: expectation.left.clone(),
            right: expectation.right.clone(),
            expected,
            deterministic_resolution: resolution_label(&packet.deterministic),
            team_route: route_label(route),
            packet_hash: packet.binding_hash()?,
        });
    }
    let metrics = GithubIdentityMetrics {
        expected_pairs: loaded.fixture.expectations.len(),
        indexed_candidates_found: found,
        indexed_candidate_pairs_total: discovered_pairs.len(),
        unexpected_indexed_candidate_pairs: discovered_pairs.difference(&expected_pairs).count(),
        deterministic_correct,
        agent_recommendations_routed_to_review: reviewed,
        wrong,
        team_reviews_required: team_reviews,
    };
    Ok(IdentityEvaluation {
        metrics,
        outcomes,
        discovered_pairs,
    })
}

fn plan_fixture_merge(
    loaded: &LoadedFixture,
    outcomes: &[GithubIdentityOutcome],
    discovered_pairs: &BTreeSet<(String, String)>,
) -> PocResult<KnowledgeMergePlan> {
    let target_space = &loaded.fixture.spaces[0];
    let source_space = &loaded.fixture.spaces[1];
    let target = fixture_snapshot(target_space)?;
    let source = fixture_snapshot(source_space)?;
    let target_hash = target.hash().to_string();
    let source_hash = source.hash().to_string();
    let candidates = discovered_pairs
        .iter()
        .map(|(target, source)| CandidatePair::new(target, source))
        .collect::<BTreeSet<_>>();
    let resolutions = outcomes
        .iter()
        .map(|outcome| {
            let pair = CandidatePair::new(&outcome.left, &outcome.right);
            let target_revision = &loaded.records[&outcome.left].revision_id;
            let source_revision = &loaded.records[&outcome.right].revision_id;
            if outcome.team_route.starts_with("human_review:") {
                IdentityResolution::reviewed(
                    pair,
                    outcome.expected,
                    target_revision,
                    source_revision,
                    &outcome.packet_hash,
                    "fixture-team-review",
                )
            } else {
                IdentityResolution::policy(
                    pair,
                    outcome.expected,
                    target_revision,
                    source_revision,
                    &outcome.packet_hash,
                    &outcome.deterministic_resolution,
                )
            }
        })
        .collect::<PocResult<Vec<_>>>()?;
    let plan = plan_knowledge_merge(&target, &source, &candidates, &resolutions)?;
    let result = materialize_knowledge_merge(&target, &source, &plan)?;
    if target.hash() != target_hash
        || source.hash() != source_hash
        || result
            .entities()
            .values()
            .map(|entity| entity.contributions.len())
            .sum::<usize>()
            != target.entities().len() + source.entities().len()
        || result.relation_assertion_count()
            != target.relation_assertion_count() + source.relation_assertion_count()
    {
        return Err(PocError::Invalid(
            "generic merge planner failed immutable source accounting".to_string(),
        ));
    }
    Ok(plan)
}

fn fixture_snapshot(space: &SpaceFixture) -> PocResult<KnowledgeSnapshot> {
    let sources = space
        .sources
        .iter()
        .map(|source| (source.id.as_str(), source.url.as_str()))
        .collect::<BTreeMap<_, _>>();
    let entities = space.records.iter().map(|record| {
        let source_refs = record
            .sources
            .iter()
            .chain(record.relations.iter().map(|relation| &relation.source))
            .map(|source_id| {
                (*sources
                    .get(source_id.as_str())
                    .expect("fixture source IDs were validated"))
                .to_string()
            })
            .collect();
        LocalEntity {
            logical_id: record.logical_id.clone(),
            revision_id: format!("{}:{}", space.repository.commit, record.logical_id),
            label: record.label.clone(),
            aliases: record.aliases.iter().cloned().collect(),
            kind: record.kind.clone(),
            identifiers: record.identifiers.clone(),
            description: record.description.clone(),
            source_refs,
        }
    });
    let relations = space.records.iter().flat_map(|record| {
        record.relations.iter().map(|relation| LocalRelation {
            subject: record.logical_id.clone(),
            predicate: relation.relation_type.clone(),
            object: relation.target.clone(),
            source_ref: (*sources
                .get(relation.source.as_str())
                .expect("fixture source IDs were validated"))
            .to_string(),
        })
    });
    KnowledgeSnapshot::from_local(
        &space.space_id,
        &space.repository.commit,
        entities,
        relations,
    )
}

fn render_preview(
    loaded: &LoadedFixture,
    outcomes: &[GithubIdentityOutcome],
    merge_plan: &KnowledgeMergePlan,
) -> PocResult<SemanticMergePreview> {
    let target = &loaded.fixture.spaces[0];
    let source = &loaded.fixture.spaces[1];
    let fixture_records = loaded
        .fixture
        .spaces
        .iter()
        .flat_map(|space| &space.records)
        .map(|record| (record.logical_id.as_str(), record))
        .collect::<BTreeMap<_, _>>();
    let merged_targets = merge_plan
        .entity_actions
        .iter()
        .filter_map(|action| {
            if let EntityAction::Merge { source, target, .. } = action {
                Some((source.as_str(), target.as_str()))
            } else {
                None
            }
        })
        .collect::<BTreeMap<_, _>>();
    let added_records = source
        .records
        .iter()
        .filter(|record| !merged_targets.contains_key(record.logical_id.as_str()))
        .collect::<Vec<_>>();

    let mut lines = preview_header(target, source);
    append_identity_diff(&mut lines, outcomes, &fixture_records)?;
    append_entity_diff(&mut lines, &added_records);
    append_relation_diff(&mut lines, source, &fixture_records, &merged_targets)?;
    append_non_edge_diff(
        &mut lines,
        &loaded.fixture.guarded_non_edges,
        &loaded.fixture.negative_checks,
    );
    lines.push(String::new());
    lines.push(format!(
        "# summary: {} reviewed merges, {} distinct candidate pairs, {} added entities, {} added relations, 0 deletions",
        merge_plan.merged_entities(),
        merge_plan.separate_candidate_pairs,
        merge_plan.added_entities(),
        merge_plan.relation_actions.len()
    ));

    Ok(SemanticMergePreview {
        target_space: target.space_id.clone(),
        source_space: source.space_id.clone(),
        merged_entities: merge_plan.merged_entities(),
        kept_separate_candidate_pairs: merge_plan.separate_candidate_pairs,
        added_entities: merge_plan.added_entities(),
        added_relations: merge_plan.relation_actions.len(),
        deleted_entities: 0,
        diff: lines.join("\n"),
    })
}

fn preview_header(target: &SpaceFixture, source: &SpaceFixture) -> Vec<String> {
    vec![
        format!(
            "memory-graph diff --into {}@{} --from {}@{}",
            target.project_name,
            short_sha(&target.repository.commit),
            source.project_name,
            short_sha(&source.repository.commit)
        ),
        "# proposed target-graph changeset; both source spaces remain immutable".to_string(),
        "# '~?' requires team review, '!' preserves distinct identity, '+' adds data".to_string(),
        String::new(),
        "@@ identity decisions @@".to_string(),
    ]
}

fn append_identity_diff(
    lines: &mut Vec<String>,
    outcomes: &[GithubIdentityOutcome],
    fixture_records: &BTreeMap<&str, &RecordFixture>,
) -> PocResult<()> {
    for outcome in outcomes {
        let left = fixture_records.get(outcome.left.as_str()).ok_or_else(|| {
            PocError::Invalid(format!("missing preview record {:?}", outcome.left))
        })?;
        let right = fixture_records.get(outcome.right.as_str()).ok_or_else(|| {
            PocError::Invalid(format!("missing preview record {:?}", outcome.right))
        })?;
        match outcome.expected {
            IdentityDecision::Same => {
                lines.push(format!("~? {}  <=  {}", left.label, right.label));
                lines.push(format!(
                    "   identity: {}; route: {}",
                    outcome.deterministic_resolution, outcome.team_route
                ));
                lines.push(format!("   + observation: {}", right.description));
            }
            IdentityDecision::Different => {
                lines.push(format!(
                    "! keep separate: {}  !=  {}",
                    left.label, right.label
                ));
                lines.push(format!(
                    "   identity: {}; route: {}",
                    outcome.deterministic_resolution, outcome.team_route
                ));
            }
            IdentityDecision::Undetermined => {
                lines.push(format!("? unresolved: {}  ?  {}", left.label, right.label));
            }
        }
    }
    Ok(())
}

fn append_entity_diff(lines: &mut Vec<String>, added_records: &[&RecordFixture]) {
    lines.push(String::new());
    lines.push("@@ entities added to target space @@".to_string());
    for record in added_records {
        lines.push(format!(
            "+ entity {} [{}] <{}>",
            record.label, record.kind, record.logical_id
        ));
        lines.push(format!("  + summary: {}", record.description));
    }
}

fn append_relation_diff(
    lines: &mut Vec<String>,
    source: &SpaceFixture,
    fixture_records: &BTreeMap<&str, &RecordFixture>,
    merged_targets: &BTreeMap<&str, &str>,
) -> PocResult<()> {
    lines.push(String::new());
    lines.push("@@ relations imported from source space @@".to_string());
    for record in &source.records {
        let subject = merged_targets
            .get(record.logical_id.as_str())
            .and_then(|id| fixture_records.get(id))
            .copied()
            .unwrap_or(record);
        for relation in &record.relations {
            let original_target =
                fixture_records
                    .get(relation.target.as_str())
                    .ok_or_else(|| {
                        PocError::Invalid(format!("missing relation target {:?}", relation.target))
                    })?;
            let target_record = merged_targets
                .get(relation.target.as_str())
                .and_then(|id| fixture_records.get(id))
                .copied()
                .unwrap_or(original_target);
            let rewired = if target_record.logical_id == original_target.logical_id {
                String::new()
            } else {
                format!(" [rewired from {}]", original_target.logical_id)
            };
            lines.push(format!(
                "+ relation {} --{}--> {}{}",
                subject.label, relation.relation_type, target_record.label, rewired
            ));
        }
    }
    Ok(())
}

fn append_non_edge_diff(
    lines: &mut Vec<String>,
    guarded_non_edges: &[String],
    checks: &[NegativeCheckFixture],
) {
    lines.push(String::new());
    lines.push("@@ guarded non-edges at these snapshots @@".to_string());
    lines.extend(
        guarded_non_edges
            .iter()
            .map(|non_edge| format!("! {non_edge}")),
    );
    for check in checks {
        lines.push(format!(
            "  evidence: /{}/ matched {} files in {}",
            check.query, check.match_count, check.scope
        ));
    }
}

fn github_rules() -> IdentityRules {
    IdentityRules::new()
        .with_policy_version("github-repository-merge-policy-v1")
        .with_authoritative_namespace("github_repository")
        .with_authoritative_namespace("github_owner")
        .with_authoritative_namespace("source_repository")
        .with_authoritative_namespace("homebrew_cask")
        .with_authoritative_namespace("homebrew_tap")
        .with_authoritative_namespace("npm_package")
        .with_authoritative_namespace("spdx_license")
        .with_authoritative_namespace("spdx_expression")
        .with_authoritative_namespace("crates_io_package")
        .with_authoritative_namespace("rust_item")
        .with_authoritative_namespace("lean_project")
        .with_authoritative_namespace("lean_module")
        .with_authoritative_namespace("lean_declaration")
        .with_authoritative_namespace("lean_namespace")
        .with_authoritative_namespace("lean_toolchain")
        .with_authoritative_namespace("source_file")
        .with_authoritative_namespace("version_policy")
}

fn proposal_for(packet: &IdentityPacket, recommendation: IdentityDecision) -> AgentProposal {
    let evidence_ids = packet
        .evidence
        .iter()
        .filter(|evidence| evidence.strength == EvidenceStrength::Context)
        .map(|evidence| evidence.id)
        .collect::<Vec<_>>();
    AgentProposal {
        pair: packet.pair.clone(),
        packet_hash: packet.binding_hash().expect("fixture packet serializes"),
        first_revision: packet.first.revision_id.clone(),
        second_revision: packet.second.revision_id.clone(),
        recommendation,
        evidence_ids,
        relation_suggestions: Vec::new(),
        rationale: "Curated from complete pinned repository snapshots for architecture evaluation."
            .to_string(),
        model_id: "codex-repository-orientation-2026-07-18".to_string(),
        prompt_version: "github-space-merge-v1".to_string(),
    }
}

fn parse_decision(value: &str) -> PocResult<IdentityDecision> {
    match value {
        "same" => Ok(IdentityDecision::Same),
        "different" => Ok(IdentityDecision::Different),
        "undetermined" => Ok(IdentityDecision::Undetermined),
        other => Err(PocError::Invalid(format!(
            "unknown repository fixture decision {other:?}"
        ))),
    }
}

fn resolution_label(resolution: &DeterministicResolution) -> String {
    match resolution {
        DeterministicResolution::ProvenSame { basis } => format!("proven_same:{basis}"),
        DeterministicResolution::ProvenDifferent { basis } => {
            format!("proven_different:{basis}")
        }
        DeterministicResolution::ConflictingProofs { basis } => {
            format!("conflicting_proofs:{basis}")
        }
        DeterministicResolution::NeedsAgent => "needs_agent".to_string(),
        DeterministicResolution::NotCandidate => "not_candidate".to_string(),
    }
}

fn route_label(route: PolicyRoute) -> String {
    match route {
        PolicyRoute::AlreadyDecided(decision) => format!("already_decided:{decision:?}"),
        PolicyRoute::Revalidate { known, observed } => {
            format!("revalidate:{known:?}:{observed:?}")
        }
        PolicyRoute::AutoApply(decision) => format!("auto_apply:{decision:?}"),
        PolicyRoute::AskAgent => "ask_agent".to_string(),
        PolicyRoute::HumanReview(decision) => format!("human_review:{decision:?}"),
        PolicyRoute::KeepSeparate => "keep_separate".to_string(),
    }
    .to_lowercase()
}

fn is_full_lower_hex_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn short_sha(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{evaluate_repository_fixture, FRAMEWORK_FIXTURE_JSON, MATH_CODE_FIXTURE_JSON};

    fn framework_fixture() -> Value {
        serde_json::from_str(FRAMEWORK_FIXTURE_JSON).expect("fixture parses")
    }

    fn math_code_fixture() -> Value {
        serde_json::from_str(MATH_CODE_FIXTURE_JSON).expect("fixture parses")
    }

    fn evaluate_mutation(value: &Value) -> String {
        evaluate_repository_fixture(&serde_json::to_string(value).expect("mutation serializes"))
            .expect_err("mutation must fail closed")
            .to_string()
    }

    #[test]
    fn rejects_source_url_that_is_not_commit_pinned() {
        let mut fixture = framework_fixture();
        fixture["spaces"][1]["sources"][0]["url"] =
            Value::String("https://github.com/actix/actix-web/blob/main/README.md".to_string());

        assert!(evaluate_mutation(&fixture).contains("source provenance is not pinned"));
    }

    #[test]
    fn rejects_dangling_relation_endpoint() {
        let mut fixture = framework_fixture();
        fixture["spaces"][1]["records"][0]["relations"][0]["target"] =
            Value::String("missing-organization".to_string());

        assert!(evaluate_mutation(&fixture).contains("malformed repository entity"));
    }

    #[test]
    fn rejects_duplicate_logical_id() {
        let mut fixture = framework_fixture();
        fixture["spaces"][1]["records"][1]["logical_id"] =
            fixture["spaces"][1]["records"][0]["logical_id"].clone();

        assert!(evaluate_mutation(&fixture).contains("duplicate logical ID"));
    }

    #[test]
    fn rejects_missing_guarded_non_edges() {
        let mut fixture = framework_fixture();
        fixture["guarded_non_edges"] = Value::Array(Vec::new());

        assert!(evaluate_mutation(&fixture).contains("guarded non-edges"));
    }

    #[test]
    fn rejects_dirty_negative_search_evidence() {
        let mut fixture = framework_fixture();
        fixture["negative_checks"][0]["match_count"] = Value::from(1);

        assert!(evaluate_mutation(&fixture).contains("negative search evidence"));
    }

    #[test]
    fn rejects_curated_same_decision_for_conflicting_rust_items() {
        let mut fixture = framework_fixture();
        fixture["expectations"][2]["decision"] = Value::String("same".to_string());

        assert!(evaluate_mutation(&fixture).contains("unexpected identity result"));
    }

    #[test]
    fn rejects_unexpected_homonym_candidate() {
        let mut fixture = framework_fixture();
        let aliases = fixture["spaces"][1]["records"][0]["aliases"]
            .as_array_mut()
            .expect("aliases is an array");
        aliases.push(Value::String("shared async runtime package".to_string()));

        assert!(evaluate_mutation(&fixture).contains("unexpected identity result"));
    }

    #[test]
    fn rejects_mathlib_toolchain_policy_collapsed_into_lean_master() {
        let mut fixture = math_code_fixture();
        fixture["spaces"][1]["records"][3]["identifiers"]["version_policy"] =
            Value::String("mathlib-fixed-toolchain".to_string());

        assert!(evaluate_mutation(&fixture).contains("unexpected identity result"));
    }

    #[test]
    fn rejects_mathlib_tactics_collapsed_into_core_tactics() {
        let mut fixture = math_code_fixture();
        fixture["spaces"][1]["records"][19]["identifiers"]["lean_module"] =
            Value::String("Mathlib.Tactic".to_string());

        assert!(evaluate_mutation(&fixture).contains("unexpected identity result"));
    }

    #[test]
    fn rejects_extra_cross_domain_homonym_candidate() {
        let mut fixture = math_code_fixture();
        let aliases = fixture["spaces"][1]["records"][2]["aliases"]
            .as_array_mut()
            .expect("aliases is an array");
        aliases.push(Value::String("natural numbers".to_string()));

        assert!(evaluate_mutation(&fixture).contains("unexpected identity result"));
    }
}
