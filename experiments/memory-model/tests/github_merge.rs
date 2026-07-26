use openmemory_memory_model_poc::github_merge::evaluate_github_repository_merge;

const EXPECTED_DIFF: &str = include_str!("../results/codex_homebrew_tools_merge.diff");

#[test]
fn pinned_repository_merge_is_complete_and_conservative() {
    let report = evaluate_github_repository_merge().expect("fixture evaluates");

    assert_eq!(report.coverage.len(), 2);
    assert_eq!(report.coverage[0].tracked_files, 5_609);
    assert_eq!(report.coverage[1].tracked_files, 12);
    assert_eq!(report.identity.expected_pairs, 6);
    assert_eq!(report.identity.indexed_candidates_found, 6);
    assert_eq!(report.identity.indexed_candidate_pairs_total, 6);
    assert_eq!(report.identity.unexpected_indexed_candidate_pairs, 0);
    assert_eq!(report.identity.deterministic_correct, 4);
    assert_eq!(report.identity.agent_recommendations_routed_to_review, 2);
    assert_eq!(report.identity.wrong, 0);
    assert_eq!(report.identity.team_reviews_required, 4);
    assert_eq!(report.preview.merged_entities, 3);
    assert_eq!(report.preview.kept_separate_candidate_pairs, 3);
    assert_eq!(report.preview.added_entities, 13);
    assert_eq!(report.preview.added_relations, 19);
    assert_eq!(report.preview.deleted_entities, 0);
    assert_eq!(report.preview.diff, EXPECTED_DIFF.trim_end());
}

#[test]
fn preview_refuses_the_false_codex_tap_integration() {
    let report = evaluate_github_repository_merge().expect("fixture evaluates");

    assert!(report
        .preview
        .diff
        .contains("openai/tools Homebrew tap -/-> Codex CLI"));
    assert!(report.preview.diff.contains("Codex CLI  !=  OpenAI CLI"));
    assert!(report
        .preview
        .diff
        .contains("Codex Homebrew cask  !=  OpenAI CLI Homebrew cask"));
}

#[test]
fn report_is_byte_deterministic() {
    let first = evaluate_github_repository_merge().expect("first fixture evaluation");
    let second = evaluate_github_repository_merge().expect("second fixture evaluation");

    assert_eq!(
        serde_json::to_vec(&first).expect("first report serializes"),
        serde_json::to_vec(&second).expect("second report serializes")
    );
}
