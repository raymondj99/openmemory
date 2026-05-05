//! Hybrid-search smoke bench. Times `HybridSearchEngine::search` in hybrid
//! mode against the default keyword backend (FTS5 when on, BM25 otherwise).
//!
//! Two corpus sizes; one query string. Catches a 10× regression, not subtle
//! shifts.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use open_memory_index::{FlatVectorIndex, HybridSearchEngine, IndexEntry, SearchMode};

#[cfg(not(feature = "fts5"))]
use open_memory_index::Bm25Store;
#[cfg(feature = "fts5")]
use open_memory_index::Fts5Store;

const DIMENSIONS: usize = 128;
const TOP_K: usize = 10;

fn synthetic_vector(seed: u64, dim: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..dim)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ((state >> 33) as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect()
}

const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray",
];

fn synthetic_text(seed: u64) -> String {
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let mut out = String::new();
    for _ in 0..16 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let idx = (state as usize) % WORDS.len();
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(WORDS[idx]);
    }
    out
}

#[cfg(feature = "fts5")]
fn make_engine(n: usize) -> HybridSearchEngine<FlatVectorIndex, Fts5Store> {
    let v = FlatVectorIndex::new();
    let f = Fts5Store::open_in_memory().unwrap();
    let entries: Vec<IndexEntry> = (0..n)
        .map(|i| IndexEntry {
            uri: format!("u://{i}"),
            text: synthetic_text(i as u64 + 1),
            chunk_index: 0,
            vector: synthetic_vector(i as u64 + 1, DIMENSIONS),
        })
        .collect();
    let engine = HybridSearchEngine::new(v, f, 0.7);
    engine.insert(&entries).unwrap();
    engine
}

#[cfg(not(feature = "fts5"))]
fn make_engine(n: usize) -> HybridSearchEngine<FlatVectorIndex, Bm25Store> {
    let v = FlatVectorIndex::new();
    let f = Bm25Store::new();
    let entries: Vec<IndexEntry> = (0..n)
        .map(|i| IndexEntry {
            uri: format!("u://{i}"),
            text: synthetic_text(i as u64 + 1),
            chunk_index: 0,
            vector: synthetic_vector(i as u64 + 1, DIMENSIONS),
        })
        .collect();
    let engine = HybridSearchEngine::new(v, f, 0.7);
    engine.insert(&entries).unwrap();
    engine
}

fn bench_hybrid_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_search");
    let query_vec = synthetic_vector(0, DIMENSIONS);
    let query_text = "alpha bravo charlie";

    for size in [500usize, 5_000] {
        let engine = make_engine(size);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let r = engine
                    .search(
                        black_box(&query_vec),
                        black_box(query_text),
                        black_box(TOP_K),
                        SearchMode::Hybrid,
                    )
                    .unwrap();
                black_box(r);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_hybrid_search);
criterion_main!(benches);
