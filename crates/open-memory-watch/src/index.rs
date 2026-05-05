//! Per-file indexing: hash, dedup against the metadata store, push
//! into the hybrid engine.
//!
//! Each file becomes one chunk under `file://<absolute-path>`. Hashing
//! is BLAKE3 over the on-disk bytes; if the metadata table already
//! holds the same hash, the file is skipped (the cheap-fast-path that
//! makes a re-run of `open-memory watch` over an unchanged tree free).
//! When the hash differs, the watcher first deletes the old URI from
//! the search engine, then inserts the new content.
//!
//! ## URI shape
//!
//! `file://<canonical-absolute-path>`. Encoding follows the
//! convention used elsewhere in open-memory (`memory://observation/<id>`,
//! `note://...`, etc.) — bare path with no escaping. Watcher roots are
//! canonicalised in `Watcher::new`, so the path component is always
//! absolute and the URI is stable across runs.
//!
//! ## Source-kind discriminator
//!
//! v0.2 leaves [`SourceKind`] alone: file-watcher rows reuse
//! [`SourceKind::IndexText`] (same conceptual shape as the MCP
//! `open_memory_index_text` tool — free-text URI content) and tag the
//! row's `source_type` field with `"file:watcher"` so callers can split
//! the two later if they need to.

use std::path::{Path, PathBuf};

use open_memory_graph::MemoryStore;
use open_memory_index::{IndexEntry, SourceKind, SourceRecord};
use tracing::{debug, warn};

use crate::error::WatchResult;
use crate::WatchOptions;

/// `source_type` tag for file-watcher rows in the metadata table.
pub const SOURCE_TYPE_FILE_WATCHER: &str = "file:watcher";

/// Outcome of a single [`process_file`] call. Surfaced to the caller
/// so the indexer can keep aggregate counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// Newly indexed. Watcher sees this on first scan or when the file
    /// did not previously exist in the metadata store.
    Inserted,
    /// Hash differed from the stored hash; old URI was dropped from the
    /// search engine and replaced with the new content.
    Updated,
    /// Hash matched; no engine work was done.
    Unchanged,
    /// File rejected by the size filter or unreadable.
    Skipped(SkipReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    TooLarge,
    UnreadableUtf8,
    EmptyContent,
}

/// Scan-wide tally collected by [`crate::Watcher::scan_initial`] and
/// the runtime event loop (commit 9c).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub removed: usize,
}

impl ScanReport {
    pub fn record(&mut self, outcome: &ProcessOutcome) {
        match outcome {
            ProcessOutcome::Inserted => self.inserted += 1,
            ProcessOutcome::Updated => self.updated += 1,
            ProcessOutcome::Unchanged => self.unchanged += 1,
            ProcessOutcome::Skipped(_) => self.skipped += 1,
        }
    }
}

/// Convert a canonical absolute path into the watcher URI shape.
pub fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// Index `path` into the underlying [`MemoryStore`]'s engine, deduping
/// against the metadata store. See module docs for the semantics.
pub fn process_file(
    memory: &MemoryStore,
    path: &Path,
    options: &WatchOptions,
) -> WatchResult<ProcessOutcome> {
    let metadata_fs = std::fs::metadata(path)?;
    if metadata_fs.len() > options.max_size {
        debug!(
            target: "open_memory_watch::index",
            path = %path.display(),
            size = metadata_fs.len(),
            cap = options.max_size,
            "skip: file exceeds max_size"
        );
        return Ok(ProcessOutcome::Skipped(SkipReason::TooLarge));
    }

    let bytes = std::fs::read(path)?;
    if bytes.is_empty() {
        return Ok(ProcessOutcome::Skipped(SkipReason::EmptyContent));
    }
    // The watcher only cares about textual content. Reject non-UTF-8
    // bytes rather than mojibake-feed FTS5 with `from_utf8_lossy`.
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(ProcessOutcome::Skipped(SkipReason::UnreadableUtf8));
    };

    let hash = blake3::hash(&bytes);
    let hash_bytes = hash.as_bytes().to_vec();
    let uri = path_to_uri(path);

    let existing = memory.engine().metadata.get(&uri)?;
    let now = memory.clock().now_secs();

    let outcome = match &existing {
        Some(prev) if prev.content_hash == hash_bytes => ProcessOutcome::Unchanged,
        Some(_) => {
            // Replace: drop old engine entry, insert fresh, upsert
            // metadata. delete_by_uri returns the row count that went
            // away — best-effort, log if zero (engine drift).
            let removed = memory.engine().engine.delete_by_uri(&uri)?;
            if removed == 0 {
                warn!(
                    target: "open_memory_watch::index",
                    uri = %uri,
                    "metadata claimed an existing row but engine had none — re-inserting"
                );
            }
            ProcessOutcome::Updated
        }
        None => ProcessOutcome::Inserted,
    };

    if matches!(outcome, ProcessOutcome::Inserted | ProcessOutcome::Updated) {
        let entry = IndexEntry::new(uri.clone(), text.to_string());
        memory.engine().engine.insert(&[entry])?;
        let record = SourceRecord {
            uri,
            kind: SourceKind::IndexText,
            content_hash: hash_bytes,
            size_bytes: metadata_fs.len(),
            source_type: SOURCE_TYPE_FILE_WATCHER.to_string(),
            chunk_count: 1,
            status: "indexed".into(),
            created_at: existing.as_ref().map_or(now, |p| p.created_at),
            updated_at: now,
        };
        memory.engine().metadata.upsert(&record)?;
    }

    Ok(outcome)
}

/// Drop a file's URI from both the search engine and the metadata
/// store. Idempotent — a path that was never indexed produces a no-op
/// `Removed` count of zero.
pub fn remove_path(memory: &MemoryStore, path: &Path) -> WatchResult<bool> {
    let uri = path_to_uri(path);
    let removed_engine = memory.engine().engine.delete_by_uri(&uri)?;
    let removed_meta = memory.engine().metadata.delete(&uri)?;
    Ok(removed_engine > 0 || removed_meta)
}

impl crate::Watcher {
    /// Walk the watch root once and index every candidate file.
    /// Files whose stored hash matches the on-disk hash are skipped.
    pub fn scan_initial(&self) -> WatchResult<ScanReport> {
        let mut report = ScanReport::default();
        let walker = crate::scan::build_walker(&self.root)?;
        for path_result in crate::scan::iter_indexable(walker, &self.options) {
            let path: PathBuf = match path_result {
                Ok(p) => p,
                Err(e) => {
                    warn!(target: "open_memory_watch::scan", error = %e, "skip walker entry");
                    continue;
                }
            };
            match process_file(&self.memory, &path, &self.options) {
                Ok(outcome) => report.record(&outcome),
                Err(e) => warn!(
                    target: "open_memory_watch::index",
                    path = %path.display(),
                    error = %e,
                    "indexing failed"
                ),
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use open_memory_core::config::Config;

    fn make_store() -> Arc<MemoryStore> {
        Arc::new(MemoryStore::open_in_memory(&Config::default()).unwrap())
    }

    fn opts() -> WatchOptions {
        WatchOptions::from_config(&Config::default())
    }

    #[test]
    fn process_inserts_new_file() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hello.md");
        std::fs::write(&p, "# Hello\n\nWorld").unwrap();

        let outcome = process_file(&store, &p, &opts()).unwrap();
        assert_eq!(outcome, ProcessOutcome::Inserted);
        assert_eq!(store.engine().metadata.stats().unwrap().total_sources, 1);
    }

    #[test]
    fn process_skips_unchanged_file() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.md");
        std::fs::write(&p, "alpha").unwrap();

        let first = process_file(&store, &p, &opts()).unwrap();
        assert_eq!(first, ProcessOutcome::Inserted);
        let second = process_file(&store, &p, &opts()).unwrap();
        assert_eq!(second, ProcessOutcome::Unchanged);
    }

    #[test]
    fn process_updates_when_content_changes() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.md");
        std::fs::write(&p, "v1").unwrap();
        process_file(&store, &p, &opts()).unwrap();

        std::fs::write(&p, "v2 different content").unwrap();
        let outcome = process_file(&store, &p, &opts()).unwrap();
        assert_eq!(outcome, ProcessOutcome::Updated);
        // Still one URI in metadata (we replaced, not added).
        assert_eq!(store.engine().metadata.stats().unwrap().total_sources, 1);
    }

    #[test]
    fn process_skips_oversize_file() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("huge.md");
        std::fs::write(&p, "abcdef").unwrap();
        let mut o = opts();
        o.max_size = 3;
        let outcome = process_file(&store, &p, &o).unwrap();
        assert_eq!(outcome, ProcessOutcome::Skipped(SkipReason::TooLarge));
    }

    #[test]
    fn process_skips_empty_file() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.md");
        std::fs::write(&p, "").unwrap();
        let outcome = process_file(&store, &p, &opts()).unwrap();
        assert_eq!(outcome, ProcessOutcome::Skipped(SkipReason::EmptyContent));
    }

    #[test]
    fn process_skips_non_utf8_file() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bin.md");
        std::fs::write(&p, [0xFFu8, 0xFE, 0x00, 0x00]).unwrap();
        let outcome = process_file(&store, &p, &opts()).unwrap();
        assert_eq!(outcome, ProcessOutcome::Skipped(SkipReason::UnreadableUtf8));
    }

    #[test]
    fn remove_drops_engine_and_metadata_entries() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.md");
        std::fs::write(&p, "content").unwrap();
        process_file(&store, &p, &opts()).unwrap();
        assert_eq!(store.engine().metadata.stats().unwrap().total_sources, 1);

        let removed = remove_path(&store, &p).unwrap();
        assert!(removed);
        assert_eq!(store.engine().metadata.stats().unwrap().total_sources, 0);
    }

    #[test]
    fn remove_path_for_unknown_uri_returns_false() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.md");
        // Do not create the file; just call remove_path on a path that
        // was never indexed.
        let removed = remove_path(&store, &p).unwrap();
        assert!(!removed);
    }

    #[test]
    fn scan_initial_indexes_tree_and_dedups_on_rerun() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "alpha note").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta note").unwrap();
        std::fs::write(dir.path().join("c.bin"), [0u8; 8]).unwrap(); // wrong ext

        let watcher = crate::Watcher::new(store.clone(), dir.path().into(), opts()).unwrap();
        let r1 = watcher.scan_initial().unwrap();
        assert_eq!(r1.inserted, 2);
        assert_eq!(r1.unchanged, 0);
        assert_eq!(r1.skipped, 0);

        // Second scan: every file is unchanged, nothing new gets indexed.
        let r2 = watcher.scan_initial().unwrap();
        assert_eq!(r2.inserted, 0);
        assert_eq!(r2.unchanged, 2);
    }

    #[test]
    fn path_to_uri_uses_file_scheme() {
        let p = Path::new("/tmp/notes.md");
        assert_eq!(path_to_uri(p), "file:///tmp/notes.md");
    }
}
