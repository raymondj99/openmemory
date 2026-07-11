//! Model-aware two-level embedding cache.
//!
//! A small in-memory LRU removes SQLite traffic from repeated interactive
//! queries; the persistent cache survives process restarts. Keys include the
//! model configuration and task purpose so query and document prefixes never
//! alias.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;

use crate::{Embedder, EmbeddingCache, OnnxEmbedder};

const MEMORY_CACHE_CAPACITY: usize = 2_048;

#[derive(Clone, Copy)]
enum Purpose {
    Raw,
    Query,
    Document,
}

impl Purpose {
    const fn label(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Query => "query",
            Self::Document => "document",
        }
    }
}

/// Cached production embedder returned by [`crate::load_embedder`].
pub struct CachedEmbedder {
    inner: OnnxEmbedder,
    persistent: EmbeddingCache,
    memory: Mutex<LruCache<String, Vec<f32>>>,
    fingerprint: String,
}

impl CachedEmbedder {
    pub fn new(inner: OnnxEmbedder, persistent: EmbeddingCache) -> Self {
        let fingerprint = inner.cache_fingerprint();
        Self {
            inner,
            persistent,
            memory: Mutex::new(LruCache::new(
                NonZeroUsize::new(MEMORY_CACHE_CAPACITY).expect("non-zero cache capacity"),
            )),
            fingerprint,
        }
    }

    pub fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    pub fn cache_stats(&self) -> (u64, u64) {
        self.persistent.stats()
    }

    fn key(&self, purpose: Purpose, text: &str) -> String {
        format!(
            "om-embed-v1\0{}\0{}\0{text}",
            self.fingerprint,
            purpose.label()
        )
    }

    fn embed_cached(
        &self,
        texts: &[&str],
        purpose: Purpose,
        compute: impl FnOnce(&OnnxEmbedder, &[&str]) -> Vec<Vec<f32>>,
    ) -> Vec<Vec<f32>> {
        if texts.is_empty() {
            return Vec::new();
        }

        let keys: Vec<String> = texts.iter().map(|text| self.key(purpose, text)).collect();
        let mut results: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        {
            let mut memory = self
                .memory
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for (index, key) in keys.iter().enumerate() {
                if let Some(vector) = memory.get(key) {
                    results[index] = Some(vector.clone());
                }
            }
        }

        let mut uncached: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, key) in keys.iter().enumerate() {
            if results[index].is_some() {
                continue;
            }
            uncached.entry(key.as_str()).or_default().push(index);
        }

        let unique: Vec<&str> = uncached.keys().copied().collect();
        let persistent = self.persistent.get_batch(&unique);
        let mut missing: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut memory = self
            .memory
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (key, vector) in unique.into_iter().zip(persistent) {
            let positions = uncached
                .remove(key)
                .expect("unique key originated from uncached map");
            if let Some(vector) = vector {
                for &position in &positions {
                    results[position] = Some(vector.clone());
                }
                memory.put(key.to_string(), vector);
            } else {
                missing.insert(key, positions);
            }
        }
        drop(memory);

        if !missing.is_empty() {
            let missing_items: Vec<(&str, &str)> = missing
                .iter()
                .map(|(key, positions)| (*key, texts[positions[0]]))
                .collect();
            let missing_texts: Vec<&str> = missing_items.iter().map(|(_, text)| *text).collect();
            let vectors = compute(&self.inner, &missing_texts);
            if vectors.len() == missing_items.len() {
                let cache_entries: Vec<(&str, &[f32])> = missing_items
                    .iter()
                    .zip(vectors.iter())
                    .map(|((key, _), vector)| (*key, vector.as_slice()))
                    .collect();
                self.persistent.put_batch(&cache_entries);
                let mut memory = self
                    .memory
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                for ((key, _), vector) in missing_items.iter().zip(vectors) {
                    if let Some(positions) = missing.get(key) {
                        for &position in positions {
                            results[position] = Some(vector.clone());
                        }
                    }
                    memory.put((*key).to_string(), vector);
                }
            }
        }

        results.into_iter().map(Option::unwrap_or_default).collect()
    }
}

impl Embedder for CachedEmbedder {
    fn embed(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        self.embed_cached(texts, Purpose::Raw, Embedder::embed)
    }

    fn embed_query(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        self.embed_cached(texts, Purpose::Query, Embedder::embed_query)
    }

    fn embed_documents(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        self.embed_cached(texts, Purpose::Document, Embedder::embed_documents)
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
}
