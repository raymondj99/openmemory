//! Hybrid search engine: vector + keyword fused via Reciprocal Rank Fusion.
//!
//! Owns a [`VectorStore`] and a [`FullTextStore`] and exposes a single
//! [`HybridSearchEngine::search`] entry point. Three modes: vector-only,
//! keyword-only, and hybrid (the default). Hybrid fetches `top_k * 3`
//! candidates from each store, fuses with RRF, and returns the merged
//! top-k.

use std::collections::HashMap;

use crate::error::IndexResult;
use crate::traits::{FullTextStore, IndexEntry, SearchMode, SearchResult, VectorStore};

/// Default RRF constant. Trades off how aggressively rank position dominates.
pub const DEFAULT_RRF_K: f32 = 60.0;

/// Hybrid search engine combining vector similarity and keyword search via
/// Reciprocal Rank Fusion.
pub struct HybridSearchEngine<V: VectorStore, F: FullTextStore> {
    vector_store: V,
    fulltext_store: F,
    /// Mixing weight in `[0.0, 1.0]`. 0.0 = pure keyword, 1.0 = pure vector.
    /// Always read via [`alpha`](Self::alpha) — the field is clamped on
    /// construction and on every `set_alpha`.
    alpha: f32,
    rrf_k: f32,
}

impl<V: VectorStore, F: FullTextStore> HybridSearchEngine<V, F> {
    /// Construct an engine with `alpha` clamped into `[0.0, 1.0]` and the
    /// default RRF constant.
    pub fn new(vector_store: V, fulltext_store: F, alpha: f32) -> Self {
        Self::with_rrf_k(vector_store, fulltext_store, alpha, DEFAULT_RRF_K)
    }

    /// Construct with an explicit RRF constant.
    pub fn with_rrf_k(vector_store: V, fulltext_store: F, alpha: f32, rrf_k: f32) -> Self {
        Self {
            vector_store,
            fulltext_store,
            alpha: alpha.clamp(0.0, 1.0),
            rrf_k,
        }
    }

    pub fn alpha(&self) -> f32 {
        self.alpha
    }

    pub fn set_alpha(&mut self, alpha: f32) {
        self.alpha = alpha.clamp(0.0, 1.0);
    }

    pub fn rrf_k(&self) -> f32 {
        self.rrf_k
    }

    pub fn vector_store(&self) -> &V {
        &self.vector_store
    }

    pub fn fulltext_store(&self) -> &F {
        &self.fulltext_store
    }

    /// Insert an entry into both stores. Vector store gets the full record;
    /// the full-text store ignores the `vector` field.
    pub fn insert(&self, entries: &[IndexEntry]) -> IndexResult<()> {
        self.vector_store.insert(entries)?;
        self.fulltext_store.insert(entries)?;
        Ok(())
    }

    /// Run a search according to `mode`. Hybrid mode does RRF fusion.
    pub fn search(
        &self,
        query_vector: &[f32],
        query_text: &str,
        top_k: usize,
        mode: SearchMode,
    ) -> IndexResult<Vec<SearchResult>> {
        match mode {
            SearchMode::VectorOnly => self.vector_store.search(query_vector, top_k),
            SearchMode::KeywordOnly => self.fulltext_store.search(query_text, top_k),
            SearchMode::Hybrid => {
                let fetch = top_k.saturating_mul(3).max(top_k);
                let v = self.vector_store.search(query_vector, fetch)?;
                let k = self.fulltext_store.search(query_text, fetch)?;
                Ok(rrf_fuse(&v, &k, self.alpha, self.rrf_k, top_k))
            }
        }
    }

    /// Delete from both stores. Returns the count from the vector store
    /// (the keyword store may report a different number for the same URI
    /// because chunks are tracked once each).
    pub fn delete_by_uri(&self, uri: &str) -> IndexResult<u64> {
        let n = self.vector_store.delete_by_uri(uri)?;
        self.fulltext_store.delete_by_uri(uri)?;
        Ok(n)
    }

    /// Total entries in the vector store.
    pub fn count(&self) -> IndexResult<u64> {
        self.vector_store.count()
    }
}

/// Reciprocal Rank Fusion of two ranked lists.
///
/// Raw RRF score for a result at zero-based `rank` in list `L` is
/// `weight(L) / (k + rank + 1)`. With `k = 60`, the maximum raw score from
/// one list is `1/(k+1)`. We multiply by `(k+1)` so that a result ranked #1
/// in both lists scores 1.0 — a human-friendly normalisation.
pub(crate) fn rrf_fuse(
    vector: &[SearchResult],
    keyword: &[SearchResult],
    alpha: f32,
    rrf_k: f32,
    top_k: usize,
) -> Vec<SearchResult> {
    let capacity = vector.len() + keyword.len();
    let mut scores: HashMap<(&str, u32), (Source, f32)> = HashMap::with_capacity(capacity);

    for (rank, r) in vector.iter().enumerate() {
        let key = (r.uri.as_str(), r.chunk_index);
        let s = alpha / (rrf_k + rank as f32 + 1.0);
        scores
            .entry(key)
            .and_modify(|(_, score)| *score += s)
            .or_insert((Source::Vector(rank), s));
    }
    for (rank, r) in keyword.iter().enumerate() {
        let key = (r.uri.as_str(), r.chunk_index);
        let s = (1.0 - alpha) / (rrf_k + rank as f32 + 1.0);
        scores
            .entry(key)
            .and_modify(|(_, score)| *score += s)
            .or_insert((Source::Keyword(rank), s));
    }

    let norm = rrf_k + 1.0;
    let mut fused: Vec<(Source, f32)> = scores
        .into_values()
        .map(|(src, score)| (src, score * norm))
        .collect();

    if fused.len() > top_k && top_k > 0 {
        fused.select_nth_unstable_by(top_k - 1, |a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });
        fused.truncate(top_k);
    }
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    fused
        .into_iter()
        .map(|(src, score)| {
            let base = match src {
                Source::Vector(i) => &vector[i],
                Source::Keyword(i) => &keyword[i],
            };
            SearchResult {
                uri: base.uri.clone(),
                text: base.text.clone(),
                chunk_index: base.chunk_index,
                score,
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Source {
    Vector(usize),
    Keyword(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(uri: &str, score: f32) -> SearchResult {
        SearchResult {
            uri: uri.into(),
            text: "t".into(),
            chunk_index: 0,
            score,
        }
    }

    #[test]
    fn alpha_is_clamped_on_new() {
        let v = crate::FlatVectorIndex::new();
        let f = make_fts();
        let e = HybridSearchEngine::new(v, f, -0.5);
        assert!((e.alpha() - 0.0).abs() < f32::EPSILON);

        let v2 = crate::FlatVectorIndex::new();
        let f2 = make_fts();
        let e2 = HybridSearchEngine::new(v2, f2, 1.5);
        assert!((e2.alpha() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn alpha_is_clamped_on_set() {
        let v = crate::FlatVectorIndex::new();
        let f = make_fts();
        let mut e = HybridSearchEngine::new(v, f, 0.5);
        e.set_alpha(2.0);
        assert!((e.alpha() - 1.0).abs() < f32::EPSILON);
        e.set_alpha(-2.0);
        assert!((e.alpha() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn rrf_fuse_gives_b_top_when_in_both() {
        let v = vec![r("a", 0.9), r("b", 0.8)];
        let k = vec![r("b", 5.0), r("c", 4.0)];
        let f = rrf_fuse(&v, &k, 0.5, 60.0, 10);
        assert_eq!(f[0].uri, "b");
    }

    #[test]
    fn rrf_fuse_alpha_zero_is_keyword_only() {
        let v = vec![r("vonly", 0.9)];
        let k = vec![r("kfirst", 5.0), r("ksecond", 1.0)];
        let f = rrf_fuse(&v, &k, 0.0, 60.0, 10);
        // First should be the top keyword result.
        assert_eq!(f[0].uri, "kfirst");
        // Vector-only entries get score 0 under alpha=0.
        let vonly = f.iter().find(|x| x.uri == "vonly").unwrap();
        assert!(vonly.score.abs() < 1e-6);
    }

    #[test]
    fn rrf_fuse_alpha_one_is_vector_only() {
        let v = vec![r("vfirst", 0.9), r("vsecond", 0.8)];
        let k = vec![r("konly", 5.0)];
        let f = rrf_fuse(&v, &k, 1.0, 60.0, 10);
        assert_eq!(f[0].uri, "vfirst");
        let konly = f.iter().find(|x| x.uri == "konly").unwrap();
        assert!(konly.score.abs() < 1e-6);
    }

    #[test]
    fn rrf_fuse_truncates_to_top_k() {
        let v: Vec<_> = (0..20).map(|i| r(&format!("v{i}"), 1.0)).collect();
        let k: Vec<_> = (0..20).map(|i| r(&format!("k{i}"), 1.0)).collect();
        let f = rrf_fuse(&v, &k, 0.5, 60.0, 5);
        assert_eq!(f.len(), 5);
    }

    #[test]
    fn rrf_fuse_score_at_rank_one_in_both_is_one() {
        let v = vec![r("x", 1.0)];
        let k = vec![r("x", 1.0)];
        let f = rrf_fuse(&v, &k, 0.5, 60.0, 1);
        assert_eq!(f.len(), 1);
        // rank=0 in both; score = (0.5 / 61 + 0.5 / 61) * 61 = 1.0
        assert!((f[0].score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn rrf_fuse_known_values() {
        // Hand-computed: alpha=0.5, k=60.
        // Doc "x" at rank 0 in vector list, rank 0 in keyword list:
        //   raw  = 0.5/(60+1) + 0.5/(60+1) = 1/61
        //   norm = (1/61) * 61 = 1.0
        // Doc "y" at rank 1 in vector, missing from keyword:
        //   raw  = 0.5/(60+2) + 0 = 0.5/62
        //   norm = (0.5/62) * 61 = 0.491935...
        let v = vec![r("x", 1.0), r("y", 0.5)];
        let k = vec![r("x", 1.0)];
        let fused = rrf_fuse(&v, &k, 0.5, 60.0, 10);
        let by_uri: HashMap<&str, f32> = fused
            .iter()
            .map(|r| (r.uri.as_str(), r.score))
            .collect();
        assert!((by_uri["x"] - 1.0).abs() < 1e-5);
        let expected_y = (0.5 / 62.0) * 61.0;
        assert!((by_uri["y"] - expected_y).abs() < 1e-5);
    }

    #[test]
    fn rrf_fuse_results_descend() {
        let v: Vec<_> = (0..5)
            .map(|i| r(&format!("v{i}"), 0.9 - 0.1 * i as f32))
            .collect();
        let k: Vec<_> = (0..5)
            .map(|i| r(&format!("k{i}"), 5.0 - i as f32))
            .collect();
        let f = rrf_fuse(&v, &k, 0.5, 60.0, 10);
        for w in f.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
    }

    #[test]
    fn rrf_fuse_handles_empty_inputs() {
        let f = rrf_fuse(&[], &[], 0.5, 60.0, 10);
        assert!(f.is_empty());
    }

    #[test]
    fn rrf_fuse_distinguishes_chunk_index() {
        // Same URI, different chunk indices = different result identities.
        let mut a = r("uri", 0.9);
        a.chunk_index = 0;
        let mut b = r("uri", 0.9);
        b.chunk_index = 1;
        let f = rrf_fuse(&[a, b], &[], 1.0, 60.0, 10);
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn engine_hybrid_search_finds_inserted() {
        let v = crate::FlatVectorIndex::new();
        let f = make_fts();
        let engine = HybridSearchEngine::new(v, f, 0.7);
        engine
            .insert(&[IndexEntry {
                uri: "u://a".into(),
                text: "hello world from rust".into(),
                chunk_index: 0,
                vector: vec![1.0, 0.0, 0.0],
            }])
            .unwrap();

        let r = engine
            .search(&[1.0, 0.0, 0.0], "hello", 10, SearchMode::Hybrid)
            .unwrap();
        assert_eq!(r[0].uri, "u://a");
    }

    #[test]
    fn engine_vector_only_skips_keyword() {
        let v = crate::FlatVectorIndex::new();
        let f = make_fts();
        let engine = HybridSearchEngine::new(v, f, 0.5);
        engine
            .insert(&[IndexEntry {
                uri: "u://a".into(),
                text: "irrelevant text".into(),
                chunk_index: 0,
                vector: vec![1.0, 0.0],
            }])
            .unwrap();
        let r = engine
            .search(&[1.0, 0.0], "totallyabsentword", 10, SearchMode::VectorOnly)
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].uri, "u://a");
    }

    #[test]
    fn engine_keyword_only_skips_vector() {
        let v = crate::FlatVectorIndex::new();
        let f = make_fts();
        let engine = HybridSearchEngine::new(v, f, 0.5);
        engine
            .insert(&[IndexEntry {
                uri: "u://a".into(),
                text: "needle haystack".into(),
                chunk_index: 0,
                vector: vec![0.0, 0.0],
            }])
            .unwrap();
        let r = engine
            .search(&[1.0, 0.0], "needle", 10, SearchMode::KeywordOnly)
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].uri, "u://a");
    }

    #[test]
    fn engine_delete_removes_from_both() {
        let v = crate::FlatVectorIndex::new();
        let f = make_fts();
        let engine = HybridSearchEngine::new(v, f, 0.5);
        engine
            .insert(&[
                IndexEntry {
                    uri: "u://a".into(),
                    text: "hello".into(),
                    chunk_index: 0,
                    vector: vec![1.0, 0.0],
                },
                IndexEntry {
                    uri: "u://b".into(),
                    text: "world".into(),
                    chunk_index: 0,
                    vector: vec![0.0, 1.0],
                },
            ])
            .unwrap();
        let removed = engine.delete_by_uri("u://a").unwrap();
        assert_eq!(removed, 1);
        assert_eq!(engine.count().unwrap(), 1);
        let r = engine
            .search(&[1.0, 0.0], "hello", 10, SearchMode::KeywordOnly)
            .unwrap();
        assert!(r.is_empty());
    }

    #[cfg(feature = "fts5")]
    fn make_fts() -> crate::Fts5Store {
        crate::Fts5Store::open_in_memory().unwrap()
    }

    #[cfg(not(feature = "fts5"))]
    fn make_fts() -> crate::Bm25Store {
        crate::Bm25Store::new()
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn list_strategy(prefix: &'static str, n: usize) -> Vec<SearchResult> {
            (0..n)
                .map(|i| SearchResult {
                    uri: format!("{prefix}{i}"),
                    text: "t".into(),
                    chunk_index: 0,
                    score: 1.0 - 0.01 * i as f32,
                })
                .collect()
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn fused_len_bounded(
                nv in 0..50usize,
                nk in 0..50usize,
                alpha in 0.0f32..=1.0,
                top_k in 1..30usize,
            ) {
                let v = list_strategy("v", nv);
                let k = list_strategy("k", nk);
                let f = rrf_fuse(&v, &k, alpha, 60.0, top_k);
                prop_assert!(f.len() <= top_k);
                prop_assert!(f.len() <= nv + nk);
            }

            #[test]
            fn scores_non_negative_and_bounded(
                nv in 0..30usize,
                nk in 0..30usize,
                alpha in 0.0f32..=1.0,
                top_k in 1..20usize,
            ) {
                let v = list_strategy("v", nv);
                let k = list_strategy("k", nk);
                let f = rrf_fuse(&v, &k, alpha, 60.0, top_k);
                for r in &f {
                    prop_assert!(r.score >= 0.0);
                    prop_assert!(r.score <= 1.0 + 1e-5);
                }
            }

            #[test]
            fn descending_scores(
                nv in 1..30usize,
                nk in 1..30usize,
                alpha in 0.0f32..=1.0,
                top_k in 1..20usize,
            ) {
                let v = list_strategy("v", nv);
                let k = list_strategy("k", nk);
                let f = rrf_fuse(&v, &k, alpha, 60.0, top_k);
                for w in f.windows(2) {
                    prop_assert!(w[0].score >= w[1].score - 1e-6);
                }
            }
        }
    }
}
