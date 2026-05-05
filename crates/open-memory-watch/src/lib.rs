//! Filesystem watcher with incremental re-indexing for `open-memory`.
//!
//! Phase 9 surface: point [`Watcher`] at a directory, it walks the tree
//! once on startup (BLAKE3-deduped against the metadata store, so a
//! re-run of `open-memory watch` over an unchanged tree is free), then
//! tails [`notify-debouncer-full`] events to keep the index in sync as
//! files are created, modified, or removed.
//!
//! # URI shape
//!
//! Each watched file becomes one chunk under
//! `file://<absolute-path>`. v0.2 indexes whole files (`chunk_index =
//! 0`); chunked output for large files is queued for v0.3.
//!
//! # Ignore semantics
//!
//! Standard ignore-file precedence via the [`ignore`] crate, plus a
//! per-tree `.open-memory-ignore` file with the same syntax. The
//! `.git/`, `target/`, `node_modules/`, and `*.lock` directories /
//! patterns are always skipped, regardless of explicit configuration —
//! they are noise for an agent-memory index.
//!
//! # Concurrency
//!
//! [`Watcher::run`] is synchronous and blocking; the CLI spawns it on
//! a dedicated thread. The watcher takes an `Arc<MemoryStore>` so a
//! future `open-memory mcp --watch DIR` can share the same handle the
//! MCP server uses; cross-thread access is fine because `MemoryStore`
//! is `Send + Sync` (see the graph crate's concurrency docs).

#![forbid(unsafe_code)]

pub mod error;
pub mod events;
pub mod index;
pub mod runtime;
pub mod scan;

pub use error::{WatchError, WatchResult};
pub use index::{
    path_to_uri, process_file, remove_path, ProcessOutcome, ScanReport, SkipReason,
    SOURCE_TYPE_FILE_WATCHER,
};
pub use runtime::BatchSummary;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use open_memory_core::config::Config;
use open_memory_graph::MemoryStore;

/// Default extensions if neither `WatchSection::extensions` nor the
/// caller supplied an explicit list. Curated to match what an agent
/// would treat as observation-shaped text.
pub const DEFAULT_EXTENSIONS: &[&str] = &[
    "md", "markdown", "mdx", "txt", "org", "rst", // prose
    "rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "c", "h", "cpp", "hpp", // code
    "toml", "yaml", "yml", "json", // config
];

/// Always-skip directory names, regardless of any ignore-file config.
pub const ALWAYS_IGNORE_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "__pycache__"];

/// Always-skip glob patterns. Applied via the `ignore::WalkBuilder`
/// override layer in commit 9b.
pub const ALWAYS_IGNORE_GLOBS: &[&str] = &["*.lock", "*.lockb"];

/// Per-tree ignore-file name layered on top of `.gitignore`. Same
/// syntax; entries here win over standard ignore files.
pub const IGNORE_FILE_NAME: &str = ".open-memory-ignore";

/// Caller-tunable knobs for [`Watcher::new`]. Defaults are derived from
/// [`open_memory_core::config::WatchSection`]; the CLI surface lets a
/// user override per-invocation.
#[derive(Debug, Clone)]
pub struct WatchOptions {
    /// Quiet window before debounced events fire.
    pub debounce: Duration,
    /// File extensions (without leading dot) considered indexable.
    pub extensions: Vec<String>,
    /// Skip files larger than this many bytes.
    pub max_size: u64,
    /// Whether to do an initial-tree scan on startup. Almost always
    /// `true`; the test suite flips it off to isolate event-loop
    /// correctness.
    pub initial_scan: bool,
}

impl WatchOptions {
    /// Resolve effective options from a [`Config`]. Caller can clone +
    /// override before handing to [`Watcher::new`].
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let extensions = if config.watch.extensions.is_empty() {
            DEFAULT_EXTENSIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        } else {
            config.watch.extensions.clone()
        };
        Self {
            debounce: Duration::from_millis(config.watch.debounce_ms),
            extensions,
            max_size: config.watch.max_size,
            initial_scan: true,
        }
    }
}

/// Filesystem watcher. Constructed once per `open-memory watch` run.
///
/// The watcher does *not* own the [`MemoryStore`] — it borrows it via
/// `Arc` so a future "MCP server with embedded watcher" deployment can
/// share a single handle. Dropping the [`Watcher`] cancels the watcher
/// loop on the next iteration.
pub struct Watcher {
    pub(crate) memory: Arc<MemoryStore>,
    pub(crate) root: PathBuf,
    pub(crate) options: WatchOptions,
}

impl Watcher {
    /// Build a new watcher rooted at `root` with the given options. The
    /// root is canonicalised; a non-existent or non-directory path
    /// returns [`WatchError::InvalidInput`].
    pub fn new(
        memory: Arc<MemoryStore>,
        root: PathBuf,
        options: WatchOptions,
    ) -> WatchResult<Self> {
        let root = std::fs::canonicalize(&root).map_err(|e| {
            WatchError::InvalidInput(format!("could not canonicalise {}: {e}", root.display()))
        })?;
        if !root.is_dir() {
            return Err(WatchError::InvalidInput(format!(
                "watch root is not a directory: {}",
                root.display()
            )));
        }
        Ok(Self {
            memory,
            root,
            options,
        })
    }

    /// Canonicalised absolute path the watcher is rooted at.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Effective options after [`WatchOptions::from_config`] resolution.
    pub fn options(&self) -> &WatchOptions {
        &self.options
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_memory_graph::MemoryStore;

    fn store() -> Arc<MemoryStore> {
        Arc::new(MemoryStore::open_in_memory(&Config::default()).unwrap())
    }

    #[test]
    fn options_from_config_defaults_to_curated_extensions() {
        let opts = WatchOptions::from_config(&Config::default());
        assert_eq!(opts.debounce, Duration::from_millis(200));
        assert_eq!(opts.max_size, 10 * 1024 * 1024);
        assert!(opts.extensions.iter().any(|e| e == "md"));
        assert!(opts.extensions.iter().any(|e| e == "rs"));
        assert!(opts.initial_scan);
    }

    #[test]
    fn options_from_config_uses_custom_extensions_when_set() {
        let mut cfg = Config::default();
        cfg.watch.extensions = vec!["md".into(), "txt".into()];
        let opts = WatchOptions::from_config(&cfg);
        assert_eq!(opts.extensions, vec!["md".to_string(), "txt".to_string()]);
    }

    #[test]
    fn new_canonicalises_root() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        // Round-trip through a relative segment so canonicalisation
        // actually has work to do.
        let pretty = dir.path().join("a/./b");
        let watcher = Watcher::new(
            store(),
            pretty,
            WatchOptions::from_config(&Config::default()),
        )
        .unwrap();
        assert_eq!(watcher.root(), nested.canonicalize().unwrap());
    }

    #[test]
    fn new_rejects_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does/not/exist");
        match Watcher::new(
            store(),
            missing,
            WatchOptions::from_config(&Config::default()),
        ) {
            Ok(_) => panic!("expected error"),
            Err(WatchError::InvalidInput(_)) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn new_rejects_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.md");
        std::fs::write(&file, "hello").unwrap();
        match Watcher::new(store(), file, WatchOptions::from_config(&Config::default())) {
            Ok(_) => panic!("expected error"),
            Err(WatchError::InvalidInput(_)) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn always_ignore_directories_include_git_and_target() {
        assert!(ALWAYS_IGNORE_DIRS.contains(&".git"));
        assert!(ALWAYS_IGNORE_DIRS.contains(&"target"));
        assert!(ALWAYS_IGNORE_DIRS.contains(&"node_modules"));
    }

    #[test]
    fn ignore_file_name_is_dot_open_memory_ignore() {
        assert_eq!(IGNORE_FILE_NAME, ".open-memory-ignore");
    }
}
