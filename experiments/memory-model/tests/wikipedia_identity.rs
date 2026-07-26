use openmemory_memory_model_poc::real_world::evaluate_wikipedia_fixture;

#[test]
fn wikipedia_two_project_corpus_exposes_legacy_failures_and_validates_revision() {
    let report = evaluate_wikipedia_fixture().expect("evaluate fixture");

    assert_eq!(report.project_names.len(), 2);
    assert_eq!(report.source_count, 15);
    assert_eq!(report.legacy.expected_pairs, 11);
    assert_eq!(report.legacy.label_only_candidates_found, 8);
    assert_eq!(report.legacy.label_only_candidates_missed, 3);
    assert_eq!(report.legacy.label_only_correct, 1);
    assert_eq!(report.legacy.label_only_false_same, 7);
    assert_eq!(report.legacy.label_only_false_different, 3);
    assert_eq!(
        report.legacy.same_identity_blocked_by_string_kind_conflict,
        2
    );

    assert_eq!(report.revised.expected_pairs, 11);
    assert_eq!(report.revised.indexed_candidates_found, 11);
    assert_eq!(report.revised.indexed_candidate_pairs_total, 11);
    assert_eq!(report.revised.unexpected_indexed_candidate_pairs, 0);
    assert_eq!(report.revised.source_relation_only_candidates_found, 0);
    assert_eq!(report.revised.deterministic_correct, 7);
    assert_eq!(report.revised.reviewed_correct_recommendations, 4);
    assert_eq!(report.revised.wrong, 0);
    assert_eq!(report.revised.validated_relation_suggestions, 7);
    assert_eq!(report.revised.grouped_relation_review_batches, 1);
}
