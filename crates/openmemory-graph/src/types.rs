//! Core data types for the knowledge graph.
//!
//! Three primary entities:
//!
//! - [`Entity`] — a stable, named concept (a person, project, fact, …).
//! - [`Observation`] — an atomic, bi-temporally validated fact about an
//!   entity. The `observed_at` timestamp records when the agent learned the
//!   fact; `valid_from` / `valid_until` record when the fact was true in the
//!   real world.
//! - [`Relation`] — a directed edge between two entities.
//!
//! All identifiers are UUIDv7 strings, minted via [`new_id`] which wraps
//! [`uuid::Uuid::now_v7`]. The version-7 layout encodes a millisecond Unix
//! timestamp in the high bits, so IDs sort lexicographically by creation
//! time — a property the graph store relies on for stable ordering.

use serde::{Deserialize, Serialize};

/// Mint a fresh UUIDv7 string. Sortable by creation time.
#[must_use]
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

/// A named concept in the graph. Names are unique (case-insensitive); the
/// store enforces a single entity per `(name, entity_type)` pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub entity_type: EntityType,
    pub created_at: i64,
    pub updated_at: i64,
    pub confidence: f32,
    pub source: String,
}

impl Entity {
    /// Construct a new entity with default confidence (1.0) and an empty
    /// source string.
    #[must_use]
    pub fn new(name: impl Into<String>, entity_type: EntityType, now_secs: i64) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            entity_type,
            created_at: now_secs,
            updated_at: now_secs,
            confidence: 1.0,
            source: String::new(),
        }
    }
}

/// Classification for entities. Maps 1:1 to the values used by sift's
/// `sift_remember` MCP tool so memory imported from sift round-trips cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Person,
    Project,
    Concept,
    Tool,
    Preference,
    Fact,
    Event,
    Location,
    Organization,
}

impl EntityType {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Project => "project",
            Self::Concept => "concept",
            Self::Tool => "tool",
            Self::Preference => "preference",
            Self::Fact => "fact",
            Self::Event => "event",
            Self::Location => "location",
            Self::Organization => "organization",
        }
    }

    /// Parse a lowercase entity-type string. Unknown variants return
    /// `None` so callers can fall back to a default.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "person" => Self::Person,
            "project" => Self::Project,
            "concept" => Self::Concept,
            "tool" => Self::Tool,
            "preference" => Self::Preference,
            "fact" => Self::Fact,
            "event" => Self::Event,
            "location" => Self::Location,
            "organization" => Self::Organization,
            _ => return None,
        })
    }

    /// Iterate over every entity-type variant in declaration order.
    pub fn all() -> [Self; 9] {
        [
            Self::Person,
            Self::Project,
            Self::Concept,
            Self::Tool,
            Self::Preference,
            Self::Fact,
            Self::Event,
            Self::Location,
            Self::Organization,
        ]
    }
}

impl std::fmt::Display for EntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Observation
// ---------------------------------------------------------------------------

/// An atomic fact about an entity. Bi-temporal: `observed_at` is when the
/// agent learned the fact; `valid_from` / `valid_until` bound when the fact
/// is true in the real world. A `valid_until` of `None` means "still valid".
///
/// `tombstoned` is the soft-delete marker. `forget` flips this to `true`;
/// `prune` later sweeps tombstoned rows older than the configured TTL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub id: String,
    pub entity_id: String,
    pub content: String,
    pub observed_at: i64,
    #[serde(default)]
    pub valid_from: Option<i64>,
    #[serde(default)]
    pub valid_until: Option<i64>,
    pub confidence: f32,
    pub source: String,
    #[serde(default)]
    pub tombstoned: bool,
    #[serde(default)]
    pub access_count: u32,
    /// Consolidation stage. Defaults to [`MemoryTier::Episodic`]; the
    /// column exists since schema v1 but the in-memory `Observation`
    /// only learned to surface it in v0.3.
    #[serde(default = "MemoryTier::default_episodic")]
    pub memory_tier: MemoryTier,
    /// Caller-supplied title (very-high-weight indexing field). v2.
    #[serde(default)]
    pub title: Option<String>,
    /// Caller-supplied short summary. v2.
    #[serde(default)]
    pub summary: Option<String>,
    /// Caller-supplied importance in `[0.0, 1.0]`. Used as a ranking
    /// prior, never indexed as text. v2.
    #[serde(default)]
    pub importance: Option<f32>,
    /// Free-form source-kind discriminator. v2.
    #[serde(default)]
    pub source_kind: Option<String>,
    /// Caller-supplied concept tags. Indexed via the
    /// `observation_concepts` side table. v2.
    #[serde(default)]
    pub concepts: Vec<String>,
    /// Caller-supplied source-file paths. Indexed via the
    /// `observation_source_files` side table. v2.
    #[serde(default)]
    pub source_files: Vec<String>,
}

impl Observation {
    /// Construct a new observation valid from `now_secs` indefinitely.
    #[must_use]
    pub fn new(entity_id: impl Into<String>, content: impl Into<String>, now_secs: i64) -> Self {
        Self {
            id: new_id(),
            entity_id: entity_id.into(),
            content: content.into(),
            observed_at: now_secs,
            valid_from: Some(now_secs),
            valid_until: None,
            confidence: 1.0,
            source: String::new(),
            tombstoned: false,
            access_count: 0,
            memory_tier: MemoryTier::Episodic,
            title: None,
            summary: None,
            importance: None,
            source_kind: None,
            concepts: Vec::new(),
            source_files: Vec::new(),
        }
    }

    /// Whether this observation is valid at `at_secs`. An observation with
    /// `valid_from = None` is treated as valid since `observed_at`.
    #[must_use]
    pub fn is_valid_at(&self, at_secs: i64) -> bool {
        let from = self.valid_from.unwrap_or(self.observed_at);
        let after_start = at_secs >= from;
        let before_end = self.valid_until.is_none_or(|end| at_secs < end);
        after_start && before_end && !self.tombstoned
    }
}

// ---------------------------------------------------------------------------
// Relation
// ---------------------------------------------------------------------------

/// A directed edge between two entities. `relation_type` is a free-form
/// active-voice verb phrase (e.g. `"maintains"`, `"prefers"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    pub id: String,
    pub from_entity: String,
    pub to_entity: String,
    pub relation_type: String,
    pub weight: f32,
    pub created_at: i64,
    #[serde(default)]
    pub valid_from: Option<i64>,
    #[serde(default)]
    pub valid_until: Option<i64>,
    pub source: String,
}

impl Relation {
    /// Construct a new relation valid from `now_secs` indefinitely with
    /// default weight 1.0.
    #[must_use]
    pub fn new(
        from_entity: impl Into<String>,
        to_entity: impl Into<String>,
        relation_type: impl Into<String>,
        now_secs: i64,
    ) -> Self {
        Self {
            id: new_id(),
            from_entity: from_entity.into(),
            to_entity: to_entity.into(),
            relation_type: relation_type.into(),
            weight: 1.0,
            created_at: now_secs,
            valid_from: Some(now_secs),
            valid_until: None,
            source: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryTier
// ---------------------------------------------------------------------------

/// Consolidation stage of an observation. Reserved for v0.2 but encoded in
/// the schema today so an episodic-only v0.1 store upgrades cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    Episodic,
    Semantic,
    Procedural,
}

impl MemoryTier {
    /// Helper used by serde's `default` attribute on `Observation`. The
    /// in-memory default matches the SQL column default so a row that
    /// never had its tier set surfaces as Episodic on read.
    pub(crate) fn default_episodic() -> Self {
        MemoryTier::Episodic
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "episodic" => Self::Episodic,
            "semantic" => Self::Semantic,
            "procedural" => Self::Procedural,
            _ => return None,
        })
    }
}

impl std::fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_type_round_trip_via_str() {
        for ty in EntityType::all() {
            let parsed = EntityType::parse(ty.as_str()).unwrap();
            assert_eq!(parsed, ty);
        }
    }

    #[test]
    fn entity_type_unknown_returns_none() {
        assert!(EntityType::parse("unknown").is_none());
        assert!(EntityType::parse("").is_none());
    }

    #[test]
    fn entity_type_serde_round_trip() {
        for ty in EntityType::all() {
            let s = serde_json::to_string(&ty).unwrap();
            let back: EntityType = serde_json::from_str(&s).unwrap();
            assert_eq!(back, ty);
        }
    }

    #[test]
    fn entity_type_serde_uses_snake_case() {
        let s = serde_json::to_string(&EntityType::Organization).unwrap();
        assert_eq!(s, "\"organization\"");
    }

    #[test]
    fn entity_type_display() {
        assert_eq!(EntityType::Person.to_string(), "person");
        assert_eq!(EntityType::Organization.to_string(), "organization");
    }

    #[test]
    fn entity_new_sets_timestamps_and_id() {
        let e = Entity::new("Raymond", EntityType::Person, 42);
        assert_eq!(e.name, "Raymond");
        assert_eq!(e.entity_type, EntityType::Person);
        assert_eq!(e.created_at, 42);
        assert_eq!(e.updated_at, 42);
        assert!((e.confidence - 1.0).abs() < f32::EPSILON);
        assert!(!e.id.is_empty());
        assert!(uuid::Uuid::parse_str(&e.id).is_ok());
    }

    #[test]
    fn observation_new_validity_open_ended() {
        let o = Observation::new("e1", "x", 100);
        assert_eq!(o.entity_id, "e1");
        assert_eq!(o.content, "x");
        assert_eq!(o.observed_at, 100);
        assert_eq!(o.valid_from, Some(100));
        assert_eq!(o.valid_until, None);
        assert!(!o.tombstoned);
    }

    #[test]
    fn observation_validity_check() {
        let mut o = Observation::new("e1", "x", 100);
        assert!(o.is_valid_at(100));
        assert!(o.is_valid_at(200));
        assert!(!o.is_valid_at(99));

        o.valid_until = Some(150);
        assert!(o.is_valid_at(149));
        assert!(!o.is_valid_at(150));
        assert!(!o.is_valid_at(200));
    }

    #[test]
    fn observation_tombstoned_is_invalid() {
        let mut o = Observation::new("e1", "x", 100);
        o.tombstoned = true;
        assert!(!o.is_valid_at(200));
    }

    #[test]
    fn observation_serde_round_trip() {
        let o = Observation::new("e1", "fact", 100);
        let s = serde_json::to_string(&o).unwrap();
        let back: Observation = serde_json::from_str(&s).unwrap();
        assert_eq!(back, o);
    }

    #[test]
    fn observation_serde_accepts_minimal_input() {
        // valid_from / valid_until / tombstoned / access_count are #[serde(default)]
        // so an upstream JSON producer can omit them.
        let json = r#"{
            "id": "abc",
            "entity_id": "e",
            "content": "c",
            "observed_at": 0,
            "confidence": 1.0,
            "source": ""
        }"#;
        let o: Observation = serde_json::from_str(json).unwrap();
        assert_eq!(o.valid_from, None);
        assert_eq!(o.valid_until, None);
        assert!(!o.tombstoned);
        assert_eq!(o.access_count, 0);
    }

    #[test]
    fn relation_new_sets_defaults() {
        let r = Relation::new("a", "b", "maintains", 100);
        assert_eq!(r.from_entity, "a");
        assert_eq!(r.to_entity, "b");
        assert_eq!(r.relation_type, "maintains");
        assert!((r.weight - 1.0).abs() < f32::EPSILON);
        assert_eq!(r.valid_from, Some(100));
    }

    #[test]
    fn relation_serde_round_trip() {
        let r = Relation::new("a", "b", "uses", 100);
        let s = serde_json::to_string(&r).unwrap();
        let back: Relation = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn memory_tier_round_trip() {
        for t in [
            MemoryTier::Episodic,
            MemoryTier::Semantic,
            MemoryTier::Procedural,
        ] {
            assert_eq!(MemoryTier::parse(t.as_str()).unwrap(), t);
            let s = serde_json::to_string(&t).unwrap();
            let back: MemoryTier = serde_json::from_str(&s).unwrap();
            assert_eq!(back, t);
        }
    }

    #[test]
    fn memory_tier_unknown_returns_none() {
        assert!(MemoryTier::parse("unknown").is_none());
    }

    #[test]
    fn observation_serde_round_trips_memory_tier() {
        let mut o = Observation::new("e1", "x", 0);
        o.memory_tier = MemoryTier::Semantic;
        let s = serde_json::to_string(&o).unwrap();
        let back: Observation = serde_json::from_str(&s).unwrap();
        assert_eq!(back.memory_tier, MemoryTier::Semantic);
    }

    #[test]
    fn observation_serde_defaults_memory_tier_to_episodic() {
        let json = r#"{
            "id":"a","entity_id":"e","content":"c",
            "observed_at":0,"confidence":1.0,"source":""
        }"#;
        let o: Observation = serde_json::from_str(json).unwrap();
        assert_eq!(o.memory_tier, MemoryTier::Episodic);
    }

    #[test]
    fn new_id_returns_unique_uuidv7_strings() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        let parsed = uuid::Uuid::parse_str(&a).unwrap();
        assert_eq!(parsed.get_version_num(), 7);
    }
}
