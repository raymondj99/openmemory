//! SINGLE-WRITE, MULTIPLE-RETRIEVAL: 5 simultaneous writers each
//! publish ONE distinct memory; 10 simultaneous readers each retrieve
//! OTHER agents' memories, polling from the moment the swarm starts.
//!
//! This is the cross-agent visibility test the storm harness does not
//! cover (its read-your-writes probes stay within one agent):
//!
//! - **publish-to-visibility**: the durable ack means the write is
//!   committed, so any reader poll STARTED after a writer's ack must
//!   find the memory. A miss after ack is a hard failure (this also
//!   regression-tests the facade recall cache: readers cache their
//!   empty pre-publication results, and only the write-version bump
//!   makes the next poll see the commit — if invalidation broke,
//!   readers would starve until the cache TTL).
//! - **content fidelity**: readers verify the EXACT observation text
//!   the writer published, by marker recall, by topical search (the
//!   writer's distinctive vocabulary, no marker), and by entity
//!   lookup.
//! - Writers stagger their writes so early reader polls genuinely miss
//!   and the visibility lag is measured mid-flight, not trivially.
//!
//! Run: cargo run --release -p openmemory-mcp --features mcp-http --example swmr

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use serde_json::{json, Value};

mod common;
use common::{McpClient, TestServer};

#[derive(Parser, Debug, Clone)]
#[command(
    name = "swmr",
    about = "Single-write, multiple-retrieval visibility test"
)]
struct Args {
    /// Simultaneous writers (one memory each).
    #[arg(long, default_value_t = 5)]
    writers: usize,
    /// Simultaneous readers (each retrieves two writers' memories).
    #[arg(long, default_value_t = 10)]
    readers: usize,
    /// Storage domains for the profile under test.
    #[arg(long, default_value_t = 4)]
    domains: usize,
    /// Engine shards (must be a multiple of domains).
    #[arg(long, default_value_t = 32)]
    shards: usize,
    /// Gap between consecutive writers' publishes, in milliseconds, so
    /// readers measurably poll-miss before each publication.
    #[arg(long, default_value_t = 150)]
    stagger_ms: u64,
    /// Reader poll interval in milliseconds.
    #[arg(long, default_value_t = 25)]
    poll_ms: u64,
    /// Per-target retrieval timeout in seconds (a starved reader is a
    /// failure, not a hang).
    #[arg(long, default_value_t = 10)]
    timeout_secs: u64,
}

/// Distinctive vocabulary per writer: topical retrieval must work
/// without the marker, so each memory carries unique-ish words.
const TOPICS: [(&str, &str); 5] = [
    (
        "launch-plan",
        "the obsidian falcon launch ships behind a gradual rollout flag",
    ),
    (
        "incident-review",
        "the cobalt heron outage traced to a stale replica failover",
    ),
    (
        "hiring-update",
        "the velvet osprey panel approved two staff engineer offers",
    ),
    (
        "budget-decision",
        "the amber kestrel budget moved four percent into capacity",
    ),
    (
        "roadmap-shift",
        "the ivory petrel roadmap defers the plugin marketplace a quarter",
    ),
];

fn marker(writer: usize) -> String {
    format!("swmrtoken{writer:02}")
}

/// One writer's published memory, shared with readers for exact-content
/// verification.
struct Published {
    entity: String,
    observation: String,
    /// Nanoseconds since test start at which the durable ack returned;
    /// 0 = not yet published.
    ack_ns: AtomicU64,
}

#[derive(Default)]
struct Failures(Mutex<Vec<String>>);

impl Failures {
    fn push(&self, message: String) {
        self.0.lock().unwrap().push(message);
    }
}

/// Poll one retrieval route until the published content is visible,
/// enforcing the after-ack visibility contract on every miss.
#[allow(clippy::too_many_arguments)]
fn poll_until_visible(
    client: &McpClient,
    route: &str,
    tool: &str,
    arguments: &Value,
    expect_content: &str,
    published_ack: &AtomicU64,
    epoch: Instant,
    args: &Args,
    failures: &Failures,
) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    loop {
        let poll_start_ns = epoch.elapsed().as_nanos() as u64;
        match client.call(tool, arguments.clone()) {
            Ok(payload) => {
                if payload.to_string().contains(expect_content) {
                    return Some(epoch.elapsed().as_nanos() as u64);
                }
                // Visibility contract: a poll that STARTED after the
                // writer's durable ack must see the memory.
                let ack = published_ack.load(Ordering::Acquire);
                if ack != 0 && poll_start_ns > ack {
                    failures.push(format!(
                        "{route}: poll started {:.1?} after durable ack but content invisible",
                        Duration::from_nanos(poll_start_ns - ack),
                    ));
                    return None;
                }
            }
            Err(e) => {
                failures.push(format!("{route}: {e}"));
                return None;
            }
        }
        if Instant::now() >= deadline {
            failures.push(format!(
                "{route}: not visible within {}s",
                args.timeout_secs
            ));
            return None;
        }
        std::thread::park_timeout(Duration::from_millis(args.poll_ms));
    }
}

#[allow(clippy::too_many_lines)]
fn main() {
    let args = Args::parse();

    // ---- Profile + in-process MCP HTTP server --------------------------
    let server = TestServer::start(args.domains, args.shards);
    let base = server.base.clone();

    // ---- Shared state ----------------------------------------------------
    let published: Vec<Arc<Published>> = (0..args.writers)
        .map(|w| {
            let (kind, body) = TOPICS[w % TOPICS.len()];
            Arc::new(Published {
                entity: format!("{kind}-{w:02}"),
                observation: format!("{body} [{}]", marker(w)),
                ack_ns: AtomicU64::new(0),
            })
        })
        .collect();
    let failures = Arc::new(Failures::default());
    let visibility_lags = Arc::new(Mutex::new(Vec::<u64>::new()));
    let retrievals = Arc::new(AtomicUsize::new(0));
    let epoch = Instant::now();
    let barrier = Arc::new(Barrier::new(args.writers + args.readers + 1));

    // ---- Writers: one staggered durable write each -------------------------
    let writer_handles: Vec<_> = (0..args.writers)
        .map(|w| {
            let published = Arc::clone(&published[w]);
            let failures = Arc::clone(&failures);
            let barrier = Arc::clone(&barrier);
            let url = base.clone();
            let stagger = Duration::from_millis(args.stagger_ms * w as u64);
            std::thread::Builder::new()
                .name(format!("swmr-writer-{w}"))
                .spawn(move || {
                    let client = McpClient::new(url);
                    barrier.wait();
                    if !stagger.is_zero() {
                        std::thread::park_timeout(stagger);
                    }
                    let result = client.call(
                        "openmemory_remember",
                        json!({
                            "entity": published.entity,
                            "entity_type": "event",
                            "observations": [published.observation],
                            "source": format!("swmr:writer-{w}"),
                        }),
                    );
                    match result {
                        Ok(receipt)
                            if receipt["accepted"] == json!(true)
                                && receipt["durable"] == json!(true) =>
                        {
                            published
                                .ack_ns
                                .store(epoch.elapsed().as_nanos() as u64, Ordering::Release);
                        }
                        Ok(receipt) => {
                            failures.push(format!("writer {w}: bad receipt {receipt}"));
                        }
                        Err(e) => failures.push(format!("writer {w}: {e}")),
                    }
                })
                .expect("spawn writer")
        })
        .collect();

    // ---- Readers: each retrieves two OTHER writers' memories --------------
    let reader_handles: Vec<_> = (0..args.readers)
        .map(|r| {
            let targets = [r % args.writers, (r + 2) % args.writers];
            let published: Vec<Arc<Published>> =
                targets.iter().map(|&w| Arc::clone(&published[w])).collect();
            let failures = Arc::clone(&failures);
            let lags = Arc::clone(&visibility_lags);
            let retrievals = Arc::clone(&retrievals);
            let barrier = Arc::clone(&barrier);
            let url = base.clone();
            let args = args.clone();
            std::thread::Builder::new()
                .name(format!("swmr-reader-{r}"))
                .spawn(move || {
                    let client = McpClient::new(url);
                    barrier.wait();
                    for (&w, target) in targets.iter().zip(&published) {
                        let mark = marker(w);
                        // Route 1: marker recall (keyword-exact).
                        let route = format!("reader {r} -> writer {w} (marker)");
                        if let Some(hit_ns) = poll_until_visible(
                            &client,
                            &route,
                            "openmemory_recall",
                            &json!({"query": mark, "limit": 5, "mode": "keyword"}),
                            &target.observation,
                            &target.ack_ns,
                            epoch,
                            &args,
                            &failures,
                        ) {
                            retrievals.fetch_add(1, Ordering::Relaxed);
                            let ack = target.ack_ns.load(Ordering::Acquire);
                            if ack != 0 {
                                lags.lock().unwrap().push(hit_ns.saturating_sub(ack));
                            }
                        }
                        // Route 2: topical search, no marker — the
                        // writer's distinctive vocabulary must rank it.
                        let (_, body) = TOPICS[w % TOPICS.len()];
                        let topic_words: Vec<&str> = body.split(' ').skip(1).take(2).collect();
                        let route = format!("reader {r} -> writer {w} (topical)");
                        if poll_until_visible(
                            &client,
                            &route,
                            "openmemory_recall",
                            &json!({"query": topic_words.join(" "), "limit": 5}),
                            &mark,
                            &target.ack_ns,
                            epoch,
                            &args,
                            &failures,
                        )
                        .is_some()
                        {
                            retrievals.fetch_add(1, Ordering::Relaxed);
                        }
                        // Route 3: entity lookup returns the same content.
                        let route = format!("reader {r} -> writer {w} (entity)");
                        if poll_until_visible(
                            &client,
                            &route,
                            "openmemory_get_entity",
                            &json!({"entity": target.entity}),
                            &target.observation,
                            &target.ack_ns,
                            epoch,
                            &args,
                            &failures,
                        )
                        .is_some()
                        {
                            retrievals.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
                .expect("spawn reader")
        })
        .collect();

    barrier.wait();
    let t0 = Instant::now();
    for handle in writer_handles {
        handle.join().expect("writer thread");
    }
    let writers_done = t0.elapsed();
    for handle in reader_handles {
        handle.join().expect("reader thread");
    }
    let wall = t0.elapsed();

    // ---- Validation + report ----------------------------------------------
    let mut failed: Vec<String> = failures.0.lock().unwrap().clone();
    let status = server.domains.status().expect("status");
    if status.total_observations != args.writers as u64 {
        failed.push(format!(
            "store holds {} observations, expected {}",
            status.total_observations, args.writers
        ));
    }
    if server.engine.stats().write_errors.load(Ordering::Relaxed) != 0 {
        failed.push("engine write errors".into());
    }
    let expected_retrievals = args.readers * 2 * 3;
    let done = retrievals.load(Ordering::Relaxed);
    if done != expected_retrievals {
        failed.push(format!(
            "{done} of {expected_retrievals} retrievals completed"
        ));
    }

    let mut lags = visibility_lags.lock().unwrap().clone();
    lags.sort_unstable();
    let pctl = |p: f64| -> Duration { common::pctl(&lags, p) };

    println!(
        "swmr: {} writers (staggered {}ms) + {} readers simultaneous, {} domains",
        args.writers, args.stagger_ms, args.readers, args.domains
    );
    println!("  writers done    : {writers_done:.2?} (all durable acks)");
    println!("  all reads done  : {wall:.2?}");
    println!(
        "  retrievals      : {done}/{expected_retrievals} (marker, topical, entity x {} readers x 2 targets)",
        args.readers
    );
    println!(
        "  ack->visibility : p50 {:.2?}  max {:.2?}  ({} marker-route samples)",
        pctl(0.50),
        pctl(1.0),
        lags.len()
    );
    println!(
        "  store           : {} entities, {} observations",
        status.total_entities, status.total_observations
    );

    let _ = server.shutdown();

    if failed.is_empty() {
        println!("\nPASS: every retrieval exact, no post-ack invisibility");
    } else {
        println!("\nFAIL:");
        for failure in failed.iter().take(20) {
            println!("  - {failure}");
        }
        std::process::exit(1);
    }
}
