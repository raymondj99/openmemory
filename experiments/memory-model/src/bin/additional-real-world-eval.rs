use openmemory_memory_model_poc::real_world::{evaluate_java_fixture, evaluate_python_fixture};
use serde_json::json;

fn main() {
    let result = evaluate_java_fixture().and_then(|java| {
        Ok(json!({
            "java": java,
            "python": evaluate_python_fixture()?,
        }))
    });
    match result {
        Ok(report) => println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("evaluation report serializes")
        ),
        Err(error) => {
            eprintln!("additional real-world evaluation failed: {error}");
            std::process::exit(1);
        }
    }
}
