//! `consolidate` — periodic maintenance for the memory graph.
//!
//! Two phases, idempotent in aggregate:
//!
//! 1. **dedup** — within each entity, merge near-duplicate observations.
//!    Two observations on the same entity are "near-duplicate" when their
//!    Jaccard text similarity is at or above [`ConsolidateConfig::dedup_text_threshold`]
//!    (default 0.95). The older observation is tombstoned and its access
//!    count rolls into the survivor.
//!
//! 2. **decay/prune** — score every live observation with the same
//!    Ebbinghaus formula recall uses; observations whose score falls
//!    below [`ConsolidateConfig::prune_floor`] are tombstoned. Then the
//!    upstream [`MemoryStore::prune`] sweep collects orphaned entities.
//!
//! `consolidate(...)` is safe to call repeatedly: running it twice in a
//! row should report `duplicates_merged == 0` and `observations_pruned == 0`
//! on the second call.

use std::collections::HashMap;

use rusqlite::params;

use crate::error::MemoryResult;
use crate::store::MemoryStore;

/// One entity's row of observations as gathered by the dedup pass.
/// `(observation_id, content, access_count, observed_at)` tuples.
type DedupCandidateRow = (String, String, u32, i64);
/// Entity-id-keyed bag of dedup candidates.
type DedupCandidates = HashMap<String, Vec<DedupCandidateRow>>;

/// Tuning parameters for [`MemoryStore::consolidate`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsolidateConfig {
    /// Minimum Jaccard text similarity for two observations on the same
    /// entity to be merged. Default 0.95 — matches the Phase 4 roadmap.
    pub dedup_text_threshold: f32,
    /// Score floor under which observations are tombstoned. Compared
    /// against the same Ebbinghaus * confidence * retrieval product the
    /// recall path uses (without the search-relevance multiplier — there
    /// isn't one for an unqueried observation).
    pub prune_floor: f32,
    /// Skip observations younger than this in seconds. Default 1 day.
    /// Keeps freshly-remembered facts from being pruned before they get a
    /// chance to be retrieved.
    pub min_age_secs: i64,
    /// Lambda for the Ebbinghaus curve. Defaults to whatever the store's
    /// `decay_rate` is set to (passed via [`Self::for_store`]).
    pub decay_rate: f64,
}

impl Default for ConsolidateConfig {
    fn default() -> Self {
        Self {
            dedup_text_threshold: 0.95,
            prune_floor: 0.05,
            min_age_secs: 86_400,
            decay_rate: 0.01,
        }
    }
}

impl ConsolidateConfig {
    /// Construct a config seeded from a `MemoryStore`'s active settings.
    /// Resolves `decay_rate` from the store, leaves the rest at defaults.
    #[must_use]
    pub fn for_store(store: &MemoryStore) -> Self {
        Self {
            decay_rate: store.decay_rate(),
            ..Self::default()
        }
    }

    /// Apply [`openmemory_core::config::Config`] memory section values.
    #[must_use]
    pub fn from_config(cfg: &openmemory_core::config::Config) -> Self {
        Self {
            dedup_text_threshold: cfg.memory.dedup_threshold,
            prune_floor: cfg.memory.prune_floor,
            decay_rate: cfg.memory.decay_rate,
            min_age_secs: 86_400,
        }
    }
}

/// Counts emitted by [`MemoryStore::consolidate`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsolidateReport {
    pub duplicates_merged: usize,
    pub observations_pruned: usize,
    pub entities_pruned: usize,
}

impl MemoryStore {
    /// Run the dedup + decay/prune consolidation pipeline and return a
    /// report. Safe to call repeatedly; the report's counts converge to
    /// zero across runs once nothing more needs to change.
    pub fn consolidate(&self, config: &ConsolidateConfig) -> MemoryResult<ConsolidateReport> {
        let now = self.clock().now_secs();
        let mut report = ConsolidateReport::default();

        // Phases 1+2: collect candidates, then apply tombstones in one tx.
        // The rebuild write-lock spans the dedup/decay collection and the
        // tombstone-and-engine-sync write, then drops before we delegate
        // to `prune` for orphan cleanup. `prune` re-acquires the same lock;
        // taking it twice while it's already held would deadlock since the
        // RwLock is non-reentrant.
        {
            let _guard = self.write_rebuild();

            let candidate_groups = self.collect_dedup_candidates()?;
            let mut to_tombstone: Vec<String> = Vec::new();
            let mut access_rollups: HashMap<String, u32> = HashMap::new();

            for observations in candidate_groups.values() {
                let merges = dedup_within_entity(observations, config.dedup_text_threshold);
                for (survivor_id, loser_ids, accumulated_access) in merges {
                    if !loser_ids.is_empty() {
                        *access_rollups.entry(survivor_id).or_default() += accumulated_access;
                    }
                    to_tombstone.extend(loser_ids);
                }
            }
            report.duplicates_merged = to_tombstone.len();

            let prune_targets = self.collect_decay_prune_targets(config, now)?;
            report.observations_pruned = prune_targets.len();

            if !to_tombstone.is_empty() || !prune_targets.is_empty() || !access_rollups.is_empty() {
                let mut conn = self.lock_db();
                let tx = conn.transaction()?;
                for id in &to_tombstone {
                    tx.execute(
                        "UPDATE observations
                         SET tombstoned = 1,
                             valid_until = COALESCE(valid_until, ?1)
                         WHERE id = ?2 AND tombstoned = 0",
                        params![now, id],
                    )?;
                }
                for id in &prune_targets {
                    tx.execute(
                        "UPDATE observations
                         SET tombstoned = 1,
                             valid_until = COALESCE(valid_until, ?1)
                         WHERE id = ?2 AND tombstoned = 0",
                        params![now, id],
                    )?;
                }
                for (survivor_id, bonus) in &access_rollups {
                    tx.execute(
                        "UPDATE observations
                         SET access_count = access_count + ?1
                         WHERE id = ?2",
                        params![i64::from(*bonus), survivor_id],
                    )?;
                }
                tx.commit()?;
                drop(conn);

                // Hybrid-engine sync (best-effort).
                for id in to_tombstone.iter().chain(prune_targets.iter()) {
                    let uri = format!("memory://observation/{id}");
                    let _ = self.engine().engine.delete_by_uri(&uri);
                }
                self.flush_engine();
            }
        } // <-- rebuild_lock released here so prune() can acquire it

        // Orphan-cleanup pass. prune() takes write_rebuild internally; we
        // must not be holding it.
        let prune_report = self.prune()?;
        report.entities_pruned = prune_report.entities_removed;

        // Stamp the meta table so callers can surface "last consolidation"
        // through status later.
        let conn = self.lock_db();
        conn.execute(
            "INSERT OR REPLACE INTO memory_meta (key, value)
             VALUES ('last_consolidation', ?1)",
            params![now.to_string()],
        )?;
        drop(conn);

        Ok(report)
    }

    /// Pull all live observations grouped by entity. Used by the dedup
    /// phase. Each tuple is `(observation_id, content, access_count,
    /// observed_at)`.
    fn collect_dedup_candidates(&self) -> MemoryResult<DedupCandidates> {
        let conn = self.lock_db();
        let mut stmt = conn.prepare(
            "SELECT entity_id, id, content, access_count, observed_at
             FROM observations
             WHERE tombstoned = 0
             ORDER BY entity_id, observed_at DESC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out: DedupCandidates = HashMap::new();
        while let Some(row) = rows.next()? {
            let entity_id: String = row.get(0)?;
            let id: String = row.get(1)?;
            let content: String = row.get(2)?;
            let access_count: i64 = row.get(3)?;
            let observed_at: i64 = row.get(4)?;
            out.entry(entity_id).or_default().push((
                id,
                content,
                access_count.max(0) as u32,
                observed_at,
            ));
        }
        Ok(out)
    }

    /// Find observations whose decay-adjusted score falls below the prune
    /// floor and which are old enough to be eligible.
    fn collect_decay_prune_targets(
        &self,
        config: &ConsolidateConfig,
        now: i64,
    ) -> MemoryResult<Vec<String>> {
        let conn = self.lock_db();
        let mut stmt = conn.prepare(
            "SELECT id, observed_at, access_count, confidence, source
             FROM observations
             WHERE tombstoned = 0",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let observed_at: i64 = row.get(1)?;
            let access_count: i64 = row.get(2)?;
            let confidence: f32 = row.get(3)?;
            let source: String = row.get(4)?;

            if now - observed_at < config.min_age_secs {
                continue;
            }
            let score = decay_score(
                observed_at,
                access_count.max(0) as u32,
                confidence,
                &source,
                now,
                config.decay_rate,
            );
            if score < config.prune_floor {
                out.push(id);
            }
        }
        Ok(out)
    }
}

/// Shared text-similarity primitive: Jaccard on whitespace-tokenised
/// lowercased content. Cheap, no allocation per call beyond the two sets.
fn jaccard_similarity(a: &str, b: &str) -> f32 {
    let normalise = |s: &str| -> std::collections::HashSet<String> {
        s.to_lowercase()
            .split_whitespace()
            .map(|w| w.to_string())
            .collect()
    };
    let a_set = normalise(a);
    let b_set = normalise(b);
    if a_set.is_empty() && b_set.is_empty() {
        return 1.0;
    }
    if a_set.is_empty() || b_set.is_empty() {
        return 0.0;
    }
    let intersection = a_set.intersection(&b_set).count();
    let union = a_set.union(&b_set).count();
    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}

/// Decay-only score, no search-relevance term. Mirrors the recall formula
/// minus the raw_score multiplier so consolidate can rank observations on
/// their own without a query.
fn decay_score(
    observed_at: i64,
    access_count: u32,
    confidence: f32,
    source: &str,
    now: i64,
    lambda: f64,
) -> f32 {
    let days_since = ((now - observed_at).max(0)) as f64 / 86_400.0;
    let base_decay = (-lambda * days_since).exp() as f32;
    let retrieval_boost = 1.0_f32 + 0.15_f32 * (1.0_f32 + access_count as f32).ln();
    let correction_boost = if source == "correction" || source == "cortex:correction" {
        crate::recall::CORRECTION_RETRIEVAL_BOOST
    } else {
        1.0
    };
    base_decay * retrieval_boost * confidence.clamp(0.0, 1.0) * correction_boost
}

/// Within an entity, find groups of near-duplicate observations and pick a
/// survivor for each group. Iterates through observations in `observed_at`
/// DESC order (the input order), which means the freshest is preferred as
/// survivor — losers are older copies tombstoned by `consolidate`.
///
/// Returns `(survivor_id, loser_ids, loser_access_count_total)` per group.
/// `loser_access_count_total` is the sum of just the losers — the caller
/// adds it to the survivor's existing count, which is already in the DB.
fn dedup_within_entity(
    observations: &[DedupCandidateRow],
    threshold: f32,
) -> Vec<(String, Vec<String>, u32)> {
    let mut visited = vec![false; observations.len()];
    let mut groups: Vec<(String, Vec<String>, u32)> = Vec::new();

    for (i, (id, content, _access, _observed_at)) in observations.iter().enumerate() {
        if visited[i] {
            continue;
        }
        visited[i] = true;
        let mut loser_access_sum: u32 = 0;
        let mut losers: Vec<String> = Vec::new();
        for j in (i + 1)..observations.len() {
            if visited[j] {
                continue;
            }
            let (other_id, other_content, other_access, _) = &observations[j];
            if jaccard_similarity(content, other_content) >= threshold {
                visited[j] = true;
                loser_access_sum = loser_access_sum.saturating_add(*other_access);
                losers.push(other_id.clone());
            }
        }
        if !losers.is_empty() {
            groups.push((id.clone(), losers, loser_access_sum));
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remember::ObservationInput;
    use crate::types::EntityType;
    use openmemory_core::clock::{Clock, FixedClock};
    use openmemory_core::config::Config;
    use std::sync::Arc;

    fn open_with_clock() -> (MemoryStore, Arc<FixedClock>) {
        let clock = Arc::new(FixedClock::new(1_000_000));
        let store = MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_clock(clock.clone() as Arc<dyn Clock>);
        (store, clock)
    }

    #[test]
    fn jaccard_identical() {
        assert!((jaccard_similarity("hello world", "hello world") - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn jaccard_disjoint() {
        assert!(jaccard_similarity("hello world", "foo bar") < f32::EPSILON);
    }

    #[test]
    fn jaccard_partial() {
        let sim = jaccard_similarity("hello world foo", "hello world bar");
        assert!(sim > 0.3 && sim < 0.8);
    }

    #[test]
    fn jaccard_case_insensitive() {
        assert!((jaccard_similarity("Hello World", "hello world") - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn jaccard_empty_strings_treated_identical() {
        assert!((jaccard_similarity("", "") - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn dedup_merges_near_duplicates_within_entity() {
        let (store, _) = open_with_clock();
        let outcome = store
            .remember(
                "Raymond",
                EntityType::Person,
                &[
                    ObservationInput::new("prefers Rust over Python"),
                    ObservationInput::new("Prefers Rust over Python"),
                    ObservationInput::new("ships openmemory"),
                ],
                &[],
                "test",
            )
            .unwrap();

        let report = store
            .consolidate(&ConsolidateConfig::for_store(&store))
            .unwrap();
        assert_eq!(report.duplicates_merged, 1, "second copy should merge");
        // The survivor is the first observation in observed_at-DESC order;
        // both copies have the same observed_at so first inserted wins.
        let live = store
            .get_entity_observations(&store.get_entity("Raymond").unwrap().unwrap().id)
            .unwrap();
        assert_eq!(live.len(), 2);
        assert!(live.iter().any(|o| o.id == outcome.observation_ids[2]));
    }

    #[test]
    fn dedup_does_not_cross_entities() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "A",
                EntityType::Concept,
                &[ObservationInput::new("the same fact")],
                &[],
                "t",
            )
            .unwrap();
        store
            .remember(
                "B",
                EntityType::Concept,
                &[ObservationInput::new("the same fact")],
                &[],
                "t",
            )
            .unwrap();
        let report = store
            .consolidate(&ConsolidateConfig::for_store(&store))
            .unwrap();
        assert_eq!(report.duplicates_merged, 0);
    }

    #[test]
    fn consolidate_is_idempotent() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "X",
                EntityType::Fact,
                &[
                    ObservationInput::new("hello world"),
                    ObservationInput::new("hello world"),
                    ObservationInput::new("hello world"),
                ],
                &[],
                "t",
            )
            .unwrap();
        let cfg = ConsolidateConfig::for_store(&store);
        let first = store.consolidate(&cfg).unwrap();
        let second = store.consolidate(&cfg).unwrap();
        assert!(first.duplicates_merged > 0);
        assert_eq!(second.duplicates_merged, 0);
        assert_eq!(second.observations_pruned, 0);
    }

    #[test]
    fn decay_prune_does_not_touch_recent_observations() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("fresh fact")],
                &[],
                "t",
            )
            .unwrap();
        let mut cfg = ConsolidateConfig::for_store(&store);
        cfg.prune_floor = 1.0; // anything mortal is below 1.0 — but min_age guards
        let report = store.consolidate(&cfg).unwrap();
        assert_eq!(report.observations_pruned, 0);
    }

    #[test]
    fn decay_prune_reaps_low_score_observations() {
        let (store, clock) = open_with_clock();
        clock.set(0);
        store
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("ancient memo").with_confidence(0.1)],
                &[],
                "t",
            )
            .unwrap();
        // Jump 1000 days ahead.
        clock.set(1000 * 86_400);
        let mut cfg = ConsolidateConfig::for_store(&store);
        cfg.prune_floor = 0.5; // baseline confidence + decay should drop here
        let report = store.consolidate(&cfg).unwrap();
        assert_eq!(report.observations_pruned, 1);
    }

    #[test]
    fn consolidate_records_last_run_timestamp() {
        let (store, clock) = open_with_clock();
        clock.set(12_345);
        store.consolidate(&ConsolidateConfig::default()).unwrap();
        let conn = store.lock_db();
        let v: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = 'last_consolidation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, "12345");
    }

    #[test]
    fn dedup_rolls_access_count_into_survivor() {
        let (store, _) = open_with_clock();
        let outcome = store
            .remember(
                "X",
                EntityType::Fact,
                &[
                    ObservationInput::new("dup fact"),
                    ObservationInput::new("dup fact"),
                ],
                &[],
                "t",
            )
            .unwrap();
        // Bump access_count on the loser (older copy is kept since DESC
        // order — but actually both have same observed_at, so the survivor
        // is whichever appears first in the DESC ordering).
        {
            let conn = store.lock_db();
            conn.execute(
                "UPDATE observations SET access_count = 4 WHERE id IN (?1, ?2)",
                rusqlite::params![&outcome.observation_ids[0], &outcome.observation_ids[1]],
            )
            .unwrap();
        }
        let report = store
            .consolidate(&ConsolidateConfig::for_store(&store))
            .unwrap();
        assert_eq!(report.duplicates_merged, 1);

        // The single survivor should have access_count = 4 (its own) + 4
        // (rolled up from loser) = 8.
        let conn = store.lock_db();
        let max_access: i64 = conn
            .query_row(
                "SELECT MAX(access_count) FROM observations
                 WHERE tombstoned = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(max_access, 8);
    }

    #[test]
    fn config_from_openmemory_config_round_trip() {
        let mut cfg = Config::default();
        cfg.memory.dedup_threshold = 0.92;
        cfg.memory.prune_floor = 0.10;
        cfg.memory.decay_rate = 0.02;
        let cc = ConsolidateConfig::from_config(&cfg);
        assert!((cc.dedup_text_threshold - 0.92).abs() < f32::EPSILON);
        assert!((cc.prune_floor - 0.10).abs() < f32::EPSILON);
        assert!((cc.decay_rate - 0.02).abs() < f64::EPSILON);
    }
}
