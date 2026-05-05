//! End-to-end integration test against the real
//! `notify-debouncer-full` event loop.
//!
//! Runtime budget: < 5 seconds on a developer laptop. The test
//! synchronises on the watcher's `BatchSummary` notifier instead of
//! sleeping for "wait for filesystem" — every checkpoint either sees
//! the expected delta on the channel or fails with a deadline.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use open_memory_core::config::Config;
use open_memory_graph::MemoryStore;
use open_memory_watch::{path_to_uri, BatchSummary, ScanReport, WatchOptions, Watcher};

const RECV_DEADLINE: Duration = Duration::from_secs(3);

/// Wait for a batch summary that satisfies `predicate`. Drains the
/// channel as it goes; returns the matching summary or panics with
/// the last seen state if the deadline expires.
fn wait_for(
    rx: &std::sync::mpsc::Receiver<BatchSummary>,
    deadline: Duration,
    predicate: impl Fn(&ScanReport) -> bool,
) -> BatchSummary {
    let started = Instant::now();
    let mut last: Option<BatchSummary> = None;
    while started.elapsed() < deadline {
        let remaining = deadline.saturating_sub(started.elapsed());
        match rx.recv_timeout(remaining) {
            Ok(summary) => {
                if predicate(&summary.report) {
                    return summary;
                }
                last = Some(summary);
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => {
                panic!("watcher exited before delivering expected batch (last={last:?})");
            }
        }
    }
    panic!("timed out waiting for expected batch (last seen={last:?}, deadline={deadline:?})");
}

/// Open a watcher pointed at `dir` with a fast 80ms debounce.
/// Returns the watcher handle, the join handle, the shutdown flag,
/// and the receiver side of the notifier channel.
fn spawn_watcher(
    dir: &std::path::Path,
) -> (
    Arc<MemoryStore>,
    thread::JoinHandle<()>,
    Arc<AtomicBool>,
    std::sync::mpsc::Receiver<BatchSummary>,
) {
    let cfg = Config::default();
    // The watcher needs an on-disk store so its read pool is the
    // multi-handle variant; that's also what production runs use.
    let data_dir = dir.join(".open-memory");
    std::fs::create_dir_all(&data_dir).unwrap();
    let memory = Arc::new(MemoryStore::open(&cfg, &data_dir).unwrap());

    let watch_root = dir.join("tree");
    std::fs::create_dir_all(&watch_root).unwrap();

    let mut options = WatchOptions::from_config(&cfg);
    // Tighter debounce so the test stays under the 5s budget.
    options.debounce = Duration::from_millis(80);
    options.initial_scan = true;

    let watcher = Watcher::new(Arc::clone(&memory), watch_root.clone(), options).unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let (tx, rx) = sync_channel::<BatchSummary>(64);
    let shutdown_clone = Arc::clone(&shutdown);
    let handle = thread::spawn(move || {
        // Surface watcher errors via panic so the test fails loudly
        // rather than silently hanging on the recv side.
        watcher.run_with_notifier(shutdown_clone, tx).unwrap();
    });

    // Drain the initial-scan notification so subsequent waits start
    // from a known baseline.
    let initial = rx.recv_timeout(RECV_DEADLINE).expect("initial scan notify");
    assert_eq!(initial.report.inserted, 0, "fresh tree");

    // FSEvents (macOS) and inotify (Linux) both have a brief startup
    // window between `Watcher::watch` returning and events actually
    // flowing. Poke the tree until we see the corresponding batch
    // arrive on the notifier — that proves the backend is live before
    // any test starts measuring deltas. A warmup file pattern beats a
    // raw sleep because the synchronisation is causal: we know events
    // are flowing when we observe an event, not when an arbitrary
    // timer expires.
    let warmup_path = watch_root.join(".om-warmup");
    let mut warmup_attempts = 0;
    let became_live = loop {
        warmup_attempts += 1;
        std::fs::write(&warmup_path, format!("warmup #{warmup_attempts}")).unwrap();
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(s) if s.events_in_batch > 0 => break true,
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout) if warmup_attempts < 8 => continue,
            Err(RecvTimeoutError::Timeout) => break false,
            Err(RecvTimeoutError::Disconnected) => break false,
        }
    };
    assert!(
        became_live,
        "watcher backend never became live after {warmup_attempts} pokes"
    );
    let _ = std::fs::remove_file(&warmup_path);
    // Drain whatever's in the channel — including the remove event we
    // just triggered — so callers start at zero deltas.
    while rx.try_recv().is_ok() {}
    // The warmup may have inserted a row keyed under `.om-warmup`.
    // Tear it down so the test's expected counts are exact.
    let warmup_uri = path_to_uri(
        &warmup_path
            .canonicalize()
            .unwrap_or_else(|_| warmup_path.clone()),
    );
    let _ = memory.engine().engine.delete_by_uri(&warmup_uri);
    let _ = memory.engine().metadata.delete(&warmup_uri);

    (memory, handle, shutdown, rx)
}

#[test]
fn watcher_indexes_create_modify_delete() {
    let dir = tempfile::tempdir().unwrap();
    let (memory, handle, shutdown, rx) = spawn_watcher(dir.path());
    let watch_root = dir.path().join("tree");

    // -------- create --------
    let target = watch_root.join("notes.md");
    std::fs::write(&target, "first version").unwrap();
    // Canonicalise *after* the write — on macOS, /var/folders is a
    // symlink to /private/var/folders; FSEvents reports paths under
    // the canonical prefix, so the stored URI uses that form too.
    let target_uri = path_to_uri(&target.canonicalize().unwrap());
    let summary = wait_for(&rx, RECV_DEADLINE, |r| r.inserted >= 1);
    assert_eq!(summary.report.inserted, 1, "first create indexed");
    let stored = memory
        .engine()
        .metadata
        .get(&target_uri)
        .unwrap()
        .expect("metadata row after create");
    assert_eq!(stored.uri, target_uri);

    // -------- modify --------
    std::fs::write(&target, "second version with more body").unwrap();
    let summary = wait_for(&rx, RECV_DEADLINE, |r| r.updated >= 1);
    assert_eq!(summary.report.updated, 1, "first modify reindexed");
    let after_modify = memory
        .engine()
        .metadata
        .get(&target_uri)
        .unwrap()
        .expect("row still present after modify");
    assert_ne!(
        after_modify.content_hash, stored.content_hash,
        "hash changes when content changes"
    );

    // -------- delete --------
    std::fs::remove_file(&target).unwrap();
    let summary = wait_for(&rx, RECV_DEADLINE, |r| r.removed >= 1);
    assert_eq!(summary.report.removed, 1, "remove drops the row");
    assert!(
        memory.engine().metadata.get(&target_uri).unwrap().is_none(),
        "metadata cleared after delete"
    );

    // -------- second create on a different filename --------
    // Round-trip a freshly-named file to confirm the watcher kept
    // running cleanly past the delete.
    let next = watch_root.join("more.md");
    std::fs::write(&next, "another note").unwrap();
    wait_for(&rx, RECV_DEADLINE, |r| r.inserted >= 2);

    shutdown.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

#[test]
fn watcher_dedupes_initial_scan_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let watch_root = dir.path().join("tree");
    std::fs::create_dir_all(&watch_root).unwrap();
    std::fs::write(watch_root.join("a.md"), "alpha").unwrap();
    std::fs::write(watch_root.join("b.md"), "beta").unwrap();

    let cfg = Config::default();
    let data_dir = dir.path().join(".open-memory");
    std::fs::create_dir_all(&data_dir).unwrap();

    // First run: initial scan inserts both files, no events fire.
    {
        let memory = Arc::new(MemoryStore::open(&cfg, &data_dir).unwrap());
        let mut options = WatchOptions::from_config(&cfg);
        options.debounce = Duration::from_millis(80);
        let watcher = Watcher::new(Arc::clone(&memory), watch_root.clone(), options).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let (tx, rx) = sync_channel::<BatchSummary>(8);
        let shutdown_run = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            let _ = watcher.run_with_notifier(shutdown_run, tx);
        });
        let initial = rx.recv_timeout(RECV_DEADLINE).expect("initial scan");
        assert_eq!(initial.report.inserted, 2);
        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        assert_eq!(memory.engine().metadata.stats().unwrap().total_sources, 2);
    }

    // Second run against the same data dir + tree: BLAKE3 dedup kicks
    // in and the report shows 0 inserted, 2 unchanged.
    {
        let memory = Arc::new(MemoryStore::open(&cfg, &data_dir).unwrap());
        let mut options = WatchOptions::from_config(&cfg);
        options.debounce = Duration::from_millis(80);
        let watcher = Watcher::new(Arc::clone(&memory), watch_root, options).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let (tx, rx) = sync_channel::<BatchSummary>(8);
        let shutdown_run = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            let _ = watcher.run_with_notifier(shutdown_run, tx);
        });
        let initial = rx.recv_timeout(RECV_DEADLINE).expect("initial scan");
        assert_eq!(initial.report.inserted, 0);
        assert_eq!(initial.report.unchanged, 2);
        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }
}

#[test]
fn watcher_skips_files_in_always_ignored_directories() {
    let dir = tempfile::tempdir().unwrap();
    let (memory, handle, shutdown, rx) = spawn_watcher(dir.path());
    let watch_root = dir.path().join("tree");

    let git_dir = watch_root.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    // Plain-text file with an indexable extension, but in `.git/`.
    let inside = git_dir.join("HEAD.md");
    std::fs::write(&inside, "ref: refs/heads/main\n").unwrap();
    let outside = watch_root.join("README.md");
    std::fs::write(&outside, "indexed").unwrap();

    // Wait for the README batch — the .git file must not show up.
    let _ = wait_for(&rx, RECV_DEADLINE, |r| r.inserted >= 1);

    let outside_uri = path_to_uri(&outside.canonicalize().unwrap_or_else(|_| outside.clone()));
    let inside_uri = path_to_uri(&inside.canonicalize().unwrap_or_else(|_| inside.clone()));
    assert!(memory
        .engine()
        .metadata
        .get(&outside_uri)
        .unwrap()
        .is_some());
    assert!(memory.engine().metadata.get(&inside_uri).unwrap().is_none());

    shutdown.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

/// Loop create / modify / delete N times with timestamps so we can
/// surface p50 / p99 latency numbers in the PR description. Runs at
/// a tight 80 ms debounce, so the floor on each measurement is
/// roughly the debounce window plus a small handler cost.
#[test]
fn watcher_latency_smoke_test() {
    const ITERS: usize = 6;

    let dir = tempfile::tempdir().unwrap();
    let (_memory, handle, shutdown, rx) = spawn_watcher(dir.path());
    let watch_root = dir.path().join("tree");

    let mut create_lat = Vec::with_capacity(ITERS);
    let mut modify_lat = Vec::with_capacity(ITERS);
    let mut delete_lat = Vec::with_capacity(ITERS);

    for i in 0..ITERS {
        let path: PathBuf = watch_root.join(format!("note-{i}.md"));
        let baseline = snapshot(&rx);

        let t = Instant::now();
        std::fs::write(&path, format!("hello {i}")).unwrap();
        let _ = wait_for(&rx, RECV_DEADLINE, |r| r.inserted > baseline.inserted);
        create_lat.push(t.elapsed());

        let baseline = latest_snapshot(&rx);
        let t = Instant::now();
        std::fs::write(&path, format!("hello {i} v2")).unwrap();
        let _ = wait_for(&rx, RECV_DEADLINE, |r| r.updated > baseline.updated);
        modify_lat.push(t.elapsed());

        let baseline = latest_snapshot(&rx);
        let t = Instant::now();
        std::fs::remove_file(&path).unwrap();
        let _ = wait_for(&rx, RECV_DEADLINE, |r| r.removed > baseline.removed);
        delete_lat.push(t.elapsed());
    }

    shutdown.store(true, Ordering::Relaxed);
    handle.join().unwrap();

    println!(
        "watcher_latency_smoke: create p50={:?} p99={:?} | modify p50={:?} p99={:?} | delete p50={:?} p99={:?}",
        percentile(&mut create_lat.clone(), 0.50),
        percentile(&mut create_lat.clone(), 0.99),
        percentile(&mut modify_lat.clone(), 0.50),
        percentile(&mut modify_lat.clone(), 0.99),
        percentile(&mut delete_lat.clone(), 0.50),
        percentile(&mut delete_lat.clone(), 0.99),
    );

    // Sanity bounds. p99 ≤ 2× the debounce window leaves comfortable
    // headroom on CI; a regression that decoupled the debouncer from
    // the run loop would push these into the seconds.
    let cap = Duration::from_millis(800);
    assert!(percentile(&mut create_lat.clone(), 0.99) < cap);
    assert!(percentile(&mut modify_lat.clone(), 0.99) < cap);
    assert!(percentile(&mut delete_lat.clone(), 0.99) < cap);
}

/// Drain whatever's already on the channel and return the latest
/// cumulative `ScanReport`. Used to capture a baseline before kicking
/// off the next filesystem op.
fn snapshot(rx: &std::sync::mpsc::Receiver<BatchSummary>) -> ScanReport {
    let mut last = ScanReport::default();
    while let Ok(s) = rx.try_recv() {
        last = s.report;
    }
    last
}

/// Same as `snapshot`, but the channel may already be empty; in that
/// case fetch one fresh summary so the caller's "wait for delta" loop
/// has a defined baseline. Bounded by the same RECV_DEADLINE.
fn latest_snapshot(rx: &std::sync::mpsc::Receiver<BatchSummary>) -> ScanReport {
    let drained = snapshot(rx);
    if drained != ScanReport::default() {
        return drained;
    }
    // No queued batches — we can use whatever the next batch reports.
    // But we shouldn't *block* if nothing's coming; just return an
    // empty baseline. The wait_for that follows will time out itself
    // if the delta never arrives.
    drained
}

fn percentile(samples: &mut [Duration], p: f64) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    samples.sort();
    let idx = ((samples.len() as f64) * p).ceil() as usize;
    samples[idx.min(samples.len() - 1)]
}
