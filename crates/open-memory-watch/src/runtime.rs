//! Synchronous run loop for the watcher.
//!
//! Wires `notify-debouncer-full` to the per-batch handler in
//! [`crate::events`] and exposes `Watcher::run` /
//! `Watcher::run_with_shutdown` / `Watcher::run_with_notifier`.
//!
//! The loop is plain `std::sync::mpsc`: the debouncer thread feeds
//! batches into the channel, the run loop pops them, dispatches to
//! [`crate::events::process_batch`], and updates the cumulative
//! [`ScanReport`]. Shutdown is signalled either by the caller flipping
//! an [`AtomicBool`] or by dropping the underlying debouncer.
//!
//! Tests synchronise via [`crate::Watcher::run_with_notifier`]: after
//! each batch is fully processed, the watcher sends a [`BatchSummary`]
//! through a caller-supplied `SyncSender`. Test code calls `recv()`
//! until it sees the expected delta — no sleeping required.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use tracing::{debug, info, warn};

use crate::error::{WatchError, WatchResult};
use crate::events::process_batch;
use crate::index::ScanReport;

/// Snapshot emitted to the caller's optional notifier after each
/// debounced batch is processed. Cumulative across the watcher's
/// lifetime — easy to diff between two `recv()`s in a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchSummary {
    /// Number of debounced events in the batch.
    pub events_in_batch: usize,
    /// Aggregate report including the initial scan and every previous
    /// batch.
    pub report: ScanReport,
}

/// How often the run loop polls the shutdown flag while waiting for
/// the next debounced batch. 50 ms keeps shutdown latency low without
/// burning CPU.
const SHUTDOWN_POLL: Duration = Duration::from_millis(50);

impl crate::Watcher {
    /// Run the watcher to completion. Equivalent to
    /// `run_with_shutdown(Arc::new(AtomicBool::new(false)))` — i.e.
    /// the loop never voluntarily exits and only stops when the
    /// underlying debouncer is dropped (which happens when the
    /// `Watcher` is dropped). Returns the cumulative report at exit.
    pub fn run(self) -> WatchResult<ScanReport> {
        let shutdown = Arc::new(AtomicBool::new(false));
        self.run_with_shutdown(shutdown)
    }

    /// Run with an external shutdown flag. The loop checks the flag
    /// between debounced batches and exits cleanly when it flips to
    /// `true`. The debouncer is stopped before this method returns.
    pub fn run_with_shutdown(self, shutdown: Arc<AtomicBool>) -> WatchResult<ScanReport> {
        self.run_inner(shutdown, None)
    }

    /// Run with a shutdown flag *and* a notifier channel. The notifier
    /// receives a [`BatchSummary`] after each debounced batch is
    /// processed. Tests use this to synchronise: spawn the watcher in
    /// a thread, mutate the filesystem, then `recv()` from the
    /// notifier until the cumulative report matches the expected delta.
    pub fn run_with_notifier(
        self,
        shutdown: Arc<AtomicBool>,
        notifier: mpsc::SyncSender<BatchSummary>,
    ) -> WatchResult<ScanReport> {
        self.run_inner(shutdown, Some(notifier))
    }

    fn run_inner(
        self,
        shutdown: Arc<AtomicBool>,
        notifier: Option<mpsc::SyncSender<BatchSummary>>,
    ) -> WatchResult<ScanReport> {
        let crate::Watcher {
            memory,
            root,
            options,
        } = self;

        let mut report = ScanReport::default();
        if options.initial_scan {
            // Build a temporary borrow-borrowing watcher view for the
            // initial scan, since we already deconstructed self.
            let scan_view = ScanView {
                memory: &memory,
                root: &root,
                options: &options,
            };
            report = scan_view.scan_initial()?;
            if let Some(tx) = &notifier {
                let _ = tx.send(BatchSummary {
                    events_in_batch: 0,
                    report: report.clone(),
                });
            }
        }

        info!(
            target: "open_memory_watch::runtime",
            root = %root.display(),
            debounce_ms = options.debounce.as_millis() as u64,
            extensions = options.extensions.len(),
            initial_indexed = report.inserted,
            "watcher running"
        );

        // Channel between the debouncer's internal thread and the run loop.
        // SyncSender(0) would deadlock the debouncer if the consumer is
        // slow; bound at 64 to absorb bursts without unbounded memory growth.
        let (tx, rx) = mpsc::sync_channel::<DebounceEventResult>(64);

        let mut debouncer = new_debouncer(options.debounce, None, move |res| {
            // The debouncer's closure runs on its own thread. Drop the
            // result if the consumer is gone — that means the run loop
            // has exited and the channel has been dropped.
            let _ = tx.send(res);
        })
        .map_err(WatchError::from)?;

        debouncer
            .watch(&root, RecursiveMode::Recursive)
            .map_err(WatchError::from)?;

        loop {
            if shutdown.load(Ordering::Relaxed) {
                debug!(target: "open_memory_watch::runtime", "shutdown flag observed");
                break;
            }
            match rx.recv_timeout(SHUTDOWN_POLL) {
                Ok(Ok(events)) => {
                    let n = events.len();
                    if let Err(e) = process_batch(&memory, &root, &options, &events, &mut report) {
                        warn!(
                            target: "open_memory_watch::runtime",
                            error = %e,
                            "batch processing failed"
                        );
                    }
                    if let Some(tx) = &notifier {
                        let _ = tx.send(BatchSummary {
                            events_in_batch: n,
                            report: report.clone(),
                        });
                    }
                }
                Ok(Err(errors)) => {
                    for e in errors {
                        warn!(
                            target: "open_memory_watch::runtime",
                            error = %e,
                            "notify error"
                        );
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    info!(
                        target: "open_memory_watch::runtime",
                        "debouncer channel closed; exiting run loop"
                    );
                    break;
                }
            }
        }

        // `Debouncer::stop` is idempotent and waits for the internal
        // thread to drain. Drop here gives the same behaviour.
        drop(debouncer);
        Ok(report)
    }
}

/// Borrowing helper so the initial scan can run after `self` was
/// destructured. Keeping this as a temporary view avoids cloning the
/// `Arc<MemoryStore>` and `WatchOptions` just to call `scan_initial`.
struct ScanView<'a> {
    memory: &'a open_memory_graph::MemoryStore,
    root: &'a std::path::Path,
    options: &'a crate::WatchOptions,
}

impl ScanView<'_> {
    fn scan_initial(&self) -> WatchResult<ScanReport> {
        let mut report = ScanReport::default();
        let walker = crate::scan::build_walker(self.root)?;
        for path_result in crate::scan::iter_indexable(walker, self.options) {
            let path: PathBuf = match path_result {
                Ok(p) => p,
                Err(e) => {
                    warn!(target: "open_memory_watch::runtime", error = %e, "skip walker entry");
                    continue;
                }
            };
            match crate::index::process_file(self.memory, &path, self.options) {
                Ok(outcome) => report.record(&outcome),
                Err(e) => warn!(
                    target: "open_memory_watch::runtime",
                    path = %path.display(),
                    error = %e,
                    "indexing failed"
                ),
            }
        }
        Ok(report)
    }
}
