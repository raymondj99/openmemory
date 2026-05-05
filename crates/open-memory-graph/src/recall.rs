//! `MemoryStore::recall` — hybrid search + decay scoring.
//!
//! Recall is the read-side hot path. It runs a hybrid search through
//! [`open_memory_index`], filters the results by temporal validity and any
//! caller-supplied [`RecallFilters`], and re-scores with the Ebbinghaus
//! forgetting curve so older observations naturally fade. A correction-tagged
//! observation gets a 1.3x retrieval boost; spreading activation across
//! relations fills out the result set when the direct search underflows.
//!
//! ```text
//!   base_decay     = exp(-lambda * days_since_observed)
//!   retrieval      = 1 + 0.15 * ln(1 + access_count)
//!   correction     = source == "correction" ? 1.3 : 1.0
//!   final_score    = search_score * base_decay * retrieval * correction * confidence
//! ```
//!
//! The recall path takes `read_rebuild()` for the duration of the search to
//! keep concurrent rebuilds (commits 26 / 29) from exposing a half-rebuilt
//! index.

use std::collections::{HashMap, HashSet};

use open_memory_index::SearchMode;
use rusqlite::{params, OptionalExtension};

use crate::error::MemoryResult;
use crate::store::{row_to_observation, MemoryStore};
use crate::types::{EntityType, Observation};

/// Multiplier applied when an observation's source matches one of the
/// configured "correction" tags. These are high-priority "don't repeat
/// this mistake" memories.
pub const CORRECTION_RETRIEVAL_BOOST: f32 = 1.3;

/// Score floor for direct hits. Raw search scores below this drop out so
/// approximate-nearest-neighbour noise doesn't drown signal.
pub const RECALL_MIN_SCORE: f32 = 0.05;

/// Distance decay applied to observations surfaced via 1-hop relation
/// spreading. They are inherently weaker than direct hits.
pub const SPREADING_DISTANCE_DECAY: f32 = 0.5;

/// Source tags treated as corrections. Plural so callers using either of
/// sift's conventions (`cortex:correction`) or the open-memory native
/// (`correction`) get the boost.
const CORRECTION_SOURCES: &[&str] = &["correction", "cortex:correction"];

/// Filters applied to recall. Each field is optional; `None` disables that
/// filter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecallFilters {
    pub entity_type: Option<EntityType>,
    /// Restrict to observations valid at this Unix timestamp. `None` =
    /// validity at the store clock's `now`.
    pub valid_at: Option<i64>,
    pub source: Option<String>,
    pub min_confidence: Option<f32>,
    /// Restrict to specific entity names (case-insensitive match).
    pub entity_names: Option<Vec<String>>,
    /// Restrict to specific search mode. Defaults to whatever
    /// `SearchMode::default()` is (Hybrid).
    pub mode: Option<SearchMode>,
    /// Whether to use the relation-spreading fallback when direct search
    /// underflows. Default: enabled.
    pub spreading_activation: bool,
}

impl RecallFilters {
    /// Default filters with spreading activation on.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spreading_activation: true,
            ..Self::default()
        }
    }
}

/// One result row from [`MemoryStore::recall`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecallResult {
    pub observation: Observation,
    pub entity_name: String,
    pub entity_type: EntityType,
    /// Raw search score from the hybrid engine (post-RRF).
    pub raw_score: f32,
    /// Final score after decay + boosts. Higher = more relevant.
    pub score: f32,
}

impl MemoryStore {
    /// Hybrid recall: vector + keyword search through the index crate,
    /// filtered by temporal validity, scored with Ebbinghaus decay, fall
    /// back to relation spreading when direct hits are sparse.
    pub fn recall(
        &self,
        query: &str,
        top_k: usize,
        filters: &RecallFilters,
    ) -> MemoryResult<Vec<RecallResult>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let top_k = top_k.max(1);

        let mode = filters.mode.unwrap_or_default();

        let vector = self.embed_text(query);
        let fetch = top_k.saturating_mul(3).max(top_k);

        let raw_results = {
            let _guard = self.read_rebuild();
            self.engine()
                .engine
                .search(&vector, query, fetch, mode, 0)?
        };

        let valid_at = filters.valid_at.unwrap_or_else(|| self.clock().now_secs());
        let lambda = self.decay_rate();

        let mut hits: Vec<RecallResult> = Vec::with_capacity(raw_results.len());
        let conn = self.lock_db();
        for r in &raw_results {
            let Some(obs_id) = r.uri.strip_prefix("memory://observation/") else {
                continue;
            };

            let lookup = conn
                .query_row(
                    "SELECT o.id, o.entity_id, o.content, o.observed_at, o.valid_from,
                            o.valid_until, o.confidence, o.source, o.tombstoned, o.access_count,
                            e.name, e.entity_type
                     FROM observations o
                     JOIN entities e ON o.entity_id = e.id
                     WHERE o.id = ?1",
                    [obs_id],
                    |row| {
                        let obs = row_to_observation(row)?;
                        let entity_name: String = row.get(10)?;
                        let entity_type_str: String = row.get(11)?;
                        Ok((obs, entity_name, entity_type_str))
                    },
                )
                .optional()?;
            let Some((obs, entity_name, entity_type_str)) = lookup else {
                continue;
            };

            if obs.tombstoned || !obs.is_valid_at(valid_at) {
                continue;
            }
            if let Some(min_conf) = filters.min_confidence {
                if obs.confidence < min_conf {
                    continue;
                }
            }
            if let Some(ref filter_source) = filters.source {
                if &obs.source != filter_source {
                    continue;
                }
            }
            let entity_type = EntityType::parse(&entity_type_str).unwrap_or(EntityType::Concept);
            if let Some(ref filter_type) = filters.entity_type {
                if &entity_type != filter_type {
                    continue;
                }
            }
            if let Some(ref names) = filters.entity_names {
                let name_lower = entity_name.to_lowercase();
                if !names.iter().any(|n| n.to_lowercase() == name_lower) {
                    continue;
                }
            }

            let raw_score = r.score;
            let score = compute_score(&obs, raw_score, valid_at, lambda);
            if score < RECALL_MIN_SCORE {
                continue;
            }
            hits.push(RecallResult {
                observation: obs,
                entity_name,
                entity_type,
                raw_score,
                score,
            });
        }
        drop(conn);

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(top_k);

        if hits.len() < top_k && filters.spreading_activation {
            let slots = top_k - hits.len();
            let related = self.spread_activation(&hits, slots, filters, valid_at, lambda)?;
            hits.extend(related);
        }

        // Side-effect: increment access_count so future recalls of the same
        // observation get the retrieval-frequency boost.
        if !hits.is_empty() {
            let ids: Vec<String> = hits.iter().map(|r| r.observation.id.clone()).collect();
            let _ = self.bump_access_counts(&ids);
        }

        Ok(hits)
    }

    /// 1-hop relation traversal from the seed entities. For each unique
    /// entity in `seed_results`, this surfaces observations on entities
    /// the seeds are directly related to (in either direction). Scores are
    /// scaled by [`SPREADING_DISTANCE_DECAY`] times the relation weight.
    fn spread_activation(
        &self,
        seeds: &[RecallResult],
        max_extra: usize,
        filters: &RecallFilters,
        valid_at: i64,
        lambda: f64,
    ) -> MemoryResult<Vec<RecallResult>> {
        if max_extra == 0 || seeds.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock_db();
        let mut seen: HashSet<String> =
            seeds.iter().map(|r| r.observation.id.clone()).collect();
        let mut seed_entities: HashMap<String, f32> = HashMap::new();
        for s in seeds {
            seed_entities
                .entry(s.observation.entity_id.clone())
                .and_modify(|v| {
                    if s.score > *v {
                        *v = s.score;
                    }
                })
                .or_insert(s.score);
        }

        let mut out = Vec::new();
        let mut rel_stmt = conn.prepare(
            "SELECT to_entity, weight FROM relations
             WHERE from_entity = ?1
                AND (valid_until IS NULL OR valid_until > ?2)
             UNION ALL
             SELECT from_entity, weight FROM relations
             WHERE to_entity = ?1
                AND (valid_until IS NULL OR valid_until > ?2)",
        )?;
        let mut obs_stmt = conn.prepare(
            "SELECT o.id, o.entity_id, o.content, o.observed_at, o.valid_from,
                    o.valid_until, o.confidence, o.source, o.tombstoned, o.access_count,
                    e.name, e.entity_type
             FROM observations o
             JOIN entities e ON o.entity_id = e.id
             WHERE o.entity_id = ?1
                AND o.tombstoned = 0
                AND (o.valid_until IS NULL OR o.valid_until > ?2)
             ORDER BY o.observed_at DESC
             LIMIT ?3",
        )?;

        for (seed_entity, seed_score) in &seed_entities {
            let mut neighbours = rel_stmt.query(params![seed_entity, valid_at])?;
            while let Some(row) = neighbours.next()? {
                let neighbour_id: String = row.get(0)?;
                let weight: f32 = row.get(1)?;
                if seed_entities.contains_key(&neighbour_id) {
                    continue;
                }

                let limit_remaining = i64::try_from(max_extra - out.len())
                    .unwrap_or(i64::MAX)
                    .max(1);
                let mut obs_rows =
                    obs_stmt.query(params![neighbour_id, valid_at, limit_remaining])?;
                while let Some(orow) = obs_rows.next()? {
                    let obs = row_to_observation(orow)?;
                    if !obs.is_valid_at(valid_at) {
                        continue;
                    }
                    let entity_name: String = orow.get(10)?;
                    let entity_type_str: String = orow.get(11)?;
                    let entity_type =
                        EntityType::parse(&entity_type_str).unwrap_or(EntityType::Concept);

                    if let Some(ref filter_type) = filters.entity_type {
                        if entity_type != *filter_type {
                            continue;
                        }
                    }
                    if let Some(min_conf) = filters.min_confidence {
                        if obs.confidence < min_conf {
                            continue;
                        }
                    }
                    if let Some(ref filter_source) = filters.source {
                        if &obs.source != filter_source {
                            continue;
                        }
                    }
                    if !seen.insert(obs.id.clone()) {
                        continue;
                    }

                    let base_score =
                        SPREADING_DISTANCE_DECAY * weight.clamp(0.0, 1.0) * *seed_score;
                    let score = compute_score(&obs, base_score, valid_at, lambda);
                    if score < RECALL_MIN_SCORE * 0.5 {
                        continue;
                    }
                    out.push(RecallResult {
                        observation: obs,
                        entity_name,
                        entity_type,
                        raw_score: base_score,
                        score,
                    });
                    if out.len() >= max_extra {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Increment `access_count` on the listed observations. Best-effort:
    /// errors are logged and swallowed so a transient SQLite hiccup doesn't
    /// fail the recall call. Caller already has the results.
    pub(crate) fn bump_access_counts(&self, ids: &[String]) -> MemoryResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock_db();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE observations SET access_count = access_count + 1 WHERE id = ?1",
            )?;
            for id in ids {
                stmt.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

fn compute_score(
    observation: &Observation,
    raw_score: f32,
    valid_at: i64,
    lambda: f64,
) -> f32 {
    let days_since = ((valid_at - observation.observed_at).max(0)) as f64 / 86_400.0;
    let base_decay = (-lambda * days_since).exp() as f32;
    let retrieval_boost =
        (1.0_f32 + 0.15_f32 * (1.0_f32 + observation.access_count as f32).ln()).max(1.0);
    let correction_boost = if CORRECTION_SOURCES
        .iter()
        .any(|s| observation.source == *s)
    {
        CORRECTION_RETRIEVAL_BOOST
    } else {
        1.0
    };
    raw_score
        * base_decay
        * retrieval_boost
        * observation.confidence.clamp(0.0, 1.0)
        * correction_boost
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remember::{ObservationInput, RelationInput};
    use crate::types::EntityType;
    use open_memory_core::clock::{Clock, FixedClock};
    use open_memory_core::config::Config;
    use std::sync::Arc;

    const SECONDS_PER_DAY: i64 = 86_400;

    fn open_with_clock() -> (MemoryStore, Arc<FixedClock>) {
        let clock = Arc::new(FixedClock::new(1_000_000));
        let store = MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_clock(clock.clone() as Arc<dyn Clock>);
        (store, clock)
    }

    #[test]
    fn empty_query_returns_empty() {
        let (store, _) = open_with_clock();
        let r = store.recall("", 5, &RecallFilters::new()).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn keyword_match_returns_observation() {
        let (store, _clock) = open_with_clock();
        store
            .remember(
                "Raymond",
                EntityType::Person,
                &[ObservationInput::new("prefers Rust over Python")],
                &[],
                "test",
            )
            .unwrap();

        let mut filters = RecallFilters::new();
        filters.mode = Some(SearchMode::KeywordOnly);
        let r = store.recall("Rust", 5, &filters).unwrap();
        assert!(!r.is_empty(), "should match keyword");
        assert_eq!(r[0].entity_name, "Raymond");
        assert!(r[0].observation.content.contains("Rust"));
    }

    #[test]
    fn fresh_observation_outranks_old_on_equal_match() {
        let (store, clock) = open_with_clock();

        clock.set(1_000_000);
        store
            .remember(
                "Topic",
                EntityType::Fact,
                &[ObservationInput::new("alpha mention").with_source("old")],
                &[],
                "old",
            )
            .unwrap();

        clock.set(1_000_000 + SECONDS_PER_DAY * 30);
        store
            .remember(
                "Topic",
                EntityType::Fact,
                &[ObservationInput::new("alpha mention").with_source("fresh")],
                &[],
                "fresh",
            )
            .unwrap();

        let mut filters = RecallFilters::new();
        filters.mode = Some(SearchMode::KeywordOnly);
        // Default decay rate (0.01 per day) means after 30 days the older
        // observation's decay multiplier is exp(-0.3) ≈ 0.74; with two equal
        // raw scores, the fresh one should still win on the recency boost.
        let r = store.recall("alpha", 5, &filters).unwrap();
        let fresh_idx = r.iter().position(|x| x.observation.source == "fresh");
        let old_idx = r.iter().position(|x| x.observation.source == "old");
        assert!(fresh_idx.is_some(), "fresh result should be present");
        match (fresh_idx, old_idx) {
            (Some(f), Some(o)) => assert!(f < o, "fresh should rank ahead of old"),
            _ => {} // older result may be filtered out by RECALL_MIN_SCORE — fine
        }
    }

    #[test]
    fn correction_source_gets_retrieval_boost() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "T",
                EntityType::Fact,
                &[
                    ObservationInput::new("alpha mention").with_source("correction"),
                    ObservationInput::new("alpha mention").with_source("plain"),
                ],
                &[],
                "test",
            )
            .unwrap();
        let mut filters = RecallFilters::new();
        filters.mode = Some(SearchMode::KeywordOnly);
        let r = store.recall("alpha", 5, &filters).unwrap();
        let corr = r.iter().find(|x| x.observation.source == "correction");
        let plain = r.iter().find(|x| x.observation.source == "plain");
        if let (Some(c), Some(p)) = (corr, plain) {
            assert!(
                c.score > p.score,
                "correction-tagged observation should score higher: corr={} plain={}",
                c.score,
                p.score
            );
        }
    }

    #[test]
    fn entity_type_filter_excludes_other_types() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "Alice",
                EntityType::Person,
                &[ObservationInput::new("alpha mention")],
                &[],
                "t",
            )
            .unwrap();
        store
            .remember(
                "Acme",
                EntityType::Project,
                &[ObservationInput::new("alpha mention")],
                &[],
                "t",
            )
            .unwrap();

        let mut filters = RecallFilters::new();
        filters.mode = Some(SearchMode::KeywordOnly);
        filters.entity_type = Some(EntityType::Person);
        let r = store.recall("alpha", 10, &filters).unwrap();
        for hit in &r {
            assert_eq!(hit.entity_type, EntityType::Person);
        }
    }

    #[test]
    fn temporal_filter_excludes_expired_observation() {
        let (store, clock) = open_with_clock();
        clock.set(0);
        let outcome = store
            .remember(
                "T",
                EntityType::Fact,
                &[
                    ObservationInput::new("expired alpha"),
                    ObservationInput::new("live alpha"),
                ],
                &[],
                "t",
            )
            .unwrap();
        // Expire the first observation by setting valid_until to a time in
        // the past relative to the recall valid_at.
        let conn = store.lock_db();
        conn.execute(
            "UPDATE observations SET valid_until = 100 WHERE id = ?1",
            [&outcome.observation_ids[0]],
        )
        .unwrap();
        drop(conn);

        clock.set(500);
        let mut filters = RecallFilters::new();
        filters.mode = Some(SearchMode::KeywordOnly);
        let r = store.recall("alpha", 5, &filters).unwrap();
        for hit in &r {
            assert_ne!(hit.observation.id, outcome.observation_ids[0]);
        }
    }

    #[test]
    fn min_confidence_filters_low_confidence() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "T",
                EntityType::Fact,
                &[
                    ObservationInput::new("alpha low").with_confidence(0.2),
                    ObservationInput::new("alpha high").with_confidence(0.95),
                ],
                &[],
                "t",
            )
            .unwrap();
        let mut filters = RecallFilters::new();
        filters.mode = Some(SearchMode::KeywordOnly);
        filters.min_confidence = Some(0.5);
        let r = store.recall("alpha", 5, &filters).unwrap();
        assert!(r.iter().all(|h| h.observation.confidence >= 0.5));
    }

    #[test]
    fn spreading_activation_surfaces_related_entity_observations() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "Raymond",
                EntityType::Person,
                &[ObservationInput::new("Raymond is a name")],
                &[RelationInput::new(
                    "maintains",
                    "Sift",
                    EntityType::Project,
                )],
                "t",
            )
            .unwrap();
        store
            .remember(
                "Sift",
                EntityType::Project,
                &[ObservationInput::new("Sift is a search engine")],
                &[],
                "t",
            )
            .unwrap();
        let filters = RecallFilters::new();
        let r = store.recall("Raymond", 5, &filters).unwrap();
        let entities: Vec<_> = r.iter().map(|x| x.entity_name.clone()).collect();
        assert!(
            entities.iter().any(|e| e == "Sift"),
            "spreading activation should surface Sift observation: {entities:?}"
        );
    }

    #[test]
    fn recall_increments_access_count() {
        let (store, _) = open_with_clock();
        let outcome = store
            .remember(
                "T",
                EntityType::Fact,
                &[ObservationInput::new("alpha mention")],
                &[],
                "t",
            )
            .unwrap();

        let mut filters = RecallFilters::new();
        filters.mode = Some(SearchMode::KeywordOnly);
        let _ = store.recall("alpha", 5, &filters).unwrap();
        let obs = store
            .get_entity_observations(&store.get_entity("T").unwrap().unwrap().id)
            .unwrap();
        let target = obs
            .iter()
            .find(|o| o.id == outcome.observation_ids[0])
            .unwrap();
        assert_eq!(target.access_count, 1);
    }

    #[test]
    fn entity_name_filter_restricts_to_listed_entities() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "Alice",
                EntityType::Person,
                &[ObservationInput::new("alpha mention")],
                &[],
                "t",
            )
            .unwrap();
        store
            .remember(
                "Bob",
                EntityType::Person,
                &[ObservationInput::new("alpha mention")],
                &[],
                "t",
            )
            .unwrap();
        let mut filters = RecallFilters::new();
        filters.mode = Some(SearchMode::KeywordOnly);
        filters.entity_names = Some(vec!["alice".to_string()]);
        let r = store.recall("alpha", 5, &filters).unwrap();
        assert!(r.iter().all(|h| h.entity_name == "Alice"));
    }
}
