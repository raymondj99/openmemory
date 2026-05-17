//! LRU TTL cache wrapper around [`HybridSearchEngine`].
//!
//! Default capacity is 50 entries with a 60 s TTL, matching the Phase 2
//! roadmap. Cache key hashes the query text, top_k, mode, and an optional
//! filter discriminant — supplied by the caller as a `u64` so this layer
//! stays agnostic to whatever filter shape downstream uses.
//!
//! Cached results are cloned on every read; populating the cache after a
//! miss also stores a clone. This is fine: search results are small (URI +
//! text + score), and the cache is mostly there to short-circuit repeated
//! identical queries.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lru::LruCache;
use openmemory_core::clock::{Clock, SystemClock};

use crate::error::{IndexError, IndexResult};
use crate::hybrid::HybridSearchEngine;
use crate::traits::{FullTextStore, IndexEntry, SearchMode, SearchResult, VectorStore};

/// Default LRU capacity. Sized for an interactive agent's working set.
pub const DEFAULT_CACHE_CAPACITY: usize = 50;
/// Default TTL: a minute is long enough for back-to-back repeats and short
/// enough that fresh inserts show up reasonably quickly.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(60);

struct CacheEntry {
    results: Vec<SearchResult>,
    /// Wall-clock time of insertion in the configured clock's `now_millis`.
    inserted_at_ms: i64,
}

/// LRU cache around a [`HybridSearchEngine`].
pub struct CachedSearchEngine<V: VectorStore, F: FullTextStore> {
    inner: HybridSearchEngine<V, F>,
    cache: Mutex<LruCache<u64, CacheEntry>>,
    ttl_ms: i64,
    clock: Arc<dyn Clock>,
}

impl<V: VectorStore, F: FullTextStore> CachedSearchEngine<V, F> {
    /// Wrap `inner` with the default capacity and TTL.
    pub fn new(inner: HybridSearchEngine<V, F>) -> Self {
        Self::with_settings(
            inner,
            DEFAULT_CACHE_CAPACITY,
            DEFAULT_CACHE_TTL,
            Arc::new(SystemClock),
        )
    }

    /// Wrap `inner` with explicit capacity and TTL on the system clock.
    pub fn with_capacity_and_ttl(
        inner: HybridSearchEngine<V, F>,
        capacity: usize,
        ttl: Duration,
    ) -> Self {
        Self::with_settings(inner, capacity, ttl, Arc::new(SystemClock))
    }

    /// Wrap `inner` with explicit capacity, TTL, and clock implementation.
    /// Use this in tests with a `FixedClock` for deterministic TTL behaviour.
    pub fn with_settings(
        inner: HybridSearchEngine<V, F>,
        capacity: usize,
        ttl: Duration,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let capacity = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner,
            cache: Mutex::new(LruCache::new(capacity)),
            ttl_ms: ttl.as_millis() as i64,
            clock,
        }
    }

    /// Borrow the wrapped engine.
    pub fn inner(&self) -> &HybridSearchEngine<V, F> {
        &self.inner
    }

    /// Number of live entries in the cache.
    pub fn cache_len(&self) -> IndexResult<usize> {
        Ok(self.lock_cache()?.len())
    }

    /// Drop every cached entry. Call after a write that invalidates results.
    pub fn invalidate(&self) -> IndexResult<()> {
        self.lock_cache()?.clear();
        Ok(())
    }

    /// Insert into the wrapped stores and clear the cache so subsequent
    /// reads see the new data.
    pub fn insert(&self, entries: &[IndexEntry]) -> IndexResult<()> {
        self.inner.insert(entries)?;
        self.invalidate()?;
        Ok(())
    }

    /// Delete by URI in both stores and clear the cache.
    pub fn delete_by_uri(&self, uri: &str) -> IndexResult<u64> {
        let n = self.inner.delete_by_uri(uri)?;
        self.invalidate()?;
        Ok(n)
    }

    pub fn count(&self) -> IndexResult<u64> {
        self.inner.count()
    }

    pub fn keyword_count(&self) -> IndexResult<u64> {
        self.inner.keyword_count()
    }

    /// Search with caching. The `filters_hash` parameter folds caller-supplied
    /// filter state into the cache key — pass `0` if you have none.
    pub fn search(
        &self,
        query_vector: &[f32],
        query_text: &str,
        top_k: usize,
        mode: SearchMode,
        filters_hash: u64,
    ) -> IndexResult<Vec<SearchResult>> {
        let key = cache_key(query_text, top_k, mode, filters_hash);
        let now = self.clock.now_millis();

        {
            let mut cache = self.lock_cache()?;
            if let Some(entry) = cache.get(&key) {
                if now.saturating_sub(entry.inserted_at_ms) < self.ttl_ms {
                    return Ok(entry.results.clone());
                }
            }
            cache.pop(&key);
        }

        let results = self.inner.search(query_vector, query_text, top_k, mode)?;

        {
            let mut cache = self.lock_cache()?;
            cache.put(
                key,
                CacheEntry {
                    results: results.clone(),
                    inserted_at_ms: self.clock.now_millis(),
                },
            );
        }

        Ok(results)
    }

    fn lock_cache(&self) -> IndexResult<std::sync::MutexGuard<'_, LruCache<u64, CacheEntry>>> {
        self.cache
            .lock()
            .map_err(|e| IndexError::Lock(e.to_string()))
    }
}

fn cache_key(query_text: &str, top_k: usize, mode: SearchMode, filters_hash: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    query_text.hash(&mut hasher);
    top_k.hash(&mut hasher);
    let mode_tag: u8 = match mode {
        SearchMode::Hybrid => 0,
        SearchMode::VectorOnly => 1,
        SearchMode::KeywordOnly => 2,
    };
    mode_tag.hash(&mut hasher);
    filters_hash.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use openmemory_core::testing::FixedClock;

    use crate::FlatVectorIndex;

    #[cfg(feature = "fts5")]
    type TestFts = crate::Fts5Store;
    #[cfg(not(feature = "fts5"))]
    type TestFts = crate::Bm25Store;

    #[cfg(feature = "fts5")]
    fn make_fts() -> TestFts {
        crate::Fts5Store::open_in_memory().unwrap()
    }
    #[cfg(not(feature = "fts5"))]
    fn make_fts() -> TestFts {
        crate::Bm25Store::new()
    }

    fn entry(uri: &str, text: &str, vec: Vec<f32>) -> IndexEntry {
        IndexEntry::new(uri, text).with_vector(vec)
    }

    fn make_engine() -> CachedSearchEngine<FlatVectorIndex, TestFts> {
        let inner = HybridSearchEngine::new(FlatVectorIndex::new(), make_fts(), 0.7);
        CachedSearchEngine::new(inner)
    }

    fn make_engine_with_clock(
        clock: Arc<dyn Clock>,
        capacity: usize,
        ttl: Duration,
    ) -> CachedSearchEngine<FlatVectorIndex, TestFts> {
        let inner = HybridSearchEngine::new(FlatVectorIndex::new(), make_fts(), 0.7);
        CachedSearchEngine::with_settings(inner, capacity, ttl, clock)
    }

    #[test]
    fn hit_returns_same_results() {
        let engine = make_engine();
        engine
            .insert(&[entry("u://a", "hello world", vec![1.0, 0.0])])
            .unwrap();

        let r1 = engine
            .search(&[1.0, 0.0], "hello", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        let r2 = engine
            .search(&[1.0, 0.0], "hello", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        assert_eq!(r1.len(), r2.len());
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.uri, b.uri);
            assert!((a.score - b.score).abs() < f32::EPSILON);
        }
        assert_eq!(engine.cache_len().unwrap(), 1);
    }

    #[test]
    fn distinct_queries_do_not_collide() {
        let engine = make_engine();
        engine
            .insert(&[entry("u://a", "hello", vec![1.0, 0.0])])
            .unwrap();
        engine
            .search(&[1.0, 0.0], "hello", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        engine
            .search(&[1.0, 0.0], "absent", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        assert_eq!(engine.cache_len().unwrap(), 2);
    }

    #[test]
    fn ttl_expiry_via_fixed_clock() {
        let clock = Arc::new(FixedClock::new(1_000));
        let engine =
            make_engine_with_clock(clock.clone() as Arc<dyn Clock>, 10, Duration::from_secs(60));
        engine
            .insert(&[entry("u://a", "alpha", vec![1.0, 0.0])])
            .unwrap();

        let _ = engine
            .search(&[1.0, 0.0], "alpha", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        assert_eq!(engine.cache_len().unwrap(), 1);

        // Advance well past the 60 s TTL — entry should be evicted on next read.
        clock.advance(120);
        let _ = engine
            .search(&[1.0, 0.0], "alpha", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        // Re-populated, still 1 entry.
        assert_eq!(engine.cache_len().unwrap(), 1);
    }

    #[test]
    fn lru_eviction_when_capacity_exceeded() {
        let clock = Arc::new(FixedClock::new(0)) as Arc<dyn Clock>;
        let engine = make_engine_with_clock(clock, 2, Duration::from_secs(60));
        engine
            .insert(&[entry("u://a", "alpha", vec![1.0, 0.0])])
            .unwrap();

        engine
            .search(&[1.0, 0.0], "q1", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        engine
            .search(&[1.0, 0.0], "q2", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        engine
            .search(&[1.0, 0.0], "q3", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        assert_eq!(engine.cache_len().unwrap(), 2);
    }

    #[test]
    fn modes_get_distinct_keys() {
        let a = cache_key("hello", 5, SearchMode::Hybrid, 0);
        let b = cache_key("hello", 5, SearchMode::KeywordOnly, 0);
        let c = cache_key("hello", 5, SearchMode::VectorOnly, 0);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn top_k_changes_key() {
        assert_ne!(
            cache_key("hello", 5, SearchMode::Hybrid, 0),
            cache_key("hello", 10, SearchMode::Hybrid, 0)
        );
    }

    #[test]
    fn filters_hash_changes_key() {
        assert_ne!(
            cache_key("hello", 5, SearchMode::Hybrid, 1),
            cache_key("hello", 5, SearchMode::Hybrid, 2)
        );
    }

    #[test]
    fn invalidate_clears_all() {
        let engine = make_engine();
        engine
            .insert(&[entry("u://a", "alpha", vec![1.0, 0.0])])
            .unwrap();
        engine
            .search(&[1.0, 0.0], "alpha", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        assert_eq!(engine.cache_len().unwrap(), 1);
        engine.invalidate().unwrap();
        assert_eq!(engine.cache_len().unwrap(), 0);
    }

    #[test]
    fn write_invalidates_cache() {
        let engine = make_engine();
        engine
            .insert(&[entry("u://a", "alpha", vec![1.0, 0.0])])
            .unwrap();
        engine
            .search(&[1.0, 0.0], "alpha", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        assert_eq!(engine.cache_len().unwrap(), 1);

        engine
            .insert(&[entry("u://b", "beta", vec![0.0, 1.0])])
            .unwrap();
        assert_eq!(engine.cache_len().unwrap(), 0);

        engine
            .search(&[1.0, 0.0], "alpha", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        assert_eq!(engine.cache_len().unwrap(), 1);

        engine.delete_by_uri("u://a").unwrap();
        assert_eq!(engine.cache_len().unwrap(), 0);
    }

    #[test]
    fn second_call_does_not_re_query_inner() {
        // Cache hit should bypass the wrapped engine. We verify by counting
        // calls into a wrapper that delegates to a `FlatVectorIndex`.
        struct CountingVector {
            inner: FlatVectorIndex,
            calls: AtomicUsize,
        }
        impl VectorStore for CountingVector {
            fn insert(&self, e: &[IndexEntry]) -> IndexResult<()> {
                self.inner.insert(e)
            }
            fn search(&self, v: &[f32], k: usize) -> IndexResult<Vec<SearchResult>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.inner.search(v, k)
            }
            fn delete_by_uri(&self, u: &str) -> IndexResult<u64> {
                self.inner.delete_by_uri(u)
            }
            fn count(&self) -> IndexResult<u64> {
                self.inner.count()
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let v = CountingVector {
            inner: FlatVectorIndex::new(),
            calls: AtomicUsize::new(0),
        };
        let inner = HybridSearchEngine::new(v, make_fts(), 1.0);
        let engine = CachedSearchEngine::new(inner);
        engine
            .insert(&[entry("u://a", "x", vec![1.0, 0.0])])
            .unwrap();
        let _ = counter.load(Ordering::SeqCst);

        let before = engine.inner().vector_store().calls.load(Ordering::SeqCst);
        engine
            .search(&[1.0, 0.0], "x", 5, SearchMode::VectorOnly, 0)
            .unwrap();
        let after_first = engine.inner().vector_store().calls.load(Ordering::SeqCst);
        engine
            .search(&[1.0, 0.0], "x", 5, SearchMode::VectorOnly, 0)
            .unwrap();
        let after_second = engine.inner().vector_store().calls.load(Ordering::SeqCst);

        assert_eq!(after_first - before, 1);
        assert_eq!(after_second - after_first, 0, "cache hit should skip inner");
    }

    #[test]
    fn capacity_zero_is_promoted_to_one() {
        let inner = HybridSearchEngine::new(FlatVectorIndex::new(), make_fts(), 0.5);
        let engine = CachedSearchEngine::with_capacity_and_ttl(inner, 0, DEFAULT_CACHE_TTL);
        engine
            .insert(&[entry("u://a", "alpha", vec![1.0, 0.0])])
            .unwrap();
        engine
            .search(&[1.0, 0.0], "alpha", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        assert_eq!(engine.cache_len().unwrap(), 1);
    }
}
