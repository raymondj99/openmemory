//! Vector-search smoke bench. Catches "did we regress 10×?" — not a
//! microbenchmark of every code path.
//!
//! Generates synthetic 256-dim vectors at three corpus sizes and times
//! `FlatVectorIndex::search` on a single random query.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use openmemory_index::{FlatVectorIndex, IndexEntry, VectorStore};

const DIMENSIONS: usize = 256;
const TOP_K: usize = 10;

fn synthetic_vector(seed: u64, dim: usize) -> Vec<f32> {
    // Deterministic, low-cost pseudo-random: a 64-bit linear congruential
    // sequence is plenty here. We're not stressing distribution quality.
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..dim)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            // Map to [-1, 1)
            ((state >> 33) as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect()
}

fn populated_index(n: usize) -> FlatVectorIndex {
    let entries: Vec<IndexEntry> = (0..n)
        .map(|i| IndexEntry {
            uri: format!("u://{i}"),
            text: format!("doc {i}"),
            chunk_index: 0,
            vector: synthetic_vector(i as u64 + 1, DIMENSIONS),
        })
        .collect();
    let store = FlatVectorIndex::new();
    store.insert(&entries).unwrap();
    store
}

fn bench_flat_vector_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("flat_vector_search");
    for size in [100usize, 1_000, 10_000] {
        let store = populated_index(size);
        let query = synthetic_vector(0, DIMENSIONS);

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let r = store.search(black_box(&query), black_box(TOP_K)).unwrap();
                black_box(r);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_flat_vector_search);
criterion_main!(benches);
