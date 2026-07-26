use openmemory_memory_model_poc::github_merge::evaluate_math_code_repository_merge;

const EXPECTED_DIFF: &str = include_str!("../results/mathlib_lean4_merge.diff");

#[test]
fn pinned_math_code_merge_is_complete_and_conservative() {
    let report = evaluate_math_code_repository_merge().expect("fixture evaluates");

    assert_eq!(report.coverage.len(), 2);
    assert_eq!(report.coverage[0].tracked_files, 8_986);
    assert_eq!(report.coverage[0].entity_records, 30);
    assert_eq!(report.coverage[0].graph_relations, 43);
    assert_eq!(report.coverage[1].tracked_files, 13_121);
    assert_eq!(report.coverage[1].entity_records, 34);
    assert_eq!(report.coverage[1].graph_relations, 50);
    assert_eq!(report.identity.expected_pairs, 22);
    assert_eq!(report.identity.indexed_candidates_found, 22);
    assert_eq!(report.identity.indexed_candidate_pairs_total, 22);
    assert_eq!(report.identity.unexpected_indexed_candidate_pairs, 0);
    assert_eq!(report.identity.deterministic_correct, 16);
    assert_eq!(report.identity.agent_recommendations_routed_to_review, 6);
    assert_eq!(report.identity.wrong, 0);
    assert_eq!(report.identity.team_reviews_required, 14);
    assert_eq!(report.preview.merged_entities, 14);
    assert_eq!(report.preview.kept_separate_candidate_pairs, 8);
    assert_eq!(report.preview.added_entities, 20);
    assert_eq!(report.preview.added_relations, 50);
    assert_eq!(report.preview.deleted_entities, 0);
    assert_eq!(report.preview.diff, EXPECTED_DIFF.trim_end());
}

#[test]
fn dependency_identity_rewires_without_collapsing_project_specific_code() {
    let report = evaluate_math_code_repository_merge().expect("fixture evaluates");
    let diff = &report.preview.diff;

    assert!(diff.contains("leanprover/lean4  <=  leanprover/lean4"));
    assert!(diff.contains("leanprover-community/mathlib4  <=  leanprover-community/mathlib4"));
    assert!(diff.contains("Lake  <=  Lake"));
    assert!(diff.contains("Nat  <=  Nat"));
    assert!(diff.contains("Mathlib.Tactic  !=  Init.Tactics"));
    assert!(diff.contains("Mathlib.Lean.Meta  !=  Lean.Meta"));
    assert!(diff.contains("Mathlib.Data.Nat.Prime.Basic  !=  Init.Prelude natural-number core"));
    assert!(diff
        .contains("Mathlib's leanprover/lean4:v4.33.0-rc1 toolchain -/-> Lean master 4.34.0-pre"));
    assert!(diff.contains(
        "+ relation leanprover/lean4 --tests_against--> leanprover-community/mathlib4 [rewired from b-mathlib-downstream]"
    ));
    assert!(diff.contains(
        "+ relation Lake --scaffolds_projects_using--> leanprover-community/mathlib4 [rewired from b-mathlib-downstream]"
    ));

    let upstream_repository = report
        .outcomes
        .iter()
        .find(|outcome| outcome.left == "a-lean-upstream")
        .expect("upstream repository outcome exists");
    assert_eq!(
        upstream_repository.deterministic_resolution,
        "proven_same:shared_authoritative_id"
    );
    let tactic_modules = report
        .outcomes
        .iter()
        .find(|outcome| outcome.left == "a-tactic-module")
        .expect("tactic-module outcome exists");
    assert_eq!(
        tactic_modules.deterministic_resolution,
        "proven_different:conflicting_authoritative_id"
    );
}

#[test]
fn math_code_report_is_byte_deterministic() {
    let first = evaluate_math_code_repository_merge().expect("first fixture evaluation");
    let second = evaluate_math_code_repository_merge().expect("second fixture evaluation");

    assert_eq!(
        serde_json::to_vec(&first).expect("first report serializes"),
        serde_json::to_vec(&second).expect("second report serializes")
    );
}
