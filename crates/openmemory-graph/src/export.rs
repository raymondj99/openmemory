//! Raw export/import for domain-count migration.
//!
//! Every read API on [`MemoryStore`] filters: `get_entity_observations`
//! drops tombstoned and expired rows, `get_entity_relations` drops
//! expired edges, `list_entities` paginates and joins. A migration must
//! move EVERYTHING byte-exactly — tombstones are pending prune-TTL
//! audit state, `access_count` feeds the retrieval boost, bi-temporal
//! validity is knowledge — so this module exposes unfiltered row dumps
//! and a write path that preserves them.
//!
//! `import_raw` is the deliberate opposite of `remember`/`remember_batch`:
//! no id minting, no timestamp stamping, no entity-name normalization,
//! no search-index sync. Rows land exactly as exported, in one
//! transaction; the caller rebuilds the search index from its own
//! exported entries (vectors carry over by URI, so nothing is
//! re-embedded).

use rusqlite::params;

use crate::error::MemoryResult;
use crate::store::{row_to_observation, MemoryStore};
use crate::types::{Entity, Observation, Relation};

impl MemoryStore {
    /// Every `entities` row, unfiltered, in insertion-stable id order.
    pub fn export_entities_raw(&self) -> MemoryResult<Vec<Entity>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, entity_type, created_at, updated_at, confidence, source
                 FROM entities ORDER BY id",
            )?;
            let mut out = Vec::new();
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                out.push(crate::store::row_to_entity(row)?);
            }
            Ok(out)
        })
    }

    /// Every `observations` row — including tombstoned and expired ones —
    /// with the `concepts` / `source_files` side tables populated.
    pub fn export_observations_raw(&self) -> MemoryResult<Vec<Observation>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, entity_id, content, observed_at, valid_from, valid_until,
                        confidence, source, tombstoned, access_count, memory_tier,
                        title, summary, importance, source_kind
                 FROM observations ORDER BY id",
            )?;
            let mut out = Vec::new();
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                out.push(row_to_observation(row)?);
            }

            // Populate side tables in one query each, batched over every
            // observation id (chunked to stay under SQLite's bound-
            // parameter limit).
            const CHUNK: usize = 512;
            let ids: Vec<&str> = out.iter().map(|o| o.id.as_str()).collect();
            let mut concepts = std::collections::HashMap::new();
            let mut files = std::collections::HashMap::new();
            for chunk in ids.chunks(CHUNK) {
                concepts.extend(crate::store::load_observation_side_table(
                    conn,
                    "SELECT observation_id, concept FROM observation_concepts WHERE observation_id IN",
                    chunk,
                )?);
                files.extend(crate::store::load_observation_side_table(
                    conn,
                    "SELECT observation_id, file_path FROM observation_source_files WHERE observation_id IN",
                    chunk,
                )?);
            }
            for obs in &mut out {
                if let Some(v) = concepts.remove(&obs.id) {
                    obs.concepts = v;
                }
                if let Some(v) = files.remove(&obs.id) {
                    obs.source_files = v;
                }
            }
            Ok(out)
        })
    }

    /// Every `relations` row, unfiltered (expired edges included).
    pub fn export_relations_raw(&self) -> MemoryResult<Vec<Relation>> {
        self.with_reader(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, from_entity, to_entity, relation_type, weight, created_at,
                        valid_from, valid_until, source
                 FROM relations ORDER BY id",
            )?;
            let mut out = Vec::new();
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                out.push(crate::store::row_to_relation(row)?);
            }
            Ok(out)
        })
    }

    /// Insert exported rows verbatim, in one transaction.
    ///
    /// Preserves every field: ids, timestamps, tombstone flags, access
    /// counts, tiers, bi-temporal validity, and the v2 fielded columns
    /// plus side tables. Rows are inserted in foreign-key order
    /// (entities, then observations, then relations); a constraint
    /// violation (duplicate id, dangling reference) rolls the whole
    /// import back. The search index is intentionally NOT touched — the
    /// migration rebuilds it from exported index entries.
    pub fn import_raw(
        &self,
        entities: &[Entity],
        observations: &[Observation],
        relations: &[Relation],
    ) -> MemoryResult<()> {
        let _guard = self.write_rebuild();
        let mut conn = self.lock_db();
        let tx = conn.transaction()?;

        for entity in entities {
            tx.execute(
                "INSERT INTO entities
                    (id, name, entity_type, created_at, updated_at, confidence, source)
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
        }

        for obs in observations {
            tx.execute(
                "INSERT INTO observations
                    (id, entity_id, content, observed_at, valid_from, valid_until,
                     confidence, source, tombstoned, access_count, memory_tier,
                     title, summary, importance, source_kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    obs.id,
                    obs.entity_id,
                    obs.content,
                    obs.observed_at,
                    obs.valid_from,
                    obs.valid_until,
                    obs.confidence,
                    obs.source,
                    i64::from(obs.tombstoned),
                    i64::from(obs.access_count),
                    obs.memory_tier.as_str(),
                    obs.title,
                    obs.summary,
                    obs.importance,
                    obs.source_kind,
                ],
            )?;
            for concept in &obs.concepts {
                tx.execute(
                    "INSERT OR IGNORE INTO observation_concepts (observation_id, concept)
                     VALUES (?1, ?2)",
                    params![obs.id, concept],
                )?;
            }
            for file in &obs.source_files {
                tx.execute(
                    "INSERT OR IGNORE INTO observation_source_files (observation_id, file_path)
                     VALUES (?1, ?2)",
                    params![obs.id, file],
                )?;
            }
        }

        for rel in relations {
            tx.execute(
                "INSERT INTO relations
                    (id, from_entity, to_entity, relation_type, weight, created_at,
                     valid_from, valid_until, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    rel.id,
                    rel.from_entity,
                    rel.to_entity,
                    rel.relation_type,
                    rel.weight,
                    rel.created_at,
                    rel.valid_from,
                    rel.valid_until,
                    rel.source,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::remember::{ObservationInput, RelationInput};
    use crate::store::MemoryStore;
    use crate::types::EntityType;
    use openmemory_core::clock::{Clock, FixedClock};
    use openmemory_core::config::Config;

    fn open_at(now: i64) -> MemoryStore {
        MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_clock(Arc::new(FixedClock::new(now)) as Arc<dyn Clock>)
    }

    /// Build a store exercising every preservation-sensitive feature:
    /// tombstones, expired validity, access counts, tiers, v2 fields,
    /// side tables, expired relations.
    fn seeded() -> MemoryStore {
        let store = open_at(1_000);
        let outcome = store
            .remember(
                "alpha",
                EntityType::Project,
                &[
                    ObservationInput::new("kept fact")
                        .with_title("a title")
                        .with_summary("a summary")
                        .with_importance(0.7)
                        .with_source_kind("note")
                        .with_concepts(vec!["x".into(), "y".into()])
                        .with_source_files(vec!["docs/a.md".into()]),
                    ObservationInput::new("tombstoned fact"),
                ],
                &[RelationInput::new("uses", "bravo", EntityType::Tool)],
                "seed",
            )
            .unwrap();
        store.forget(&outcome.observation_ids[1]).unwrap();
        store
            .set_observation_memory_tier(
                &outcome.observation_ids[0],
                crate::types::MemoryTier::Semantic,
            )
            .unwrap();
        store
    }

    #[test]
    fn export_raw_includes_tombstoned_rows() {
        let store = seeded();
        let observations = store.export_observations_raw().unwrap();
        assert_eq!(observations.len(), 2, "tombstoned row must be exported");
        let tombstoned = observations.iter().find(|o| o.tombstoned).unwrap();
        assert_eq!(tombstoned.content, "tombstoned fact");
        let kept = observations.iter().find(|o| !o.tombstoned).unwrap();
        assert_eq!(kept.concepts.len(), 2);
        assert_eq!(kept.source_files, vec!["docs/a.md".to_string()]);
        assert_eq!(kept.title.as_deref(), Some("a title"));
        assert_eq!(kept.memory_tier, crate::types::MemoryTier::Semantic);
    }

    #[test]
    fn import_raw_round_trips_byte_exactly() {
        let source = seeded();
        let entities = source.export_entities_raw().unwrap();
        let observations = source.export_observations_raw().unwrap();
        let relations = source.export_relations_raw().unwrap();
        assert_eq!(entities.len(), 2, "alpha + bravo");
        assert_eq!(relations.len(), 1);

        let target = open_at(9_999);
        target
            .import_raw(&entities, &observations, &relations)
            .unwrap();

        // Re-export from the target: every row identical, ids and
        // timestamps included. Sort-stable order makes Vec equality
        // meaningful.
        assert_eq!(target.export_entities_raw().unwrap(), entities);
        assert_eq!(target.export_observations_raw().unwrap(), observations);
        assert_eq!(target.export_relations_raw().unwrap(), relations);

        // The import did not stamp the target's clock anywhere.
        let alpha = target.get_entity("alpha").unwrap().unwrap();
        assert_eq!(alpha.created_at, 1_000);
        assert_eq!(alpha.updated_at, 1_000);
    }

    #[test]
    fn import_raw_rolls_back_on_constraint_violation() {
        let source = seeded();
        let entities = source.export_entities_raw().unwrap();
        let observations = source.export_observations_raw().unwrap();

        let target = open_at(0);
        // Dangling entity reference: every row must roll back.
        let mut bad = observations.clone();
        bad[0].entity_id = "no-such-entity".into();
        let err = target.import_raw(&entities, &bad, &[]).unwrap_err();
        drop(err);
        assert_eq!(target.export_entities_raw().unwrap().len(), 0);
        assert_eq!(target.export_observations_raw().unwrap().len(), 0);
    }
}
