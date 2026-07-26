use openmemory_memory_model_poc::real_world::{evaluate_java_fixture, evaluate_python_fixture};

fn assert_legacy_homonym_and_rename_failures(
    report: &openmemory_memory_model_poc::real_world::WikipediaEvaluation,
) {
    assert_eq!(report.project_names.len(), 2);
    assert_eq!(report.source_count, 3);
    assert_eq!(report.legacy.expected_pairs, 3);
    assert_eq!(report.legacy.label_only_candidates_found, 1);
    assert_eq!(report.legacy.label_only_candidates_missed, 2);
    assert_eq!(report.legacy.label_only_correct, 1);
    assert_eq!(report.legacy.label_only_false_same, 1);
    assert_eq!(report.legacy.label_only_false_different, 1);
    assert_eq!(
        report.legacy.same_identity_blocked_by_string_kind_conflict,
        0
    );
}

#[test]
fn java_corpus_resolves_language_island_and_coffee_without_wrong_decisions() {
    let report = evaluate_java_fixture().expect("evaluate Java fixture");
    assert_legacy_homonym_and_rename_failures(&report);

    assert_eq!(report.revised.expected_pairs, 3);
    assert_eq!(report.revised.indexed_candidates_found, 3);
    assert_eq!(report.revised.indexed_candidate_pairs_total, 3);
    assert_eq!(report.revised.unexpected_indexed_candidate_pairs, 0);
    assert_eq!(report.revised.source_relation_only_candidates_found, 0);
    assert_eq!(report.revised.deterministic_correct, 2);
    assert_eq!(report.revised.reviewed_correct_recommendations, 1);
    assert_eq!(report.revised.wrong, 0);
    assert_eq!(report.revised.validated_relation_suggestions, 1);
    assert_eq!(report.revised.grouped_relation_review_batches, 1);
}

#[test]
fn python_corpus_discovers_and_resolves_non_homonymous_namesake() {
    let report = evaluate_python_fixture().expect("evaluate Python fixture");
    assert_legacy_homonym_and_rename_failures(&report);

    assert_eq!(report.revised.expected_pairs, 3);
    assert_eq!(report.revised.indexed_candidates_found, 3);
    assert_eq!(report.revised.indexed_candidate_pairs_total, 3);
    assert_eq!(report.revised.unexpected_indexed_candidate_pairs, 0);
    assert_eq!(report.revised.source_relation_only_candidates_found, 1);
    assert_eq!(report.revised.deterministic_correct, 3);
    assert_eq!(report.revised.reviewed_correct_recommendations, 0);
    assert_eq!(report.revised.wrong, 0);
    assert_eq!(report.revised.validated_relation_suggestions, 1);
    assert_eq!(report.revised.grouped_relation_review_batches, 1);
}
