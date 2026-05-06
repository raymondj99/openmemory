//! `forget`, `forget_entity`, and `prune` — the destructive surface.
//!
//! Three calls, with progressively wider blast radius:
//!
//! - [`MemoryStore::forget`] — soft-delete a single observation. Sets
//!   `tombstoned = 1`; the row stays around for audit and lets `prune`
//!   reclaim it later.
//! - [`MemoryStore::forget_entity`] — hard-delete an entity. CASCADE
//!   removes its observations and relations. Returns the number of
//!   observations purged.
//! - [`MemoryStore::prune`] — sweep tombstoned observations older than the
//!   configured TTL, plus orphaned entities (no live observations, no
//!   live relations). Returns a [`PruneReport`].
//!
//! The search index is kept in lockstep: every observation that disappears
//! from SQLite is removed from the hybrid engine via `delete_by_uri`. Search
//! deletes are best-effort — a transient FTS5 hiccup logs a warning and
//! leaves SQLite as the source of truth.

use rusqlite::params;

use crate::error::{MemoryError, MemoryResult};
use crate::store::MemoryStore;

/// Default TTL for tombstoned observations: 14 days. Caller can override
/// per-call via [`MemoryStore::prune_with_ttl`].
pub const DEFAULT_TOMBSTONE_TTL_SECS: i64 = 14 * 24 * 60 * 60;

/// Counts of what `prune` removed in one pass. Numbers are mutually
/// exclusive — `tombstones_removed` is observations swept; `entities_removed`
/// is entities that became orphaned after the sweep.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub tombstones_removed: usize,
    pub entities_removed: usize,
}

impl MemoryStore {
    /// Soft-delete observation `id`. Idempotent: a no-op if the observation
    /// is already tombstoned. Returns `true` if a row was modified.
    pub fn forget(&self, observation_id: &str) -> MemoryResult<bool> {
        let _guard = self.write_rebuild();

        let conn = self.lock_db();
        let now = self.clock().now_secs();
        let updated = conn.execute(
            "UPDATE observations
             SET tombstoned = 1, valid_until = COALESCE(valid_until, ?1)
             WHERE id = ?2 AND tombstoned = 0",
            params![now, observation_id],
        )?;
        drop(conn);

        if updated > 0 {
            // Drop the search-index entry so recall stops returning it. If
            // the index is briefly out of sync, the recall path's tombstone
            // filter still excludes it.
            let uri = format!("memory://observation/{observation_id}");
            if let Err(e) = self.engine().engine.delete_by_uri(&uri) {
                tracing::warn!(
                    target: "openmemory_graph::forget",
                    error = %e,
                    observation_id,
                    "search-index delete failed; SQLite tombstone remains authoritative"
                );
            }
        }
        Ok(updated > 0)
    }

    /// Hard-delete an entity by name. CASCADE removes observations and
    /// relations. Returns the number of observations purged.
    ///
    /// Returns [`MemoryError::EntityNotFound`] when the name doesn't match
    /// any entity. Pair with `get_entity_by_name_and_type` upstream when
    /// the name is ambiguous across types — this method deletes the first
    /// match.
    pub fn forget_entity(&self, name: &str) -> MemoryResult<usize> {
        if name.trim().is_empty() {
            return Err(MemoryError::InvalidInput(
                "entity name must not be empty".into(),
            ));
        }
        let _guard = self.write_rebuild();

        let conn = self.lock_db();
        let entity_id: String = match conn.query_row(
            "SELECT id FROM entities WHERE name = ?1",
            params![name],
            |row| row.get(0),
        ) {
            Ok(id) => id,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(MemoryError::EntityNotFound(name.to_string()));
            }
            Err(e) => return Err(e.into()),
        };

        let observation_ids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT id FROM observations WHERE entity_id = ?1")?;
            let mut rows = stmt.query(params![entity_id])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                out.push(r.get::<_, String>(0)?);
            }
            out
        };

        conn.execute("DELETE FROM entities WHERE id = ?1", params![entity_id])?;
        drop(conn);

        // Hybrid-engine cleanup (best-effort).
        for obs_id in &observation_ids {
            let uri = format!("memory://observation/{obs_id}");
            let _ = self.engine().engine.delete_by_uri(&uri);
        }

        Ok(observation_ids.len())
    }

    /// Sweep tombstoned observations older than the default TTL plus any
    /// entities that are now orphaned. See [`Self::prune_with_ttl`] for the
    /// configurable form.
    pub fn prune(&self) -> MemoryResult<PruneReport> {
        self.prune_with_ttl(DEFAULT_TOMBSTONE_TTL_SECS)
    }

    /// As [`prune`](Self::prune) but with a caller-supplied tombstone TTL
    /// in seconds. A `ttl_secs` of 0 sweeps every tombstoned observation;
    /// a negative `ttl_secs` is clamped to 0.
    pub fn prune_with_ttl(&self, ttl_secs: i64) -> MemoryResult<PruneReport> {
        let _guard = self.write_rebuild();

        let now = self.clock().now_secs();
        let cutoff = now - ttl_secs.max(0);

        let mut conn = self.lock_db();
        let tx = conn.transaction()?;

        // Phase 1: collect tombstone IDs eligible for hard delete.
        let stale_ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM observations
                 WHERE tombstoned = 1
                    AND COALESCE(valid_until, observed_at) <= ?1",
            )?;
            let mut rows = stmt.query(params![cutoff])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                out.push(r.get::<_, String>(0)?);
            }
            out
        };

        for id in &stale_ids {
            tx.execute("DELETE FROM observations WHERE id = ?1", params![id])?;
        }

        // Phase 2: collect orphaned entities — no observations at all
        // (tombstoned ones still count as keeping the entity alive so phase
        // 1 has a chance to sweep them first) and no live relations.
        let orphan_ids: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT e.id, e.name FROM entities e
                 WHERE NOT EXISTS (
                    SELECT 1 FROM observations o
                    WHERE o.entity_id = e.id
                 )
                 AND NOT EXISTS (
                    SELECT 1 FROM relations r
                    WHERE (r.from_entity = e.id OR r.to_entity = e.id)
                       AND (r.valid_until IS NULL OR r.valid_until > ?1)
                 )",
            )?;
            let mut rows = stmt.query(params![now])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                out.push((r.get::<_, String>(0)?, r.get::<_, String>(1)?));
            }
            out
        };

        for (id, _name) in &orphan_ids {
            tx.execute("DELETE FROM entities WHERE id = ?1", params![id])?;
        }

        tx.commit()?;
        drop(conn);

        // Sync the search engine with both kinds of removed observation
        // (tombstones swept + observations cascaded by orphan-entity
        // deletes). The cascade happens in SQLite via FK; we only have the
        // IDs of the directly-removed tombstones, but the search engine
        // already filters tombstoned URIs at recall time, and a future
        // rebuild_if_stale call will catch up the rest.
        for id in &stale_ids {
            let uri = format!("memory://observation/{id}");
            let _ = self.engine().engine.delete_by_uri(&uri);
        }

        Ok(PruneReport {
            tombstones_removed: stale_ids.len(),
            entities_removed: orphan_ids.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remember::{ObservationInput, RelationInput};
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
    fn forget_marks_tombstoned() {
        let (store, _) = open_with_clock();
        let outcome = store
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("alpha")],
                &[],
                "t",
            )
            .unwrap();
        let id = &outcome.observation_ids[0];
        assert!(store.forget(id).unwrap());

        let conn = store.lock_db();
        let row: (i64,) = conn
            .query_row(
                "SELECT tombstoned FROM observations WHERE id = ?1",
                [id],
                |r| Ok((r.get::<_, i64>(0)?,)),
            )
            .unwrap();
        assert_eq!(row.0, 1);
    }

    #[test]
    fn forget_is_idempotent() {
        let (store, _) = open_with_clock();
        let outcome = store
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("alpha")],
                &[],
                "t",
            )
            .unwrap();
        assert!(store.forget(&outcome.observation_ids[0]).unwrap());
        assert!(!store.forget(&outcome.observation_ids[0]).unwrap());
    }

    #[test]
    fn forget_unknown_id_returns_false() {
        let (store, _) = open_with_clock();
        assert!(!store.forget("does-not-exist").unwrap());
    }

    #[test]
    fn forget_excludes_observation_from_recall() {
        let (store, _) = open_with_clock();
        let outcome = store
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("alpha mention")],
                &[],
                "t",
            )
            .unwrap();
        store.forget(&outcome.observation_ids[0]).unwrap();

        let mut filters = crate::recall::RecallFilters::new();
        filters.mode = Some(openmemory_index::SearchMode::KeywordOnly);
        let r = store.recall("alpha", 5, &filters).unwrap();
        assert!(r.is_empty(), "tombstoned observations should not surface");
    }

    #[test]
    fn forget_entity_cascades_observations_and_relations() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "Raymond",
                EntityType::Person,
                &[ObservationInput::new("a"), ObservationInput::new("b")],
                &[RelationInput::new("uses", "Sift", EntityType::Project)],
                "t",
            )
            .unwrap();

        let removed = store.forget_entity("Raymond").unwrap();
        assert_eq!(removed, 2);

        let s = store.status().unwrap();
        // Raymond's observations + the relation are cascaded; "Sift" stays
        // (it has no observations of its own and no relations after the
        // cascade — `prune` will collect it next).
        assert_eq!(s.total_observations, 0);
        assert_eq!(s.total_relations, 0);
        assert_eq!(s.total_entities, 1, "Sift remains until prune sweeps");
    }

    #[test]
    fn forget_entity_unknown_returns_not_found() {
        let (store, _) = open_with_clock();
        let err = store.forget_entity("missing").unwrap_err();
        assert!(matches!(err, MemoryError::EntityNotFound(_)));
    }

    #[test]
    fn forget_entity_rejects_empty_name() {
        let (store, _) = open_with_clock();
        let err = store.forget_entity("").unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));
    }

    #[test]
    fn prune_respects_tombstone_ttl() {
        let (store, clock) = open_with_clock();
        clock.set(0);
        let outcome = store
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("alpha")],
                &[],
                "t",
            )
            .unwrap();
        store.forget(&outcome.observation_ids[0]).unwrap();

        clock.set(100); // 100s after the tombstone
        let report = store.prune_with_ttl(86_400).unwrap();
        assert_eq!(report.tombstones_removed, 0, "TTL not yet elapsed");

        clock.set(1_000_000);
        let report = store.prune_with_ttl(86_400).unwrap();
        assert_eq!(report.tombstones_removed, 1);
    }

    #[test]
    fn prune_zero_ttl_sweeps_immediately() {
        let (store, _) = open_with_clock();
        let outcome = store
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("alpha")],
                &[],
                "t",
            )
            .unwrap();
        store.forget(&outcome.observation_ids[0]).unwrap();

        let report = store.prune_with_ttl(0).unwrap();
        assert_eq!(report.tombstones_removed, 1);
        let s = store.status().unwrap();
        assert_eq!(s.tombstoned_observations, 0);
    }

    #[test]
    fn prune_collects_orphaned_entities() {
        let (store, _) = open_with_clock();
        store
            .remember("Lonely", EntityType::Concept, &[], &[], "t")
            .unwrap();
        let report = store.prune().unwrap();
        assert_eq!(report.entities_removed, 1);
        assert_eq!(store.status().unwrap().total_entities, 0);
    }

    #[test]
    fn prune_keeps_entity_with_live_observation() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "Live",
                EntityType::Concept,
                &[ObservationInput::new("kept")],
                &[],
                "t",
            )
            .unwrap();
        store.prune().unwrap();
        assert_eq!(store.status().unwrap().total_entities, 1);
    }

    #[test]
    fn prune_keeps_entity_with_live_relation() {
        let (store, _) = open_with_clock();
        store
            .remember(
                "A",
                EntityType::Concept,
                &[],
                &[RelationInput::new("links", "B", EntityType::Concept)],
                "t",
            )
            .unwrap();
        store.prune().unwrap();
        assert_eq!(store.status().unwrap().total_entities, 2);
    }
}
