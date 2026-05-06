//! Performance benchmarks for openmemory's critical paths.
//!
//! Six benchmark groups covering the read hot paths and maintenance
//! operations that dominate real-world workloads:
//!
//! 1. **flat_vector_search** — brute-force cosine similarity at 100 / 1k / 10k vectors.
//! 2. **hybrid_search** — RRF fusion of vector + FTS5 keyword results at 500 / 5k docs.
//! 3. **recall** — end-to-end `MemoryStore::recall` (keyword and hybrid modes) at
//!    three corpus sizes, including Ebbinghaus decay scoring.
//! 4. **recall_spreading** — spreading-activation fallback enabled vs disabled.
//! 5. **consolidate_dirty** — Jaccard dedup on a corpus seeded with near-duplicates.
//! 6. **consolidate_clean** — scan-and-skip path on a corpus with no duplicates.
//!
//! All benchmarks use deterministic synthetic data (LCG-seeded vectors and
//! NATO-alphabet text) so results are reproducible across runs.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use openmemory_core::clock::{Clock, FixedClock};
use openmemory_core::config::Config;
use openmemory_core::testing::{Embedder, FakeEmbedder};
use openmemory_graph::{
    ConsolidateConfig, EntityType, MemoryStore, ObservationInput, RecallFilters, RelationInput,
};
use openmemory_index::{
    FlatVectorIndex, Fts5Store, HybridSearchEngine, IndexEntry, SearchMode, VectorStore,
};

const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray",
];

fn lcg_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *state
}

fn synthetic_vector(seed: u64, dim: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..dim)
        .map(|_| {
            lcg_next(&mut state);
            ((state >> 33) as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect()
}

fn synthetic_text(seed: u64, n_words: usize) -> String {
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let mut out = String::new();
    for _ in 0..n_words {
        lcg_next(&mut state);
        let idx = (state as usize) % WORDS.len();
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(WORDS[idx]);
    }
    out
}

// ---------------------------------------------------------------------------
// 1. Flat vector search
// ---------------------------------------------------------------------------

const VEC_DIM: usize = 256;
const TOP_K: usize = 10;

fn populated_vector_index(n: usize) -> FlatVectorIndex {
    let entries: Vec<IndexEntry> = (0..n)
        .map(|i| IndexEntry {
            uri: format!("u://{i}"),
            text: format!("doc {i}"),
            chunk_index: 0,
            vector: synthetic_vector(i as u64 + 1, VEC_DIM),
        })
        .collect();
    let store = FlatVectorIndex::new();
    store.insert(&entries).unwrap();
    store
}

fn bench_flat_vector_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("flat_vector_search");
    for size in [100usize, 1_000, 10_000] {
        let store = populated_vector_index(size);
        let query = synthetic_vector(0, VEC_DIM);

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

// ---------------------------------------------------------------------------
// 2. Hybrid (RRF) search
// ---------------------------------------------------------------------------

const HYBRID_DIM: usize = 128;

fn make_hybrid_engine(n: usize) -> HybridSearchEngine<FlatVectorIndex, Fts5Store> {
    let v = FlatVectorIndex::new();
    let f = Fts5Store::open_in_memory().unwrap();
    let entries: Vec<IndexEntry> = (0..n)
        .map(|i| IndexEntry {
            uri: format!("u://{i}"),
            text: synthetic_text(i as u64 + 1, 16),
            chunk_index: 0,
            vector: synthetic_vector(i as u64 + 1, HYBRID_DIM),
        })
        .collect();
    let engine = HybridSearchEngine::new(v, f, 0.7);
    engine.insert(&entries).unwrap();
    engine
}

fn bench_hybrid_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_search");
    let query_vec = synthetic_vector(0, HYBRID_DIM);
    let query_text = "alpha bravo charlie";

    for size in [500usize, 5_000] {
        let engine = make_hybrid_engine(size);
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

// ---------------------------------------------------------------------------
// 3. Memory graph recall (decay scoring + spreading activation)
// ---------------------------------------------------------------------------

fn entity_name(i: usize) -> String {
    format!("entity_{i}")
}

const EMBED_DIM: usize = 128;

fn populated_memory_store(n_entities: usize, obs_per_entity: usize) -> MemoryStore {
    let clock = Arc::new(FixedClock::new(1_000_000));
    let embedder: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new(EMBED_DIM));
    let store = MemoryStore::open_in_memory(&Config::default())
        .unwrap()
        .with_clock(clock as Arc<dyn Clock>)
        .with_embedder(embedder);

    for i in 0..n_entities {
        let observations: Vec<ObservationInput> = (0..obs_per_entity)
            .map(|j| ObservationInput::new(synthetic_text((i * obs_per_entity + j) as u64 + 1, 12)))
            .collect();

        let relations: Vec<RelationInput> = if i > 0 && i % 5 == 0 {
            vec![RelationInput::new(
                "related_to",
                entity_name(i - 1),
                EntityType::Concept,
            )]
        } else {
            vec![]
        };

        store
            .remember(
                &entity_name(i),
                EntityType::Concept,
                &observations,
                &relations,
                "bench",
            )
            .unwrap();
    }

    store
}

fn bench_recall(c: &mut Criterion) {
    let mut group = c.benchmark_group("recall");

    for (n_entities, obs_per) in [(50, 3), (200, 5), (500, 5)] {
        let total_obs = n_entities * obs_per;
        let store = populated_memory_store(n_entities, obs_per);

        group.throughput(Throughput::Elements(total_obs as u64));

        let mut kw = RecallFilters::new();
        kw.mode = Some(SearchMode::KeywordOnly);

        group.bench_with_input(
            BenchmarkId::new("keyword", total_obs),
            &total_obs,
            |b, _| {
                b.iter(|| {
                    let r = store
                        .recall(black_box("alpha bravo charlie"), black_box(10), &kw)
                        .unwrap();
                    black_box(r);
                });
            },
        );

        let mut hybrid = RecallFilters::new();
        hybrid.mode = Some(SearchMode::Hybrid);

        group.bench_with_input(BenchmarkId::new("hybrid", total_obs), &total_obs, |b, _| {
            b.iter(|| {
                let r = store
                    .recall(black_box("alpha bravo charlie"), black_box(10), &hybrid)
                    .unwrap();
                black_box(r);
            });
        });
    }

    group.finish();
}

fn spreading_store() -> MemoryStore {
    let clock = Arc::new(FixedClock::new(1_000_000));
    let embedder: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new(EMBED_DIM));
    let store = MemoryStore::open_in_memory(&Config::default())
        .unwrap()
        .with_clock(clock as Arc<dyn Clock>)
        .with_embedder(embedder);

    // Seed entity whose observation matches the query. Give it exactly 1
    // observation so direct hits alone cannot fill top_k=20.
    store
        .remember(
            "seed_entity",
            EntityType::Person,
            &[ObservationInput::new("zebra yukon walrus")],
            &[],
            "bench",
        )
        .unwrap();

    // 40 related entities, each linked to the seed via a relation. Their
    // observations use different vocabulary so they only surface through
    // relation traversal.
    for i in 0..40 {
        store
            .remember(
                &format!("related_{i}"),
                EntityType::Concept,
                &[ObservationInput::new(format!(
                    "related fact number {i} about topic"
                ))],
                &[],
                "bench",
            )
            .unwrap();
        store
            .remember(
                "seed_entity",
                EntityType::Person,
                &[],
                &[RelationInput::new(
                    "knows_about",
                    format!("related_{i}"),
                    EntityType::Concept,
                )],
                "bench",
            )
            .unwrap();
    }

    store
}

fn bench_recall_spreading(c: &mut Criterion) {
    let mut group = c.benchmark_group("recall_spreading");
    let store = spreading_store();

    let mut with = RecallFilters::new();
    with.mode = Some(SearchMode::KeywordOnly);
    with.spreading_activation = true;

    let mut without = RecallFilters::new();
    without.mode = Some(SearchMode::KeywordOnly);
    without.spreading_activation = false;

    // "zebra" matches the seed's single observation; top_k=20 underflows
    // direct hits, so the enabled case traverses relations.
    group.bench_function("enabled", |b| {
        b.iter(|| {
            let r = store
                .recall(black_box("zebra"), black_box(20), &with)
                .unwrap();
            black_box(r);
        });
    });

    group.bench_function("disabled", |b| {
        b.iter(|| {
            let r = store
                .recall(black_box("zebra"), black_box(20), &without)
                .unwrap();
            black_box(r);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 4. Consolidation (Jaccard dedup + decay prune)
// ---------------------------------------------------------------------------

fn store_with_duplicates(n_entities: usize, obs_per_entity: usize) -> MemoryStore {
    let clock = Arc::new(FixedClock::new(1_000_000));
    let embedder: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new(EMBED_DIM));
    let store = MemoryStore::open_in_memory(&Config::default())
        .unwrap()
        .with_clock(clock as Arc<dyn Clock>)
        .with_embedder(embedder);

    for i in 0..n_entities {
        let base_text = synthetic_text(i as u64 + 1, 12);
        let observations: Vec<ObservationInput> = (0..obs_per_entity)
            .map(|j| {
                if j == 0 {
                    ObservationInput::new(base_text.clone())
                } else {
                    let mut dup = base_text.clone();
                    if let Some(first_space) = dup.find(' ') {
                        let first_word = dup[..first_space].to_uppercase();
                        dup = format!("{first_word}{}", &dup[first_space..]);
                    }
                    ObservationInput::new(dup)
                }
            })
            .collect();

        store
            .remember(
                &format!("entity_{i}"),
                EntityType::Concept,
                &observations,
                &[],
                "bench",
            )
            .unwrap();
    }

    store
}

fn store_with_unique_observations(n_entities: usize, obs_per_entity: usize) -> MemoryStore {
    let clock = Arc::new(FixedClock::new(1_000_000));
    let embedder: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new(EMBED_DIM));
    let store = MemoryStore::open_in_memory(&Config::default())
        .unwrap()
        .with_clock(clock as Arc<dyn Clock>)
        .with_embedder(embedder);

    for i in 0..n_entities {
        let observations: Vec<ObservationInput> = (0..obs_per_entity)
            .map(|j| ObservationInput::new(synthetic_text((i * obs_per_entity + j) as u64 + 1, 12)))
            .collect();

        store
            .remember(
                &format!("entity_{i}"),
                EntityType::Concept,
                &observations,
                &[],
                "bench",
            )
            .unwrap();
    }

    store
}

fn bench_consolidate_dirty(c: &mut Criterion) {
    let mut group = c.benchmark_group("consolidate_dirty");

    for (n_entities, obs_per) in [(50, 3), (200, 3), (500, 3)] {
        let total_obs = n_entities * obs_per;
        group.throughput(Throughput::Elements(total_obs as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(total_obs),
            &total_obs,
            |b, _| {
                b.iter_with_setup(
                    || store_with_duplicates(n_entities, obs_per),
                    |store| {
                        let cfg = ConsolidateConfig::for_store(&store);
                        let r = store.consolidate(black_box(&cfg)).unwrap();
                        black_box(r);
                    },
                );
            },
        );
    }

    group.finish();
}

fn bench_consolidate_clean(c: &mut Criterion) {
    let mut group = c.benchmark_group("consolidate_clean");

    for (n_entities, obs_per) in [(50, 5), (200, 5), (500, 5)] {
        let total_obs = n_entities * obs_per;
        group.throughput(Throughput::Elements(total_obs as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(total_obs),
            &total_obs,
            |b, _| {
                b.iter_with_setup(
                    || store_with_unique_observations(n_entities, obs_per),
                    |store| {
                        let cfg = ConsolidateConfig::for_store(&store);
                        let r = store.consolidate(black_box(&cfg)).unwrap();
                        black_box(r);
                    },
                );
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_flat_vector_search,
    bench_hybrid_search,
    bench_recall,
    bench_recall_spreading,
    bench_consolidate_dirty,
    bench_consolidate_clean,
);
criterion_main!(benches);
