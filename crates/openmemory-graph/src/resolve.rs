//! Resolving an entity *name* to the entity or entities that carry it.
//!
//! # Why this exists
//!
//! A name is not an identity. `01-contract-and-invariants.md` says so
//! ("Equal labels … create candidates only"), but until this module the
//! read path did not: `get_entity(name)` was
//! `SELECT … FROM entities WHERE name = ?1` through `query_row`, which
//! returns whichever row SQLite reaches first and **silently discards
//! the rest**.
//!
//! That is not hypothetical. The live profile store at
//! `~/.openmemory/data/default` carries `ProjectAlpha` twice — once as a
//! `concept` and once as a `project` — so one of those two entities has
//! been unreachable by name, and the caller had no way to tell that a
//! choice was made. `idx_entities_name_type` is UNIQUE over
//! `(name, entity_type)`, so today a name can collide only *across*
//! types; once that constraint is relaxed it collides within a type too
//! (see `plan/16-production-memory-spaces/11-entity-identity-migration.md`).
//!
//! # What this module guarantees
//!
//! - Ambiguity is **representable in the return type**. A caller cannot
//!   accidentally ignore it, because [`EntityResolution`] has no
//!   `Option`-shaped escape that hides the ambiguous case.
//! - When a surface must still return one entity, it collapses the set
//!   with a **stated rule** ([`EntityResolution::choose_oldest`]) and
//!   reports that it did. Never SQLite row order.
//! - The candidate set is **bounded** and reports truncation, so a
//!   caller can tell "these are all of them" from "these are some".
//!
//! # Ordering cost
//!
//! The query orders by `(created_at, id)` so the answer is stable across
//! runs, machines, and vacuum. While `idx_entities_name_type` is UNIQUE
//! a name matches at most one row per [`EntityType`] — nine rows — so
//! the sort is free. That stops being true when the uniqueness
//! constraint is relaxed: `idx_entities_name_type` is `(name,
//! entity_type)` and does not order by `id`, so `ORDER BY` would then
//! sort the whole homonym group. The migration document records the fix
//! (resolve candidate ids through a covering `(name, entity_id, …)`
//! directory index) and the measurement behind it. Do not lift this
//! query onto a relaxed schema without reading that first.

use rusqlite::params;

use crate::error::MemoryResult;
use crate::store::row_to_entity;
use crate::types::{Entity, EntityType};

/// Most entities a single name lookup will materialise.
///
/// Bounds the work a hot label can cause. Nine would suffice under
/// today's UNIQUE constraint (one row per [`EntityType`]); the headroom
/// is for the relaxed schema, where a genuinely popular name may be
/// carried by many entities.
pub const MAX_NAME_CANDIDATES: usize = 64;

/// The entities that carry one name, when more than one does.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityCandidates {
    name: String,
    entities: Vec<Entity>,
    truncated: bool,
}

impl EntityCandidates {
    /// The name that was ambiguous.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Every candidate, ordered by `(created_at, id)`. Always at least two.
    #[must_use]
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    /// True when more entities carry this name than [`MAX_NAME_CANDIDATES`]
    /// allowed, so `entities()` is a prefix rather than the whole set.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// `id (entity_type)` for each candidate — the form an error or a
    /// tool payload uses to tell a caller what to disambiguate between.
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        self.entities
            .iter()
            .map(|entity| format!("{} ({})", entity.id, entity.entity_type.as_str()))
            .collect()
    }

    /// A one-line rendering for an error message or a tool payload.
    #[must_use]
    pub fn describe(&self) -> String {
        let listed = self
            .entities
            .iter()
            .map(|entity| format!("{} ({})", entity.id, entity.entity_type.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        if self.truncated {
            format!(
                "{} or more entities are named {:?}: {listed}",
                self.entities.len(),
                self.name
            )
        } else {
            format!(
                "{} entities are named {:?}: {listed}",
                self.entities.len(),
                self.name
            )
        }
    }
}

/// What a name resolved to.
///
/// The three cases are exhaustive and a caller must handle each one.
/// That is the point: the previous `Option<Entity>` could not say
/// "several", so it said "one" and threw the rest away.
#[derive(Debug, Clone, PartialEq)]
pub enum EntityResolution {
    /// No entity carries this name.
    NotFound,
    /// Exactly one entity carries it.
    Unique(Entity),
    /// More than one does. The caller chooses; the store will not.
    Ambiguous(EntityCandidates),
}

impl EntityResolution {
    /// The entity when exactly one matched, `None` when none or several did.
    ///
    /// Use this where "several" genuinely has no answer. Where a surface
    /// must return something, use [`Self::choose_oldest`] so the choice
    /// is a stated rule rather than an accident.
    #[must_use]
    pub fn unique(self) -> Option<Entity> {
        match self {
            Self::Unique(entity) => Some(entity),
            Self::NotFound | Self::Ambiguous(_) => None,
        }
    }

    /// Borrow every matching entity: 0, 1, or many.
    #[must_use]
    pub fn candidates(&self) -> &[Entity] {
        match self {
            Self::NotFound => &[],
            Self::Unique(entity) => std::slice::from_ref(entity),
            Self::Ambiguous(candidates) => candidates.entities(),
        }
    }

    /// Number of entities carrying the name, capped at [`MAX_NAME_CANDIDATES`].
    #[must_use]
    pub fn candidate_count(&self) -> usize {
        self.candidates().len()
    }

    #[must_use]
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous(_))
    }

    /// The ambiguity, when there was one.
    #[must_use]
    pub fn ambiguity(&self) -> Option<&EntityCandidates> {
        match self {
            Self::Ambiguous(candidates) => Some(candidates),
            Self::NotFound | Self::Unique(_) => None,
        }
    }

    /// Collapse to one entity by an explicit rule: **oldest `created_at`,
    /// ties broken by ascending `id`**.
    ///
    /// Returns the chosen entity and, when the name was ambiguous, the
    /// full candidate set — so a caller with a single-entity contract can
    /// answer *and* tell the user that a choice was made. A caller that
    /// discards the second element is choosing to hide that, which is the
    /// behaviour this module exists to remove; it should be a deliberate
    /// decision with a comment, not a default.
    #[must_use]
    pub fn choose_oldest(self) -> Option<(Entity, Option<EntityCandidates>)> {
        match self {
            Self::NotFound => None,
            Self::Unique(entity) => Some((entity, None)),
            Self::Ambiguous(candidates) => {
                // `entities` is already ordered by (created_at, id), so
                // the first element is the oldest under that rule.
                let chosen = candidates.entities.first().cloned()?;
                Some((chosen, Some(candidates)))
            }
        }
    }
}

/// Build a resolution from rows already ordered by `(created_at, id)`.
fn from_ordered_rows(name: &str, mut rows: Vec<Entity>, truncated: bool) -> EntityResolution {
    match rows.len() {
        0 => EntityResolution::NotFound,
        1 if !truncated => EntityResolution::Unique(rows.remove(0)),
        _ => EntityResolution::Ambiguous(EntityCandidates {
            name: name.to_owned(),
            entities: rows,
            truncated,
        }),
    }
}

const SELECT_ENTITY_COLUMNS: &str =
    "SELECT id, name, entity_type, created_at, updated_at, confidence, source FROM entities";

impl crate::store::MemoryStore {
    /// Resolve `name` to the entity or entities carrying it.
    ///
    /// This is the honest replacement for the old
    /// `get_entity(name) -> Option<Entity>`, which returned an arbitrary
    /// row when a name was shared. See the module documentation for the
    /// live-store case that motivated it.
    pub fn resolve_entity(&self, name: &str) -> MemoryResult<EntityResolution> {
        let limit = MAX_NAME_CANDIDATES as i64 + 1;
        self.with_reader(|conn| {
            let sql =
                format!("{SELECT_ENTITY_COLUMNS} WHERE name = ?1 ORDER BY created_at, id LIMIT ?2");
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = Vec::new();
            let mut cursor = stmt.query(params![name, limit])?;
            while let Some(row) = cursor.next()? {
                rows.push(row_to_entity(row)?);
            }
            let truncated = rows.len() > MAX_NAME_CANDIDATES;
            rows.truncate(MAX_NAME_CANDIDATES);
            Ok(from_ordered_rows(name, rows, truncated))
        })
    }

    /// Resolve `(name, entity_type)`.
    ///
    /// Under today's UNIQUE `(name, entity_type)` index this can only be
    /// `NotFound` or `Unique`. It still returns [`EntityResolution`] so
    /// that callers are already correct when that constraint is relaxed,
    /// rather than needing a second sweep then.
    pub fn resolve_entity_by_name_and_type(
        &self,
        name: &str,
        entity_type: EntityType,
    ) -> MemoryResult<EntityResolution> {
        let limit = MAX_NAME_CANDIDATES as i64 + 1;
        self.with_reader(|conn| {
            let sql = format!(
                "{SELECT_ENTITY_COLUMNS} WHERE name = ?1 AND entity_type = ?2 \
                 ORDER BY created_at, id LIMIT ?3"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = Vec::new();
            let mut cursor = stmt.query(params![name, entity_type.as_str(), limit])?;
            while let Some(row) = cursor.next()? {
                rows.push(row_to_entity(row)?);
            }
            let truncated = rows.len() > MAX_NAME_CANDIDATES;
            rows.truncate(MAX_NAME_CANDIDATES);
            Ok(from_ordered_rows(name, rows, truncated))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(id: &str, name: &str, entity_type: EntityType, created_at: i64) -> Entity {
        Entity {
            id: id.to_owned(),
            name: name.to_owned(),
            entity_type,
            created_at,
            updated_at: created_at,
            confidence: 1.0,
            source: String::new(),
        }
    }

    #[test]
    fn no_rows_is_not_found() {
        let resolution = from_ordered_rows("X", Vec::new(), false);
        assert_eq!(resolution, EntityResolution::NotFound);
        assert_eq!(resolution.candidate_count(), 0);
        assert!(!resolution.is_ambiguous());
        assert!(resolution.unique().is_none());
    }

    #[test]
    fn one_row_is_unique() {
        let row = entity("a", "X", EntityType::Person, 1);
        let resolution = from_ordered_rows("X", vec![row.clone()], false);
        assert!(!resolution.is_ambiguous());
        assert_eq!(resolution.candidate_count(), 1);
        assert_eq!(resolution.unique(), Some(row));
    }

    #[test]
    fn two_rows_are_ambiguous_and_unique_refuses_to_guess() {
        let first = entity("a", "X", EntityType::Concept, 1);
        let second = entity("b", "X", EntityType::Project, 2);
        let resolution = from_ordered_rows("X", vec![first, second], false);
        assert!(resolution.is_ambiguous());
        assert_eq!(resolution.candidate_count(), 2);
        assert!(
            resolution.clone().unique().is_none(),
            "unique() must not silently pick one"
        );
    }

    #[test]
    fn choose_oldest_states_its_rule_and_reports_the_alternatives() {
        let oldest = entity("z", "X", EntityType::Concept, 1);
        let newer = entity("a", "X", EntityType::Project, 5);
        // Ordered by (created_at, id): the older row sorts first even
        // though its id sorts last.
        let resolution = from_ordered_rows("X", vec![oldest.clone(), newer], false);
        let (chosen, ambiguity) = resolution.choose_oldest().expect("a candidate exists");
        assert_eq!(chosen, oldest);
        let ambiguity = ambiguity.expect("the alternatives must be reported");
        assert_eq!(ambiguity.entities().len(), 2);
        assert!(ambiguity.describe().contains("2 entities are named"));
    }

    #[test]
    fn choose_oldest_on_a_unique_name_reports_no_ambiguity() {
        let only = entity("a", "X", EntityType::Person, 1);
        let (chosen, ambiguity) = from_ordered_rows("X", vec![only.clone()], false)
            .choose_oldest()
            .expect("a candidate exists");
        assert_eq!(chosen, only);
        assert!(ambiguity.is_none());
    }

    #[test]
    fn truncation_is_visible_and_never_reported_as_unique() {
        let rows = vec![entity("a", "X", EntityType::Person, 1)];
        let resolution = from_ordered_rows("X", rows, true);
        assert!(
            resolution.is_ambiguous(),
            "a truncated set is not a unique match"
        );
        let ambiguity = resolution.ambiguity().expect("ambiguous");
        assert!(ambiguity.truncated());
        assert!(ambiguity.describe().contains("or more entities"));
    }
}
