use openmemory_memory_model_poc::real_world::evaluate_wikipedia_fixture;

fn main() {
    match evaluate_wikipedia_fixture() {
        Ok(report) => println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serialize report")
        ),
        Err(error) => {
            eprintln!("Wikipedia identity evaluation failed: {error}");
            std::process::exit(1);
        }
    }
}
