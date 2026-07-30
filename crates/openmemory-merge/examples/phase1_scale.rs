#[path = "../scale_support/mod.rs"]
mod scale_support;

use std::time::Instant;

use scale_support::ScaleCase;

fn main() {
    let count = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "100000".to_string())
        .parse::<usize>()
        .expect("entity count must be a positive integer");
    assert!(count > 1 && count <= 100_000);
    let build_started = Instant::now();
    let mut case = ScaleCase::new(count);
    let build_elapsed = build_started.elapsed();
    let canonical_bytes = case.canonical_snapshot_bytes();
    let mode = std::env::args().nth(2);
    if mode.as_deref() == Some("build-only") {
        println!(
            "source_entities={count} source_relations={count} build_ms={:.3} canonical_snapshot_bytes={canonical_bytes}",
            build_elapsed.as_secs_f64() * 1_000.0,
        );
        return;
    }
    let samples = mode.map_or(1, |value| {
        value
            .parse::<usize>()
            .expect("sample count must be a positive integer")
    });
    assert!((1..=100).contains(&samples));
    let mut elapsed_ms = Vec::with_capacity(samples);
    let mut plan = None;
    for _ in 0..samples {
        let plan_started = Instant::now();
        plan = Some(case.run());
        elapsed_ms.push(plan_started.elapsed().as_secs_f64() * 1_000.0);
    }
    let plan = plan.expect("positive sample count always produces a plan");
    let counts = plan.action_counts();
    let actions = counts.target_entities
        + counts.source_entities
        + counts.target_observations
        + counts.source_observations
        + counts.target_relations
        + counts.source_relations;
    if samples == 1 {
        println!(
            "source_entities={count} source_relations={count} build_ms={:.3} plan_ms={:.3} canonical_snapshot_bytes={canonical_bytes} actions={actions} plan_hash={}",
            build_elapsed.as_secs_f64() * 1_000.0,
            elapsed_ms[0],
            plan.plan_hash()
        );
    } else {
        elapsed_ms.sort_by(f64::total_cmp);
        println!(
            "source_entities={count} source_relations={count} samples={samples} build_ms={:.3} p50_ms={:.3} p95_ms={:.3} p99_ms={:.3} canonical_snapshot_bytes={canonical_bytes} actions={actions} plan_hash={}",
            build_elapsed.as_secs_f64() * 1_000.0,
            percentile(&elapsed_ms, 50),
            percentile(&elapsed_ms, 95),
            percentile(&elapsed_ms, 99),
            plan.plan_hash()
        );
    }
}

fn percentile(sorted: &[f64], percentile: usize) -> f64 {
    let rank = sorted.len().saturating_mul(percentile).saturating_add(99) / 100;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}
