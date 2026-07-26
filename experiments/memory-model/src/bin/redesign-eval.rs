use openmemory_memory_model_poc::github_merge::{
    evaluate_framework_repository_merge, evaluate_github_repository_merge,
    evaluate_math_code_repository_merge, GithubMergeEvaluation,
};
use serde::Serialize;

#[derive(Serialize)]
struct ExampleSummary<'a> {
    example: &'a str,
    target_space: &'a str,
    source_space: &'a str,
    candidate_pairs: usize,
    unexpected_candidates: usize,
    wrong_decisions: usize,
    reviewed_merges: usize,
    distinct_pairs: usize,
    added_entities: usize,
    imported_relations: usize,
    deletions: usize,
}

fn summarize<'a>(example: &'a str, report: &'a GithubMergeEvaluation) -> ExampleSummary<'a> {
    ExampleSummary {
        example,
        target_space: &report.preview.target_space,
        source_space: &report.preview.source_space,
        candidate_pairs: report.identity.indexed_candidate_pairs_total,
        unexpected_candidates: report.identity.unexpected_indexed_candidate_pairs,
        wrong_decisions: report.identity.wrong,
        reviewed_merges: report.preview.merged_entities,
        distinct_pairs: report.preview.kept_separate_candidate_pairs,
        added_entities: report.preview.added_entities,
        imported_relations: report.preview.added_relations,
        deletions: report.preview.deleted_entities,
    }
}

fn main() {
    let codex = evaluate_github_repository_merge().expect("Codex/Homebrew example evaluates");
    let frameworks = evaluate_framework_repository_merge().expect("Axum/Actix example evaluates");
    let math_code = evaluate_math_code_repository_merge().expect("Mathlib/Lean example evaluates");
    let summaries = [
        summarize("codex-homebrew-tools", &codex),
        summarize("axum-actix-web", &frameworks),
        summarize("mathlib-lean4", &math_code),
    ];
    println!(
        "{}",
        serde_json::to_string_pretty(&summaries).expect("summary serializes")
    );
}
