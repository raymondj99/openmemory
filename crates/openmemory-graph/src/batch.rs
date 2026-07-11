//! `MemoryStore::remember_batch` — many entity groups, one transaction.
//!
//! `remember` pays a full SQLite transaction (plus a search-engine flush)
//! per call. That is the dominant cost under concurrent ingestion: a
//! write-behind layer that drains a queue of pending writes wants to pay
//! it once per drain, not once per entity. `remember_batch` takes a slice
//! of [`RememberRequest`]s and commits all of them inside a single
//! transaction with a single search-index sync afterwards.
//!
//! Two batch-only options exist for ingestion layers:
//!
//! - `normalize: false` skips the fuzzy entity-name scan per group.
//!   Bulk sources that pre-resolve entity names (or rely on offline
//!   consolidation for dedup) save the candidate query + strsim pass.
//! - `checkpoint: Some((key, value))` upserts a row into `memory_meta`
//!   inside the same transaction. openmemory-engine stores its per-shard
//!   journal watermark this way, which is what makes journal replay
//!   exactly-once: the batch and the "this batch was applied" marker
//!   commit atomically.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::MemoryResult;
use crate::remember::{
    validate_group, write_group, ObservationInput, PersistIndex, RelationInput, RememberOutcome,
};
use crate::store::MemoryStore;
use crate::types::EntityType;

/// One entity group in a [`MemoryStore::remember_batch`] call. The owned
/// twin of `remember`'s borrowed parameter list; serde derives let
/// write-behind layers journal pending requests to disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RememberRequest {
    pub name: String,
    pub entity_type: EntityType,
    pub observations: Vec<ObservationInput>,
    pub relations: Vec<RelationInput>,
    pub source: String,
}

impl RememberRequest {
    /// Validate this request without opening a transaction. Ingestion
    /// front-ends should call this before acknowledging queue admission so
    /// malformed writes fail synchronously instead of poisoning an epoch.
    pub fn validate(&self) -> MemoryResult<()> {
        validate_group(&self.name, &self.observations, &self.relations)
    }

    #[must_use]
    pub fn new(name: impl Into<String>, entity_type: EntityType) -> Self {
        Self {
            name: name.into(),
            entity_type,
            observations: Vec::new(),
            relations: Vec::new(),
            source: String::new(),
        }
    }

    #[must_use]
    pub fn with_observations(mut self, observations: Vec<ObservationInput>) -> Self {
        self.observations = observations;
        self
    }

    #[must_use]
    pub fn with_relations(mut self, relations: Vec<RelationInput>) -> Self {
        self.relations = relations;
        self
    }

    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }
}

/// Options for [`MemoryStore::remember_batch`].
#[derive(Debug, Clone)]
pub struct BatchOptions {
    /// Run the fuzzy entity-name normalization scan per group (subject to
    /// the store-level `normalization.enabled` config). Default `true`,
    /// matching `remember`.
    pub normalize: bool,
    /// Key/value upserted into `memory_meta` inside the batch
    /// transaction. Used by write-behind layers as an applied-watermark
    /// for exactly-once journal replay. Keys should be namespaced (e.g.
    /// `engine:journal:<shard>`); the bare key `schema_version` is
    /// reserved by the migrator.
    pub checkpoint: Option<(String, u64)>,
}

impl Default for BatchOptions {
    fn default() -> Self {
        Self {
            normalize: true,
            checkpoint: None,
        }
    }
}

impl MemoryStore {
    /// Atomic multi-group write: every request's entity upsert,
    /// observations, and relations commit in one SQLite transaction; the
    /// search index syncs once afterwards. If any group fails, the whole
    /// batch rolls back (including the checkpoint).
    ///
    /// Outcomes are returned in request order. Validation errors surface
    /// before any transaction is opened.
    ///
    /// Embeddings for every observation are computed in one batched
    /// embedder call BEFORE the rebuild write lock is taken, so the
    /// lock-hold time covers only SQLite and index-insert work.
    ///
    /// Durability note: the vector index is updated in memory but NOT
    /// saved to disk here — the save is O(corpus), so batch callers
    /// persist on their own cadence via
    /// [`MemoryStore::persist_search_index`] (the context engine's
    /// maintenance tick does this), with the store's `Drop` as a
    /// clean-exit backstop. SQLite remains authoritative throughout.
    pub fn remember_batch(
        &self,
        requests: &[RememberRequest],
        opts: &BatchOptions,
    ) -> MemoryResult<Vec<RememberOutcome>> {
        for req in requests {
            validate_group(&req.name, &req.observations, &req.relations)?;
        }
        if requests.is_empty() && opts.checkpoint.is_none() {
            return Ok(Vec::new());
        }

        // One batched embedder call for the whole drain, off the
        // write-critical path. Order matches the flattened
        // (request, observation) payload order built under the lock.
        let texts: Vec<String> = requests
            .iter()
            .flat_map(|req| {
                req.observations
                    .iter()
                    .map(|o| MemoryStore::search_body_text(&req.name, &o.content))
            })
            .collect();
        let vectors = self.embed_documents_batch(&texts);

        let now = self.clock().now_secs();
        let norm = self.normalization_params(opts.normalize);

        // Same locking discipline as `remember`: hold the rebuild write
        // lock across the SQLite write + search sync so concurrent recall
        // never observes the half-applied state.
        let _guard = self.write_rebuild();

        let mut conn = self.lock_db();
        let tx = conn.transaction()?;

        let mut outcomes = Vec::with_capacity(requests.len());
        let mut sync_groups = Vec::with_capacity(requests.len());
        for req in requests {
            let group = write_group(
                &tx,
                &req.name,
                req.entity_type,
                &req.observations,
                &req.relations,
                &req.source,
                now,
                norm,
            )?;
            if !group.payload.is_empty() {
                sync_groups.push((req.name.clone(), group.payload));
            }
            outcomes.push(group.outcome);
        }

        if let Some((key, value)) = &opts.checkpoint {
            // Monotonic upsert: a checkpoint never moves backwards, so a
            // late-committing batch with a lower watermark cannot make
            // already-applied journal entries look unapplied.
            tx.execute(
                "INSERT INTO memory_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value
                 WHERE CAST(excluded.value AS INTEGER) > CAST(memory_meta.value AS INTEGER)",
                params![key, value.to_string()],
            )?;
        }

        tx.commit()?;
        drop(conn);

        self.sync_search_groups(&sync_groups, vectors, PersistIndex::Defer)?;

        Ok(outcomes)
    }

    /// Read a `memory_meta` value previously written via
    /// [`BatchOptions::checkpoint`]. Returns `None` when the key is
    /// missing or does not parse as `u64`.
    pub fn get_checkpoint(&self, key: &str) -> MemoryResult<Option<u64>> {
        self.with_reader(|conn| {
            let value: Option<String> = conn
                .query_row(
                    "SELECT value FROM memory_meta WHERE key = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(value.and_then(|v| v.parse().ok()))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::error::MemoryError;
    use openmemory_core::clock::{Clock, FixedClock};
    use openmemory_core::config::Config;

    fn open() -> MemoryStore {
        let clock = Arc::new(FixedClock::new(1_000));
        MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_clock(clock as Arc<dyn Clock>)
    }

    fn req(name: &str, contents: &[&str]) -> RememberRequest {
        RememberRequest::new(name, EntityType::Fact)
            .with_observations(contents.iter().map(|c| ObservationInput::new(*c)).collect())
            .with_source("batch-test")
    }

    #[test]
    fn batch_commits_many_groups_in_request_order() {
        let store = open();
        let outcomes = store
            .remember_batch(
                &[
                    req("alpha", &["fact one", "fact two"]),
                    req("bravo", &["fact three"]),
                    req("charlie", &["fact four"]),
                ],
                &BatchOptions::default(),
            )
            .unwrap();

        assert_eq!(outcomes.len(), 3);
        assert_eq!(outcomes[0].observation_ids.len(), 2);
        assert_eq!(outcomes[1].observation_ids.len(), 1);

        let s = store.status().unwrap();
        assert_eq!(s.total_entities, 3);
        assert_eq!(s.total_observations, 4);

        // Order: outcome[i] matches request[i].
        let alpha = store.get_entity("alpha").unwrap().unwrap();
        assert_eq!(outcomes[0].entity_id, alpha.id);
    }

    #[test]
    fn batch_matches_remember_semantics_for_existing_entities() {
        let store = open();
        let first = store
            .remember(
                "alpha",
                EntityType::Fact,
                &[ObservationInput::new("seed")],
                &[],
                "direct",
            )
            .unwrap();

        let outcomes = store
            .remember_batch(&[req("alpha", &["appended"])], &BatchOptions::default())
            .unwrap();
        assert_eq!(outcomes[0].entity_id, first.entity_id);
        assert!(outcomes[0].entity_existed);
    }

    #[test]
    fn batch_handles_relations_across_groups() {
        let store = open();
        let outcomes = store
            .remember_batch(
                &[
                    RememberRequest::new("Raymond", EntityType::Person)
                        .with_observations(vec![ObservationInput::new("builds things")])
                        .with_relations(vec![RelationInput::new(
                            "maintains",
                            "openmemory",
                            EntityType::Project,
                        )])
                        .with_source("t"),
                    RememberRequest::new("openmemory", EntityType::Project)
                        .with_observations(vec![ObservationInput::new("a memory engine")])
                        .with_source("t"),
                ],
                &BatchOptions::default(),
            )
            .unwrap();

        // The relation target created in group 1 must be the same entity
        // group 2 appends to (no duplicate "openmemory").
        assert_eq!(store.status().unwrap().total_entities, 2);
        let project = store.get_entity("openmemory").unwrap().unwrap();
        assert_eq!(outcomes[1].entity_id, project.id);
        assert_eq!(outcomes[0].relation_ids.len(), 1);
    }

    #[test]
    fn batch_rolls_back_whole_batch_on_validation_error() {
        let store = open();
        let err = store
            .remember_batch(
                &[req("alpha", &["good"]), req("bravo", &[""])],
                &BatchOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));
        assert_eq!(store.status().unwrap().total_entities, 0, "nothing lands");
    }

    #[test]
    fn batch_normalize_false_skips_fuzzy_merge() {
        let store = open();
        store
            .remember_batch(&[req("Project Alpha", &["one"])], &BatchOptions::default())
            .unwrap();
        // With normalize=false a near-duplicate name creates a second
        // entity instead of auto-merging.
        store
            .remember_batch(
                &[req("project alpha", &["two"])],
                &BatchOptions {
                    normalize: false,
                    ..BatchOptions::default()
                },
            )
            .unwrap();
        assert_eq!(store.status().unwrap().total_entities, 2);
    }

    #[test]
    fn batch_normalize_true_auto_merges_like_remember() {
        let store = open();
        store
            .remember_batch(&[req("Project Alpha", &["one"])], &BatchOptions::default())
            .unwrap();
        let outcomes = store
            .remember_batch(&[req("project alpha", &["two"])], &BatchOptions::default())
            .unwrap();
        assert!(outcomes[0].entity_existed);
        assert_eq!(store.status().unwrap().total_entities, 1);
    }

    #[test]
    fn batch_checkpoint_commits_atomically_with_writes() {
        let store = open();
        store
            .remember_batch(
                &[req("alpha", &["fact"])],
                &BatchOptions {
                    checkpoint: Some(("engine:journal:0".into(), 41)),
                    ..BatchOptions::default()
                },
            )
            .unwrap();
        assert_eq!(store.get_checkpoint("engine:journal:0").unwrap(), Some(41));

        // Upsert: a later batch advances the same key.
        store
            .remember_batch(
                &[req("bravo", &["fact"])],
                &BatchOptions {
                    checkpoint: Some(("engine:journal:0".into(), 42)),
                    ..BatchOptions::default()
                },
            )
            .unwrap();
        assert_eq!(store.get_checkpoint("engine:journal:0").unwrap(), Some(42));
    }

    #[test]
    fn batch_checkpoint_never_regresses() {
        let store = open();
        store
            .remember_batch(
                &[],
                &BatchOptions {
                    checkpoint: Some(("engine:journal:3".into(), 100)),
                    ..BatchOptions::default()
                },
            )
            .unwrap();
        // A late commit with a lower watermark must not move it backwards.
        store
            .remember_batch(
                &[],
                &BatchOptions {
                    checkpoint: Some(("engine:journal:3".into(), 90)),
                    ..BatchOptions::default()
                },
            )
            .unwrap();
        assert_eq!(store.get_checkpoint("engine:journal:3").unwrap(), Some(100));
    }

    #[test]
    fn batch_checkpoint_only_commits_without_groups() {
        let store = open();
        let outcomes = store
            .remember_batch(
                &[],
                &BatchOptions {
                    checkpoint: Some(("engine:journal:7".into(), 5)),
                    ..BatchOptions::default()
                },
            )
            .unwrap();
        assert!(outcomes.is_empty());
        assert_eq!(store.get_checkpoint("engine:journal:7").unwrap(), Some(5));
    }

    #[test]
    fn checkpoint_missing_key_is_none() {
        let store = open();
        assert_eq!(store.get_checkpoint("engine:journal:99").unwrap(), None);
    }

    #[test]
    fn batch_empty_is_a_no_op() {
        let store = open();
        let outcomes = store.remember_batch(&[], &BatchOptions::default()).unwrap();
        assert!(outcomes.is_empty());
        assert_eq!(store.status().unwrap().total_entities, 0);
    }

    #[test]
    fn batch_observations_are_searchable() {
        let store = open();
        store
            .remember_batch(
                &[req(
                    "quarterly-sync",
                    &["decided to adopt the context engine"],
                )],
                &BatchOptions::default(),
            )
            .unwrap();
        let results = store
            .engine()
            .engine
            .search(
                &[],
                "context engine",
                10,
                openmemory_index::SearchMode::KeywordOnly,
                0,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    /// Embedder-dependent behavior, compiled only when the `testing`
    /// feature provides the [`Embedder`] trait surface (workspace test
    /// runs unify it on via openmemory-bench).
    #[cfg(feature = "testing")]
    mod embedding {
        use super::*;
        use openmemory_core::testing::Embedder;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        /// Counts embed calls and the texts per call, returning a tiny
        /// deterministic vector per text.
        #[derive(Default)]
        struct CountingEmbedder {
            calls: AtomicUsize,
            texts: AtomicUsize,
        }

        impl Embedder for CountingEmbedder {
            fn embed(&self, texts: &[&str]) -> Vec<Vec<f32>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.texts.fetch_add(texts.len(), Ordering::SeqCst);
                texts
                    .iter()
                    .map(|t| {
                        let h = t.len() as f32;
                        vec![h, 1.0, 0.0, 0.0]
                    })
                    .collect()
            }

            fn dimensions(&self) -> usize {
                4
            }
        }

        fn store_with(embedder: Arc<CountingEmbedder>) -> MemoryStore {
            MemoryStore::open_in_memory(&Config::default())
                .unwrap()
                .with_embedder(embedder)
        }

        #[test]
        fn batch_embeds_every_observation_in_one_call() {
            let embedder = Arc::new(CountingEmbedder::default());
            let store = store_with(Arc::clone(&embedder));

            store
                .remember_batch(
                    &[
                        req("alpha", &["one", "two"]),
                        req("bravo", &["three"]),
                        req("charlie", &["four", "five", "six"]),
                    ],
                    &BatchOptions::default(),
                )
                .unwrap();

            assert_eq!(
                embedder.calls.load(Ordering::SeqCst),
                1,
                "all observations must embed in one batched call"
            );
            assert_eq!(embedder.texts.load(Ordering::SeqCst), 6);
            assert_eq!(
                store.status().unwrap().vector_count,
                6,
                "every embedded observation lands in the vector index"
            );
        }

        #[test]
        fn remember_embeds_observations_in_one_call() {
            let embedder = Arc::new(CountingEmbedder::default());
            let store = store_with(Arc::clone(&embedder));

            store
                .remember(
                    "alpha",
                    EntityType::Fact,
                    &[
                        ObservationInput::new("one"),
                        ObservationInput::new("two"),
                        ObservationInput::new("three"),
                    ],
                    &[],
                    "test",
                )
                .unwrap();

            assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
            assert_eq!(embedder.texts.load(Ordering::SeqCst), 3);
            assert_eq!(store.status().unwrap().vector_count, 3);
        }

        #[test]
        fn drop_persists_deferred_vector_index() {
            let dir = tempfile::tempdir().unwrap();
            let embedder = Arc::new(CountingEmbedder::default());
            {
                let store = MemoryStore::open(&Config::default(), dir.path())
                    .unwrap()
                    .with_embedder(Arc::clone(&embedder) as Arc<dyn Embedder>);
                // Batched write defers the vector save...
                store
                    .remember_batch(&[req("alpha", &["one", "two"])], &BatchOptions::default())
                    .unwrap();
                // ...and Drop is the clean-exit backstop.
            }
            let reopened = MemoryStore::open(&Config::default(), dir.path()).unwrap();
            assert_eq!(
                reopened.status().unwrap().vector_count,
                2,
                "vectors written by a batch must survive a clean drop"
            );
        }
    }

    #[test]
    fn request_round_trips_through_serde() {
        let request = RememberRequest::new("alpha", EntityType::Event)
            .with_observations(vec![ObservationInput::new("x").with_title("t")])
            .with_relations(vec![RelationInput::new("uses", "y", EntityType::Tool)])
            .with_source("journal");
        let json = serde_json::to_string(&request).unwrap();
        let back: RememberRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, request);
    }
}
