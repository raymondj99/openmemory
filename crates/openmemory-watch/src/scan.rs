//! Tree walking + ignore-file resolution.
//!
//! Wraps the [`ignore`] crate's [`WalkBuilder`] with the watcher's
//! always-ignore overrides ([`ALWAYS_IGNORE_DIRS`],
//! [`ALWAYS_IGNORE_GLOBS`]) and the per-tree
//! [`IGNORE_FILE_NAME`] custom ignore file. Returns an iterator of
//! candidate file paths that the indexer (commit 9b's other half)
//! filters by extension + size.
//!
//! The walker respects standard `.gitignore` precedence (file +
//! parent + global), hides hidden files (`.bashrc` etc.) by default,
//! and lets a tree opt back into specific patterns via
//! `.openmemory-ignore` (which has *higher* precedence than
//! `.gitignore`, matching the [`ignore::WalkBuilder`] documented
//! semantics).

use std::path::{Path, PathBuf};

use ignore::{overrides::OverrideBuilder, WalkBuilder};

use crate::error::{WatchError, WatchResult};
use crate::{
    has_indexable_extension, WatchOptions, ALWAYS_IGNORE_DIRS, ALWAYS_IGNORE_GLOBS,
    IGNORE_FILE_NAME,
};

/// Build a configured `ignore::Walk` rooted at `root`. Returns the
/// pre-built walker; callers iterate it via [`iter_indexable`] to apply
/// the per-extension / per-size filters.
pub fn build_walker(root: &Path) -> WatchResult<ignore::Walk> {
    let mut overrides = OverrideBuilder::new(root);
    for dir in ALWAYS_IGNORE_DIRS {
        // Two patterns each: top-level + nested. ignore-crate gitignore
        // syntax treats `name/**` as anchored to the override root, so a
        // bare `!.git/**` would miss `subdir/.git/**`. The `!**/...` form
        // covers nested cases.
        overrides.add(&format!("!{dir}/"))?;
        overrides.add(&format!("!**/{dir}/"))?;
    }
    for glob in ALWAYS_IGNORE_GLOBS {
        overrides.add(&format!("!**/{glob}"))?;
    }
    let overrides = overrides.build()?;

    let walker = WalkBuilder::new(root)
        .overrides(overrides)
        .add_custom_ignore_filename(IGNORE_FILE_NAME)
        // standard_filters covers .gitignore + .ignore + hidden + git
        // global ignore. Keep it on so the watcher inherits a user's
        // existing ignore-file investment without duplicating rules.
        .standard_filters(true)
        // Walk parent .gitignore files so a watch root inside a repo
        // honours the repo's ignore policy.
        .parents(true)
        .build();
    Ok(walker)
}

/// Map a configured walker to the candidate file iterator. Filters:
///
/// 1. Skip non-files (directories, symlinks-to-dir, etc.).
/// 2. Skip files whose extension isn't in `options.extensions`.
/// 3. Surface `ignore::Error`s as `WatchError::Ignore` so the caller
///    can log + continue without aborting the scan.
pub fn iter_indexable(
    walker: ignore::Walk,
    options: &WatchOptions,
) -> impl Iterator<Item = WatchResult<PathBuf>> + '_ {
    walker.filter_map(move |entry| match entry {
        Err(e) => Some(Err(WatchError::from(e))),
        Ok(entry) => {
            // file_type() is None for the root entry on some platforms;
            // treat that conservatively as "not a file" rather than
            // unwrapping.
            let is_file = entry.file_type().is_some_and(|t| t.is_file());
            if !is_file {
                return None;
            }
            let path = entry.path();
            if !has_indexable_extension(path, &options.extensions) {
                return None;
            }
            Some(Ok(path.to_path_buf()))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    fn opts_with(extensions: &[&str]) -> WatchOptions {
        WatchOptions {
            debounce: std::time::Duration::from_millis(50),
            extensions: extensions.iter().map(|s| (*s).to_string()).collect(),
            max_size: u64::MAX,
            initial_scan: true,
        }
    }

    fn collect(walker: ignore::Walk, options: &WatchOptions) -> HashSet<PathBuf> {
        iter_indexable(walker, options)
            .filter_map(Result::ok)
            .collect()
    }

    #[test]
    fn iter_yields_only_indexable_extensions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "# hello").unwrap();
        fs::write(dir.path().join("b.txt"), "world").unwrap();
        fs::write(dir.path().join("c.bin"), [0u8; 4]).unwrap();
        fs::write(dir.path().join("d"), "no extension").unwrap();

        let walker = build_walker(dir.path()).unwrap();
        let found = collect(walker, &opts_with(&["md", "txt"]));
        let names: HashSet<_> = found
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect();
        assert!(names.contains("a.md"));
        assert!(names.contains("b.txt"));
        assert!(!names.contains("c.bin"));
        assert!(!names.contains("d"));
    }

    #[test]
    fn iter_skips_always_ignored_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        fs::write(dir.path().join(".git/config"), "[core]").unwrap();
        fs::write(dir.path().join("target/build.md"), "do not index").unwrap();
        fs::write(dir.path().join("node_modules/pkg.md"), "do not index").unwrap();
        fs::write(dir.path().join("real.md"), "index me").unwrap();

        let walker = build_walker(dir.path()).unwrap();
        let found = collect(walker, &opts_with(&["md"]));
        let names: HashSet<_> = found
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect();
        assert_eq!(names, HashSet::from(["real.md".to_string()]));
    }

    #[test]
    fn iter_skips_always_ignored_glob_patterns() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.lock"), "lock").unwrap();
        fs::write(dir.path().join("real.txt"), "ok").unwrap();

        let walker = build_walker(dir.path()).unwrap();
        let found = collect(walker, &opts_with(&["lock", "txt"]));
        let names: HashSet<_> = found
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect();
        assert!(names.contains("real.txt"));
        assert!(!names.contains("Cargo.lock"));
    }

    #[test]
    fn iter_skips_editor_and_tool_noise_even_when_extensions_are_allowed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("#draft.md#"), "emacs autosave").unwrap();
        fs::write(dir.path().join("draft.md~"), "backup").unwrap();
        fs::write(dir.path().join("watchexec.42.log"), "tool log").unwrap();
        fs::write(dir.path().join("cache.pyc"), "bytecode").unwrap();
        fs::write(dir.path().join("real.md"), "ok").unwrap();

        let walker = build_walker(dir.path()).unwrap();
        let found = collect(walker, &opts_with(&["md", "md#", "md~", "log", "pyc"]));
        let names: HashSet<_> = found
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect();
        assert_eq!(names, HashSet::from(["real.md".to_string()]));
    }

    #[test]
    fn iter_honours_openmemory_ignore_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".openmemory-ignore"), "secret.md\n").unwrap();
        fs::write(dir.path().join("public.md"), "public").unwrap();
        fs::write(dir.path().join("secret.md"), "secret").unwrap();

        let walker = build_walker(dir.path()).unwrap();
        let found = collect(walker, &opts_with(&["md"]));
        let names: HashSet<_> = found
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect();
        assert!(names.contains("public.md"));
        assert!(!names.contains("secret.md"));
    }

    #[test]
    fn iter_honours_gitignore_inside_git_repo() {
        // ignore::WalkBuilder::standard_filters reads .gitignore only
        // when a parent `.git/` exists — that's the documented contract,
        // so the test seeds one. ALWAYS_IGNORE_DIRS keeps `.git`'s own
        // contents from leaking into the scan.
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".gitignore"), "draft/\n").unwrap();
        fs::create_dir_all(dir.path().join("draft")).unwrap();
        fs::write(dir.path().join("draft/wip.md"), "wip").unwrap();
        fs::write(dir.path().join("ship.md"), "ship").unwrap();

        let walker = build_walker(dir.path()).unwrap();
        let found = collect(walker, &opts_with(&["md"]));
        let names: HashSet<_> = found
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect();
        assert!(names.contains("ship.md"));
        assert!(!names.contains("wip.md"));
    }

    #[test]
    fn iter_honours_dot_ignore_file() {
        // .ignore (the ignore-crate's universal ignore file) does not
        // require a .git dir to take effect, so it's the right knob for
        // tooling that wants to layer ignore rules without depending on
        // git. Useful when the watcher runs over a non-repo tree.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".ignore"), "wip.md\n").unwrap();
        fs::write(dir.path().join("wip.md"), "wip").unwrap();
        fs::write(dir.path().join("ship.md"), "ship").unwrap();

        let walker = build_walker(dir.path()).unwrap();
        let found = collect(walker, &opts_with(&["md"]));
        let names: HashSet<_> = found
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect();
        assert!(names.contains("ship.md"));
        assert!(!names.contains("wip.md"));
    }

    #[test]
    fn iter_lowercases_extension_for_match() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("README.MD"), "uppercase").unwrap();
        let walker = build_walker(dir.path()).unwrap();
        let found = collect(walker, &opts_with(&["md"]));
        assert_eq!(found.len(), 1);
    }
}
