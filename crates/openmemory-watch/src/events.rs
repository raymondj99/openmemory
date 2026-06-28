//! Runtime event handling — translates debounced [`notify`] events
//! into `process_file` / `remove_path` calls.
//!
//! The translation is intentionally narrow:
//!
//! - **Create / Modify** → if the path still exists and looks
//!   indexable, run `process_file` (which BLAKE3-dedupes against the
//!   metadata store). If the path no longer exists, treat as a remove.
//! - **Remove** → `remove_path`.
//! - **Rename** (paired by `notify-debouncer-full` into a single
//!   event with `paths = [from, to]`) → `remove_path(from)` then
//!   `process_file(to)`.
//! - **Access / Other** → ignored.
//!
//! ## Path filtering at runtime
//!
//! The initial scan honours `.gitignore` / `.ignore` /
//! `.openmemory-ignore` via [`ignore::WalkBuilder`]. Runtime events
//! get a lighter check: extension match + an always-ignore directory
//! sweep over the path components. v0.2 deliberately does not
//! re-evaluate per-tree ignore files for every event — that would
//! require parsing every `.gitignore` ancestor on each event and is
//! tracked as a v0.3 follow-up. Files placed in `.git/`, `target/`,
//! etc. via runtime events are still skipped.

use std::path::Path;

use notify::EventKind;
use notify_debouncer_full::DebouncedEvent;
use openmemory_graph::MemoryStore;
use tracing::warn;

use crate::error::WatchResult;
use crate::index::{process_file, remove_path, ScanReport};
use crate::{has_indexable_extension, WatchOptions, ALWAYS_IGNORE_DIRS, ALWAYS_IGNORE_GLOBS};

/// Process one debounced batch. Mutates `report` in-place.
pub fn process_batch(
    memory: &MemoryStore,
    root: &Path,
    options: &WatchOptions,
    events: &[DebouncedEvent],
    report: &mut ScanReport,
) -> WatchResult<()> {
    for de in events {
        match &de.event.kind {
            EventKind::Modify(notify::event::ModifyKind::Name(_)) if de.event.paths.len() == 2 => {
                handle_rename(
                    memory,
                    root,
                    options,
                    &de.event.paths[0],
                    &de.event.paths[1],
                    report,
                );
            }
            EventKind::Create(_) | EventKind::Modify(_) => {
                for path in &de.event.paths {
                    handle_upsert(memory, root, options, path, report);
                }
            }
            EventKind::Remove(_) => {
                for path in &de.event.paths {
                    handle_remove(memory, path, report);
                }
            }
            // Access / Other — debouncer-full already filters most of
            // these, but be defensive.
            _ => {}
        }
    }
    Ok(())
}

fn handle_upsert(
    memory: &MemoryStore,
    root: &Path,
    options: &WatchOptions,
    path: &Path,
    report: &mut ScanReport,
) {
    if !is_under_root(path, root) {
        return;
    }
    if !path.exists() {
        // Modify-then-delete race: the debouncer flushed a Modify but
        // the file is already gone. Treat as a remove.
        handle_remove(memory, path, report);
        return;
    }
    if !path.is_file() {
        return;
    }
    if !is_runtime_indexable(path, root, options) {
        return;
    }
    match process_file(memory, path, options) {
        Ok(outcome) => report.record(&outcome),
        Err(e) => warn!(
            target: "openmemory_watch::events",
            path = %path.display(),
            error = %e,
            "process_file failed"
        ),
    }
}

fn handle_remove(memory: &MemoryStore, path: &Path, report: &mut ScanReport) {
    match remove_path(memory, path) {
        Ok(true) => report.removed += 1,
        Ok(false) => {}
        Err(e) => warn!(
            target: "openmemory_watch::events",
            path = %path.display(),
            error = %e,
            "remove_path failed"
        ),
    }
}

fn handle_rename(
    memory: &MemoryStore,
    root: &Path,
    options: &WatchOptions,
    from: &Path,
    to: &Path,
    report: &mut ScanReport,
) {
    handle_remove(memory, from, report);
    handle_upsert(memory, root, options, to, report);
}

/// Path has to start with the watch root. Filters out symlink-resolved
/// events that point outside the tree.
fn is_under_root(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// Lightweight runtime filter: extension + always-ignore components.
/// Runtime events skip the full `.gitignore` / `.openmemory-ignore`
/// resolution; see module-level docs for the v0.3 follow-up.
pub(crate) fn is_runtime_indexable(path: &Path, root: &Path, options: &WatchOptions) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    for component in rel.components() {
        let name = component.as_os_str();
        if ALWAYS_IGNORE_DIRS
            .iter()
            .any(|d| std::ffi::OsStr::new(d) == name)
        {
            return false;
        }
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        for glob in ALWAYS_IGNORE_GLOBS {
            if matches_simple_glob(glob, name) {
                return false;
            }
        }
    }
    has_indexable_extension(path, &options.extensions)
}

/// Tiny shell-glob matcher for the always-ignore file-name globs.
/// Supports `*` and `?`, which is enough for the constants we ship;
/// broader pattern support lives in the `ignore` crate which the
/// initial scan already uses.
fn matches_simple_glob(pattern: &str, name: &str) -> bool {
    let pattern = pattern.as_bytes();
    let name = name.as_bytes();
    let (mut pat_idx, mut name_idx) = (0usize, 0usize);
    let mut star_idx = None;
    let mut star_match_idx = 0usize;

    while name_idx < name.len() {
        if pat_idx < pattern.len()
            && (pattern[pat_idx] == b'?' || pattern[pat_idx] == name[name_idx])
        {
            pat_idx += 1;
            name_idx += 1;
        } else if pat_idx < pattern.len() && pattern[pat_idx] == b'*' {
            star_idx = Some(pat_idx);
            star_match_idx = name_idx;
            pat_idx += 1;
        } else if let Some(star) = star_idx {
            pat_idx = star + 1;
            star_match_idx += 1;
            name_idx = star_match_idx;
        } else {
            return false;
        }
    }

    while pat_idx < pattern.len() && pattern[pat_idx] == b'*' {
        pat_idx += 1;
    }

    pat_idx == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};
    use notify::Event;
    use std::sync::Arc;
    use std::time::Instant;

    use openmemory_core::config::Config;
    use openmemory_index::SourceKind;

    fn store() -> Arc<MemoryStore> {
        Arc::new(MemoryStore::open_in_memory(&Config::default()).unwrap())
    }
    fn opts() -> WatchOptions {
        WatchOptions::from_config(&Config::default())
    }

    fn make_event(kind: EventKind, paths: Vec<std::path::PathBuf>) -> DebouncedEvent {
        let mut event = Event::new(kind);
        event.paths = paths;
        DebouncedEvent {
            event,
            time: Instant::now(),
        }
    }

    #[test]
    fn create_event_indexes_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = store();
        let p = root.join("a.md");
        std::fs::write(&p, "alpha").unwrap();

        let mut report = ScanReport::default();
        process_batch(
            &store,
            &root,
            &opts(),
            &[make_event(
                EventKind::Create(CreateKind::File),
                vec![p.clone()],
            )],
            &mut report,
        )
        .unwrap();
        assert_eq!(report.inserted, 1);
        assert_eq!(store.engine().metadata.stats().unwrap().total_sources, 1);
    }

    #[test]
    fn modify_event_updates_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = store();
        let p = root.join("a.md");
        std::fs::write(&p, "v1").unwrap();
        process_file(&store, &p, &opts()).unwrap();

        std::fs::write(&p, "v2 changed").unwrap();
        let mut report = ScanReport::default();
        process_batch(
            &store,
            &root,
            &opts(),
            &[make_event(
                EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
                vec![p.clone()],
            )],
            &mut report,
        )
        .unwrap();
        assert_eq!(report.updated, 1);
    }

    #[test]
    fn remove_event_drops_indexed_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = store();
        let p = root.join("a.md");
        std::fs::write(&p, "alpha").unwrap();
        process_file(&store, &p, &opts()).unwrap();
        std::fs::remove_file(&p).unwrap();

        let mut report = ScanReport::default();
        process_batch(
            &store,
            &root,
            &opts(),
            &[make_event(EventKind::Remove(RemoveKind::File), vec![p])],
            &mut report,
        )
        .unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(store.engine().metadata.stats().unwrap().total_sources, 0);
    }

    #[test]
    fn modify_for_disappeared_file_is_treated_as_remove() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = store();
        let p = root.join("a.md");
        std::fs::write(&p, "alpha").unwrap();
        process_file(&store, &p, &opts()).unwrap();
        std::fs::remove_file(&p).unwrap();

        let mut report = ScanReport::default();
        process_batch(
            &store,
            &root,
            &opts(),
            // The file is gone but a stale Modify event arrived.
            &[make_event(
                EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
                vec![p],
            )],
            &mut report,
        )
        .unwrap();
        assert_eq!(report.removed, 1);
    }

    #[test]
    fn paired_rename_event_replaces_uri() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = store();
        let from = root.join("old.md");
        std::fs::write(&from, "alpha").unwrap();
        process_file(&store, &from, &opts()).unwrap();

        let to = root.join("new.md");
        std::fs::rename(&from, &to).unwrap();

        let mut report = ScanReport::default();
        process_batch(
            &store,
            &root,
            &opts(),
            &[make_event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                vec![from.clone(), to.clone()],
            )],
            &mut report,
        )
        .unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(report.inserted, 1);

        let stored = store
            .engine()
            .metadata
            .get(&format!("file://{}", to.display()))
            .unwrap()
            .unwrap();
        assert_eq!(stored.kind, SourceKind::IndexText);
    }

    #[test]
    fn event_outside_root_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = store();
        let outside = other.path().join("a.md");
        std::fs::write(&outside, "alpha").unwrap();

        let mut report = ScanReport::default();
        process_batch(
            &store,
            &root,
            &opts(),
            &[make_event(
                EventKind::Create(CreateKind::File),
                vec![outside],
            )],
            &mut report,
        )
        .unwrap();
        assert_eq!(report.inserted, 0);
        assert_eq!(report.removed, 0);
    }

    #[test]
    fn always_ignore_dir_event_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = store();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let p = root.join(".git/HEAD");
        std::fs::write(&p, "ref: refs/heads/main\n").unwrap();

        // Even though `HEAD` is plain text, `.git/` is in
        // ALWAYS_IGNORE_DIRS so the watcher must skip it.
        let mut report = ScanReport::default();
        process_batch(
            &store,
            &root,
            &opts(),
            &[make_event(EventKind::Create(CreateKind::File), vec![p])],
            &mut report,
        )
        .unwrap();
        assert_eq!(report.inserted, 0);
    }

    #[test]
    fn lock_glob_event_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = store();
        let mut o = opts();
        o.extensions.push("lock".to_string());
        let p = root.join("Cargo.lock");
        std::fs::write(&p, "[[package]]\n").unwrap();

        let mut report = ScanReport::default();
        process_batch(
            &store,
            &root,
            &o,
            &[make_event(EventKind::Create(CreateKind::File), vec![p])],
            &mut report,
        )
        .unwrap();
        assert_eq!(report.inserted, 0);
    }

    #[test]
    fn noisy_editor_batch_is_filtered_before_indexing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = store();
        let mut o = opts();
        o.extensions.extend(
            [
                "kate-swp", "lock", "lockb", "log", "md#", "md~", "pyc", "pyo", "swo", "swp",
                "swpx",
            ]
            .into_iter()
            .map(str::to_string),
        );

        let mut events = Vec::new();
        for i in 0..50 {
            for name in [
                format!(".#draft-{i}.md"),
                format!("#draft-{i}.md#"),
                format!(".draft-{i}.swp"),
                format!(".draft-{i}.swo"),
                format!(".draft-{i}.swpx"),
                format!(".draft-{i}.kate-swp"),
                format!("draft-{i}.md~"),
                format!("Cargo-{i}.lock"),
                format!("package-{i}.lockb"),
                format!("cache-{i}.pyc"),
                format!("cache-{i}.pyo"),
                format!("watchexec.{i}.log"),
            ] {
                let path = root.join(name);
                std::fs::write(&path, "scratch").unwrap();
                events.push(make_event(EventKind::Create(CreateKind::File), vec![path]));
            }
        }

        let real = root.join("real.md");
        std::fs::write(&real, "real memory").unwrap();
        events.push(make_event(
            EventKind::Create(CreateKind::File),
            vec![real.clone()],
        ));

        let mut report = ScanReport::default();
        process_batch(&store, &root, &o, &events, &mut report).unwrap();

        assert_eq!(report.inserted, 1);
        assert_eq!(store.engine().metadata.stats().unwrap().total_sources, 1);
        assert!(store
            .engine()
            .metadata
            .get(&format!("file://{}", real.display()))
            .unwrap()
            .is_some());
    }

    #[test]
    fn matches_simple_glob_handles_star_prefix() {
        assert!(matches_simple_glob("*.lock", "Cargo.lock"));
        assert!(matches_simple_glob("*.lockb", "package.lockb"));
        assert!(!matches_simple_glob("*.lock", "Cargo.toml"));
        assert!(matches_simple_glob(".#*", ".#draft.md"));
        assert!(matches_simple_glob("#*#", "#draft.md#"));
        assert!(matches_simple_glob("*.sw?", ".draft.swp"));
        assert!(matches_simple_glob("watchexec.*.log", "watchexec.42.log"));
        assert!(!matches_simple_glob("watchexec.*.log", "watchexec.log"));
        assert!(matches_simple_glob("exact", "exact"));
    }
}
