use openmemory_memory_model_poc::github_merge::evaluate_github_repository_merge;

fn main() {
    match evaluate_github_repository_merge() {
        Ok(report) => {
            println!("{}", report.preview.diff);
            println!("\n@@ machine-readable report @@");
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("evaluation report serializes")
            );
        }
        Err(error) => {
            eprintln!("GitHub repository merge evaluation failed: {error}");
            std::process::exit(1);
        }
    }
}
