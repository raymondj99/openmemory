use std::hint::black_box;
use std::time::Instant;

use openmemory_memory_model_poc::github_merge::evaluate_github_repository_merge;

const ITERATIONS: u32 = 10_000;

fn main() {
    black_box(evaluate_github_repository_merge().expect("warm-up evaluation succeeds"));
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        black_box(evaluate_github_repository_merge().expect("measured evaluation succeeds"));
    }
    let elapsed = started.elapsed();
    println!("iterations={ITERATIONS}");
    println!("elapsed_ms={:.3}", elapsed.as_secs_f64() * 1_000.0);
    println!(
        "mean_us={:.3}",
        elapsed.as_secs_f64() * 1_000_000.0 / f64::from(ITERATIONS)
    );
}
