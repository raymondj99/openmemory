//! Knowledge-graph tools.
//!
//! Nine tools wrap the [`openmemory_graph::MemoryStore`] API:
//!
//! - `openmemory_remember` — atomic write (entity + observations + relations)
//! - `openmemory_recall` — hybrid search + decay scoring
//! - `openmemory_list_entities` — paginated entity index
//! - `openmemory_get_entity` — entity + observations + relations bundle
//! - `openmemory_add_relation` — attach a relation between two existing entities
//! - `openmemory_promote_observation` — move an observation between memory tiers
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
    EntityType, MemoryError, MemoryTier, NormalizeMatch, ObservationInput, RecallFilters,
    RelationInput,
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
    #[serde(alias = "entity_name", alias = "name")]
    pub entity: String,
    /// Entity classification. Defaults to `concept`.
    #[serde(default)]
    pub entity_type: Option<EntityTypeParam>,
    /// One or more atomic facts about the entity. Each entry is either
    /// a bare string (legacy shape) or an object with the v0.3 fielded
    /// inputs (`content`, plus optional `title`, `summary`,
    /// `importance`, `source_kind`, `concepts`, `source_files`). Empty
    /// list rejected.
    pub observations: Vec<RememberObservationInput>,
    /// Optional directed relationships to other entities.
    #[serde(default)]
    pub relations: Option<Vec<RelationInputParam>>,
    /// Origin tag for audit. Defaults to "mcp".
    #[serde(default)]
    pub source: Option<String>,
    /// Per-observation confidence. Defaults to 1.0; clamped into [0.0, 1.0].
    #[serde(default)]
    pub confidence: Option<f32>,
    /// Memory tier for the new observations. Defaults to `episodic`.
    #[serde(default)]
    pub memory_tier: Option<MemoryTierParam>,
}

/// One observation on the remember API. Either a bare string (the
/// legacy shape) or a detailed object carrying the v0.3 fielded inputs.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RememberObservationInput {
    /// Bare-content shape, identical to v0.2 callers.
    Plain(String),
    /// Detailed shape with the v0.3 fielded inputs.
    Detailed(ObservationInputBody),
}

/// Detailed observation body. All fields except `content` are optional;
/// `importance` is clamped into `[0.0, 1.0]`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ObservationInputBody {
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub importance: Option<f32>,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub concepts: Vec<String>,
    #[serde(default)]
    pub source_files: Vec<String>,
}

/// One relation input on the remember API. Field names mirror
/// `AddRelationInput` so agents that learn one shape work with the
/// other. Common aliases (`to`, `to_type`, `type`,
/// `target_name`, `target_entity_type`, `relationship_type`) are
/// accepted so a wider range of natural agent guesses parse cleanly.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RelationInputParam {
    /// Target entity name (case-sensitive). Example: `"toml_edit"`.
    #[serde(alias = "to", alias = "target_name", alias = "target")]
    pub to_entity: String,
    /// Target entity type. Defaults to `concept`. Example: `"tool"`.
    #[serde(
        default,
        alias = "to_type",
        alias = "target_entity_type",
        alias = "target_type"
    )]
    pub to_entity_type: Option<EntityTypeParam>,
    /// Relationship type, active-voice verb (`maintains`, `prefers`,
    /// `uses`). Example: `"uses"`.
    #[serde(alias = "type", alias = "relationship_type")]
    pub relation_type: String,
}

const REMEMBER_DESC: &str =
    "Store facts about an entity in persistent memory. Creates or updates the entity and \
     appends observations (atomic facts). Optionally records directed relationships to other \
     entities, creating those entities lazily if they do not yet exist. Entity names are \
     fuzzy-matched against existing entities of the same type: near-identical names \
     (e.g. \"ProjectAlpha\" vs \"Project Alpha\") auto-merge; close matches create a SAME_AS \
     relation. The response includes a 'normalized' field when normalization fires. Use this \
     to persist knowledge across sessions: user preferences, project decisions, learned patterns. \
     \n\nIdempotency: observations are always APPENDED, not deduplicated at write time. Calling \
     this twice with the same `entity` + identical `content` creates two parallel observation \
     rows (with different ids and timestamps). Run `openmemory_consolidate` periodically to \
     dedup near-duplicates by text similarity. Callers that need write-time dedup should \
     `openmemory_recall` the proposed title first and skip the write when a high-scoring hit \
     already exists. Relations follow the same append-only contract: identical edges are \
     duplicated, not merged. \
     \n\nRelation shape: each entry is \
     `{\"relation_type\": \"<verb>\", \"to_entity\": \"<name>\", \"to_entity_type\": \"<type>\"}`. \
     Example: `[{\"relation_type\": \"uses\", \"to_entity\": \"toml_edit\", \"to_entity_type\": \"tool\"}]`. \
     Field names match `openmemory_add_relation`; older keys (`type`, `to`, `to_type`, \
     `target_name`, `target_entity_type`) are accepted as aliases for backward compatibility.";

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
        let memory_tier = req
            .memory_tier
            .map_or(MemoryTier::Episodic, |p| p.to_tier());
        let observations: Vec<ObservationInput> = req
            .observations
            .iter()
            .filter_map(|raw| {
                let body = match raw {
                    RememberObservationInput::Plain(s) => {
                        if s.trim().is_empty() {
                            return None;
                        }
                        ObservationInput::new(s)
                    }
                    RememberObservationInput::Detailed(d) => {
                        if d.content.trim().is_empty() {
                            return None;
                        }
                        let mut inp = ObservationInput::new(&d.content)
                            .with_concepts(d.concepts.clone())
                            .with_source_files(d.source_files.clone());
                        if let Some(title) = d.title.as_deref() {
                            inp = inp.with_title(title);
                        }
                        if let Some(summary) = d.summary.as_deref() {
                            inp = inp.with_summary(summary);
                        }
                        if let Some(importance) = d.importance {
                            inp = inp.with_importance(importance);
                        }
                        if let Some(kind) = d.source_kind.as_deref() {
                            inp = inp.with_source_kind(kind);
                        }
                        inp
                    }
                };
                Some(
                    body.with_confidence(confidence)
                        .with_source(&source)
                        .with_memory_tier(memory_tier),
                )
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
                    .to_entity_type
                    .map_or(EntityType::Concept, |p| p.to_entity_type());
                RelationInput::new(r.relation_type, r.to_entity, target_type)
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
        filters.memory_tier = req.memory_tier.map(|p| p.to_tier());

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
                    "memory_tier": h.observation.memory_tier.as_str(),
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
    #[serde(alias = "entity_name", alias = "name")]
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
                    "memory_tier": o.memory_tier.as_str(),
                    "title": o.title,
                    "summary": o.summary,
                    "importance": o.importance,
                    "source_kind": o.source_kind,
                    "concepts": o.concepts,
                    "source_files": o.source_files,
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
    #[serde(alias = "entity_name", alias = "name")]
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
// openmemory_add_relation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AddRelationInput {
    /// Name of the entity the relation originates from. Must already
    /// exist (use `openmemory_remember` to create it first).
    #[serde(alias = "from", alias = "from_name", alias = "source_entity")]
    pub from_entity: String,
    /// Entity type of `from_entity`. Defaults to `concept` when
    /// omitted, matching the resolution policy elsewhere in the API.
    #[serde(default, alias = "from_type", alias = "source_entity_type")]
    pub from_entity_type: Option<EntityTypeParam>,
    /// Name of the entity the relation points to. Must already exist.
    #[serde(
        alias = "to",
        alias = "to_name",
        alias = "target_name",
        alias = "target"
    )]
    pub to_entity: String,
    /// Entity type of `to_entity`. Defaults to `concept`.
    #[serde(
        default,
        alias = "to_type",
        alias = "target_entity_type",
        alias = "target_type"
    )]
    pub to_entity_type: Option<EntityTypeParam>,
    /// Relationship kind, e.g. `supersedes`, `clarifies`,
    /// `depends_on`, `instance_of`. Free-form string; conventions are
    /// agent-side, not server-enforced.
    #[serde(alias = "type", alias = "relationship_type")]
    pub relation_type: String,
    /// Edge weight in `[0, 1]`. Defaults to `1.0` when omitted.
    #[serde(default)]
    pub weight: Option<f32>,
    /// Source tag for audit/dedup (e.g. `"curator"`, `"omdemos:..."`).
    #[serde(default)]
    pub source: Option<String>,
}

const ADD_RELATION_DESC: &str =
    "Attach a relation between two existing entities. Use this when an observation you just \
     wrote needs an explicit edge to another entity, e.g. recording that a new decision \
     `supersedes` an older one, or that a runbook `clarifies` an incident note. Both entities \
     must already exist; the tool will not silently create them. Returns the new relation id. \
     Idempotent in spirit but not in storage: calling twice creates two parallel edges, so \
     callers should check `openmemory_get_entity` first if dedup matters. Field aliases such as \
     `from`, `to`, `type`, `from_type`, and `to_type` are accepted for backward compatibility.";

/// Handler for the `openmemory_add_relation` MCP tool. Attaches a
/// relation between two existing entities resolved by `(name, type)`.
pub struct OpenMemoryAddRelationTool;
impl Tool for OpenMemoryAddRelationTool {
    const NAME: &'static str = "openmemory_add_relation";
    const SUMMARY: &'static str =
        "Attach a relation (e.g. `supersedes`) between two existing entities.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: ADD_RELATION_DESC.into(),
            input_schema: schema_for::<AddRelationInput>(),
            annotations: Some(write_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: AddRelationInput = parse_args(args)?;
        if req.relation_type.trim().is_empty() {
            return Err(JsonRpcError::invalid_params(
                "relation_type must not be empty",
            ));
        }
        let from_type = req
            .from_entity_type
            .map_or(EntityType::Concept, |p| p.to_entity_type());
        let to_type = req
            .to_entity_type
            .map_or(EntityType::Concept, |p| p.to_entity_type());
        let from = server
            .memory()
            .get_entity_by_name_and_type(&req.from_entity, from_type)
            .map_err(map_memory_err)?
            .ok_or_else(|| JsonRpcError {
                code: -32004,
                message: format!(
                    "from_entity not found: {:?} ({})",
                    req.from_entity,
                    from_type.as_str()
                ),
                data: None,
            })?;
        let to = server
            .memory()
            .get_entity_by_name_and_type(&req.to_entity, to_type)
            .map_err(map_memory_err)?
            .ok_or_else(|| JsonRpcError {
                code: -32004,
                message: format!(
                    "to_entity not found: {:?} ({})",
                    req.to_entity,
                    to_type.as_str()
                ),
                data: None,
            })?;
        let source = req.source.as_deref().unwrap_or("mcp");
        let rel_id = server
            .memory()
            .add_relation(&from.id, &to.id, &req.relation_type, req.weight, source)
            .map_err(map_memory_err)?;
        json_text_result(&json!({
            "relation_id": rel_id,
            "from_entity_id": from.id,
            "to_entity_id": to.id,
            "relation_type": req.relation_type,
        }))
    }
}

// ---------------------------------------------------------------------------
// openmemory_promote_observation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PromoteObservationInput {
    /// Observation ID (UUIDv7) returned by `openmemory_recall` or
    /// `openmemory_remember`.
    pub observation_id: String,
    /// Target tier: `episodic`, `semantic`, or `procedural`.
    pub memory_tier: MemoryTierParam,
}

const PROMOTE_DESC: &str =
    "Move an observation between memory tiers (`episodic` -> `semantic` -> `procedural`, \
     or back). Use this after an observation has survived consolidation, been accessed \
     repeatedly, or otherwise earned promotion out of short-term `episodic` storage. \
     Returns `{ modified: true }` when the row was updated, `{ modified: false }` if the \
     observation is missing or tombstoned. Tier is the only field touched; content, \
     importance, and relations are unchanged.";

/// Handler for the `openmemory_promote_observation` MCP tool. Mutates
/// a single observation's `memory_tier` in place.
pub struct OpenMemoryPromoteObservationTool;
impl Tool for OpenMemoryPromoteObservationTool {
    const NAME: &'static str = "openmemory_promote_observation";
    const SUMMARY: &'static str = "Move an observation between memory tiers.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: PROMOTE_DESC.into(),
            input_schema: schema_for::<PromoteObservationInput>(),
            annotations: Some(write_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: PromoteObservationInput = parse_args(args)?;
        let tier = req.memory_tier.to_tier();
        let modified = server
            .memory()
            .set_observation_memory_tier(&req.observation_id, tier)
            .map_err(map_memory_err)?;
        json_text_result(&json!({
            "modified": modified,
            "memory_tier": tier.as_str(),
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
    out.push(entry::<OpenMemoryAddRelationTool>());
    out.push(entry::<OpenMemoryPromoteObservationTool>());
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
    fn remember_fielded_observation_round_trips_via_get_entity() {
        let s = server();
        let _ = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "Sift",
                "entity_type": "project",
                "observations": [
                    {
                        "content": "uses fielded indexing",
                        "title": "fielded indexing landed",
                        "summary": "v0.3 ships fielded inputs",
                        "importance": 0.7,
                        "source_kind": "note",
                        "concepts": ["indexing", "schema"],
                        "source_files": ["docs/storage.md"]
                    }
                ]
            }),
        )
        .unwrap();

        let r = OpenMemoryGetEntityTool::call(&s, json!({"entity": "Sift"})).unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"title\": \"fielded indexing landed\""));
        assert!(body.contains("\"summary\": \"v0.3 ships fielded inputs\""));
        assert!(body.contains("\"source_kind\": \"note\""));
        assert!(body.contains("\"concepts\""));
        assert!(body.contains("indexing"));
        assert!(body.contains("schema"));
        assert!(body.contains("docs/storage.md"));
        // importance is serialised as a JSON number; tolerate the f32
        // rendering as either 0.7 or 0.699...
        assert!(body.contains("\"importance\""));
    }

    #[test]
    fn remember_plain_observation_strings_still_work() {
        let s = server();
        let _ = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "PlainShape",
                "observations": ["bare string observation"],
            }),
        )
        .unwrap();
        let r = OpenMemoryGetEntityTool::call(&s, json!({"entity": "PlainShape"})).unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("bare string observation"));
    }

    #[test]
    fn remember_then_recall_round_trips_semantic_tier() {
        let s = server();
        let _ = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "T",
                "observations": ["alpha"],
                "memory_tier": "semantic",
            }),
        )
        .unwrap();

        let r = OpenMemoryRecallTool::call(
            &s,
            json!({
                "query": "alpha",
                "mode": "keyword",
                "memory_tier": "semantic",
            }),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(
            body.contains("\"memory_tier\": \"semantic\""),
            "missing semantic tier in body: {body}",
        );

        // A tier-scoped recall with no semantic hits stays empty.
        let r = OpenMemoryRecallTool::call(
            &s,
            json!({
                "query": "alpha",
                "mode": "keyword",
                "memory_tier": "procedural",
            }),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(
            body.contains("\"results\": []"),
            "procedural tier should yield no hits, got: {body}",
        );
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
    fn entity_name_alias_works_for_entity_tools() {
        let s = server();
        let r = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity_name": "Alias Project",
                "entity_type": "project",
                "observations": ["stored via entity_name alias"],
            }),
        )
        .expect("remember should accept entity_name alias");
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"observation_ids\""));

        let fetched = OpenMemoryGetEntityTool::call(
            &s,
            json!({
                "entity_name": "Alias Project",
            }),
        )
        .expect("get_entity should accept entity_name alias");
        let fetched_body = match &fetched.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(fetched_body.contains("\"found\": true"));

        OpenMemoryForgetEntityTool::call(
            &s,
            json!({
                "entity_name": "Alias Project",
            }),
        )
        .expect("forget_entity should accept entity_name alias");
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

    #[test]
    fn add_relation_links_two_existing_entities() {
        let s = server();
        s.memory()
            .remember(
                "Current Policy",
                EntityType::Fact,
                &[ObservationInput::new("active threshold = 0.075")],
                &[],
                "test",
            )
            .unwrap();
        s.memory()
            .remember(
                "Legacy Policy",
                EntityType::Fact,
                &[ObservationInput::new("old threshold = 0.100")],
                &[],
                "test",
            )
            .unwrap();
        let r = OpenMemoryAddRelationTool::call(
            &s,
            json!({
                "from_entity": "Current Policy",
                "from_entity_type": "fact",
                "to_entity": "Legacy Policy",
                "to_entity_type": "fact",
                "relation_type": "supersedes",
                "source": "curator-test",
            }),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"relation_type\": \"supersedes\""));
        assert!(body.contains("\"relation_id\""));
    }

    #[test]
    fn add_relation_rejects_missing_entity_with_typed_error() {
        let s = server();
        let err = OpenMemoryAddRelationTool::call(
            &s,
            json!({
                "from_entity": "GhostA",
                "to_entity": "GhostB",
                "relation_type": "supersedes",
            }),
        )
        .unwrap_err();
        assert_eq!(
            err.code, -32004,
            "should surface entity-not-found typed error"
        );
    }

    /// Codex (and other agents) idiomatically guess
    /// `{"relation_type", "to_entity", "to_entity_type"}`, matching
    /// `openmemory_add_relation`'s field names, when wiring up
    /// `openmemory_remember` relations. We canonicalised on those names
    /// after a real-world demo showed agents fishing for the right
    /// shape; this test pins the contract so a regression to the old
    /// terse names (`type`/`to`/`to_type`) would fail loudly.
    ///
    /// Three independent calls with three field-name dialects, all
    /// asserted to return a non-empty `relation_ids` array.
    #[test]
    fn remember_relations_accept_canonical_and_alias_shapes() {
        fn relation_count(r: &crate::protocol::CallToolResult) -> usize {
            let body = match &r.content[0] {
                crate::protocol::Content::Text { text } => text.clone(),
            };
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            v.get("relation_ids")
                .and_then(|x| x.as_array())
                .map_or(0, std::vec::Vec::len)
        }

        let s = server();

        // Canonical: matches AddRelationInput's field naming.
        let r1 = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "AlphaProject",
                "entity_type": "project",
                "observations": ["uses something canonical"],
                "relations": [{
                    "relation_type": "uses",
                    "to_entity": "toml_edit",
                    "to_entity_type": "tool",
                }],
            }),
        )
        .expect("canonical relation shape should parse");
        assert_eq!(
            relation_count(&r1),
            1,
            "canonical write should create one relation"
        );

        // Legacy / terse: `type` / `to` / `to_type`. Pre-rename callers.
        let r2 = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "BravoVenture",
                "entity_type": "project",
                "observations": ["uses something terse"],
                "relations": [{
                    "type": "uses",
                    "to": "toml_edit",
                    "to_type": "tool",
                }],
            }),
        )
        .expect("legacy terse relation shape should parse via aliases");
        assert_eq!(
            relation_count(&r2),
            1,
            "terse aliases should create one relation"
        );

        // Natural-language guess that codex tried first in the demo.
        let r3 = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "CharlieInitiative",
                "entity_type": "project",
                "observations": ["uses something agent-y"],
                "relations": [{
                    "relation_type": "uses",
                    "target_name": "toml_edit",
                    "target_entity_type": "tool",
                }],
            }),
        )
        .expect("agent-friendly target_name/target_entity_type aliases should parse");
        assert_eq!(
            relation_count(&r3),
            1,
            "agent-friendly aliases should create one relation"
        );
    }

    /// Same forgiveness contract for `openmemory_add_relation`.
    #[test]
    fn add_relation_accepts_alias_field_names() {
        let s = server();
        s.memory()
            .remember(
                "Alpha",
                EntityType::Fact,
                &[ObservationInput::new("a")],
                &[],
                "t",
            )
            .unwrap();
        s.memory()
            .remember(
                "Beta",
                EntityType::Fact,
                &[ObservationInput::new("b")],
                &[],
                "t",
            )
            .unwrap();

        // Use every legacy / natural alias at once.
        let _ = OpenMemoryAddRelationTool::call(
            &s,
            json!({
                "from": "Alpha",
                "from_type": "fact",
                "to": "Beta",
                "to_type": "fact",
                "type": "supersedes",
            }),
        )
        .expect("add_relation should accept aliased field names");
    }

    #[test]
    fn add_relation_rejects_empty_relation_type() {
        let s = server();
        let err = OpenMemoryAddRelationTool::call(
            &s,
            json!({
                "from_entity": "X",
                "to_entity": "Y",
                "relation_type": "   ",
            }),
        )
        .unwrap_err();
        assert!(
            err.message.contains("relation_type must not be empty"),
            "unexpected message: {}",
            err.message
        );
    }

    #[test]
    fn promote_observation_updates_tier() {
        let s = server();
        let outcome = s
            .memory()
            .remember(
                "Decision",
                EntityType::Fact,
                &[ObservationInput::new("call this episodic on write")],
                &[],
                "test",
            )
            .unwrap();
        let obs_id = &outcome.observation_ids[0];
        let r = OpenMemoryPromoteObservationTool::call(
            &s,
            json!({
                "observation_id": obs_id,
                "memory_tier": "semantic",
            }),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"modified\": true"));
        assert!(body.contains("\"memory_tier\": \"semantic\""));
    }

    #[test]
    fn promote_observation_returns_false_for_unknown_id() {
        let s = server();
        let r = OpenMemoryPromoteObservationTool::call(
            &s,
            json!({
                "observation_id": "00000000-0000-0000-0000-000000000000",
                "memory_tier": "semantic",
            }),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"modified\": false"));
    }
}
