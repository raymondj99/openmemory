// Module-level allow: the History API is exercised by unit tests
// below, but its only production caller (the Search panel) lands in
// PR2. We keep the skeleton in PR1 so the file layout is settled and
// PR2 has nothing to wire up beyond consuming the API.
#![allow(dead_code)]

//! Append-only search-history persistence for the Search panel.
//!
//! PR1 ships only the load/append skeleton: schema, file layout, and
//! cap behavior. The Search panel itself (PR2) consumes [`History`]
//! via `push` after every successful recall and renders the tail in
//! the history pane.
//!
//! File layout — one JSON record per line, newline-terminated:
//!
//! ```json
//! {"ts": 1748359380, "panel": "search", "query": "ratatui rendering loop", "hits": 12, "took_ms": 47}
//! ```
//!
//! Cap: 500 lines. On [`History::load`] we read the whole file (small
//! at this cap) and keep the most recent entries; if the cap is
//! exceeded the file is rewritten to its tail on next [`History::push`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Maximum number of history entries kept on disk. When the file
/// exceeds this we drop the oldest on the next write.
pub const HISTORY_CAP: usize = 500;

/// One recorded search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub ts: i64,
    pub panel: String,
    pub query: String,
    #[serde(default)]
    pub hits: u32,
    #[serde(default)]
    pub took_ms: u32,
}

/// In-memory mirror of the on-disk history. Cheap to clone: backed by
/// a `Vec` that is bounded by [`HISTORY_CAP`].
#[derive(Debug, Clone, Default)]
pub struct History {
    entries: Vec<HistoryEntry>,
    /// Tracks whether the on-disk file is known to exceed the cap so
    /// the next write rewrites instead of appends. Maintained
    /// internally.
    needs_rewrite: bool,
}

impl History {
    /// Load from `<data_dir>/tui/history.jsonl`. Best-effort: a missing
    /// file is an empty history; a malformed line is dropped with the
    /// rest of the entries preserved.
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut entries: Vec<HistoryEntry> = raw
            .lines()
            .filter_map(|l| serde_json::from_str::<HistoryEntry>(l).ok())
            .collect();
        let needs_rewrite = entries.len() > HISTORY_CAP;
        if needs_rewrite {
            // Keep the most recent entries.
            let start = entries.len() - HISTORY_CAP;
            entries.drain(..start);
        }
        Self {
            entries,
            needs_rewrite,
        }
    }

    /// Append a new entry. Persists synchronously — the cost is one
    /// `write` per query, which is well below recall latency.
    pub fn push(&mut self, data_dir: &Path, entry: HistoryEntry) {
        self.entries.push(entry);
        if self.entries.len() > HISTORY_CAP {
            let start = self.entries.len() - HISTORY_CAP;
            self.entries.drain(..start);
            self.needs_rewrite = true;
        }
        self.flush(data_dir);
    }

    /// Snapshot of the recorded entries, newest-first. Cheap-ish at
    /// 500 entries; cloning the strings dominates over the allocation.
    pub fn recent(&self, limit: usize) -> Vec<HistoryEntry> {
        self.entries.iter().rev().take(limit).cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn flush(&mut self, data_dir: &Path) {
        let path = Self::path(data_dir);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if self.needs_rewrite {
            // Rewrite to the tail.
            let mut out = String::with_capacity(self.entries.len() * 80);
            for e in &self.entries {
                if let Ok(line) = serde_json::to_string(e) {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
            let _ = std::fs::write(&path, out);
            self.needs_rewrite = false;
        } else if let Some(last) = self.entries.last() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                if let Ok(line) = serde_json::to_string(last) {
                    let _ = writeln!(f, "{line}");
                }
            }
        }
    }

    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("tui").join("history.jsonl")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts: i64, q: &str) -> HistoryEntry {
        HistoryEntry {
            ts,
            panel: "search".into(),
            query: q.into(),
            hits: 1,
            took_ms: 1,
        }
    }

    #[test]
    fn empty_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::load(dir.path());
        assert!(h.is_empty());
    }

    #[test]
    fn push_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::default();
        h.push(dir.path(), entry(1, "alpha"));
        h.push(dir.path(), entry(2, "beta"));

        let reloaded = History::load(dir.path());
        let recent = reloaded.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].query, "beta", "recent() is newest-first");
        assert_eq!(recent[1].query, "alpha");
    }

    #[test]
    fn cap_trims_oldest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::default();
        for i in 0..(HISTORY_CAP + 5) {
            h.push(dir.path(), entry(i as i64, &format!("q{i}")));
        }
        let reloaded = History::load(dir.path());
        let recent = reloaded.recent(HISTORY_CAP + 50);
        assert_eq!(recent.len(), HISTORY_CAP);
        // Newest preserved; first five dropped.
        assert_eq!(recent[0].query, format!("q{}", HISTORY_CAP + 4));
        assert!(recent.iter().all(|e| e.query != "q0"));
    }
}
