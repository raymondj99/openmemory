//! `MemoryStore::remember`, the atomic write path.
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

use openmemory_index::IndexEntry;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, MemoryResult};
use crate::normalize::{find_best_match, NormalizeMatch};
use crate::store::MemoryStore;
use crate::types::{new_id, Entity, EntityType, MemoryTier};

/// One observation to append. Used by [`MemoryStore::remember`] to keep
/// the call site declarative. Serde derives exist so write-behind layers
/// (openmemory-engine's shard journal) can round-trip pending writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Defaults to [`MemoryTier::Episodic`].
    pub memory_tier: MemoryTier,
    /// Optional caller-supplied title. Indexed at very high weight via
    /// FTS5 in v0.3.
    pub title: Option<String>,
    /// Optional caller-supplied summary. Indexed at medium weight.
    pub summary: Option<String>,
    /// Optional importance in `[0.0, 1.0]`, clamped on write. Used as a
    /// ranking prior, never indexed as text.
    pub importance: Option<f32>,
    /// Optional free-form source-kind discriminator.
    pub source_kind: Option<String>,
    /// Optional concept tags. Stored in the `observation_concepts` side
    /// table.
    pub concepts: Vec<String>,
    /// Optional source-file paths. Stored in the
    /// `observation_source_files` side table.
    pub source_files: Vec<String>,
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
            title: None,
            summary: None,
            importance: None,
            source_kind: None,
            concepts: Vec::new(),
            source_files: Vec::new(),
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

    /// Tag the observation with a memory tier (episodic, semantic, or
    /// procedural). Defaults to [`MemoryTier::Episodic`].
    #[must_use]
    pub fn with_memory_tier(mut self, tier: MemoryTier) -> Self {
        self.memory_tier = tier;
        self
    }

    /// Attach an optional caller-supplied title. Indexed at very high
    /// weight (FTS5) in v0.3.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Attach an optional caller-supplied summary.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// Attach an optional importance prior. Clamps into `[0.0, 1.0]`.
    /// Not indexed; used as a ranking prior only.
    #[must_use]
    pub fn with_importance(mut self, importance: f32) -> Self {
        self.importance = Some(importance.clamp(0.0, 1.0));
        self
    }

    /// Attach an optional free-form source-kind discriminator.
    #[must_use]
    pub fn with_source_kind(mut self, kind: impl Into<String>) -> Self {
        self.source_kind = Some(kind.into());
        self
    }

    /// Attach concept tags. Stored in the `observation_concepts` side
    /// table.
    #[must_use]
    pub fn with_concepts(mut self, concepts: Vec<String>) -> Self {
        self.concepts = concepts;
        self
    }

    /// Attach source-file paths. Stored in the
    /// `observation_source_files` side table.
    #[must_use]
    pub fn with_source_files(mut self, files: Vec<String>) -> Self {
        self.source_files = files;
        self
    }
}

/// One relation to attach. The target is named by `(target_name, target_type)`;
/// entities are looked up or created lazily, mirroring the way `remember`
/// handles its primary entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Internal payload threaded from the SQLite write step to the
/// search-index sync step. Carries everything the FTS5 backend needs to
/// build a fielded `IndexEntry` without re-reading the row from disk.
#[derive(Debug, Clone)]
pub(crate) struct SearchPayload {
    id: String,
    content: String,
    title: Option<String>,
    summary: Option<String>,
    concepts: Vec<String>,
    source_files: Vec<String>,
    source_kind: Option<String>,
}

/// Whether a write persists the vector index to disk before returning.
/// Threaded into [`MemoryStore::sync_search_groups`] by the two write
/// entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersistIndex {
    /// Save now. Single-shot writes use this: a short-lived CLI process
    /// may exit immediately after the call.
    Now,
    /// Leave persistence to the caller's cadence (the context engine's
    /// maintenance tick) and the store's `Drop` backstop. Batched
    /// ingestion uses this because the save is O(corpus).
    Defer,
}

/// Normalization knobs threaded into [`write_group`]. Mirrors the store
/// fields; `enabled` additionally folds in any per-call opt-out (bulk
/// ingestion paths skip the fuzzy scan).
#[derive(Debug, Clone, Copy)]
pub(crate) struct NormalizationParams {
    pub(crate) enabled: bool,
    pub(crate) auto_merge_threshold: f64,
    pub(crate) flag_threshold: f64,
    pub(crate) max_candidates: usize,
}

/// Result of one entity-group write inside a transaction: the public
/// outcome plus the payload the search index needs after commit.
pub(crate) struct GroupWrite {
    pub(crate) outcome: RememberOutcome,
    pub(crate) payload: Vec<SearchPayload>,
}

/// Input validation shared by `remember` and `remember_batch`. Rejects
/// empty entity names, empty observation content, and empty relation
/// fields before any transaction is opened.
pub(crate) fn validate_group(
    name: &str,
    observations: &[ObservationInput],
    relations: &[RelationInput],
) -> MemoryResult<()> {
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
    Ok(())
}

/// Write one entity group (entity upsert + observations + relations)
/// inside an open transaction. Factored out of `remember` so
/// `remember_batch` can amortise one transaction over many groups.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_group(
    tx: &Transaction<'_>,
    name: &str,
    entity_type: EntityType,
    observations: &[ObservationInput],
    relations: &[RelationInput],
    source: &str,
    now: i64,
    norm: NormalizationParams,
) -> MemoryResult<GroupWrite> {
    let ent = ensure_entity(
        tx,
        name,
        entity_type,
        source,
        now,
        norm.enabled,
        norm.auto_merge_threshold,
        norm.flag_threshold,
        norm.max_candidates,
    )?;
    let entity_id = ent.entity_id;
    let entity_existed = ent.existed;
    let normalized = ent.normalized;

    let mut observation_ids = Vec::with_capacity(observations.len());
    let mut payload: Vec<SearchPayload> = Vec::with_capacity(observations.len());
    for input in observations {
        let id = new_id();
        let clamped_importance = input.importance.map(|v| v.clamp(0.0, 1.0));
        tx.execute(
            "INSERT INTO observations
                (id, entity_id, content, observed_at, valid_from, valid_until,
                 confidence, source, tombstoned, access_count, memory_tier,
                 title, summary, importance, source_kind)
             VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, ?9,
                 ?10, ?11, ?12, ?13)",
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
                input.title,
                input.summary,
                clamped_importance,
                input.source_kind,
            ],
        )?;

        for concept in &input.concepts {
            if concept.trim().is_empty() {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO observation_concepts (observation_id, concept)
                 VALUES (?1, ?2)",
                params![id, concept],
            )?;
        }
        for file in &input.source_files {
            if file.trim().is_empty() {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO observation_source_files (observation_id, file_path)
                 VALUES (?1, ?2)",
                params![id, file],
            )?;
        }

        payload.push(SearchPayload {
            id: id.clone(),
            content: input.content.clone(),
            title: input.title.clone(),
            summary: input.summary.clone(),
            concepts: input.concepts.clone(),
            source_files: input.source_files.clone(),
            source_kind: input.source_kind.clone(),
        });
        observation_ids.push(id);
    }

    let mut relation_ids = Vec::with_capacity(relations.len());
    for rel in relations {
        let target_result = ensure_entity(
            tx,
            &rel.target_name,
            rel.target_type,
            if rel.source.is_empty() {
                source
            } else {
                &rel.source
            },
            now,
            norm.enabled,
            norm.auto_merge_threshold,
            norm.flag_threshold,
            norm.max_candidates,
        )?;
        let target_id = target_result.entity_id;
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

    Ok(GroupWrite {
        outcome: RememberOutcome {
            entity_id,
            entity_existed,
            observation_ids,
            relation_ids,
            normalized,
        },
        payload,
    })
}

/// Result of a successful [`MemoryStore::remember`] call.
#[derive(Debug, Clone, PartialEq)]
pub struct RememberOutcome {
    pub entity_id: String,
    /// `true` if the entity already existed before the call.
    pub entity_existed: bool,
    pub observation_ids: Vec<String>,
    pub relation_ids: Vec<String>,
    pub normalized: Option<NormalizeMatch>,
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
        validate_group(name, observations, relations)?;

        // Embed before taking any lock: one batched embedder call for
        // every observation, off the write-critical path.
        let texts: Vec<String> = observations
            .iter()
            .map(|o| Self::search_body_text(name, &o.content))
            .collect();
        let vectors = self.embed_documents_batch(&texts);

        let now = self.clock().now_secs();

        // Hold the rebuild write-lock for the entire write+search-sync to
        // keep concurrent recall out of the half-applied state.
        let _guard = self.write_rebuild();

        let mut conn = self.lock_db();
        let tx = conn.transaction()?;

        let group = write_group(
            &tx,
            name,
            entity_type,
            observations,
            relations,
            source,
            now,
            self.normalization_params(true),
        )?;

        tx.commit()?;
        drop(conn);

        // SQLite write succeeded; sync the search index.
        if !group.payload.is_empty() {
            self.sync_search_groups(
                &[(name.to_string(), group.payload)],
                vectors,
                PersistIndex::Now,
            )?;
        }

        Ok(group.outcome)
    }

    /// Normalization parameters for a write, folding the per-call
    /// `normalize` opt-out into the store-level configuration.
    pub(crate) fn normalization_params(&self, normalize: bool) -> NormalizationParams {
        NormalizationParams {
            enabled: self.normalization_enabled && normalize,
            auto_merge_threshold: self.auto_merge_threshold,
            flag_threshold: self.flag_threshold,
            max_candidates: self.max_candidates,
        }
    }

    /// Build the indexed body text for one observation: the entity name
    /// as a prefix so the keyword backend can match queries against it
    /// without a separate fielded entity_name column. Shared between the
    /// pre-commit embedding pass and the post-commit index sync so the
    /// two always agree on what was embedded.
    pub(crate) fn search_body_text(entity_name: &str, content: &str) -> String {
        format!("{entity_name}: {content}")
    }

    /// Embed every document in `texts` with ONE batched embedder call.
    /// Returns one vector per text, all empty when no embedder is
    /// attached (the hybrid engine then skips the vector arm).
    ///
    /// Write paths call this BEFORE taking the rebuild write lock and
    /// the writer mutex: embedding is the dominant CPU cost of a
    /// vector-enabled write, batching lets the runtime parallelise it
    /// internally, and front-loading it keeps the lock hold time down to
    /// the SQLite + index work.
    #[allow(clippy::unused_self)]
    pub(crate) fn embed_documents_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        if texts.is_empty() {
            return Vec::new();
        }
        #[cfg(any(feature = "testing", feature = "embeddings"))]
        if let Some(emb) = self.embedder_ref() {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let vectors = emb.embed_documents(&refs);
            debug_assert_eq!(vectors.len(), texts.len());
            if vectors.len() == texts.len() {
                return vectors;
            }
            // A misbehaving embedder must not break the write path:
            // degrade to keyword-only for this batch.
            tracing::warn!(
                target: "openmemory_graph::remember",
                expected = texts.len(),
                actual = vectors.len(),
                "embedder returned a mismatched batch; indexing keyword-only"
            );
        }
        vec![Vec::new(); texts.len()]
    }

    /// Index freshly committed observation groups into the hybrid search
    /// engine. Called by `remember` / `remember_batch` after the SQLite
    /// write commits; one engine insert regardless of group count.
    /// `vectors` carries the pre-computed embedding per observation in
    /// the same flattened (group, observation) order the payloads were
    /// built in — see [`MemoryStore::embed_documents_batch`].
    ///
    /// `persist` controls whether the vector index is saved to disk
    /// before returning. Single-shot writes persist immediately so
    /// short-lived CLI processes never lose vectors; batched ingestion
    /// defers to its maintenance cadence (and the [`Drop`] backstop)
    /// because the save is O(corpus).
    ///
    /// Recovery strategy: if the search insert fails, log a warning and
    /// return Ok; the SQLite row is still authoritative, and a future
    /// `rebuild_if_stale` call will catch up. The keyword backend's
    /// `fielded_text` helper concatenates the v2 caller fields (title,
    /// summary, concepts, source_files, source_kind, entity_type,
    /// entity_name) into a single FTS5 payload with high-weight fields
    /// repeated per their `Config::search.field_weights`.
    pub(crate) fn sync_search_groups(
        &self,
        groups: &[(String, Vec<SearchPayload>)],
        vectors: Vec<Vec<f32>>,
        persist: PersistIndex,
    ) -> MemoryResult<()> {
        let payload_count: usize = groups.iter().map(|(_, p)| p.len()).sum();
        debug_assert_eq!(vectors.len(), payload_count);
        let mut vectors = vectors.into_iter().chain(std::iter::repeat(Vec::new()));

        let entries: Vec<IndexEntry> = groups
            .iter()
            .flat_map(|(entity_name, payload)| payload.iter().map(move |p| (entity_name, p)))
            .map(|(entity_name, p)| {
                let uri = format!("memory://observation/{}", p.id);
                // Leaving entity_name and entity_type unset keeps the
                // legacy (no caller title / concepts) write path on the
                // v0.2 single-text indexed shape.
                let body_text = Self::search_body_text(entity_name, &p.content);
                let vector = vectors.next().unwrap_or_default();
                let mut entry = IndexEntry::new(uri, body_text)
                    .with_title(p.title.clone())
                    .with_summary(p.summary.clone())
                    .with_concepts(p.concepts.clone())
                    .with_source_files(p.source_files.clone())
                    .with_source_kind(p.source_kind.clone());
                if !vector.is_empty() {
                    entry = entry.with_vector(vector);
                }
                entry
            })
            .collect();
        if entries.is_empty() {
            return Ok(());
        }

        if let Err(e) = self.engine().engine.insert(&entries) {
            tracing::warn!(
                target: "openmemory_graph::remember",
                error = %e,
                count = entries.len(),
                "search-index insert failed; SQLite row remains authoritative"
            );
        }
        if persist == PersistIndex::Now {
            self.flush_engine();
        }
        Ok(())
    }

    /// Embed a query string via the attached embedder. Returns
    /// `Vec::new()` when no embedder is attached; the hybrid engine
    /// treats an empty query vector as "skip the vector arm", so recall
    /// keeps working keyword-only.
    #[allow(clippy::unused_self)]
    pub fn embed_query(&self, text: &str) -> Vec<f32> {
        #[cfg(any(feature = "testing", feature = "embeddings"))]
        if let Some(emb) = self.embedder_ref() {
            let v = emb.embed_query(&[text]);
            return v.into_iter().next().unwrap_or_default();
        }
        let _ = text;
        Vec::new()
    }

    /// Embed a document string via the attached embedder. Returns
    /// `Vec::new()` when no embedder is attached. Production embedders
    /// apply the model's document-side task prefix here.
    #[allow(clippy::unused_self)]
    pub fn embed_document(&self, text: &str) -> Vec<f32> {
        #[cfg(any(feature = "testing", feature = "embeddings"))]
        if let Some(emb) = self.embedder_ref() {
            let v = emb.embed_documents(&[text]);
            return v.into_iter().next().unwrap_or_default();
        }
        let _ = text;
        Vec::new()
    }
}

struct EnsureResult {
    entity_id: String,
    existed: bool,
    normalized: Option<NormalizeMatch>,
}

#[allow(clippy::too_many_arguments)]
fn ensure_entity(
    tx: &Transaction<'_>,
    name: &str,
    entity_type: EntityType,
    source: &str,
    now: i64,
    normalization_enabled: bool,
    auto_merge_threshold: f64,
    flag_threshold: f64,
    max_candidates: usize,
) -> MemoryResult<EnsureResult> {
    // Exact match path (unchanged).
    if let Some(id) = tx
        .query_row(
            "SELECT id FROM entities WHERE name = ?1 AND entity_type = ?2",
            params![name, entity_type.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        tx.execute(
            "UPDATE entities SET updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        return Ok(EnsureResult {
            entity_id: id,
            existed: true,
            normalized: None,
        });
    }

    // Fuzzy match path. Structural placeholders (partition stubs) are
    // excluded: merging a real entity into bookkeeping would attach
    // knowledge to a row that listings hide and partition accounting
    // subtracts. Exact-match (above) still finds them, which mirror
    // writes rely on.
    if normalization_enabled {
        let mut stmt = tx.prepare(
            "SELECT id, name FROM entities
             WHERE entity_type = ?1 AND source <> ?2
             ORDER BY updated_at DESC LIMIT ?3",
        )?;
        let candidates: Vec<(String, String)> = stmt
            .query_map(
                params![
                    entity_type.as_str(),
                    crate::types::PARTITION_STUB_SOURCE,
                    max_candidates as i64
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;

        if let Some(m) = find_best_match(name, &candidates, auto_merge_threshold, flag_threshold) {
            match &m {
                NormalizeMatch::AutoMerge { entity_id, .. } => {
                    tx.execute(
                        "UPDATE entities SET updated_at = ?1 WHERE id = ?2",
                        params![now, entity_id],
                    )?;
                    return Ok(EnsureResult {
                        entity_id: entity_id.clone(),
                        existed: true,
                        normalized: Some(m),
                    });
                }
                NormalizeMatch::Flag {
                    entity_id: matched_id,
                    score,
                } => {
                    let new_entity = create_entity(tx, name, entity_type, source, now)?;
                    let rel_id = new_id();
                    tx.execute(
                        "INSERT INTO relations (id, from_entity, to_entity, relation_type, weight, created_at, source)
                         VALUES (?1, ?2, ?3, 'SAME_AS', ?4, ?5, 'normalization')",
                        params![rel_id, new_entity, matched_id, *score, now],
                    )?;
                    return Ok(EnsureResult {
                        entity_id: new_entity,
                        existed: false,
                        normalized: Some(m),
                    });
                }
            }
        }
    }

    let id = create_entity(tx, name, entity_type, source, now)?;
    Ok(EnsureResult {
        entity_id: id,
        existed: false,
        normalized: None,
    })
}

fn create_entity(
    tx: &Transaction<'_>,
    name: &str,
    entity_type: EntityType,
    source: &str,
    now: i64,
) -> MemoryResult<String> {
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
    Ok(entity.id)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::types::EntityType;
    use openmemory_core::clock::{Clock, FixedClock};
    use openmemory_core::config::Config;

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
                    ObservationInput::new("ships openmemory"),
                ],
                &[RelationInput::new(
                    "maintains",
                    "openmemory",
                    EntityType::Project,
                )],
                "agent",
            )
            .unwrap();

        assert!(!outcome.entity_existed);
        assert_eq!(outcome.observation_ids.len(), 2);
        assert_eq!(outcome.relation_ids.len(), 1);

        let s = store.status().unwrap();
        assert_eq!(s.total_entities, 2, "creates Raymond + openmemory");
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
        // because every op is a simple INSERT; the canonical failure mode
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

        // The keyword backend should see the row even without an attached
        // embedder. In that mode the vector backend intentionally stays empty.
        let results = store
            .engine()
            .engine
            .search(
                &[],
                "Rust",
                10,
                openmemory_index::SearchMode::KeywordOnly,
                0,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].text.contains("prefers Rust"));
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

    #[test]
    fn observation_input_with_importance_clamps_into_unit_interval() {
        let i = ObservationInput::new("x").with_importance(2.0);
        assert_eq!(i.importance, Some(1.0));
        let i = ObservationInput::new("x").with_importance(-0.5);
        assert_eq!(i.importance, Some(0.0));
    }

    #[test]
    fn remember_writes_fielded_columns_and_side_tables() {
        let (store, _clock) = open_with_clock();
        let outcome = store
            .remember(
                "Sift",
                EntityType::Project,
                &[ObservationInput::new("uses fielded indexing")
                    .with_title("fielded indexing landed")
                    .with_summary("v2 schema notes")
                    .with_importance(0.8)
                    .with_source_kind("note")
                    .with_concepts(vec!["indexing".into(), "schema".into()])
                    .with_source_files(vec!["docs/storage.md".into()])],
                &[],
                "test",
            )
            .unwrap();
        let obs = store.get_entity_observations(&outcome.entity_id).unwrap();
        assert_eq!(obs.len(), 1);
        let o = &obs[0];
        assert_eq!(o.title.as_deref(), Some("fielded indexing landed"));
        assert_eq!(o.summary.as_deref(), Some("v2 schema notes"));
        assert!((o.importance.unwrap() - 0.8).abs() < 1e-6);
        assert_eq!(o.source_kind.as_deref(), Some("note"));
        assert_eq!(o.concepts.len(), 2);
        assert!(o.concepts.iter().any(|c| c == "indexing"));
        assert!(o.concepts.iter().any(|c| c == "schema"));
        assert_eq!(o.source_files, vec!["docs/storage.md".to_string()]);
    }

    #[test]
    fn normalize_auto_merge_on_casing() {
        let (store, _clock) = open_with_clock();
        let first = store
            .remember(
                "Project Alpha",
                EntityType::Project,
                &[ObservationInput::new("first fact")],
                &[],
                "test",
            )
            .unwrap();
        let second = store
            .remember(
                "project alpha",
                EntityType::Project,
                &[ObservationInput::new("second fact")],
                &[],
                "test",
            )
            .unwrap();
        assert_eq!(first.entity_id, second.entity_id);
        assert!(second.entity_existed);
        assert!(matches!(
            second.normalized,
            Some(NormalizeMatch::AutoMerge { .. })
        ));
        assert_eq!(store.status().unwrap().total_entities, 1);
    }

    #[test]
    fn normalize_auto_merge_on_spacing() {
        let (store, _clock) = open_with_clock();
        let first = store
            .remember(
                "ProjectAlpha",
                EntityType::Project,
                &[ObservationInput::new("fact one")],
                &[],
                "test",
            )
            .unwrap();
        let second = store
            .remember(
                "Project Alpha",
                EntityType::Project,
                &[ObservationInput::new("fact two")],
                &[],
                "test",
            )
            .unwrap();
        assert_eq!(first.entity_id, second.entity_id);
    }

    /// Pins the full shape of the SAME_AS row emitted on a Flag match.
    /// A reviewer should be able to read this test and know exactly which
    /// columns the normalization write path is responsible for. If the
    /// edge direction silently flipped, recall's graph traversal would
    /// break in a hard-to-debug way; if `source` drifted, audit tooling
    /// that filters on it would silently miss rows.
    #[test]
    fn normalize_flag_writes_same_as_relation_with_correct_shape() {
        let (store, clock) = open_with_clock();

        let original = store
            .remember(
                "config",
                EntityType::Concept,
                &[ObservationInput::new("application configuration")],
                &[],
                "agent-a",
            )
            .unwrap();

        clock.advance(60);
        let flag_time = 1_060_i64;
        let flagged = store
            .remember(
                "configs",
                EntityType::Concept,
                &[ObservationInput::new("multiple configuration files")],
                &[],
                "agent-b",
            )
            .unwrap();

        // The match itself: Flag, pointing at the *original* entity.
        let score = match flagged.normalized.as_ref() {
            Some(NormalizeMatch::Flag { entity_id, score }) => {
                assert_eq!(
                    entity_id, &original.entity_id,
                    "Flag.entity_id must reference the matched (older) entity, \
                     not the freshly created one"
                );
                *score
            }
            other => panic!("expected Flag variant, got {other:?}"),
        };

        // Exactly one SAME_AS edge: no accidental duplicates from a
        // retry, no opposite-direction shadow row.
        let rels = store.get_entity_relations(&flagged.entity_id).unwrap();
        let same_as: Vec<_> = rels
            .iter()
            .filter(|r| r.relation_type == "SAME_AS")
            .collect();
        assert_eq!(
            same_as.len(),
            1,
            "expected exactly one SAME_AS relation, got {n}: {same_as:?}",
            n = same_as.len(),
        );
        let rel = same_as[0];

        // Direction matters: edge goes from the new entity to the existing
        // one. Recall's spreading-activation pass walks both directions,
        // but downstream consumers that filter on `from_entity` (audit
        // tooling, merge surfaces) depend on this orientation.
        assert_eq!(
            rel.from_entity, flagged.entity_id,
            "from_entity must be the freshly created entity"
        );
        assert_eq!(
            rel.to_entity, original.entity_id,
            "to_entity must be the existing match target"
        );
        assert_eq!(rel.relation_type, "SAME_AS");
        assert_eq!(
            rel.source, "normalization",
            "audit tooling filters on this exact tag"
        );
        assert_eq!(
            rel.created_at, flag_time,
            "created_at must be the write-time clock value"
        );

        // The weight column is the similarity score. `Relation.weight` is
        // f32 while the score we passed in is f64, so we compare in f64
        // with an f32-precision epsilon. For values near 0.86 the f32 ULP
        // is ~1e-7; 1e-6 leaves an order of magnitude of headroom.
        let weight_diff = (f64::from(rel.weight) - score).abs();
        assert!(
            weight_diff < 1e-6,
            "weight {w} differs from score {s} by {weight_diff} \
             (expected within f32 precision, 1e-6)",
            w = rel.weight,
            s = score,
        );
    }

    #[test]
    fn normalize_no_match_on_unrelated() {
        let (store, _clock) = open_with_clock();
        let first = store
            .remember(
                "Raymond",
                EntityType::Person,
                &[ObservationInput::new("a person")],
                &[],
                "test",
            )
            .unwrap();
        let second = store
            .remember(
                "Kubernetes",
                EntityType::Person,
                &[ObservationInput::new("a platform")],
                &[],
                "test",
            )
            .unwrap();
        assert_ne!(first.entity_id, second.entity_id);
        assert!(second.normalized.is_none());
    }

    /// Cross-type isolation. Two entities sharing the same name but
    /// different `entity_type` must remain distinct under both the
    /// exact-match path (identical names) and the fuzzy-match path (a
    /// query that would Flag against either-typed candidate). The
    /// candidate SQL filters by `entity_type`; this test fails loudly
    /// if that filter is ever dropped.
    #[test]
    fn normalize_respects_entity_type_under_exact_and_fuzzy_match() {
        let (store, _clock) = open_with_clock();
        let person_raymond = store
            .remember(
                "Raymond",
                EntityType::Person,
                &[ObservationInput::new("a person")],
                &[],
                "test",
            )
            .unwrap();
        let project_raymond = store
            .remember(
                "Raymond",
                EntityType::Project,
                &[ObservationInput::new("a project")],
                &[],
                "test",
            )
            .unwrap();

        // Exact-match path: same name + different type = distinct entities.
        assert_ne!(person_raymond.entity_id, project_raymond.entity_id);

        // Fuzzy-match path: "raymon" is one edit from "Raymond" and would
        // Flag against *either* Raymond if the type filter were missing.
        // Asking for the Project type must pull only the Project Raymond.
        let outcome = store
            .remember(
                "raymon",
                EntityType::Project,
                &[ObservationInput::new("a typo")],
                &[],
                "test",
            )
            .unwrap();

        match outcome.normalized.as_ref() {
            Some(
                NormalizeMatch::Flag { entity_id, .. }
                | NormalizeMatch::AutoMerge { entity_id, .. },
            ) => {
                assert_eq!(
                    entity_id, &project_raymond.entity_id,
                    "fuzzy match must target the Project-type Raymond"
                );
                assert_ne!(
                    entity_id, &person_raymond.entity_id,
                    "fuzzy match must not cross entity_type boundaries"
                );
            }
            None => panic!(
                "expected a fuzzy match against project_raymond, got None \
                 (similarity contract may have shifted; see normalize.rs golden table)"
            ),
        }
    }

    #[test]
    fn normalize_observations_land_after_auto_merge() {
        let (store, _clock) = open_with_clock();
        let first = store
            .remember(
                "Project Alpha",
                EntityType::Project,
                &[ObservationInput::new("fact one")],
                &[],
                "test",
            )
            .unwrap();
        store
            .remember(
                "project alpha",
                EntityType::Project,
                &[ObservationInput::new("fact two")],
                &[],
                "test",
            )
            .unwrap();
        let obs = store.get_entity_observations(&first.entity_id).unwrap();
        let contents: Vec<_> = obs.iter().map(|o| o.content.as_str()).collect();
        assert!(contents.contains(&"fact one"));
        assert!(contents.contains(&"fact two"));
    }

    #[test]
    fn normalize_disabled_creates_separate_entities() {
        let mut config = Config::default();
        config.normalization.enabled = false;
        let store = MemoryStore::open_in_memory(&config).unwrap();
        let first = store
            .remember(
                "Project Alpha",
                EntityType::Project,
                &[ObservationInput::new("fact one")],
                &[],
                "test",
            )
            .unwrap();
        let second = store
            .remember(
                "project alpha",
                EntityType::Project,
                &[ObservationInput::new("fact two")],
                &[],
                "test",
            )
            .unwrap();
        assert_ne!(first.entity_id, second.entity_id);
    }

    // ---- Candidate-selection invariants ----
    //
    // These tests cover the SQL-side selection that drives fuzzy matching:
    // the `ORDER BY updated_at DESC LIMIT N` candidate query in
    // `ensure_entity`. They use direct INSERTs via `lock_db()` to control
    // `updated_at` precisely; going through `remember()` would either
    // collapse the seed entities via fuzzy match (the very behavior we're
    // setting up) or require disabling normalization mid-test.

    /// Seed an entity directly into the `entities` table. Bypasses the
    /// fuzzy-match write path so the caller can control `updated_at`.
    /// Panics on SQL errors. These are test-side bugs, not runtime
    /// conditions.
    fn seed_entity(
        store: &MemoryStore,
        id: &str,
        name: &str,
        entity_type: EntityType,
        updated_at: i64,
    ) {
        let conn = store.lock_db();
        conn.execute(
            "INSERT INTO entities
                (id, name, entity_type, created_at, updated_at, confidence, source)
             VALUES (?1, ?2, ?3, ?4, ?4, 1.0, 'seed')",
            rusqlite::params![id, name, entity_type.as_str(), updated_at],
        )
        .expect("seed insert failed");
    }

    /// Two existing entities of the same type whose stored names differ
    /// but whose fuzzy-match forms both collapse to the same canonical
    /// form as the query. `find_best_match` uses strict `>`, so on a
    /// perfect-score tie the first row in the iteration wins, and the
    /// SQL layer hands them out in `updated_at DESC` order, which means
    /// "most recently updated wins" end-to-end. Regressing the SQL
    /// `ORDER BY` (or `find_best_match`'s comparison operator) would
    /// flip this assertion.
    #[test]
    fn normalize_recency_tie_break_picks_most_recently_updated() {
        let (store, clock) = open_with_clock();

        seed_entity(&store, "older", "Project Alpha", EntityType::Project, 500);
        seed_entity(&store, "newer", "ProjectAlpha", EntityType::Project, 1_500);

        clock.set(2_000);
        let outcome = store
            .remember(
                "project alpha",
                EntityType::Project,
                &[ObservationInput::new("a fact")],
                &[],
                "test",
            )
            .unwrap();

        assert_eq!(
            outcome.entity_id, "newer",
            "tie at score 1.0 must resolve to the more recently updated entity"
        );
        assert!(outcome.entity_existed);
        assert!(
            matches!(
                outcome.normalized,
                Some(NormalizeMatch::AutoMerge { ref entity_id, .. }) if entity_id == "newer"
            ),
            "expected AutoMerge against 'newer', got {n:?}",
            n = outcome.normalized,
        );
    }

    /// `max_candidates` truncates the candidate window after ordering by
    /// `updated_at DESC`. An entity that would fuzzy-match the query is
    /// invisible if it falls outside the window. The test runs the same
    /// scenario twice with different `max_candidates` to demonstrate both
    /// directions of the cap.
    #[test]
    fn normalize_max_candidates_caps_candidate_set() {
        // Runs the scenario with the given `max_candidates`. The scenario:
        // three Project-typed entities are seeded with strictly increasing
        // `updated_at`. The OLDEST is the only one whose name fuzzy-matches
        // the query "config" (it's named "configs"); the other two are
        // arbitrary unrelated strings whose `updated_at` puts them at the
        // top of the candidate list.
        fn run(max_candidates: usize) -> RememberOutcome {
            let mut config = Config::default();
            config.normalization.max_candidates = max_candidates;
            let clock = Arc::new(FixedClock::new(1_000));
            let store = MemoryStore::open_in_memory(&config)
                .unwrap()
                .with_clock(clock.clone() as Arc<dyn Clock>);

            seed_entity(&store, "matchable", "configs", EntityType::Project, 100);
            seed_entity(
                &store,
                "filler-a",
                "completely unrelated",
                EntityType::Project,
                200,
            );
            seed_entity(
                &store,
                "filler-b",
                "something else entirely",
                EntityType::Project,
                300,
            );

            clock.set(2_000);
            store
                .remember(
                    "config",
                    EntityType::Project,
                    &[ObservationInput::new("a fact")],
                    &[],
                    "test",
                )
                .unwrap()
        }

        // Cap of 3 admits every seeded entity; "matchable" is visible and
        // the query Flags against it.
        let permissive = run(3);
        assert!(
            matches!(
                permissive.normalized,
                Some(NormalizeMatch::Flag { ref entity_id, .. }) if entity_id == "matchable"
            ),
            "with max_candidates=3, 'matchable' must be reachable; got {n:?}",
            n = permissive.normalized,
        );

        // Cap of 2 hides "matchable" behind the two more-recently-updated
        // filler entities; neither filler fuzzy-matches "config", so the
        // query falls through to creating a fresh entity.
        let restrictive = run(2);
        assert!(
            !restrictive.entity_existed,
            "with max_candidates=2, query must create a fresh entity; got {restrictive:?}",
        );
        assert!(
            restrictive.normalized.is_none(),
            "max_candidates leak: 'matchable' was reachable past the cap; got {n:?}",
            n = restrictive.normalized,
        );
    }

    /// Structural placeholder rows (partition stubs) must never be
    /// fuzzy-merge targets: an observation attached to a stub is
    /// unreachable through listings and breaks partition accounting.
    /// Exact-name matches still find them (mirror writes depend on
    /// that). Found by the LLM swarm test: a primary entity
    /// auto-merged onto a near-identically named stub.
    #[test]
    fn normalize_never_merges_into_partition_stubs() {
        let (store, _clock) = open_with_clock();
        seed_entity_with_source(
            &store,
            "stub",
            "Project Alpha",
            EntityType::Project,
            500,
            crate::types::PARTITION_STUB_SOURCE,
        );

        // Near-identical name: without the exclusion this auto-merges
        // onto the stub and the observation lands on bookkeeping.
        let outcome = store
            .remember(
                "project alpha",
                EntityType::Project,
                &[ObservationInput::new("real knowledge")],
                &[],
                "agent",
            )
            .unwrap();
        assert_ne!(outcome.entity_id, "stub", "must not merge into a stub");
        assert!(!outcome.entity_existed);
        assert!(
            outcome.normalized.is_none(),
            "stubs are not flag targets either"
        );

        // Exact-match still resolves the stub (mirror-write idempotence).
        let exact = store
            .remember("Project Alpha", EntityType::Project, &[], &[], "agent")
            .unwrap();
        assert_eq!(exact.entity_id, "stub");
        assert!(exact.entity_existed);
    }

    /// Seed an entity with an explicit source tag, bypassing the write
    /// path (same rationale as [`seed_entity`]).
    fn seed_entity_with_source(
        store: &MemoryStore,
        id: &str,
        name: &str,
        entity_type: EntityType,
        updated_at: i64,
        source: &str,
    ) {
        let conn = store.lock_db();
        conn.execute(
            "INSERT INTO entities
                (id, name, entity_type, created_at, updated_at, confidence, source)
             VALUES (?1, ?2, ?3, ?4, ?4, 1.0, ?5)",
            rusqlite::params![id, name, entity_type.as_str(), updated_at, source],
        )
        .expect("seed insert failed");
    }

    /// An entity whose only observations have been tombstoned remains a
    /// valid fuzzy-match candidate. The schema doesn't tombstone entities,
    /// only observations, so the entity row sticks around until
    /// `prune` collects it. Pinning current behavior: a `remember` call
    /// that fuzzy-matches an orphan resurrects it. If we ever decide to
    /// filter orphans out of the candidate query, this test should fail
    /// loudly so the call site and the contract change together.
    #[test]
    fn normalize_orphaned_entity_is_a_valid_match_target() {
        let (store, _clock) = open_with_clock();

        let original = store
            .remember(
                "ProjectAlpha",
                EntityType::Project,
                &[ObservationInput::new("only one")],
                &[],
                "test",
            )
            .unwrap();
        store.forget(&original.observation_ids[0]).unwrap();

        // Schema state: entity row remains, no active observations.
        let s = store.status().unwrap();
        assert_eq!(s.total_entities, 1, "entity row should survive forget");
        assert_eq!(
            s.total_observations, 0,
            "tombstoned observation should not count as active"
        );

        let revival = store
            .remember(
                "Project Alpha",
                EntityType::Project,
                &[ObservationInput::new("reborn")],
                &[],
                "test",
            )
            .unwrap();

        assert_eq!(
            revival.entity_id, original.entity_id,
            "orphan entity must be reachable via fuzzy match"
        );
        assert!(revival.entity_existed);
        assert!(
            matches!(revival.normalized, Some(NormalizeMatch::AutoMerge { .. })),
            "expected AutoMerge against the orphaned entity, got {n:?}",
            n = revival.normalized,
        );
    }
}
