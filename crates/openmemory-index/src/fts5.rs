//! SQLite FTS5 keyword backend.
//!
//! Default `FullTextStore`. Wraps an FTS5 virtual table over a single
//! connection guarded by a [`Mutex`]. BM25 ranking is computed by SQLite via
//! the built-in `rank` alias.
//!
//! User queries are escaped before being passed to FTS5 — each whitespace
//! token becomes a quoted exact match, with a bare prefix variant for
//! tokens of length ≥ 4. See the private `fts5_escape` helper for details.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::error::{IndexError, IndexResult};
use crate::traits::{FullTextStore, IndexEntry, SearchResult};

/// SQLite FTS5-backed full-text store.
#[derive(Debug)]
pub struct Fts5Store {
    conn: Mutex<Connection>,
}

impl Fts5Store {
    /// Open or create an FTS5 database at `path`.
    pub fn open(path: &Path) -> IndexResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory FTS5 store (for tests).
    pub fn open_in_memory() -> IndexResult<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn configure(conn: &Connection) -> IndexResult<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             PRAGMA cache_size=-8000;",
        )?;
        Ok(())
    }

    fn init_schema(conn: &Connection) -> IndexResult<()> {
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                 uri UNINDEXED,
                 text,
                 chunk_index UNINDEXED,
                 tokenize = 'unicode61 remove_diacritics 2'
             );",
        )?;
        Ok(())
    }

    fn lock(&self) -> IndexResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| IndexError::Lock(e.to_string()))
    }
}

impl FullTextStore for Fts5Store {
    fn insert(&self, entries: &[IndexEntry]) -> IndexResult<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO chunks_fts (uri, text, chunk_index)
                 VALUES (?1, ?2, ?3)",
            )?;
            for e in entries {
                stmt.execute(params![e.uri, e.text, i64::from(e.chunk_index)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn search(&self, query: &str, top_k: usize) -> IndexResult<Vec<SearchResult>> {
        let escaped = fts5_escape(query);
        if escaped.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT uri, text, chunk_index, -rank
             FROM chunks_fts WHERE chunks_fts MATCH ?1
             ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![escaped, top_k as i64], |row| {
            let uri: String = row.get(0)?;
            let text: String = row.get(1)?;
            let chunk_index: i64 = row.get(2)?;
            let score: f64 = row.get(3)?;
            Ok((uri, text, chunk_index as u32, score as f32))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (uri, text, chunk_index, score) = row?;
            out.push(SearchResult {
                uri,
                text,
                chunk_index,
                score,
            });
        }

        // Tiny corpora collapse FTS5 BM25 toward zero. Rescale so the top
        // result reads as 1.0 when scores would otherwise be near-zero.
        if let Some(max) = out.iter().map(|r| r.score).reduce(f32::max) {
            if max > 0.0 && max < 0.01 {
                let scale = 1.0 / max;
                for r in &mut out {
                    r.score *= scale;
                }
            }
        }
        Ok(out)
    }

    fn delete_by_uri(&self, uri: &str) -> IndexResult<u64> {
        let conn = self.lock()?;
        let n = conn.execute("DELETE FROM chunks_fts WHERE uri = ?1", params![uri])?;
        Ok(n as u64)
    }
}

/// FTS5 reserved words that must not appear as bare prefix tokens.
const FTS5_OPERATORS: &[&str] = &["AND", "OR", "NOT", "NEAR"];

/// Escape a free-form query for FTS5 MATCH. Each token becomes a quoted exact
/// match; tokens ≥ 4 alphanumeric characters also get an unquoted prefix
/// variant. Tokens are joined with `OR`.
pub(crate) fn fts5_escape(query: &str) -> String {
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|w| w.len() >= 2)
        .filter_map(|word| {
            let escaped = word.replace('"', "\"\"");
            let cleaned: String = word
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let safe_for_prefix = cleaned.len() >= 4
                && !FTS5_OPERATORS
                    .iter()
                    .any(|op| cleaned.eq_ignore_ascii_case(op));
            if safe_for_prefix {
                Some(format!("(\"{escaped}\" OR {cleaned}*)"))
            } else if !cleaned.is_empty() {
                Some(format!("\"{escaped}\""))
            } else {
                None
            }
        })
        .collect();
    if terms.is_empty() {
        String::new()
    } else {
        terms.join(" OR ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(uri: &str, text: &str, idx: u32) -> IndexEntry {
        IndexEntry {
            uri: uri.into(),
            text: text.into(),
            chunk_index: idx,
            vector: Vec::new(),
        }
    }

    #[test]
    fn insert_and_search() {
        let store = Fts5Store::open_in_memory().unwrap();
        store
            .insert(&[
                entry("u://a", "the quick brown fox jumps over the lazy dog", 0),
                entry("u://b", "rust programming language systems", 0),
            ])
            .unwrap();
        let r = store.search("quick brown fox", 10).unwrap();
        assert!(!r.is_empty());
        assert_eq!(r[0].uri, "u://a");
    }

    #[test]
    fn empty_query_returns_empty() {
        let store = Fts5Store::open_in_memory().unwrap();
        store.insert(&[entry("u", "anything at all", 0)]).unwrap();
        assert!(store.search("", 10).unwrap().is_empty());
        assert!(store.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn single_char_terms_filtered() {
        let store = Fts5Store::open_in_memory().unwrap();
        store.insert(&[entry("u", "a b c d", 0)]).unwrap();
        assert!(store.search("a b c", 10).unwrap().is_empty());
    }

    #[test]
    fn delete_removes_rows() {
        let store = Fts5Store::open_in_memory().unwrap();
        store
            .insert(&[
                entry("u://a", "hello world greeting", 0),
                entry("u://b", "goodbye world farewell", 0),
            ])
            .unwrap();
        let removed = store.delete_by_uri("u://a").unwrap();
        assert_eq!(removed, 1);
        let r = store.search("hello greeting", 10).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn bm25_ranks_relevant_first() {
        let store = Fts5Store::open_in_memory().unwrap();
        store
            .insert(&[
                entry(
                    "u://relevant",
                    "rust programming language systems programming",
                    0,
                ),
                entry("u://irrelevant", "cooking recipes for delicious meals", 0),
            ])
            .unwrap();
        let r = store.search("rust programming", 10).unwrap();
        assert_eq!(r[0].uri, "u://relevant");
    }

    #[test]
    fn special_chars_in_query_dont_panic() {
        let store = Fts5Store::open_in_memory().unwrap();
        store
            .insert(&[entry("u://code", "handling C++ templates", 0)])
            .unwrap();
        let r = store.search("C++ templates", 10).unwrap();
        assert!(!r.is_empty());
    }

    #[test]
    fn persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fts5.db");
        {
            let store = Fts5Store::open(&path).unwrap();
            store
                .insert(&[entry("u://a", "persistent search data", 0)])
                .unwrap();
        }
        let store = Fts5Store::open(&path).unwrap();
        let r = store.search("persistent search", 10).unwrap();
        assert!(!r.is_empty());
        assert_eq!(r[0].uri, "u://a");
    }

    #[test]
    fn flush_default_is_no_op() {
        let store = Fts5Store::open_in_memory().unwrap();
        FullTextStore::flush(&store).unwrap();
    }

    #[test]
    fn fts5_escape_rules() {
        assert_eq!(
            fts5_escape("hello world"),
            "(\"hello\" OR hello*) OR (\"world\" OR world*)"
        );
        assert_eq!(fts5_escape("a b c"), "");
        assert_eq!(fts5_escape("rust"), "(\"rust\" OR rust*)");
        assert_eq!(fts5_escape("go is ok"), "\"go\" OR \"is\" OR \"ok\"");
        assert_eq!(fts5_escape("NEAR miss"), "\"NEAR\" OR (\"miss\" OR miss*)");
        assert_eq!(fts5_escape(""), "");
    }
}
