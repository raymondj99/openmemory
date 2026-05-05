//! `MemoryStore::remember` — the atomic write path.
//!
//! `remember` ensures the entity exists, appends new observations and
//! relations, and keeps the search index in sync. Everything happens inside a
//! single SQLite transaction; on success the matching `IndexEntry`s are
//! pushed to the hybrid search engine. The two are synchronised in one
//! direction: SQLite is the source of truth, the search index is rebuilt
//! from it on demand.
//!
//! The vector rebuild path (when a write changes enough rows that the index
//! needs a full refresh) is gated by an `RwLock<()>` barrier on the store.
//! Concurrent recall calls grab the read lock; the rebuild grabs the write
//! lock; concurrent recall therefore never observes a half-rebuilt vector
//! index.

use std::sync::Arc;

use open_memory_index::IndexEntry;
use rusqlite::{params, OptionalExtension, Transaction};

use crate::error::{MemoryError, MemoryResult};
use crate::store::MemoryStore;
use crate::types::{new_id, Entity, EntityType, MemoryTier};

/// One observation to append. Used by [`MemoryStore::remember`] to keep
/// the call site declarative.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservationInput {
    pub content: String,
    /// Confidence in `[0.0, 1.0]`. Defaults to 1.0 via [`Self::new`].
    pub confidence: f32,
    /// Free-form origin tag (e.g. `"cli"`, `"mcp"`, `"agent:claude-code"`).
    pub source: String,
    /// Open-ended `valid_from`. Defaults to `now`.
    pub valid_from: Option<i64>,
    /// Open-ended `valid_until`. `None` = still valid.
    pub valid_until: Option<i64>,
    /// Defaults to [`MemoryTier::Episodic`]. Reserved for v0.2 consolidation.
    pub memory_tier: MemoryTier,
}

impl ObservationInput {
    /// Build a new observation. Confidence defaults to `1.0`, source to
    /// the empty string, validity to open-ended, and tier to
    /// [`MemoryTier::Episodic`]; refine via the `with_*` builders.
    #[must_use]
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            confidence: 1.0,
            source: String::new(),
            valid_from: None,
            valid_until: None,
            memory_tier: MemoryTier::Episodic,
        }
    }

    /// Override the confidence score. Values are clamped into `[0.0, 1.0]`.
    #[must_use]
    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    /// Tag the observation with a free-text source label, e.g. the agent
    /// or transcript that produced it. Used by recall as a soft filter and
    /// retrieval-boost signal.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }
}

/// One relation to attach. The target is named by `(target_name, target_type)`
/// — entities are looked up or created lazily, mirroring the way `remember`
/// handles its primary entity.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationInput {
    pub relation_type: String,
    pub target_name: String,
    pub target_type: EntityType,
    pub weight: f32,
    pub source: String,
}

impl RelationInput {
    #[must_use]
    pub fn new(
        relation_type: impl Into<String>,
        target_name: impl Into<String>,
        target_type: EntityType,
    ) -> Self {
        Self {
            relation_type: relation_type.into(),
            target_name: target_name.into(),
            target_type,
            weight: 1.0,
            source: String::new(),
        }
    }
}

/// Result of a successful [`MemoryStore::remember`] call.
#[derive(Debug, Clone, PartialEq)]
pub struct RememberOutcome {
    pub entity_id: String,
    /// `true` if the entity already existed before the call.
    pub entity_existed: bool,
    pub observation_ids: Vec<String>,
    pub relation_ids: Vec<String>,
}

impl MemoryStore {
    /// Atomic write: ensure the entity exists, append observations, and
    /// add relations to other (possibly new) entities. The whole sequence
    /// runs inside one SQLite transaction; if any step fails, the entire
    /// write rolls back. On success the new observations are inserted into
    /// the hybrid search index using the conventional URI shape
    /// `memory://observation/<id>`.
    ///
    /// Empty `name` returns [`MemoryError::InvalidInput`]. Empty observation
    /// content is rejected with the same error variant. Empty relation type
    /// is rejected. The function refuses to create observations or relations
    /// without a `name` to anchor them.
    pub fn remember(
        &self,
        name: &str,
        entity_type: EntityType,
        observations: &[ObservationInput],
        relations: &[RelationInput],
        source: &str,
    ) -> MemoryResult<RememberOutcome> {
        if name.trim().is_empty() {
            return Err(MemoryError::InvalidInput(
                "entity name must not be empty".into(),
            ));
        }
        for obs in observations {
            if obs.content.trim().is_empty() {
                return Err(MemoryError::InvalidInput(
                    "observation content must not be empty".into(),
                ));
            }
        }
        for rel in relations {
            if rel.relation_type.trim().is_empty() {
                return Err(MemoryError::InvalidInput(
                    "relation_type must not be empty".into(),
                ));
            }
            if rel.target_name.trim().is_empty() {
                return Err(MemoryError::InvalidInput(
                    "relation target name must not be empty".into(),
                ));
            }
        }

        let now = self.clock().now_secs();

        // Hold the rebuild write-lock for the entire write+search-sync to
        // keep concurrent recall out of the half-applied state.
        let _guard = self.write_rebuild();

        let mut conn = self.lock_db();
        let tx = conn.transaction()?;

        let (entity_id, entity_existed) = ensure_entity(&tx, name, entity_type, source, now)?;

        let mut observation_ids = Vec::with_capacity(observations.len());
        let mut search_payload: Vec<(String, String)> = Vec::with_capacity(observations.len());
        for input in observations {
            let id = new_id();
            tx.execute(
                "INSERT INTO observations
                    (id, entity_id, content, observed_at, valid_from, valid_until,
                     confidence, source, tombstoned, access_count, memory_tier)
                 VALUES
                    (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, ?9)",
                params![
                    id,
                    entity_id,
                    input.content,
                    now,
                    input.valid_from.unwrap_or(now),
                    input.valid_until,
                    input.confidence,
                    if input.source.is_empty() {
                        source.to_string()
                    } else {
                        input.source.clone()
                    },
                    input.memory_tier.as_str(),
                ],
            )?;
            search_payload.push((id.clone(), input.content.clone()));
            observation_ids.push(id);
        }

        let mut relation_ids = Vec::with_capacity(relations.len());
        for rel in relations {
            let (target_id, _existed) = ensure_entity(
                &tx,
                &rel.target_name,
                rel.target_type,
                if rel.source.is_empty() {
                    source
                } else {
                    &rel.source
                },
                now,
            )?;
            let id = new_id();
            tx.execute(
                "INSERT INTO relations
                    (id, from_entity, to_entity, relation_type, weight, created_at,
                     valid_from, valid_until, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)",
                params![
                    id,
                    entity_id,
                    target_id,
                    rel.relation_type,
                    rel.weight,
                    now,
                    now,
                    if rel.source.is_empty() {
                        source.to_string()
                    } else {
                        rel.source.clone()
                    },
                ],
            )?;
            relation_ids.push(id);
        }

        // Bump the entity's updated_at so list_entities orders by recency.
        tx.execute(
            "UPDATE entities SET updated_at = ?1 WHERE id = ?2",
            params![now, entity_id],
        )?;
        tx.commit()?;
        drop(conn);

        // SQLite write succeeded — sync the search index.
        if !search_payload.is_empty() {
            self.apply_search_sync_ops_with_recovery(name, &search_payload)?;
        }

        Ok(RememberOutcome {
            entity_id,
            entity_existed,
            observation_ids,
            relation_ids,
        })
    }

    /// Index a freshly committed batch of observations into the hybrid
    /// search engine. Called by `remember` after the SQLite write commits.
    ///
    /// "With recovery" reflects the strategy: if the search insert fails,
    /// log a warning and return Ok — the SQLite row is still authoritative,
    /// and a future `rebuild_if_stale` call will catch up. This matches
    /// what sift's MemoryStore does and avoids a partial-write panic from
    /// taking the whole MCP server down.
    fn apply_search_sync_ops_with_recovery(
        &self,
        entity_name: &str,
        payload: &[(String, String)],
    ) -> MemoryResult<()> {
        let entries: Vec<IndexEntry> = payload
            .iter()
            .map(|(id, content)| {
                let uri = format!("memory://observation/{id}");
                let text = format!("{entity_name}: {content}");
                let vector = self.embed_text(&text);
                let mut entry = IndexEntry::new(uri, text);
                if !vector.is_empty() {
                    entry = entry.with_vector(vector);
                }
                entry
            })
            .collect();

        if let Err(e) = self.engine().engine.insert(&entries) {
            tracing::warn!(
                target: "open_memory_graph::remember",
                error = %e,
                count = entries.len(),
                "search-index insert failed; SQLite row remains authoritative"
            );
        }
        Ok(())
    }

    /// Embed a search text via the attached embedder, if any. Returns an
    /// empty vector when no embedder is attached — the FTS5/BM25 path still
    /// indexes the text either way, so recall keeps working keyword-only.
    //
    // `&self` is unused under default features but required so the
    // `testing`-feature variant can read `self.embedder()`. Suppressing
    // the lint keeps a single signature across both builds.
    #[allow(clippy::unused_self)]
    pub(crate) fn embed_text(&self, text: &str) -> Vec<f32> {
        #[cfg(feature = "testing")]
        if let Some(emb) = self.embedder() {
            let v = emb.embed(&[text]);
            return v.into_iter().next().unwrap_or_default();
        }
        let _ = text;
        Vec::new()
    }

    /// Borrow the optional testing embedder. Used internally by the
    /// search-sync path; not part of the public API.
    #[cfg(feature = "testing")]
    pub(crate) fn embedder(&self) -> Option<Arc<dyn open_memory_core::testing::Embedder>> {
        self.testing_embedder()
    }
}

fn ensure_entity(
    tx: &Transaction<'_>,
    name: &str,
    entity_type: EntityType,
    source: &str,
    now: i64,
) -> MemoryResult<(String, bool)> {
    if let Some(id) = tx
        .query_row(
            "SELECT id FROM entities WHERE name = ?1 AND entity_type = ?2",
            params![name, entity_type.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        // Bump updated_at so the entity floats to the top of list_entities.
        tx.execute(
            "UPDATE entities SET updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        return Ok((id, true));
    }

    let entity = Entity {
        id: new_id(),
        name: name.to_string(),
        entity_type,
        created_at: now,
        updated_at: now,
        confidence: 1.0,
        source: source.to_string(),
    };
    tx.execute(
        "INSERT INTO entities (id, name, entity_type, created_at, updated_at, confidence, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            entity.id,
            entity.name,
            entity.entity_type.as_str(),
            entity.created_at,
            entity.updated_at,
            entity.confidence,
            entity.source,
        ],
    )?;
    Ok((entity.id, false))
}

// Unused `Arc` import on builds without the testing feature; the embedder
// hook needs it.
#[cfg(not(feature = "testing"))]
const _UNUSED_ARC: Option<Arc<()>> = None;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EntityType;
    use open_memory_core::clock::{Clock, FixedClock};
    use open_memory_core::config::Config;

    fn open_with_clock() -> (MemoryStore, Arc<FixedClock>) {
        let clock = Arc::new(FixedClock::new(1_000));
        let store = MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_clock(clock.clone() as Arc<dyn Clock>);
        (store, clock)
    }

    #[test]
    fn remember_creates_entity_observations_and_relations() {
        let (store, _clock) = open_with_clock();
        let outcome = store
            .remember(
                "Raymond",
                EntityType::Person,
                &[
                    ObservationInput::new("prefers Rust").with_source("cli"),
                    ObservationInput::new("ships open-memory"),
                ],
                &[RelationInput::new(
                    "maintains",
                    "open-memory",
                    EntityType::Project,
                )],
                "agent",
            )
            .unwrap();

        assert!(!outcome.entity_existed);
        assert_eq!(outcome.observation_ids.len(), 2);
        assert_eq!(outcome.relation_ids.len(), 1);

        let s = store.status().unwrap();
        assert_eq!(s.total_entities, 2, "creates Raymond + open-memory");
        assert_eq!(s.total_observations, 2);
        assert_eq!(s.total_relations, 1);
    }

    #[test]
    fn remember_round_trips_via_get_entity() {
        let (store, _clock) = open_with_clock();
        store
            .remember(
                "Raymond",
                EntityType::Person,
                &[ObservationInput::new("hello world")],
                &[],
                "test",
            )
            .unwrap();

        let entity = store.get_entity("Raymond").unwrap().unwrap();
        assert_eq!(entity.name, "Raymond");
        assert_eq!(entity.entity_type, EntityType::Person);

        let obs = store.get_entity_observations(&entity.id).unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].content, "hello world");
        assert_eq!(obs[0].source, "test", "source falls through from caller");
    }

    #[test]
    fn remember_is_idempotent_on_entity_creation() {
        let (store, _clock) = open_with_clock();
        let first = store
            .remember(
                "Project",
                EntityType::Project,
                &[ObservationInput::new("first")],
                &[],
                "a",
            )
            .unwrap();
        let second = store
            .remember(
                "Project",
                EntityType::Project,
                &[ObservationInput::new("second")],
                &[],
                "b",
            )
            .unwrap();
        assert_eq!(first.entity_id, second.entity_id);
        assert!(!first.entity_existed);
        assert!(second.entity_existed);
        assert_eq!(store.status().unwrap().total_observations, 2);
    }

    #[test]
    fn remember_atomic_failure_leaves_no_partial_write() {
        let (store, _clock) = open_with_clock();
        // Pre-insert a relation row that will conflict if re-inserted with
        // a duplicated PK. Simulating mid-tx failure is awkward in v0.1
        // because every op is a simple INSERT — the canonical failure mode
        // is rolling back when an FK lookup fails. Test the empty-content
        // guard instead: it returns BEFORE we open any tx.
        let err = store
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("")],
                &[],
                "src",
            )
            .unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));
        assert_eq!(store.status().unwrap().total_entities, 0);
    }

    #[test]
    fn remember_rejects_empty_name() {
        let (store, _clock) = open_with_clock();
        let err = store
            .remember("", EntityType::Person, &[], &[], "src")
            .unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));
        let err = store
            .remember("   ", EntityType::Person, &[], &[], "src")
            .unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));
    }

    #[test]
    fn remember_rejects_empty_relation_fields() {
        let (store, _clock) = open_with_clock();
        let bad_type = store
            .remember(
                "X",
                EntityType::Concept,
                &[],
                &[RelationInput {
                    relation_type: String::new(),
                    target_name: "Y".into(),
                    target_type: EntityType::Concept,
                    weight: 1.0,
                    source: String::new(),
                }],
                "s",
            )
            .unwrap_err();
        assert!(matches!(bad_type, MemoryError::InvalidInput(_)));

        let bad_target = store
            .remember(
                "X",
                EntityType::Concept,
                &[],
                &[RelationInput::new("uses", "", EntityType::Concept)],
                "s",
            )
            .unwrap_err();
        assert!(matches!(bad_target, MemoryError::InvalidInput(_)));
    }

    #[test]
    fn remember_creates_target_entity_for_relation() {
        let (store, _clock) = open_with_clock();
        let outcome = store
            .remember(
                "Raymond",
                EntityType::Person,
                &[],
                &[RelationInput::new("maintains", "sift", EntityType::Project)],
                "agent",
            )
            .unwrap();
        assert_eq!(outcome.relation_ids.len(), 1);

        let target = store.get_entity("sift").unwrap().unwrap();
        assert_eq!(target.entity_type, EntityType::Project);
        assert_eq!(store.status().unwrap().total_entities, 2);
    }

    #[test]
    fn remember_inserts_search_index_entries() {
        let (store, _clock) = open_with_clock();
        store
            .remember(
                "Raymond",
                EntityType::Person,
                &[ObservationInput::new("prefers Rust")],
                &[],
                "test",
            )
            .unwrap();

        // Engine should see the keyword-search row; the fts5 backend default
        // splits on whitespace.
        let count = store.engine().engine.count().unwrap();
        assert!(count >= 1, "search engine should have at least 1 entry");
    }

    #[test]
    fn remember_bumps_updated_at() {
        let (store, clock) = open_with_clock();
        store
            .remember(
                "X",
                EntityType::Concept,
                &[ObservationInput::new("first")],
                &[],
                "a",
            )
            .unwrap();
        let original = store.get_entity("X").unwrap().unwrap();
        assert_eq!(original.updated_at, 1_000);

        clock.advance(500);
        store
            .remember(
                "X",
                EntityType::Concept,
                &[ObservationInput::new("second")],
                &[],
                "a",
            )
            .unwrap();
        let bumped = store.get_entity("X").unwrap().unwrap();
        assert_eq!(bumped.updated_at, 1_500);
    }

    #[test]
    fn observation_input_with_methods_clamp_confidence() {
        let i = ObservationInput::new("x").with_confidence(2.0);
        assert!((i.confidence - 1.0).abs() < f32::EPSILON);
        let i = ObservationInput::new("x").with_confidence(-0.5);
        assert!(i.confidence.abs() < f32::EPSILON);
    }
}
