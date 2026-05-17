//! SQLite FTS5 keyword backend.
//!
//! Default `FullTextStore`. Wraps an FTS5 virtual table over a single
//! connection guarded by a [`Mutex`]. BM25 ranking is computed by SQLite via
//! the built-in `rank` alias.
//!
//! User queries are escaped before being passed to FTS5: each whitespace
//! token becomes a quoted exact match, with a bare prefix variant for
//! tokens of length >= 4. See the private `fts5_escape` helper for details.
//!
//! ## Fielded indexing via repetition
//!
//! v0.3 introduced caller-supplied fielded inputs on `IndexEntry`
//! (`title`, `summary`, `concepts`, `source_files`, `source_kind`,
//! `entity_type`, `entity_name`). To weight matches across those fields
//! without paying the per-row cost of a multi-column FTS5 virtual table,
//! the writer concatenates the field values into the single `text` column
//! with high-weight fields repeated `n` times. That simulates per-field
//! BM25 weighting at index time while keeping the search path on the fast
//! single-column `rank` rankfn.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::error::{IndexError, IndexResult};
use crate::traits::{FullTextStore, IndexEntry, SearchResult};

/// Per-field weights used by [`fielded_text`] to encode caller-supplied
/// fielded `IndexEntry` content as a single FTS5-friendly string. Order:
/// `title`, `text`, `summary`, `concepts`, `source_files`,
/// `source_kind`, `entity_type`, `entity_name`.
pub type FieldWeights = [f32; 8];

/// Default per-field weights. Biases title and entity_name over body
/// text. Weights are interpreted as token-repetition counts (post-scale)
/// when the indexer concatenates field values into the FTS5 `text`
/// column.
pub const DEFAULT_FIELD_WEIGHTS: FieldWeights = [5.0, 1.0, 2.0, 2.0, 2.0, 0.5, 0.5, 4.0];

/// Multiplier applied to per-field weights before rounding to an integer
/// repetition count. With `WEIGHT_SCALE = 2.0`, a weight of 0.5 yields one
/// repetition, 1.0 yields two, 2.0 yields four, etc. Without the scale,
/// `0.5.round() == 1` would collapse sub-unit weights onto the same
/// repetition count as the 1.0 body-text baseline.
pub const WEIGHT_SCALE: f32 = 2.0;

/// SQLite FTS5-backed full-text store.
#[derive(Debug)]
pub struct Fts5Store {
    conn: Mutex<Connection>,
    field_weights: FieldWeights,
}

impl Fts5Store {
    /// Open or create an FTS5 database at `path`.
    pub fn open(path: &Path) -> IndexResult<Self> {
        Self::open_with_field_weights(path, DEFAULT_FIELD_WEIGHTS)
    }

    /// Open or create an FTS5 database at `path` with caller-supplied
    /// field weights for newly inserted fielded entries.
    pub fn open_with_field_weights(path: &Path, field_weights: FieldWeights) -> IndexResult<Self> {
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
            field_weights,
        })
    }

    /// Open an in-memory FTS5 store (for tests).
    pub fn open_in_memory() -> IndexResult<Self> {
        Self::open_in_memory_with_field_weights(DEFAULT_FIELD_WEIGHTS)
    }

    /// Open an in-memory FTS5 store with custom field weights (for tests).
    pub fn open_in_memory_with_field_weights(field_weights: FieldWeights) -> IndexResult<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            field_weights,
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

/// Concatenate an `IndexEntry`'s fielded content into a single FTS5
/// text payload. High-weight fields (`title`, `entity_name`) are
/// repeated to bias BM25 toward matches there; the same `weights`
/// vector configures every per-field multiplier. Empty fields contribute
/// nothing.
///
/// Weights are scaled by [`WEIGHT_SCALE`] before being rounded to an
/// integer repetition count, so sub-unit weights (e.g. 0.5) still
/// down-weight relative to the `text` baseline (1.0) instead of being
/// rounded up to a single repetition.
#[must_use]
pub fn fielded_text(entry: &IndexEntry, weights: &FieldWeights) -> String {
    fn repeat(out: &mut String, value: &str, w: f32) {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return;
        }
        let n = (w.max(0.0) * WEIGHT_SCALE).round() as u32;
        if n == 0 {
            return;
        }
        for _ in 0..n {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(trimmed);
        }
    }

    let [w_title, w_text, w_summary, w_concepts, w_files, w_kind, w_etype, w_ename] = *weights;
    let mut out = String::new();
    if let Some(title) = entry.title.as_deref() {
        repeat(&mut out, title, w_title);
    }
    if let Some(summary) = entry.summary.as_deref() {
        repeat(&mut out, summary, w_summary);
    }
    if let Some(name) = entry.entity_name.as_deref() {
        repeat(&mut out, name, w_ename);
    }
    repeat(&mut out, &entry.text, w_text);
    for concept in &entry.concepts {
        repeat(&mut out, concept, w_concepts);
    }
    for file in &entry.source_files {
        repeat(&mut out, file, w_files);
    }
    if let Some(kind) = entry.source_kind.as_deref() {
        repeat(&mut out, kind, w_kind);
    }
    if let Some(et) = entry.entity_type.as_deref() {
        repeat(&mut out, et, w_etype);
    }
    if out.is_empty() {
        entry.text.clone()
    } else {
        out
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
                let payload = if entry_is_fielded(e) {
                    fielded_text(e, &self.field_weights)
                } else {
                    e.text.clone()
                };
                stmt.execute(params![e.uri, payload, i64::from(e.chunk_index)])?;
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

    fn count(&self) -> IndexResult<u64> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM chunks_fts", [], |row| row.get(0))?;
        Ok(n as u64)
    }
}

fn entry_is_fielded(e: &IndexEntry) -> bool {
    e.title.is_some()
        || e.summary.is_some()
        || e.entity_name.is_some()
        || e.entity_type.is_some()
        || e.source_kind.is_some()
        || !e.concepts.is_empty()
        || !e.source_files.is_empty()
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
        IndexEntry::new(uri, text).with_chunk_index(idx)
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
    fn fielded_title_match_outranks_body_match_under_default_weights() {
        let store = Fts5Store::open_in_memory().unwrap();
        store
            .insert(&[
                IndexEntry::new("u://body", "alpha beta gamma")
                    .with_entity_name(Some("Body".into())),
                IndexEntry::new("u://title-hit", "lorem ipsum dolor sit")
                    .with_title(Some("alpha topic".into()))
                    .with_entity_name(Some("Title".into())),
            ])
            .unwrap();
        let r = store.search("alpha", 10).unwrap();
        assert!(!r.is_empty(), "search returned no rows");
        assert_eq!(
            r[0].uri, "u://title-hit",
            "title match should outrank body match under default weights, got {r:?}",
        );
    }

    #[test]
    fn fielded_text_concatenates_with_repetition() {
        let mut entry = IndexEntry::new("u://x", "body");
        entry.title = Some("CAPS".into());
        entry.summary = Some("SUMMARY".into());
        entry.entity_name = Some("NAME".into());
        let payload = fielded_text(&entry, &DEFAULT_FIELD_WEIGHTS);
        // Repetition counts are `(weight * WEIGHT_SCALE).round()`. With
        // WEIGHT_SCALE = 2.0 and default weights:
        //   title 5.0   -> 10 reps of CAPS
        //   summary 2.0 -> 4  reps of SUMMARY
        //   ename 4.0   -> 8  reps of NAME
        //   text 1.0    -> 2  reps of body
        let scale = WEIGHT_SCALE.round() as usize;
        assert_eq!(payload.matches("CAPS").count(), 5 * scale);
        assert_eq!(payload.matches("SUMMARY").count(), 2 * scale);
        assert_eq!(payload.matches("NAME").count(), 4 * scale);
        assert_eq!(payload.matches("body").count(), scale);
    }

    #[test]
    fn fielded_text_respects_sub_unit_weights() {
        let mut entry = IndexEntry::new("u://x", "body");
        entry.source_kind = Some("hint".into());
        // source_kind weight is 0.5 in DEFAULT_FIELD_WEIGHTS. WEIGHT_SCALE = 2.0
        // gives 1 repetition — strictly less than the text baseline (2).
        let payload = fielded_text(&entry, &DEFAULT_FIELD_WEIGHTS);
        assert!(payload.matches("hint").count() < payload.matches("body").count());
        assert!(payload.matches("hint").count() >= 1);
    }

    #[test]
    fn fielded_summary_is_searchable() {
        let store = Fts5Store::open_in_memory().unwrap();
        store
            .insert(&[IndexEntry::new("u://summary", "ordinary body")
                .with_summary(Some("rare summary token".into()))])
            .unwrap();
        let r = store.search("rare", 10).unwrap();
        assert_eq!(r[0].uri, "u://summary");
    }

    #[test]
    fn custom_field_weights_affect_new_inserts() {
        let weights = [0.0, 1.0, 6.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let store = Fts5Store::open_in_memory_with_field_weights(weights).unwrap();
        store
            .insert(&[IndexEntry::new("u://x", "body").with_summary(Some("weighted".into()))])
            .unwrap();
        let r = store.search("weighted", 10).unwrap();
        assert_eq!(r[0].uri, "u://x");
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
