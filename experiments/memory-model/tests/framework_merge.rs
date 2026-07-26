use openmemory_memory_model_poc::github_merge::evaluate_framework_repository_merge;

const EXPECTED_DIFF: &str = include_str!("../results/axum_actix_web_merge.diff");

#[test]
fn pinned_framework_merge_is_complete_and_conservative() {
    let report = evaluate_framework_repository_merge().expect("fixture evaluates");

    assert_eq!(report.coverage.len(), 2);
    assert_eq!(report.coverage[0].tracked_files, 503);
    assert_eq!(report.coverage[0].entity_records, 26);
    assert_eq!(report.coverage[0].graph_relations, 31);
    assert_eq!(report.coverage[1].tracked_files, 444);
    assert_eq!(report.coverage[1].entity_records, 33);
    assert_eq!(report.coverage[1].graph_relations, 43);
    assert_eq!(report.identity.expected_pairs, 20);
    assert_eq!(report.identity.indexed_candidates_found, 20);
    assert_eq!(report.identity.indexed_candidate_pairs_total, 20);
    assert_eq!(report.identity.unexpected_indexed_candidate_pairs, 0);
    assert_eq!(report.identity.deterministic_correct, 15);
    assert_eq!(report.identity.agent_recommendations_routed_to_review, 5);
    assert_eq!(report.identity.wrong, 0);
    assert_eq!(report.identity.team_reviews_required, 7);
    assert_eq!(report.preview.merged_entities, 7);
    assert_eq!(report.preview.kept_separate_candidate_pairs, 13);
    assert_eq!(report.preview.added_entities, 26);
    assert_eq!(report.preview.added_relations, 43);
    assert_eq!(report.preview.deleted_entities, 0);
    assert_eq!(report.preview.diff, EXPECTED_DIFF.trim_end());
}

#[test]
fn shared_concepts_do_not_collapse_framework_specific_rust_items() {
    let report = evaluate_framework_repository_merge().expect("fixture evaluates");
    let diff = &report.preview.diff;

    assert!(diff.contains("Tokio  <=  Tokio"));
    assert!(diff.contains("HTTP request routing  <=  HTTP request routing"));
    assert!(diff.contains("axum::Router  !=  actix_web::App"));
    assert!(diff.contains("axum::handler::Handler  !=  actix_web::Handler"));
    assert!(diff.contains("axum::extract::Path<T>  !=  actix_web::web::Path<T>"));
    assert!(diff.contains("Tower Service and Layer  !=  Actix Service and Transform"));
    assert!(diff.contains("http@1::Request -/-> http@0.2::Request"));

    let http_package = report
        .outcomes
        .iter()
        .find(|outcome| outcome.left == "a-http-package")
        .expect("http package outcome exists");
    assert_eq!(
        http_package.deterministic_resolution,
        "proven_same:shared_authoritative_id"
    );
    let request_types = report
        .outcomes
        .iter()
        .find(|outcome| outcome.left == "a-request-type")
        .expect("request type outcome exists");
    assert_eq!(
        request_types.deterministic_resolution,
        "proven_different:conflicting_authoritative_id"
    );
}

#[test]
fn framework_report_is_byte_deterministic() {
    let first = evaluate_framework_repository_merge().expect("first fixture evaluation");
    let second = evaluate_framework_repository_merge().expect("second fixture evaluation");

    assert_eq!(
        serde_json::to_vec(&first).expect("first report serializes"),
        serde_json::to_vec(&second).expect("second report serializes")
    );
}
