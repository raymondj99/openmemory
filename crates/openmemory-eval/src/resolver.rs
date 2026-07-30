//! Mapping recall results back to judgment keys.
//!
//! Judgments are written against stable document keys so they survive a
//! re-ingest; store rows are identified by UUIDv7 observation ids that
//! change on every ingest. Something has to bridge the two, and which
//! bridge is correct depends on how the corpus was built. That choice is
//! a [`KeyResolver`].
//!
//! Two implementations cover the cases that exist today:
//!
//! - [`ObservationIdResolver`] for query sets written directly against
//!   observation ids (fine for a store that is never rebuilt);
//! - [`MapResolver`] for corpora that emit an id → key manifest.
//!
//! A resolver returning `None` means "this result is not a judgeable
//! document". Unjudged results are neither credited nor penalised
//! beyond occupying a rank position, which is the standard treatment
//! for unjudged documents in a pooled evaluation.
//!
//! ## Coarser identities
//!
//! One store row can satisfy an information need stated at more than one
//! granularity. A chunk of `src/recall.rs` is both "chunk 7 of that file"
//! and "that file"; a question asking *which files a change touched* is
//! answered by any chunk of a touched file. Judging such a question
//! against a list of chunk keys is wrong twice over: retrieving a chunk
//! the judgment list happens not to name scores zero although it is a
//! correct answer, and the judgment set grows past K so raw `R@K` cannot
//! reach 1.0.
//!
//! [`KeyResolver::alias_keys`] therefore lets a resolver declare the
//! coarser keys a result also satisfies. A query is scored against
//! whichever granularity it actually judged: an alias is used only when
//! the query lists it, so chunk-level query sets are unaffected.

use std::collections::HashMap;

use openmemory_graph::recall::RecallResult;

/// Maps a recall result to the stable key judgments are written against.
pub trait KeyResolver {
    /// Key for this result, or `None` when it has no judgeable identity.
    fn key_for(&self, result: &RecallResult) -> Option<String>;

    /// Coarser keys this result also satisfies, most specific first.
    ///
    /// Empty by default: a resolver only declares an alias when the
    /// corpus genuinely has a coarser unit of retrieval (a file behind
    /// its chunks, say). Callers must prefer [`Self::key_for`] and fall
    /// back to an alias only when the query judges that alias, so
    /// declaring one can never change the score of a query set written
    /// at the finer granularity.
    fn alias_keys(&self, _result: &RecallResult) -> Vec<String> {
        Vec::new()
    }
}

/// Identity resolver: the judgment key *is* the observation id.
#[derive(Debug, Clone, Copy, Default)]
pub struct ObservationIdResolver;

impl KeyResolver for ObservationIdResolver {
    fn key_for(&self, result: &RecallResult) -> Option<String> {
        Some(result.observation.id.clone())
    }
}

/// Resolver backed by an explicit observation-id → document-key map,
/// typically loaded from a corpus manifest.
#[derive(Debug, Clone, Default)]
pub struct MapResolver {
    by_observation: HashMap<String, String>,
    /// Coarser key per observation, when the corpus has one.
    alias_by_observation: HashMap<String, String>,
}

impl MapResolver {
    /// Build from `(observation_id, doc_key)` pairs.
    #[must_use]
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            by_observation: pairs.into_iter().collect(),
            alias_by_observation: HashMap::new(),
        }
    }

    /// Attach `(observation_id, coarse_key)` pairs.
    ///
    /// The corpus decides what "coarser" means — this crate only needs
    /// to know that the same row answers to a second name. Observations
    /// with no entry keep exactly the behaviour they had.
    #[must_use]
    pub fn with_aliases(mut self, pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        self.alias_by_observation.extend(pairs);
        self
    }

    /// Number of mapped observations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_observation.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_observation.is_empty()
    }

    /// Every document key known to this resolver, aliases included.
    ///
    /// Used to verify that a query set's judgments actually exist in the
    /// corpus before a run, so a typo shows up as a load error rather
    /// than as a zero score. Aliases belong in this set for the same
    /// reason the canonical keys do: a query set judged at the coarser
    /// granularity is judged against keys this corpus can produce.
    #[must_use]
    pub fn known_keys(&self) -> std::collections::BTreeSet<&str> {
        self.by_observation
            .values()
            .chain(self.alias_by_observation.values())
            .map(String::as_str)
            .collect()
    }
}

impl KeyResolver for MapResolver {
    fn key_for(&self, result: &RecallResult) -> Option<String> {
        self.by_observation.get(&result.observation.id).cloned()
    }

    fn alias_keys(&self, result: &RecallResult) -> Vec<String> {
        self.alias_by_observation
            .get(&result.observation.id)
            .cloned()
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_graph::types::{EntityType, Observation};

    fn result_with_id(id: &str) -> RecallResult {
        let mut observation = Observation::new("entity-1", "content", 0);
        observation.id = id.to_string();
        RecallResult {
            observation,
            entity_name: "e".into(),
            entity_type: EntityType::Concept,
            raw_score: 1.0,
            score: 1.0,
        }
    }

    #[test]
    fn observation_id_resolver_is_identity() {
        let r = ObservationIdResolver;
        assert_eq!(
            r.key_for(&result_with_id("obs-7")).as_deref(),
            Some("obs-7")
        );
    }

    #[test]
    fn map_resolver_translates_known_ids() {
        let r = MapResolver::new([("obs-1".to_string(), "omem://a/code/x.rs#0".to_string())]);
        assert_eq!(
            r.key_for(&result_with_id("obs-1")).as_deref(),
            Some("omem://a/code/x.rs#0")
        );
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
    }

    #[test]
    fn map_resolver_returns_none_for_unknown_ids() {
        let r = MapResolver::new([("obs-1".to_string(), "k".to_string())]);
        assert!(r.key_for(&result_with_id("obs-missing")).is_none());
    }

    #[test]
    fn aliases_are_reported_alongside_the_canonical_key() {
        let r = MapResolver::new([("obs-1".to_string(), "omem://a/code/x.rs#3".to_string())])
            .with_aliases([("obs-1".to_string(), "omem://a/code/x.rs".to_string())]);
        let hit = result_with_id("obs-1");
        assert_eq!(r.key_for(&hit).as_deref(), Some("omem://a/code/x.rs#3"));
        assert_eq!(r.alias_keys(&hit), vec!["omem://a/code/x.rs".to_string()]);
        // The alias is part of the corpus vocabulary, so a query set
        // judged at file granularity does not read as drifted.
        assert!(r.known_keys().contains("omem://a/code/x.rs"));
    }

    #[test]
    fn observations_without_an_alias_are_unchanged() {
        let r = MapResolver::new([
            ("obs-1".to_string(), "k1".to_string()),
            ("obs-2".to_string(), "k2".to_string()),
        ])
        .with_aliases([("obs-1".to_string(), "coarse".to_string())]);
        assert!(r.alias_keys(&result_with_id("obs-2")).is_empty());
        assert!(r.alias_keys(&result_with_id("obs-missing")).is_empty());
    }

    #[test]
    fn known_keys_exposes_the_corpus_vocabulary() {
        let r = MapResolver::new([
            ("o1".to_string(), "k1".to_string()),
            ("o2".to_string(), "k2".to_string()),
        ]);
        let keys = r.known_keys();
        assert!(keys.contains("k1") && keys.contains("k2"));
        assert_eq!(keys.len(), 2);
    }
}
