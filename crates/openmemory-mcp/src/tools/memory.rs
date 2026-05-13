//! Knowledge-graph tools.
//!
//! Seven tools wrap the [`openmemory_graph::MemoryStore`] API:
//!
//! - `openmemory_remember` — atomic write (entity + observations + relations)
//! - `openmemory_recall` — hybrid search + decay scoring
//! - `openmemory_list_entities` — paginated entity index
//! - `openmemory_get_entity` — entity + observations + relations bundle
//! - `openmemory_forget` — soft-delete a single observation
//! - `openmemory_forget_entity` — hard-delete an entity (cascade)
//! - `openmemory_status` — counts and timestamps
//!
//! Each tool is a unit struct + an input struct + a handler. Both the
//! descriptor (`Tool::descriptor`) and the handler (`Tool::call`) live in
//! the same impl so an agent never sees a tool advertised that the
//! dispatcher cannot answer.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use openmemory_graph::{
    EntityType, MemoryError, NormalizeMatch, ObservationInput, RecallFilters, RelationInput,
};

use crate::params::{EntityTypeParam, MemoryTierParam, SearchModeParam};
use crate::protocol::{CallToolResult, JsonRpcError, ToolDescriptor};
use crate::tools::{
    destructive_annotations, entry, json_text_result, read_only_annotations, schema_for,
    write_annotations, Entry, Tool, ToolGroup,
};
use crate::OpenMemoryMcpServer;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, JsonRpcError> {
    serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("invalid arguments: {e}")))
}

fn map_memory_err(e: MemoryError) -> JsonRpcError {
    match e {
        MemoryError::InvalidInput(msg) => JsonRpcError::invalid_params(msg),
        MemoryError::EntityNotFound(_) | MemoryError::ObservationNotFound(_) => JsonRpcError {
            code: -32004,
            message: e.to_string(),
            data: None,
        },
        other => JsonRpcError::internal_error(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// openmemory_remember
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RememberInput {
    /// Entity name (e.g., "Raymond", "openmemory project", "Rust").
    pub entity: String,
    /// Entity classification. Defaults to `concept`.
    #[serde(default)]
    pub entity_type: Option<EntityTypeParam>,
    /// One or more atomic facts about the entity. Empty list rejected.
    pub observations: Vec<String>,
    /// Optional directed relationships to other entities.
    #[serde(default)]
    pub relations: Option<Vec<RelationInputParam>>,
    /// Origin tag for audit. Defaults to "mcp".
    #[serde(default)]
    pub source: Option<String>,
    /// Per-observation confidence. Defaults to 1.0; clamped into [0.0, 1.0].
    #[serde(default)]
    pub confidence: Option<f32>,
}

/// One relation input on the remember API.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RelationInputParam {
    /// Target entity name (case-sensitive).
    pub to: String,
    /// Target entity type. Defaults to `concept`.
    #[serde(default)]
    pub to_type: Option<EntityTypeParam>,
    /// Relationship type — active-voice verb (`maintains`, `prefers`, …).
    #[serde(rename = "type")]
    pub relation_type: String,
}

const REMEMBER_DESC: &str =
    "Store facts about an entity in persistent memory. Creates or updates the entity and \
     appends observations (atomic facts). Optionally records directed relationships to other \
     entities, creating those entities lazily if they do not yet exist. Entity names are \
     fuzzy-matched against existing entities of the same type: near-identical names \
     (e.g. \"ProjectAlpha\" vs \"Project Alpha\") auto-merge; close matches create a SAME_AS \
     relation. The response includes a 'normalized' field when normalization fires. Use this \
     to persist knowledge across sessions: user preferences, project decisions, learned patterns.";

/// Handler for the `openmemory_remember` MCP tool. Stores or updates
/// an entity and appends observations + optional relations atomically.
pub struct OpenMemoryRememberTool;
impl Tool for OpenMemoryRememberTool {
    const NAME: &'static str = "openmemory_remember";
    const SUMMARY: &'static str =
        "Atomic write: ensure entity exists, append observations and relations.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: REMEMBER_DESC.into(),
            input_schema: schema_for::<RememberInput>(),
            annotations: Some(write_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: RememberInput = parse_args(args)?;
        if req.entity.trim().is_empty() {
            return Err(JsonRpcError::invalid_params(
                "entity name must not be empty",
            ));
        }
        if req.observations.is_empty() {
            return Err(JsonRpcError::invalid_params(
                "at least one observation is required",
            ));
        }
        let entity_type = req
            .entity_type
            .map_or(EntityType::Concept, |p| p.to_entity_type());
        let source = req.source.as_deref().unwrap_or("mcp").to_string();

        let confidence = req.confidence.unwrap_or(1.0);
        let observations: Vec<ObservationInput> = req
            .observations
            .iter()
            .filter(|s| !s.trim().is_empty())
            .map(|content| {
                ObservationInput::new(content)
                    .with_confidence(confidence)
                    .with_source(&source)
            })
            .collect();
        if observations.is_empty() {
            return Err(JsonRpcError::invalid_params(
                "all observations were empty after trimming",
            ));
        }

        let relations: Vec<RelationInput> = req
            .relations
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                let target_type = r
                    .to_type
                    .map_or(EntityType::Concept, |p| p.to_entity_type());
                RelationInput::new(r.relation_type, r.to, target_type)
            })
            .collect();

        let outcome = server
            .memory()
            .remember(&req.entity, entity_type, &observations, &relations, &source)
            .map_err(map_memory_err)?;
        let mut response = json!({
            "entity_id": outcome.entity_id,
            "entity_existed": outcome.entity_existed,
            "observation_ids": outcome.observation_ids,
            "relation_ids": outcome.relation_ids,
        });
        if let Some(ref norm) = outcome.normalized {
            let (action, matched_id, score) = match norm {
                NormalizeMatch::AutoMerge { entity_id, score } => ("auto_merged", entity_id, score),
                NormalizeMatch::Flag { entity_id, score } => ("flagged", entity_id, score),
            };
            response.as_object_mut().unwrap().insert(
                "normalized".into(),
                json!({
                    "action": action,
                    "matched_entity_id": matched_id,
                    "score": score,
                }),
            );
        }
        json_text_result(&response)
    }
}

// ---------------------------------------------------------------------------
// openmemory_recall
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RecallInput {
    /// Natural-language query.
    pub query: String,
    /// Maximum results to return. Defaults to 10; clamped to [1, 50].
    #[serde(default)]
    pub limit: Option<u32>,
    /// Restrict to a single entity type.
    #[serde(default)]
    pub entity_type: Option<EntityTypeParam>,
    /// Restrict to observations from this source.
    #[serde(default)]
    pub source: Option<String>,
    /// Minimum confidence threshold in [0.0, 1.0].
    #[serde(default)]
    pub min_confidence: Option<f32>,
    /// Restrict to a memory tier (`episodic` | `semantic` | `procedural`).
    #[serde(default)]
    #[allow(dead_code)]
    pub memory_tier: Option<MemoryTierParam>,
    /// Restrict to specific entity names (case-insensitive).
    #[serde(default)]
    pub entity_names: Option<Vec<String>>,
    /// Search mode. Defaults to `hybrid`.
    #[serde(default)]
    pub mode: Option<SearchModeParam>,
}

const RECALL_DESC: &str =
    "Recall observations from memory by natural-language query. Hybrid (vector + keyword) \
     search with Ebbinghaus decay scoring; older facts naturally fade unless retrieved often. \
     Optional filters constrain the result set by entity type, source, confidence, or \
     specific entity names. Spreading activation through the relation graph fills in related \
     observations when direct hits underflow.";

/// Handler for the `openmemory_recall` MCP tool. Hybrid search over
/// observations with Ebbinghaus decay scoring and spreading activation.
pub struct OpenMemoryRecallTool;
impl Tool for OpenMemoryRecallTool {
    const NAME: &'static str = "openmemory_recall";
    const SUMMARY: &'static str =
        "Search memory by natural-language query. Returns scored observations.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: RECALL_DESC.into(),
            input_schema: schema_for::<RecallInput>(),
            annotations: Some(read_only_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: RecallInput = parse_args(args)?;
        if req.query.trim().is_empty() {
            return Err(JsonRpcError::invalid_params("query must not be empty"));
        }
        let limit = req.limit.unwrap_or(10).clamp(1, 50) as usize;

        let mut filters = RecallFilters::new();
        filters.entity_type = req.entity_type.map(|p| p.to_entity_type());
        filters.source = req.source;
        filters.min_confidence = req.min_confidence;
        filters.entity_names = req.entity_names;
        filters.mode = req.mode.map(|p| p.to_mode());

        let hits = server
            .memory()
            .recall(&req.query, limit, &filters)
            .map_err(map_memory_err)?;

        let results: Vec<Value> = hits
            .into_iter()
            .map(|h| {
                json!({
                    "observation_id": h.observation.id,
                    "entity_name": h.entity_name,
                    "entity_type": h.entity_type.as_str(),
                    "content": h.observation.content,
                    "observed_at": h.observation.observed_at,
                    "score": super::round2(h.score),
                    "raw_score": super::round2(h.raw_score),
                    "confidence": super::round2(h.observation.confidence),
                    "source": h.observation.source,
                    "access_count": h.observation.access_count,
                })
            })
            .collect();

        json_text_result(&json!({
            "results": results,
            "limit": limit,
        }))
    }
}

// ---------------------------------------------------------------------------
// openmemory_list_entities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ListEntitiesInput {
    /// Filter by entity type.
    #[serde(default)]
    pub entity_type: Option<EntityTypeParam>,
    /// Maximum entities to return. Defaults to 20; clamped to [1, 200].
    #[serde(default)]
    pub limit: Option<u32>,
    /// Skip first N entities for pagination.
    #[serde(default)]
    pub offset: Option<u32>,
}

const LIST_ENTITIES_DESC: &str =
    "List entities in memory, optionally filtered by entity type. Each row carries the entity \
     metadata plus its live observation count, so callers can render a one-shot summary \
     without a second round-trip. Pagination via limit + offset.";

/// Handler for the `openmemory_list_entities` MCP tool. Browses all
/// entities with their observation counts; supports type filter and
/// limit/offset pagination.
pub struct OpenMemoryListEntitiesTool;
impl Tool for OpenMemoryListEntitiesTool {
    const NAME: &'static str = "openmemory_list_entities";
    const SUMMARY: &'static str =
        "Browse all entities. Filter by type, paginate with limit/offset.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: LIST_ENTITIES_DESC.into(),
            input_schema: schema_for::<ListEntitiesInput>(),
            annotations: Some(read_only_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: ListEntitiesInput = parse_args(args)?;
        let limit = req.limit.unwrap_or(20).clamp(1, 200) as usize;
        let offset = req.offset.unwrap_or(0) as usize;
        let entity_type = req.entity_type.map(|p| p.to_entity_type());

        let rows = server
            .memory()
            .list_entities(entity_type, limit, offset)
            .map_err(map_memory_err)?;

        let entities: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "id": r.entity.id,
                    "name": r.entity.name,
                    "entity_type": r.entity.entity_type.as_str(),
                    "created_at": r.entity.created_at,
                    "updated_at": r.entity.updated_at,
                    "confidence": super::round2(r.entity.confidence),
                    "source": r.entity.source,
                    "observation_count": r.observation_count,
                })
            })
            .collect();

        json_text_result(&json!({
            "entities": entities,
            "limit": limit,
            "offset": offset,
        }))
    }
}

// ---------------------------------------------------------------------------
// openmemory_get_entity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetEntityInput {
    /// Entity name (exact match).
    pub entity: String,
}

const GET_ENTITY_DESC: &str =
    "Look up an entity by name and return all of its live observations + relations. Returns \
     `null` (and a `found: false` flag in the JSON) if the entity does not exist.";

/// Handler for the `openmemory_get_entity` MCP tool. Returns one
/// entity bundled with its live observations and relations.
pub struct OpenMemoryGetEntityTool;
impl Tool for OpenMemoryGetEntityTool {
    const NAME: &'static str = "openmemory_get_entity";
    const SUMMARY: &'static str = "Get entity + observations + relations bundle by name.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: GET_ENTITY_DESC.into(),
            input_schema: schema_for::<GetEntityInput>(),
            annotations: Some(read_only_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: GetEntityInput = parse_args(args)?;
        if req.entity.trim().is_empty() {
            return Err(JsonRpcError::invalid_params("entity must not be empty"));
        }
        let memory = server.memory();
        let entity = memory.get_entity(&req.entity).map_err(map_memory_err)?;
        let Some(entity) = entity else {
            return json_text_result(&json!({
                "found": false,
                "entity": Value::Null,
                "observations": [],
                "relations": [],
            }));
        };

        let observations = memory
            .get_entity_observations(&entity.id)
            .map_err(map_memory_err)?;
        let relations = memory
            .get_entity_relations(&entity.id)
            .map_err(map_memory_err)?;

        let observations_json: Vec<Value> = observations
            .iter()
            .map(|o| {
                json!({
                    "id": o.id,
                    "content": o.content,
                    "observed_at": o.observed_at,
                    "valid_from": o.valid_from,
                    "valid_until": o.valid_until,
                    "confidence": super::round2(o.confidence),
                    "source": o.source,
                    "access_count": o.access_count,
                })
            })
            .collect();
        let relations_json: Vec<Value> = relations
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "from_entity": r.from_entity,
                    "to_entity": r.to_entity,
                    "relation_type": r.relation_type,
                    "weight": super::round2(r.weight),
                    "created_at": r.created_at,
                    "valid_from": r.valid_from,
                    "valid_until": r.valid_until,
                    "source": r.source,
                })
            })
            .collect();

        json_text_result(&json!({
            "found": true,
            "entity": {
                "id": entity.id,
                "name": entity.name,
                "entity_type": entity.entity_type.as_str(),
                "created_at": entity.created_at,
                "updated_at": entity.updated_at,
                "confidence": super::round2(entity.confidence),
                "source": entity.source,
            },
            "observations": observations_json,
            "relations": relations_json,
        }))
    }
}

// ---------------------------------------------------------------------------
// openmemory_forget
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ForgetInput {
    /// Observation ID (UUIDv7) returned by `openmemory_recall` or
    /// `openmemory_remember`.
    pub observation_id: String,
}

const FORGET_DESC: &str =
    "Soft-delete a single observation by ID. Tombstoned observations are excluded from \
     subsequent recalls but remain in the database until `openmemory_consolidate` sweeps \
     them. Idempotent — calling with an already-tombstoned ID is a no-op.";

/// Handler for the `openmemory_forget` MCP tool. Soft-deletes one
/// observation by ID; tombstoned rows are excluded from subsequent
/// recalls and removed by the next `consolidate` sweep.
pub struct OpenMemoryForgetTool;
impl Tool for OpenMemoryForgetTool {
    const NAME: &'static str = "openmemory_forget";
    const SUMMARY: &'static str = "Soft-delete one observation by ID.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: FORGET_DESC.into(),
            input_schema: schema_for::<ForgetInput>(),
            annotations: Some(destructive_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: ForgetInput = parse_args(args)?;
        let modified = server
            .memory()
            .forget(&req.observation_id)
            .map_err(map_memory_err)?;
        json_text_result(&json!({ "modified": modified }))
    }
}

// ---------------------------------------------------------------------------
// openmemory_forget_entity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ForgetEntityInput {
    /// Entity name (exact match).
    pub entity: String,
}

const FORGET_ENTITY_DESC: &str =
    "Hard-delete an entity by name, cascading to its observations and relations. Returns the \
     observation count purged. Returns an `entity not found` error for unknown names.";

/// Handler for the `openmemory_forget_entity` MCP tool. Hard-deletes
/// an entity by name and cascades to its observations and relations.
pub struct OpenMemoryForgetEntityTool;
impl Tool for OpenMemoryForgetEntityTool {
    const NAME: &'static str = "openmemory_forget_entity";
    const SUMMARY: &'static str = "Hard-delete one entity. Cascades to observations + relations.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: FORGET_ENTITY_DESC.into(),
            input_schema: schema_for::<ForgetEntityInput>(),
            annotations: Some(destructive_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: ForgetEntityInput = parse_args(args)?;
        let removed = server
            .memory()
            .forget_entity(&req.entity)
            .map_err(map_memory_err)?;
        json_text_result(&json!({
            "observations_removed": removed,
        }))
    }
}

// ---------------------------------------------------------------------------
// openmemory_status
// ---------------------------------------------------------------------------

const STATUS_DESC: &str =
    "Show counts of entities, observations, and relations, plus per-type and per-tier \
     breakdowns and the current schema version. Use this first to understand what's in \
     memory before running queries.";

/// Handler for the `openmemory_status` MCP tool. Reports schema
/// version and aggregate counts of entities, observations, relations,
/// and tombstones in the active store.
pub struct OpenMemoryStatusTool;
impl Tool for OpenMemoryStatusTool {
    const NAME: &'static str = "openmemory_status";
    const SUMMARY: &'static str =
        "Show counts of entities, observations, relations, and schema version.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: STATUS_DESC.into(),
            input_schema: super::empty_schema(),
            annotations: Some(read_only_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, _args: Value) -> Result<CallToolResult, JsonRpcError> {
        let s = server.memory().status().map_err(map_memory_err)?;
        json_text_result(&json!({
            "total_entities": s.total_entities,
            "total_observations": s.total_observations,
            "total_relations": s.total_relations,
            "tombstoned_observations": s.tombstoned_observations,
            "schema_version": s.schema_version,
            "oldest_observation": s.oldest_observation,
            "newest_observation": s.newest_observation,
            "entity_type_counts": s.entity_type_counts,
            "tier_counts": s.tier_counts,
            "vector_count": s.vector_count,
        }))
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub(crate) fn register_all(out: &mut Vec<Entry>) {
    out.push(entry::<OpenMemoryRememberTool>());
    out.push(entry::<OpenMemoryRecallTool>());
    out.push(entry::<OpenMemoryListEntitiesTool>());
    out.push(entry::<OpenMemoryGetEntityTool>());
    out.push(entry::<OpenMemoryForgetTool>());
    out.push(entry::<OpenMemoryForgetEntityTool>());
    out.push(entry::<OpenMemoryStatusTool>());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use openmemory_core::config::Config;
    use openmemory_graph::MemoryStore;
    use serde_json::json;

    fn server() -> OpenMemoryMcpServer {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store))
    }

    #[test]
    fn descriptors_use_openmemory_prefix() {
        for d in [
            OpenMemoryRememberTool::descriptor(),
            OpenMemoryRecallTool::descriptor(),
            OpenMemoryStatusTool::descriptor(),
        ] {
            assert!(d.name.starts_with("openmemory_"));
        }
    }

    #[test]
    fn remember_round_trips_via_recall() {
        let s = server();
        let _ = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "Raymond",
                "entity_type": "person",
                "observations": ["prefers Rust over Python"],
                "source": "test",
            }),
        )
        .unwrap();

        let result = OpenMemoryRecallTool::call(
            &s,
            json!({
                "query": "Rust",
                "limit": 5,
                "mode": "keyword",
            }),
        )
        .unwrap();
        let body = match &result.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("Raymond"));
        assert!(body.contains("Rust"));
    }

    #[test]
    fn remember_rejects_empty_entity() {
        let s = server();
        let err = OpenMemoryRememberTool::call(&s, json!({"entity": "", "observations": ["x"]}))
            .unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn remember_rejects_empty_observation_list() {
        let s = server();
        let err = OpenMemoryRememberTool::call(&s, json!({"entity": "X", "observations": []}))
            .unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn list_entities_returns_inserted_rows() {
        let s = server();
        let _ = OpenMemoryRememberTool::call(
            &s,
            json!({"entity": "Alpha", "entity_type": "concept", "observations": ["x"]}),
        )
        .unwrap();
        let r = OpenMemoryListEntitiesTool::call(&s, json!({})).unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("Alpha"));
    }

    #[test]
    fn get_entity_returns_observations() {
        let s = server();
        let _ = OpenMemoryRememberTool::call(
            &s,
            json!({"entity": "Topic", "observations": ["fact one", "fact two"]}),
        )
        .unwrap();
        let r = OpenMemoryGetEntityTool::call(&s, json!({"entity": "Topic"})).unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"found\": true"));
        assert!(body.contains("fact one"));
        assert!(body.contains("fact two"));
    }

    #[test]
    fn get_entity_unknown_returns_not_found_payload() {
        let s = server();
        let r = OpenMemoryGetEntityTool::call(&s, json!({"entity": "Missing"})).unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"found\": false"));
    }

    #[test]
    fn forget_round_trips() {
        let s = server();
        let outcome = s
            .memory()
            .remember(
                "X",
                EntityType::Fact,
                &[ObservationInput::new("alpha")],
                &[],
                "t",
            )
            .unwrap();
        let r =
            OpenMemoryForgetTool::call(&s, json!({"observation_id": outcome.observation_ids[0]}))
                .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"modified\": true"));
    }

    #[test]
    fn forget_entity_unknown_surfaces_typed_error() {
        let s = server();
        let err = OpenMemoryForgetEntityTool::call(&s, json!({"entity": "Missing"})).unwrap_err();
        assert_eq!(err.code, -32004);
    }

    #[test]
    fn status_returns_zero_counts_on_fresh_store() {
        let s = server();
        let r = OpenMemoryStatusTool::call(&s, json!({})).unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"total_entities\": 0"));
    }
}
