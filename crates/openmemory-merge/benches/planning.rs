#[path = "../scale_support/mod.rs"]
mod scale_support;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use scale_support::ScaleCase;

fn phase_one_planning_benchmarks(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("merge_planning");
    group.sample_size(10);
    for entity_count in [10_000_usize, 100_000] {
        let mut case = ScaleCase::new(entity_count);
        group.throughput(Throughput::Elements(
            u64::try_from(entity_count).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(entity_count),
            &entity_count,
            |bencher, _| {
                bencher.iter(|| {
                    let plan = case.run();
                    black_box(plan.plan_hash());
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, phase_one_planning_benchmarks);
criterion_main!(benches);
