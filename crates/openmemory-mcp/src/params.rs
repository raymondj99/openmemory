//! Shared schema-friendly parameter enums.
//!
//! These enums exist so the JSON Schema rmcp emits is stable and
//! human-friendly even when the underlying graph types use lowercase
//! `serde_json` representations. Each one converts to the matching
//! [`openmemory_graph`] type in one place — adding a new variant means
//! updating both `from_*`/`to_*` and the `JsonSchema` derivation in lockstep.

use schemars::JsonSchema;
use serde::Deserialize;

use openmemory_graph::{EntityType, MemoryTier, SearchMode};

/// Entity type for the memory knowledge graph.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntityTypeParam {
    /// A person (user, teammate, etc.)
    Person,
    /// A software project or repository
    Project,
    /// An abstract concept, pattern, or idea (default)
    Concept,
    /// A tool, library, or framework
    Tool,
    /// A user preference or setting
    Preference,
    /// A standalone fact or observation
    Fact,
    /// A dated event (release, incident, meeting, etc.)
    Event,
    /// A physical or network location
    Location,
    /// A company or team
    Organization,
}

impl EntityTypeParam {
    pub fn to_entity_type(self) -> EntityType {
        match self {
            Self::Person => EntityType::Person,
            Self::Project => EntityType::Project,
            Self::Concept => EntityType::Concept,
            Self::Tool => EntityType::Tool,
            Self::Preference => EntityType::Preference,
            Self::Fact => EntityType::Fact,
            Self::Event => EntityType::Event,
            Self::Location => EntityType::Location,
            Self::Organization => EntityType::Organization,
        }
    }

    pub fn from_entity_type(t: EntityType) -> Self {
        match t {
            EntityType::Person => Self::Person,
            EntityType::Project => Self::Project,
            EntityType::Concept => Self::Concept,
            EntityType::Tool => Self::Tool,
            EntityType::Preference => Self::Preference,
            EntityType::Fact => Self::Fact,
            EntityType::Event => Self::Event,
            EntityType::Location => Self::Location,
            EntityType::Organization => Self::Organization,
        }
    }
}

/// Search mode controlling how index results are ranked.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchModeParam {
    /// Combine vector similarity and BM25 keyword search (default).
    Hybrid,
    /// BM25 full-text keyword search only — no embedding model needed.
    Keyword,
    /// Pure cosine similarity vector search — requires embedding model.
    Vector,
}

impl SearchModeParam {
    pub fn to_mode(self) -> SearchMode {
        match self {
            Self::Hybrid => SearchMode::Hybrid,
            Self::Keyword => SearchMode::KeywordOnly,
            Self::Vector => SearchMode::VectorOnly,
        }
    }
}

impl Default for SearchModeParam {
    fn default() -> Self {
        Self::Hybrid
    }
}

/// Memory tier filter accepted by recall.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryTierParam {
    /// Raw episodic observations.
    Episodic,
    /// Consolidated semantic facts.
    Semantic,
    /// Procedural knowledge (skills).
    Procedural,
}

impl MemoryTierParam {
    pub fn to_tier(self) -> MemoryTier {
        match self {
            Self::Episodic => MemoryTier::Episodic,
            Self::Semantic => MemoryTier::Semantic,
            Self::Procedural => MemoryTier::Procedural,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_type_round_trip() {
        for et in EntityType::all() {
            let p = EntityTypeParam::from_entity_type(et);
            assert_eq!(p.to_entity_type(), et);
        }
    }

    #[test]
    fn search_mode_default_is_hybrid() {
        let p = SearchModeParam::default();
        assert_eq!(p, SearchModeParam::Hybrid);
        assert_eq!(p.to_mode(), SearchMode::Hybrid);
    }

    #[test]
    fn search_mode_param_serde_lowercase() {
        let p: SearchModeParam = serde_json::from_str("\"keyword\"").unwrap();
        assert_eq!(p, SearchModeParam::Keyword);
        let p: SearchModeParam = serde_json::from_str("\"vector\"").unwrap();
        assert_eq!(p, SearchModeParam::Vector);
        let p: SearchModeParam = serde_json::from_str("\"hybrid\"").unwrap();
        assert_eq!(p, SearchModeParam::Hybrid);
    }
}
