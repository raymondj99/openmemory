//! READ-PATH VALIDATION HARNESS: measures the partitioned recall path
//! and the alternatives that were considered for it.
//!
//! Outcome (recorded in DomainStore's docs): the facade merged-result
//! cache shipped; the persistent reader pool was REJECTED (it saved
//! 6-9% of fan-out latency, below the 15-20% bar that would justify
//! its lifecycle complexity). The pool prototype stays here as the
//! re-evaluation harness: re-run if K grows well past CPU cores or
//! per-domain search cost drops under ~0.5 ms.
//!
//! Measures, at K domains:
//!   uncached — DomainStore::recall with the facade cache disabled
//!              (TTL zero): the raw fan-out cost
//!   pool     — prototype persistent worker per domain, channel-fed
//!   cached   — DomainStore::recall as shipped (facade cache on)
//! against repeated queries (hot working set), unique queries (every
//! arm does real search work), then repeats the repeated-query
//! comparison while a writer commits on the engine's epoch cadence.
//!
//! Run with: `cargo run --release -p openmemory-engine --example readpath`

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use clap::Parser;
use openmemory_core::config::Config;
use openmemory_engine::partition::DomainStore;
use openmemory_graph::recall::{RecallFilters, RecallResult};
use openmemory_graph::{EntityType, ObservationInput, RememberRequest};

mod common;
use common::{pctl, WORDS};

#[derive(Parser, Debug)]
#[command(
    name = "readpath",
    about = "Partitioned read-path validation prototype"
)]
struct Args {
    #[arg(long, default_value_t = 4)]
    domains: usize,
    /// Entities seeded before measuring.
    #[arg(long, default_value_t = 2000)]
    entities: usize,
    /// Recall samples per scenario.
    #[arg(long, default_value_t = 2000)]
    samples: usize,
    /// Distinct queries in the repeated-query working set.
    #[arg(long, default_value_t = 16)]
    hot_queries: usize,
}

/// Prototype persistent reader pool: one long-lived worker per domain,
/// fed (query, reply) over a channel. What a production pool would
/// amortise vs per-call scoped spawns.
struct ReaderPool {
    senders: Vec<mpsc::Sender<(String, mpsc::Sender<Vec<RecallResult>>)>>,
    _workers: Vec<std::thread::JoinHandle<()>>,
}

impl ReaderPool {
    fn new(store: &Arc<DomainStore>) -> Self {
        let mut senders = Vec::new();
        let mut workers = Vec::new();
        for domain in store.stores() {
            let (tx, rx) = mpsc::channel::<(String, mpsc::Sender<Vec<RecallResult>>)>();
            let domain = Arc::clone(domain);
            workers.push(std::thread::spawn(move || {
                let filters = RecallFilters::new();
                while let Ok((query, reply)) = rx.recv() {
                    let _ = reply.send(domain.recall(&query, 10, &filters).unwrap_or_default());
                }
            }));
            senders.push(tx);
        }
        Self {
            senders,
            _workers: workers,
        }
    }

    fn recall(&self, query: &str) -> Vec<RecallResult> {
        let (reply_tx, reply_rx) = mpsc::channel();
        for tx in &self.senders {
            let _ = tx.send((query.to_string(), reply_tx.clone()));
        }
        drop(reply_tx);
        let mut merged: Vec<RecallResult> = reply_rx.into_iter().flatten().collect();
        merged.sort_by(|a, b| b.score.total_cmp(&a.score));
        merged.truncate(10);
        merged
    }
}

fn measure(label: &str, samples: usize, mut f: impl FnMut(usize)) {
    let mut lats = Vec::with_capacity(samples);
    for i in 0..samples {
        let t0 = Instant::now();
        f(i);
        lats.push(t0.elapsed().as_nanos() as u64);
    }
    lats.sort_unstable();
    println!(
        "  {label:<34} p50 {:>10.2?}  p95 {:>10.2?}  p99 {:>10.2?}",
        pctl(&lats, 0.50),
        pctl(&lats, 0.95),
        pctl(&lats, 0.99),
    );
}

fn main() {
    let args = Args::parse();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = Config::default();
    // Two facades over the SAME domain files: one as shipped (cache
    // on), one with the cache disabled, so the arms isolate the cache.
    let store =
        Arc::new(DomainStore::open(&config, &dir.path().join("data"), args.domains).expect("open"));
    let uncached = Arc::new(
        DomainStore::open(&config, &dir.path().join("data"), args.domains)
            .expect("open uncached")
            .with_recall_cache_ttl(Duration::ZERO),
    );

    // Seed.
    for i in 0..args.entities {
        let a = WORDS[(i * 7) % WORDS.len()];
        let b = WORDS[(i * 13 + 5) % WORDS.len()];
        store
            .remember(
                &format!("entity-{i:05}"),
                EntityType::Fact,
                &[ObservationInput::new(format!(
                    "note {i} about {a} {b} and the quarterly review"
                ))],
                &[],
                "seed",
            )
            .unwrap();
    }

    let hot: Vec<String> = (0..args.hot_queries)
        .map(|q| {
            format!(
                "{} {}",
                WORDS[q % WORDS.len()],
                WORDS[(q * 5 + 1) % WORDS.len()]
            )
        })
        .collect();
    let filters = RecallFilters::new();
    let pool = ReaderPool::new(&uncached);

    println!(
        "readpath validation: {} domains, {} entities, {} samples\n",
        args.domains, args.entities, args.samples
    );

    println!(
        "repeated queries ({} hot, no write load):",
        args.hot_queries
    );
    measure("uncached fan-out", args.samples, |i| {
        let _ = uncached.recall(&hot[i % hot.len()], 10, &filters);
    });
    measure("pool (rejected prototype)", args.samples, |i| {
        let _ = pool.recall(&hot[i % hot.len()]);
    });
    measure("facade cache (shipped)", args.samples, |i| {
        let _ = store.recall(&hot[i % hot.len()], 10, &filters);
    });

    println!("\nunique queries (every call misses all caches):");
    measure("uncached fan-out", args.samples, |i| {
        let q = format!("{} unique-{i}", WORDS[i % WORDS.len()]);
        let _ = uncached.recall(&q, 10, &filters);
    });
    measure("pool (rejected prototype)", args.samples, |i| {
        let q = format!("{} unique-{i}", WORDS[i % WORDS.len()]);
        let _ = pool.recall(&q);
    });

    // Write load: one thread committing engine-drain-sized batches on
    // the engine's real 20 ms epoch cadence (a zero-gap writer starves
    // readers behind the writer-preferring rebuild lock indefinitely —
    // itself a finding, but not the workload the engine produces).
    println!("\nrepeated queries UNDER WRITE LOAD (200-request batches, 20ms epoch cadence):");
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let store = Arc::clone(&store);
        let stop = Arc::clone(&stop);
        let entities = args.entities;
        std::thread::spawn(move || {
            let mut i = entities;
            while !stop.load(Ordering::Relaxed) {
                let batch: Vec<RememberRequest> = (0..200)
                    .map(|j| {
                        let id = i + j;
                        RememberRequest::new(format!("entity-{id:05}"), EntityType::Fact)
                            .with_observations(vec![ObservationInput::new(format!(
                                "storm note {id} about {}",
                                WORDS[id % WORDS.len()]
                            ))])
                            .with_source("storm")
                    })
                    .collect();
                let domain = store.domain_for(&batch[0].name);
                let _ = store.remember_batch_in_domain(domain, &batch);
                i += 200;
                std::thread::park_timeout(Duration::from_millis(20));
            }
        })
    };
    std::thread::park_timeout(Duration::from_millis(100));
    measure("uncached fan-out", args.samples, |i| {
        let _ = uncached.recall(&hot[i % hot.len()], 10, &filters);
    });
    measure("facade cache (shipped)", args.samples, |i| {
        let _ = store.recall(&hot[i % hot.len()], 10, &filters);
    });
    stop.store(true, Ordering::Relaxed);
    let _ = writer.join();
}
