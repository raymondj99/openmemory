//! Pure-Rust BM25 inverted index. Used when the `fts5` feature is disabled.
//!
//! Optional persistence: open with a path and call [`Bm25Store::flush`] to
//! write the entire index as JSON. Open without a path for a purely
//! in-memory store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{IndexError, IndexResult};
use crate::traits::{ExportEntry, FullTextStore, IndexEntry, SearchResult};

/// BM25 ranking parameter `k1` — saturation of term frequency.
const K1: f64 = 1.2;
/// BM25 ranking parameter `b` — strength of length normalization.
const B: f64 = 0.75;
/// Default field weights for caller-supplied metadata. Mirrors the FTS5
/// backend's repetition-based weighting so no-default builds keep the same
/// searchable fields.
const DEFAULT_FIELD_WEIGHTS: [f32; 8] = [5.0, 1.0, 2.0, 2.0, 2.0, 0.5, 0.5, 4.0];

/// Multiplier applied to per-field weights before rounding to an integer
/// repetition count. Mirrors `openmemory_index::fts5::WEIGHT_SCALE` so the
/// two backends produce comparable surrogates.
const WEIGHT_SCALE: f32 = 2.0;

/// Pure-Rust BM25 store.
#[derive(Debug)]
pub struct Bm25Store {
    inner: Mutex<Bm25Inner>,
    path: Option<PathBuf>,
}

#[derive(Debug)]
struct Bm25Inner {
    /// term → list of (doc_id, term frequency)
    index: HashMap<String, Vec<(u32, f32)>>,
    /// doc_id → metadata
    docs: HashMap<u32, DocMeta>,
    /// Number of live documents.
    doc_count: u32,
    /// Average document length in tokens (over live docs).
    avg_dl: f64,
    /// Monotonic id counter.
    next_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DocMeta {
    uri: String,
    text: String,
    chunk_index: u32,
    doc_len: u32,
}

#[derive(Serialize, Deserialize)]
struct PersistedBm25 {
    doc_count: u32,
    avg_dl: f64,
    next_id: u32,
    docs: Vec<PersistedDoc>,
    index: HashMap<String, Vec<(u32, f32)>>,
}

#[derive(Serialize, Deserialize)]
struct PersistedDoc {
    id: u32,
    #[serde(flatten)]
    meta: DocMeta,
}

impl Bm25Store {
    /// Pure in-memory store, no on-disk persistence.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Bm25Inner::new()),
            path: None,
        }
    }

    /// Open or create a JSON-backed store at `path`.
    pub fn open(path: &Path) -> IndexResult<Self> {
        let inner = if path.exists() {
            let data = std::fs::read_to_string(path)?;
            Self::deserialize(&data)?
        } else {
            Bm25Inner::new()
        };
        Ok(Self {
            inner: Mutex::new(inner),
            path: Some(path.to_path_buf()),
        })
    }

    fn lock(&self) -> IndexResult<std::sync::MutexGuard<'_, Bm25Inner>> {
        self.inner
            .lock()
            .map_err(|e| IndexError::Lock(e.to_string()))
    }

    fn save(&self) -> IndexResult<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let inner = self.lock()?;
        let serialized = Self::serialize(&inner);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, serialized)?;
        Ok(())
    }

    fn serialize(inner: &Bm25Inner) -> String {
        let p = PersistedBm25 {
            doc_count: inner.doc_count,
            avg_dl: inner.avg_dl,
            next_id: inner.next_id,
            docs: inner
                .docs
                .iter()
                .map(|(&id, meta)| PersistedDoc {
                    id,
                    meta: meta.clone(),
                })
                .collect(),
            index: inner.index.clone(),
        };
        serde_json::to_string(&p).expect("BM25 serialization is infallible")
    }

    fn deserialize(data: &str) -> IndexResult<Bm25Inner> {
        let p: PersistedBm25 = serde_json::from_str(data).map_err(|e| IndexError::Corrupt {
            path: PathBuf::new(),
            detail: format!("bm25 json: {e}"),
        })?;
        let docs = p.docs.into_iter().map(|d| (d.id, d.meta)).collect();
        Ok(Bm25Inner {
            index: p.index,
            docs,
            doc_count: p.doc_count,
            avg_dl: p.avg_dl,
            next_id: p.next_id,
        })
    }
}

impl Default for Bm25Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm25Inner {
    fn new() -> Self {
        Self {
            index: HashMap::new(),
            docs: HashMap::new(),
            doc_count: 0,
            avg_dl: 0.0,
            next_id: 0,
        }
    }

    fn add_doc(&mut self, uri: String, display_text: String, index_text: &str, chunk_index: u32) {
        let id = self.next_id;
        self.next_id += 1;

        let tokens = tokenize(index_text);
        let doc_len = tokens.len() as u32;
        let mut term_freqs: HashMap<String, u32> = HashMap::new();
        for tok in &tokens {
            *term_freqs.entry(tok.clone()).or_insert(0) += 1;
        }
        for (term, count) in term_freqs {
            self.index.entry(term).or_default().push((id, count as f32));
        }

        let total = self.avg_dl * f64::from(self.doc_count) + f64::from(doc_len);
        self.doc_count += 1;
        self.avg_dl = total / f64::from(self.doc_count);

        self.docs.insert(
            id,
            DocMeta {
                uri,
                text: display_text,
                chunk_index,
                doc_len,
            },
        );
    }

    fn remove_doc(&mut self, doc_id: u32) {
        let Some(meta) = self.docs.remove(&doc_id) else {
            return;
        };
        if self.doc_count > 1 {
            let total = self.avg_dl * f64::from(self.doc_count) - f64::from(meta.doc_len);
            self.doc_count -= 1;
            self.avg_dl = total / f64::from(self.doc_count);
        } else {
            self.doc_count = 0;
            self.avg_dl = 0.0;
        }
        self.index.retain(|_, postings| {
            postings.retain(|(id, _)| *id != doc_id);
            !postings.is_empty()
        });
    }

    fn search(&self, query: &str, top_k: usize) -> Vec<(u32, f64)> {
        let n = f64::from(self.doc_count);
        if n == 0.0 {
            return Vec::new();
        }
        let mut scores: HashMap<u32, f64> = HashMap::new();
        for term in tokenize(query) {
            let Some(postings) = self.index.get(&term) else {
                continue;
            };
            let df = postings.len() as f64;
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            for &(doc_id, tf) in postings {
                let Some(meta) = self.docs.get(&doc_id) else {
                    continue;
                };
                let dl = f64::from(meta.doc_len);
                let tf = f64::from(tf);
                let num = tf * (K1 + 1.0);
                let denom = tf + K1 * (1.0 - B + B * dl / self.avg_dl);
                *scores.entry(doc_id).or_insert(0.0) += idf * num / denom;
            }
        }
        let mut results: Vec<(u32, f64)> = scores.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(top_k);
        results
    }
}

impl FullTextStore for Bm25Store {
    fn export_all(&self) -> IndexResult<Vec<ExportEntry>> {
        let inner = self.lock()?;
        let mut out: Vec<ExportEntry> = inner
            .docs
            .values()
            .map(|doc| ExportEntry {
                uri: doc.uri.clone(),
                text: doc.text.clone(),
                chunk_index: doc.chunk_index,
                vector: Vec::new(),
            })
            .collect();
        out.sort_by(|a, b| (a.uri.as_str(), a.chunk_index).cmp(&(b.uri.as_str(), b.chunk_index)));
        Ok(out)
    }

    fn insert(&self, entries: &[IndexEntry]) -> IndexResult<()> {
        let mut inner = self.lock()?;
        for e in entries {
            let index_text = if entry_is_fielded(e) {
                fielded_text(e, &DEFAULT_FIELD_WEIGHTS)
            } else {
                e.text.clone()
            };
            inner.add_doc(e.uri.clone(), e.text.clone(), &index_text, e.chunk_index);
        }
        Ok(())
    }

    fn search(&self, query: &str, top_k: usize) -> IndexResult<Vec<SearchResult>> {
        let inner = self.lock()?;
        let scored = inner.search(query, top_k);
        let out = scored
            .into_iter()
            .filter_map(|(id, score)| {
                let meta = inner.docs.get(&id)?;
                Some(SearchResult {
                    uri: meta.uri.clone(),
                    text: meta.text.clone(),
                    chunk_index: meta.chunk_index,
                    score: score as f32,
                })
            })
            .collect();
        Ok(out)
    }

    fn delete_by_uri(&self, uri: &str) -> IndexResult<u64> {
        let mut inner = self.lock()?;
        let ids: Vec<u32> = inner
            .docs
            .iter()
            .filter(|(_, meta)| meta.uri == uri)
            .map(|(&id, _)| id)
            .collect();
        let removed = ids.len() as u64;
        for id in ids {
            inner.remove_doc(id);
        }
        Ok(removed)
    }

    fn count(&self) -> IndexResult<u64> {
        let inner = self.lock()?;
        Ok(u64::from(inner.doc_count))
    }

    fn flush(&self) -> IndexResult<()> {
        self.save()
    }
}

/// Lower-cased ASCII-folded tokens, split on non-alphanumeric, length ≥ 2.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 2)
        .map(|s| {
            s.chars()
                .map(|c| {
                    if c.is_ascii() {
                        c.to_ascii_lowercase()
                    } else {
                        c.to_lowercase().next().unwrap_or(c)
                    }
                })
                .collect()
        })
        .collect()
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

fn fielded_text(entry: &IndexEntry, weights: &[f32; 8]) -> String {
    fn repeat(out: &mut String, value: &str, weight: f32) {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return;
        }
        let n = (weight.max(0.0) * WEIGHT_SCALE).round() as u32;
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
    if let Some(entity_type) = entry.entity_type.as_deref() {
        repeat(&mut out, entity_type, w_etype);
    }
    if out.is_empty() {
        entry.text.clone()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(uri: &str, text: &str) -> IndexEntry {
        IndexEntry::new(uri, text)
    }

    #[test]
    fn tokenize_basics() {
        let t = tokenize("Hello, World! THIS is a 1 b 22");
        assert!(t.contains(&"hello".to_string()));
        assert!(t.contains(&"world".to_string()));
        assert!(t.contains(&"this".to_string()));
        assert!(t.contains(&"is".to_string()));
        assert!(t.contains(&"22".to_string()));
        assert!(!t.contains(&"a".to_string()));
        assert!(!t.contains(&"1".to_string()));
    }

    #[test]
    fn insert_and_search_returns_relevant() {
        let store = Bm25Store::new();
        store
            .insert(&[
                entry("u://payment", "credit card payment processing"),
                entry("u://report", "quarterly revenue projections"),
            ])
            .unwrap();
        let r = FullTextStore::search(&store, "credit card", 10).unwrap();
        assert!(!r.is_empty());
        assert_eq!(r[0].uri, "u://payment");
    }

    #[test]
    fn delete_by_uri_removes_all() {
        let store = Bm25Store::new();
        store
            .insert(&[
                entry("u://a", "hello world"),
                entry("u://b", "goodbye world"),
            ])
            .unwrap();
        let n = store.delete_by_uri("u://a").unwrap();
        assert_eq!(n, 1);
        assert!(FullTextStore::search(&store, "hello", 10)
            .unwrap()
            .is_empty());
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn empty_store_search_is_empty() {
        let store = Bm25Store::new();
        assert!(FullTextStore::search(&store, "anything", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn bm25_scores_decrease() {
        let store = Bm25Store::new();
        store
            .insert(&[
                entry("u://a", "rust rust rust rust rust"),
                entry("u://b", "rust"),
                entry("u://c", "python"),
            ])
            .unwrap();
        let r = FullTextStore::search(&store, "rust", 10).unwrap();
        assert_eq!(r.len(), 2);
        assert!(r[0].score >= r[1].score);
    }

    #[test]
    fn fielded_summary_is_searchable() {
        let store = Bm25Store::new();
        store
            .insert(&[IndexEntry::new("u://summary", "ordinary body")
                .with_summary(Some("rare summary token".into()))])
            .unwrap();
        let r = FullTextStore::search(&store, "rare", 10).unwrap();
        assert_eq!(r[0].uri, "u://summary");
    }

    #[test]
    fn persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bm25.json");
        {
            let store = Bm25Store::open(&path).unwrap();
            store
                .insert(&[entry("u://a", "persistent search data")])
                .unwrap();
            store.flush().unwrap();
        }
        let store = Bm25Store::open(&path).unwrap();
        let r = FullTextStore::search(&store, "persistent search", 10).unwrap();
        assert!(!r.is_empty());
        assert_eq!(r[0].uri, "u://a");
    }

    #[test]
    fn in_memory_flush_is_no_op() {
        let store = Bm25Store::new();
        store.insert(&[entry("u://a", "anything")]).unwrap();
        store.flush().unwrap();
    }

    #[test]
    fn delete_then_reinsert_recomputes_avg_dl() {
        let store = Bm25Store::new();
        store
            .insert(&[
                entry("u://a", "one two three four five"),
                entry("u://b", "alpha"),
            ])
            .unwrap();
        store.delete_by_uri("u://b").unwrap();
        store.insert(&[entry("u://c", "one")]).unwrap();
        // Should still be searchable
        let r = FullTextStore::search(&store, "one", 10).unwrap();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn corrupt_persistent_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"this is not valid json").unwrap();
        let err = Bm25Store::open(&path).unwrap_err();
        assert!(matches!(err, IndexError::Corrupt { .. }));
    }
}
