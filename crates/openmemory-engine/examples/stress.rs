//! Concurrent-agent stress harness for the context engine.
//!
//! Simulates N agents writing organizational context (meeting notes,
//! decisions, incidents) into one shared store while reader agents run
//! `recall` the whole time. Two write paths:
//!
//! - `direct`: every agent calls `MemoryStore::remember` itself — the
//!   status quo, all writers convoying on the single writer mutex.
//! - `engine`: agents submit to the sharded write-behind
//!   [`ContextEngine`] and get an ack ticket; epoch flushes batch the
//!   writes into single SQLite transactions.
//!
//! Run with: `cargo run --release -p openmemory-engine --example stress`
//!
//! Prints throughput and latency percentiles for both, plus durability
//! lag for the engine path, and verifies no writes were lost.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use openmemory_core::config::Config;
use openmemory_engine::partition::DomainStore;
use openmemory_engine::{ContextEngine, EngineOptions};
use openmemory_graph::recall::RecallFilters;
use openmemory_graph::{EntityType, ObservationInput, RememberRequest};

mod common;
use common::{pctl, WORDS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    Direct,
    Engine,
    Both,
}

#[derive(Parser, Debug)]
#[command(name = "stress", about = "Concurrent-agent write stress test")]
struct Args {
    /// Which write path(s) to exercise.
    #[arg(long, value_enum, default_value = "both")]
    mode: Mode,
    /// Concurrent agent writer threads.
    #[arg(long, default_value_t = 1000)]
    agents: usize,
    /// Writes per agent.
    #[arg(long, default_value_t = 10)]
    ops: usize,
    /// Distinct entity names the agents write against.
    #[arg(long, default_value_t = 500)]
    entities: usize,
    /// Concurrent reader threads running recall in a loop.
    #[arg(long, default_value_t = 4)]
    readers: usize,
    /// Engine: shard count.
    #[arg(long, default_value_t = 32)]
    shards: usize,
    /// Engine: epoch flush interval in milliseconds.
    #[arg(long, default_value_t = 20)]
    flush_ms: u64,
    /// Engine: per-shard queue capacity (backpressure bound).
    #[arg(long, default_value_t = 4096)]
    shard_capacity: usize,
    /// Engine: flusher threads.
    #[arg(long, default_value_t = 2)]
    flush_threads: usize,
    /// Storage domains (independent SQLite families with parallel
    /// writers). Must divide the shard count.
    #[arg(long, default_value_t = 1)]
    domains: usize,
    /// Engine: skip fuzzy entity-name normalization on drained batches.
    #[arg(long, default_value_t = false)]
    no_normalize: bool,
    /// Engine: enable the per-shard crash-recovery journal.
    #[arg(long, default_value_t = false)]
    journal: bool,
}

const ENTITY_KINDS: [(&str, EntityType); 4] = [
    ("meeting", EntityType::Event),
    ("project", EntityType::Project),
    ("decision", EntityType::Fact),
    ("teammate", EntityType::Person),
];

fn entity_for(slot: usize) -> (String, EntityType) {
    let (prefix, ty) = ENTITY_KINDS[slot % ENTITY_KINDS.len()];
    (format!("{prefix}-{:04}", slot / ENTITY_KINDS.len()), ty)
}

fn content_for(agent: usize, op: usize) -> String {
    let a = WORDS[(agent * 7 + op) % WORDS.len()];
    let b = WORDS[(agent * 13 + op * 5) % WORDS.len()];
    let c = WORDS[(agent + op * 11) % WORDS.len()];
    format!("agent {agent} noted {a} {b} during sync; follow up on {c} item {op}")
}

#[derive(Default)]
struct LatencySink(Mutex<Vec<u64>>);

impl LatencySink {
    fn push_all(&self, mut v: Vec<u64>) {
        self.0.lock().expect("latency sink").append(&mut v);
    }
    fn into_sorted(self) -> Vec<u64> {
        let mut v = self.0.into_inner().expect("latency sink");
        v.sort_unstable();
        v
    }
}

struct ScenarioReport {
    label: String,
    wall: Duration,
    durable_wall: Duration,
    writes: u64,
    /// Expected observation rows after the run (writes + durability probes).
    expected: u64,
    write_lat: Vec<u64>,
    durability_lag: Vec<u64>,
    reads: u64,
    read_lat: Vec<u64>,
    observations_in_store: u64,
    engine_summary: Option<String>,
}

fn print_report(r: &ScenarioReport) {
    let ack_rate = r.writes as f64 / r.wall.as_secs_f64();
    let durable_rate = r.writes as f64 / r.durable_wall.as_secs_f64();
    println!("\n=== {} ===", r.label);
    println!(
        "  writes          : {} ({} in store after run)",
        r.writes, r.observations_in_store
    );
    println!(
        "  wall (ack)      : {:>10.2?}   throughput {:>10.0} writes/s",
        r.wall, ack_rate
    );
    println!(
        "  wall (durable)  : {:>10.2?}   throughput {:>10.0} writes/s",
        r.durable_wall, durable_rate
    );
    println!(
        "  write latency   : p50 {:>9.2?}  p95 {:>9.2?}  p99 {:>9.2?}  max {:>9.2?}",
        pctl(&r.write_lat, 0.50),
        pctl(&r.write_lat, 0.95),
        pctl(&r.write_lat, 0.99),
        pctl(&r.write_lat, 1.0),
    );
    if !r.durability_lag.is_empty() {
        println!(
            "  durability lag  : p50 {:>9.2?}  p99 {:>9.2?}  max {:>9.2?}  (sampled)",
            pctl(&r.durability_lag, 0.50),
            pctl(&r.durability_lag, 0.99),
            pctl(&r.durability_lag, 1.0),
        );
    }
    let read_rate = r.reads as f64 / r.durable_wall.as_secs_f64();
    println!(
        "  reads           : {} ({:.0} recalls/s under write load)",
        r.reads, read_rate
    );
    println!(
        "  read latency    : p50 {:>9.2?}  p95 {:>9.2?}  p99 {:>9.2?}  max {:>9.2?}",
        pctl(&r.read_lat, 0.50),
        pctl(&r.read_lat, 0.95),
        pctl(&r.read_lat, 0.99),
        pctl(&r.read_lat, 1.0),
    );
    if let Some(summary) = &r.engine_summary {
        println!("  engine          : {summary}");
    }
}

struct LatencySinkPair {
    lat: LatencySink,
    count: AtomicU64,
}

fn spawn_readers(
    store: &Arc<DomainStore>,
    count: usize,
    stop: &Arc<AtomicBool>,
    sink: &Arc<LatencySinkPair>,
) -> Vec<std::thread::JoinHandle<()>> {
    (0..count)
        .map(|r| {
            let store = Arc::clone(store);
            let stop = Arc::clone(stop);
            let sink = Arc::clone(sink);
            std::thread::Builder::new()
                .name(format!("reader-{r}"))
                .spawn(move || {
                    let filters = RecallFilters::new();
                    let mut lats = Vec::new();
                    let mut i = 0usize;
                    while !stop.load(Ordering::Relaxed) {
                        let query = format!(
                            "{} {}",
                            WORDS[(r * 3 + i) % WORDS.len()],
                            WORDS[(r + i * 7) % WORDS.len()]
                        );
                        let t0 = Instant::now();
                        let _ = store.recall(&query, 10, &filters);
                        lats.push(t0.elapsed().as_nanos() as u64);
                        i += 1;
                    }
                    sink.count.fetch_add(lats.len() as u64, Ordering::Relaxed);
                    sink.lat.push_all(lats);
                })
                .expect("spawn reader")
        })
        .collect()
}

const AGENT_STACK: usize = 512 * 1024;

#[allow(clippy::too_many_lines)]
fn run_scenario(args: &Args, engine_mode: bool) -> ScenarioReport {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = Config::default();
    let domains = if engine_mode { args.domains.max(1) } else { 1 };
    let store = Arc::new(
        DomainStore::open(&config, &dir.path().join("data"), domains).expect("open store"),
    );

    let engine = engine_mode.then(|| {
        Arc::new(
            ContextEngine::start_partitioned(
                Arc::clone(&store),
                EngineOptions {
                    shards: args.shards,
                    flush_interval: Duration::from_millis(args.flush_ms),
                    shard_capacity: args.shard_capacity,
                    flush_threads: args.flush_threads,
                    normalize: !args.no_normalize,
                    journal_dir: args.journal.then(|| dir.path().join("journal")),
                    ..EngineOptions::default()
                },
            )
            .expect("start engine"),
        )
    });

    let stop_readers = Arc::new(AtomicBool::new(false));
    let read_sink = Arc::new(LatencySinkPair {
        lat: LatencySink::default(),
        count: AtomicU64::new(0),
    });
    let reader_handles = spawn_readers(&store, args.readers, &stop_readers, &read_sink);

    let write_sink = Arc::new(LatencySink::default());
    let lag_sink = Arc::new(LatencySink::default());
    let barrier = Arc::new(Barrier::new(args.agents + 1));

    let writer_handles: Vec<_> = (0..args.agents)
        .map(|agent| {
            let store = Arc::clone(&store);
            let engine = engine.clone();
            let barrier = Arc::clone(&barrier);
            let write_sink = Arc::clone(&write_sink);
            let ops = args.ops;
            let entities = args.entities;
            std::thread::Builder::new()
                .name(format!("agent-{agent}"))
                .stack_size(AGENT_STACK)
                .spawn(move || {
                    let mut lats = Vec::with_capacity(ops);
                    barrier.wait();
                    for op in 0..ops {
                        let (name, ty) = entity_for((agent * ops + op) % entities);
                        let content = content_for(agent, op);
                        let t0 = Instant::now();
                        if let Some(engine) = &engine {
                            engine.submit(
                                RememberRequest::new(name, ty)
                                    .with_observations(vec![ObservationInput::new(content)])
                                    .with_source("stress"),
                            );
                        } else {
                            store
                                .remember(
                                    &name,
                                    ty,
                                    &[ObservationInput::new(content)],
                                    &[],
                                    "stress",
                                )
                                .expect("remember");
                        }
                        lats.push(t0.elapsed().as_nanos() as u64);
                    }
                    write_sink.push_all(lats);
                })
                .expect("spawn agent")
        })
        .collect();

    // Dedicated durability prober: samples steady-state submit->durable
    // lag during the storm without throttling any writer thread.
    let stop_prober = Arc::new(AtomicBool::new(false));
    let probe_count = Arc::new(AtomicU64::new(0));
    let prober_handle = engine.as_ref().map(|engine| {
        let engine = Arc::clone(engine);
        let stop = Arc::clone(&stop_prober);
        let count = Arc::clone(&probe_count);
        let lag_sink = Arc::clone(&lag_sink);
        std::thread::Builder::new()
            .name("durability-prober".into())
            .spawn(move || {
                let mut lags = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    let t0 = Instant::now();
                    let ticket = engine.submit(
                        RememberRequest::new("durability-probe", EntityType::Fact)
                            .with_observations(vec![ObservationInput::new(format!(
                                "probe at {:?}",
                                t0.elapsed()
                            ))])
                            .with_source("probe"),
                    );
                    count.fetch_add(1, Ordering::Relaxed);
                    engine.wait_durable(ticket);
                    lags.push(t0.elapsed().as_nanos() as u64);
                    std::thread::park_timeout(Duration::from_millis(50));
                }
                lag_sink.push_all(lags);
            })
            .expect("spawn prober")
    });

    barrier.wait();
    let t_start = Instant::now();
    for h in writer_handles {
        h.join().expect("agent thread");
    }
    let wall = t_start.elapsed();
    stop_prober.store(true, Ordering::Relaxed);
    if let Some(h) = prober_handle {
        h.join().expect("prober thread");
    }

    let engine_summary = engine.map(|engine| {
        let engine = Arc::try_unwrap(engine)
            .map_err(|_| ())
            .expect("sole engine ref");
        engine.quiesce();
        let stats = engine.stats();
        let summary = format!(
            "flushes {} | committed {} | max drain {} | backpressure waits {} | errors {}",
            stats.flushes.load(Ordering::Relaxed),
            stats.committed.load(Ordering::Relaxed),
            stats.max_drain.load(Ordering::Relaxed),
            stats.backpressure_waits.load(Ordering::Relaxed),
            stats.write_errors.load(Ordering::Relaxed),
        );
        engine.shutdown();
        summary
    });
    let durable_wall = t_start.elapsed();

    stop_readers.store(true, Ordering::Relaxed);
    for h in reader_handles {
        h.join().expect("reader thread");
    }

    let status = store.status().expect("status");
    let read_sink = Arc::try_unwrap(read_sink)
        .map_err(|_| ())
        .expect("sole read sink");

    ScenarioReport {
        label: if engine_summary.is_some() {
            format!(
                "ENGINE  ({} agents x {} ops, {} shards, {} domains, {}ms epochs, {} readers{}{})",
                args.agents,
                args.ops,
                args.shards,
                args.domains.max(1),
                args.flush_ms,
                args.readers,
                if args.no_normalize {
                    ", no-normalize"
                } else {
                    ""
                },
                if args.journal { ", journal" } else { "" },
            )
        } else {
            format!(
                "DIRECT  ({} agents x {} ops, {} readers)",
                args.agents, args.ops, args.readers
            )
        },
        wall,
        durable_wall,
        writes: (args.agents * args.ops) as u64,
        expected: (args.agents * args.ops) as u64 + probe_count.load(Ordering::Relaxed),
        write_lat: Arc::try_unwrap(write_sink)
            .map_err(|_| ())
            .expect("sole write sink")
            .into_sorted(),
        durability_lag: Arc::try_unwrap(lag_sink)
            .map_err(|_| ())
            .expect("sole lag sink")
            .into_sorted(),
        reads: read_sink.count.load(Ordering::Relaxed),
        read_lat: read_sink.lat.into_sorted(),
        observations_in_store: status.total_observations,
        engine_summary,
    }
}

fn main() {
    let args = Args::parse();
    println!(
        "openmemory stress: {} agents x {} ops over {} entities, {} readers",
        args.agents, args.ops, args.entities, args.readers
    );

    let mut reports = Vec::new();
    if matches!(args.mode, Mode::Direct | Mode::Both) {
        reports.push(run_scenario(&args, false));
    }
    if matches!(args.mode, Mode::Engine | Mode::Both) {
        reports.push(run_scenario(&args, true));
    }
    for r in &reports {
        print_report(r);
        assert_eq!(
            r.observations_in_store, r.expected,
            "lost writes: expected {}, store has {}",
            r.expected, r.observations_in_store
        );
    }
    println!("\nall scenarios verified: no lost writes");
}
