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
//! - `openmemory_forget_entity` — audit-retire an entity's observations
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
    EntityCandidates, EntityType, MemoryError, MemoryTier, NormalizeMatch, ObservationInput,
    RecallFilters, RelationInput,
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
        // A destructive call named something that matches several
        // entities. Refusing is the whole point — retiring the wrong
        // `Alex Chen` is unrecoverable — so hand back the candidate ids
        // as structured data and let the agent name one.
        MemoryError::AmbiguousEntityName {
            ref name,
            ref candidates,
        } => JsonRpcError {
            code: -32005,
            message: format!(
                "entity name {name:?} matches {} entities; retry with one of the listed ids",
                candidates.len()
            ),
            data: Some(json!({
                "reason": "ambiguous_entity_name",
                "name": name,
                "candidates": candidates,
            })),
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
    /// Only meaningful when the write-behind context engine is enabled
    /// (`[engine]` in config.toml): wait for the write to commit before
    /// returning. Defaults to the configured `engine.durable_ack`.
    #[serde(default)]
    pub durable: Option<bool>,
    /// Memory space to write into. Omit (or pass `default`) for the
    /// personal-global default store. A request writes to exactly one
    /// space.
    #[serde(default)]
    pub space: Option<String>,
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
    /// Unix seconds when the fact became true. `None` = open-ended.
    #[serde(default)]
    pub valid_from: Option<i64>,
    /// Unix seconds when the fact stopped being true. `None` = still
    /// valid. Set on a superseded fact so current-truth recall skips it
    /// while `valid_at`-pinned recall can still reach it.
    #[serde(default)]
    pub valid_until: Option<i64>,
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
     `target_name`, `target_entity_type`) are accepted as aliases for backward compatibility. \
     \n\nWrite-behind mode: when the context engine is enabled (`[engine]` in config.toml), \
     this tool enqueues the write into a sharded, journaled ingestion lane and returns a \
     receipt `{accepted, durable, entity, shard, seq}` instead of ids. By default it waits \
     for the write to commit (read-your-writes); pass `durable: false` for a fire-and-forget \
     acknowledgement in microseconds.";

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
        for raw in &req.observations {
            if let RememberObservationInput::Detailed(d) = raw {
                if let (Some(from), Some(until)) = (d.valid_from, d.valid_until) {
                    if from > until {
                        return Err(JsonRpcError::invalid_params(
                            "valid_from must be <= valid_until",
                        ));
                    }
                }
            }
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
                        if d.valid_from.is_some() || d.valid_until.is_some() {
                            inp = inp.with_validity(d.valid_from, d.valid_until);
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

        let memory = server.store_for(req.space.as_deref())?;

        // Write-behind path: when the context engine is enabled, submit
        // to its sharded queue instead of paying a per-call transaction.
        // The response is an ingestion receipt; ids are not minted until
        // the epoch flush commits the batch. The engine is bound to the
        // personal-global default store, so space-targeted writes always
        // take the synchronous path below.
        let default_target = matches!(req.space.as_deref(), None | Some("" | "default"));
        if default_target {
            if let Some(engine) = server.engine() {
                let wait = req.durable.unwrap_or(server.config().engine.durable_ack);
                let ticket = engine
                    .try_submit(
                        openmemory_graph::RememberRequest::new(req.entity.clone(), entity_type)
                            .with_observations(observations)
                            .with_relations(relations)
                            .with_source(source),
                    )
                    .map_err(map_memory_err)?;
                if wait {
                    engine.wait_durable_result(ticket).map_err(map_memory_err)?;
                }
                return json_text_result(&json!({
                    "accepted": true,
                    "durable": wait,
                    "entity": req.entity,
                    "shard": ticket.shard,
                    "seq": ticket.seq,
                }));
            }
        }

        let outcome = memory
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
    /// Evaluate temporal validity as of this Unix timestamp instead of
    /// now: observations whose validity window excludes the instant are
    /// filtered out, and recency decay is measured relative to it. Use
    /// for as-of/history questions; omit for current truth.
    #[serde(default)]
    pub valid_at: Option<i64>,
    /// Memory space to search. Omit (or pass `default`) for the
    /// personal-global default store.
    #[serde(default)]
    pub space: Option<String>,
    /// Ordered read set of up to four spaces to search together (use
    /// `default` for the personal-global store). Results are fused by
    /// deterministic rank interleaving in read-set order; scores are
    /// never compared across spaces. Mutually exclusive with `space`.
    #[serde(default)]
    pub read_spaces: Option<Vec<String>>,
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
        filters.valid_at = req.valid_at;

        let render = |h: &openmemory_graph::RecallResult, space: Option<&str>| {
            let mut row = json!({
                "observation_id": h.observation.id,
                "entity_name": h.entity_name,
                "entity_type": h.entity_type.as_str(),
                "content": h.observation.content,
                "observed_at": h.observation.observed_at,
                "valid_from": h.observation.valid_from,
                "valid_until": h.observation.valid_until,
                "score": super::round2(h.score),
                "raw_score": super::round2(h.raw_score),
                "confidence": super::round2(h.observation.confidence),
                "source": h.observation.source,
                "access_count": h.observation.access_count,
                "memory_tier": h.observation.memory_tier.as_str(),
            });
            if let Some(space) = space {
                row.as_object_mut()
                    .unwrap()
                    .insert("space".into(), json!(space));
            }
            row
        };

        // Layered read set: recall each space independently, then fuse
        // by deterministic rank interleaving. Scores are never compared
        // across spaces (established invariant; see plan/16).
        if let Some(read_spaces) = &req.read_spaces {
            let layers = resolve_read_spaces(server, req.space.as_deref(), read_spaces)?;
            // Layered reads are read-only composition; retrieval-
            // frequency feedback stays off so a fused preview does not
            // mutate per-space retention state.
            filters.record_access = false;
            let mut lists = Vec::with_capacity(layers.len());
            for (label, store) in &layers {
                let hits = store
                    .recall(&req.query, limit, &filters)
                    .map_err(map_memory_err)?;
                lists.push((label.clone(), hits));
            }
            let fused = openmemory_engine::space::interleave_by_rank(lists, limit);
            let results: Vec<Value> = fused
                .iter()
                .map(|hit| render(&hit.result, Some(hit.space.as_deref().unwrap_or("default"))))
                .collect();
            return json_text_result(&json!({
                "results": results,
                "limit": limit,
                "read_spaces": layers
                    .iter()
                    .map(|(label, _)| label.as_deref().unwrap_or("default"))
                    .collect::<Vec<_>>(),
                "fusion": "rank_interleave",
            }));
        }

        let memory = server.store_for(req.space.as_deref())?;
        let hits = memory
            .recall(&req.query, limit, &filters)
            .map_err(map_memory_err)?;

        let results: Vec<Value> = hits.iter().map(|h| render(h, None)).collect();

        json_text_result(&json!({
            "results": results,
            "limit": limit,
        }))
    }
}

/// One resolved layer in an ordered read set: the space label (`None`
/// for the personal-global default) and its store.
pub(crate) type ReadLayer = (
    Option<String>,
    std::sync::Arc<openmemory_engine::partition::DomainStore>,
);

/// Resolve an ordered layered read set. `read_spaces` entries name
/// managed spaces, with `default` (or an empty string) addressing the
/// personal-global store; at most [`MAX_READ_SPACES`] entries, no
/// duplicates, and `space` must not also be set.
pub(crate) fn resolve_read_spaces(
    server: &OpenMemoryMcpServer,
    space: Option<&str>,
    read_spaces: &[String],
) -> Result<Vec<ReadLayer>, JsonRpcError> {
    use openmemory_engine::space::MAX_READ_SPACES;

    if space.is_some() {
        return Err(JsonRpcError::invalid_params(
            "`space` and `read_spaces` are mutually exclusive; put every \
             space to read into `read_spaces`",
        ));
    }
    if read_spaces.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "`read_spaces` must name at least one space",
        ));
    }
    if read_spaces.len() > MAX_READ_SPACES {
        return Err(JsonRpcError::invalid_params(format!(
            "`read_spaces` allows at most {MAX_READ_SPACES} spaces"
        )));
    }
    let mut out = Vec::with_capacity(read_spaces.len());
    let mut seen = std::collections::HashSet::new();
    for name in read_spaces {
        let canonical = if name.is_empty() {
            "default"
        } else {
            name.as_str()
        };
        if !seen.insert(canonical) {
            return Err(JsonRpcError::invalid_params(format!(
                "`read_spaces` names '{canonical}' more than once"
            )));
        }
        let store = server.store_for(Some(canonical))?;
        let label = (canonical != "default").then(|| canonical.to_owned());
        out.push((label, store));
    }
    Ok(out)
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
    /// Memory space to list. Omit (or pass `default`) for the
    /// personal-global default store.
    #[serde(default)]
    pub space: Option<String>,
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
            .store_for(req.space.as_deref())?
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
    /// Memory space to read. Omit (or pass `default`) for the
    /// personal-global default store.
    #[serde(default)]
    pub space: Option<String>,
}

const GET_ENTITY_DESC: &str =
    "Look up an entity by name and return all of its live observations + relations. Returns \
     `null` (and a `found: false` flag in the JSON) if the entity does not exist. If several \
     entities share the name, `ambiguous` is true and `candidates` lists every match as \
     `{id, name, entity_type}`; the returned bundle is the oldest match by `created_at` \
     (`selected_by: \"oldest_created_at\"`). Re-issue against a specific entity type, or use \
     the listed ids, when the wrong one came back.";

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
        let memory = server.store_for(req.space.as_deref())?;
        // This tool's contract is one entity, so it must collapse a
        // candidate set — but it collapses by a stated rule (oldest
        // `created_at`, ties by id) and reports both that a choice was
        // made and what the alternatives were. It previously took
        // whichever row SQLite returned first and said nothing, which on
        // the live profile store meant `ProjectAlpha` silently resolved
        // to one of a `concept` and a `project`.
        let resolution = memory.resolve_entity(&req.entity).map_err(map_memory_err)?;
        let candidates_json: Vec<Value> = resolution
            .candidates()
            .iter()
            .map(|candidate| {
                json!({
                    "id": candidate.id,
                    "name": candidate.name,
                    "entity_type": candidate.entity_type.as_str(),
                })
            })
            .collect();
        let Some((entity, ambiguity)) = resolution.choose_oldest() else {
            return json_text_result(&json!({
                "found": false,
                "ambiguous": false,
                "entity": Value::Null,
                "observations": [],
                "relations": [],
            }));
        };
        let ambiguous = ambiguity.is_some();
        let truncated = ambiguity.as_ref().is_some_and(EntityCandidates::truncated);

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
            "ambiguous": ambiguous,
            // Always present, so a caller can branch on the list rather
            // than on a flag. One element when the name was unique.
            "candidates": candidates_json,
            "candidates_truncated": truncated,
            "selected_by": if ambiguous { "oldest_created_at" } else { "only_match" },
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
    /// Memory space holding the observation. Omit (or pass `default`)
    /// for the personal-global default store.
    #[serde(default)]
    pub space: Option<String>,
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
            .store_for(req.space.as_deref())?
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
    /// Memory space holding the entity. Omit (or pass `default`) for
    /// the personal-global default store.
    #[serde(default)]
    pub space: Option<String>,
}

const FORGET_ENTITY_DESC: &str =
    "Retire every observation for an entity by name through the audited lifecycle. The entity and \
     immutable history are retained; normal recall no longer returns its retired observations. \
     Returns the observation count retired and an `entity not found` error for unknown names.";

/// Handler for the `openmemory_forget_entity` MCP tool. Retires the entity's
/// observations without exposing irreversible destruction to an agent.
pub struct OpenMemoryForgetEntityTool;
impl Tool for OpenMemoryForgetEntityTool {
    const NAME: &'static str = "openmemory_forget_entity";
    const SUMMARY: &'static str = "Retire one entity's observations while retaining history.";
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
            .store_for(req.space.as_deref())?
            .retire_entity_observations(&req.entity)
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
    /// Memory space holding both entities. Omit (or pass `default`)
    /// for the personal-global default store. Relations never span
    /// spaces.
    #[serde(default)]
    pub space: Option<String>,
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
        let memory = server.store_for(req.space.as_deref())?;
        let from = memory
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
        let to = memory
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
        let rel_id = memory
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
    /// Memory space holding the observation. Omit (or pass `default`)
    /// for the personal-global default store.
    #[serde(default)]
    pub space: Option<String>,
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
            .store_for(req.space.as_deref())?
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

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct StatusInput {
    /// Memory space to report on. Omit (or pass `default`) for the
    /// personal-global default store.
    #[serde(default)]
    pub space: Option<String>,
}

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
            input_schema: schema_for::<StatusInput>(),
            annotations: Some(read_only_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: StatusInput = if args.is_null() {
            StatusInput::default()
        } else {
            parse_args(args)?
        };
        let s = server
            .store_for(req.space.as_deref())?
            .status()
            .map_err(map_memory_err)?;
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
    fn validity_window_plumbs_through_remember_and_recall() {
        let s = server();
        // A superseded fact (valid until t=1000) and its replacement.
        let _ = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "platform",
                "observations": [
                    {
                        "content": "the platform datastore is Postgres",
                        "valid_from": 100,
                        "valid_until": 1000
                    },
                    { "content": "the platform datastore is SQLite per tenant" }
                ]
            }),
        )
        .unwrap();

        let text_of = |r: &crate::protocol::CallToolResult| match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };

        // Current-truth recall (valid_at omitted = now): only the
        // replacement fact survives the validity filter.
        let now = OpenMemoryRecallTool::call(
            &s,
            json!({"query": "platform datastore", "mode": "keyword"}),
        )
        .unwrap();
        let body = text_of(&now);
        assert!(body.contains("SQLite per tenant"));
        assert!(!body.contains("is Postgres"));

        // As-of recall pinned inside the old window reaches the old fact
        // and drops the replacement (whose valid_from defaults to its
        // write time, after t=500).
        let asof = OpenMemoryRecallTool::call(
            &s,
            json!({"query": "platform datastore", "mode": "keyword", "valid_at": 500}),
        )
        .unwrap();
        let body = text_of(&asof);
        assert!(body.contains("is Postgres"));
        assert!(!body.contains("SQLite per tenant"));
        assert!(body.contains("\"valid_until\": 1000"));
    }

    #[test]
    fn inverted_validity_window_is_rejected() {
        let s = server();
        let err = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "platform",
                "observations": [
                    {"content": "x", "valid_from": 1000, "valid_until": 100}
                ]
            }),
        )
        .unwrap_err();
        assert!(err.message.contains("valid_from must be <= valid_until"));
    }

    /// Server fixture with the write-behind context engine enabled. Uses
    /// an on-disk store: the engine's flushers live on other threads and
    /// `:memory:` SQLite is private to the opening handle.
    fn engine_server() -> OpenMemoryMcpServer {
        let dir = tempfile::tempdir().unwrap().keep();
        let mut config = Config::default();
        config.engine.enabled = true;
        config.engine.shards = 4;
        let store = openmemory_graph::MemoryStore::open(&config, &dir).unwrap();
        OpenMemoryMcpServer::from_memory_with_engine(config, Arc::new(store)).unwrap()
    }

    /// Server fixture over a 2-domain partitioned store with the engine
    /// enabled: the full scale-out configuration.
    fn partitioned_server() -> OpenMemoryMcpServer {
        let dir = tempfile::tempdir().unwrap().keep();
        let mut config = Config::default();
        config.engine.enabled = true;
        config.engine.domains = 2;
        config.engine.shards = 4;
        let domains = openmemory_engine::partition::DomainStore::open(&config, &dir, 2).unwrap();
        OpenMemoryMcpServer::from_domain_store(config, Arc::new(domains)).unwrap()
    }

    #[test]
    fn partitioned_remember_recall_status_round_trip() {
        let s = partitioned_server();
        // Write enough entities to populate both domains.
        for i in 0..20 {
            OpenMemoryRememberTool::call(
                &s,
                json!({
                    "entity": format!("entity-{i}"),
                    "observations": [format!("partitioned observation number {i}")],
                }),
            )
            .unwrap();
        }

        // durable_ack default: read-your-writes through the fan-out.
        let r =
            OpenMemoryRecallTool::call(&s, json!({"query": "partitioned observation", "limit": 5}))
                .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("partitioned observation"), "got: {body}");

        let st = OpenMemoryStatusTool::call(&s, json!({})).unwrap();
        let body = match &st.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"total_observations\": 20"), "got: {body}");

        // Entity lookup routes by name across domains.
        let g = OpenMemoryGetEntityTool::call(&s, json!({"entity": "entity-7"})).unwrap();
        let body = match &g.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("partitioned observation number 7"));
    }

    #[test]
    fn remember_via_engine_returns_receipt_and_persists() {
        let s = engine_server();
        let r = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "EngineWrite",
                "observations": ["routed through the write-behind lane"],
            }),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"accepted\": true"), "got: {body}");
        assert!(body.contains("\"durable\": true"), "durable_ack default on");

        // durable_ack waited for the commit: read-your-writes holds.
        let g = OpenMemoryGetEntityTool::call(&s, json!({"entity": "EngineWrite"})).unwrap();
        let body = match &g.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("write-behind lane"));
    }

    #[test]
    fn remember_via_engine_durable_false_is_fire_and_forget() {
        let s = engine_server();
        let r = OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "FastAck",
                "observations": ["ack before commit"],
                "durable": false,
            }),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"durable\": false"), "got: {body}");
        // The write still lands after the engine drains.
        s.engine().unwrap().quiesce();
        let status = s.memory().status().unwrap();
        assert_eq!(status.total_observations, 1);
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

    /// Build the collision the live profile store actually contains:
    /// `ProjectAlpha` as both a `concept` and a `project`. See
    /// `openmemory-graph/tests/name_ambiguity.rs` for provenance.
    fn server_with_the_real_collision() -> OpenMemoryMcpServer {
        let s = server();
        OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "ProjectAlpha",
                "entity_type": "concept",
                "observations": ["the architectural pattern"],
            }),
        )
        .unwrap();
        OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "ProjectAlpha",
                "entity_type": "project",
                "observations": ["the shipping codebase"],
            }),
        )
        .unwrap();
        s
    }

    fn body_of(result: &CallToolResult) -> Value {
        let text = match &result.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        serde_json::from_str(&text).expect("tool payload is JSON")
    }

    #[test]
    fn get_entity_on_an_ambiguous_name_says_a_choice_was_made() {
        let s = server_with_the_real_collision();
        let body =
            body_of(&OpenMemoryGetEntityTool::call(&s, json!({"entity": "ProjectAlpha"})).unwrap());

        assert_eq!(body["found"], json!(true));
        assert_eq!(
            body["ambiguous"],
            json!(true),
            "the tool must not pretend the name was unique"
        );
        assert_eq!(body["selected_by"], json!("oldest_created_at"));

        let candidates = body["candidates"].as_array().expect("candidates array");
        assert_eq!(candidates.len(), 2);
        let types: Vec<&str> = candidates
            .iter()
            .map(|c| c["entity_type"].as_str().unwrap())
            .collect();
        assert!(
            types.contains(&"concept") && types.contains(&"project"),
            "{types:?}"
        );
        for candidate in candidates {
            assert!(
                !candidate["id"].as_str().unwrap().is_empty(),
                "an agent needs ids to disambiguate with"
            );
        }
        // The returned bundle is one of the candidates, not something else.
        let selected = body["entity"]["id"].as_str().unwrap();
        assert!(candidates.iter().any(|c| c["id"] == selected));
    }

    #[test]
    fn get_entity_on_a_unique_name_reports_no_ambiguity() {
        let s = server();
        OpenMemoryRememberTool::call(
            &s,
            json!({"entity": "SoloEntity", "observations": ["only one"]}),
        )
        .unwrap();
        let body =
            body_of(&OpenMemoryGetEntityTool::call(&s, json!({"entity": "SoloEntity"})).unwrap());
        assert_eq!(body["ambiguous"], json!(false));
        assert_eq!(body["selected_by"], json!("only_match"));
        assert_eq!(body["candidates"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn forget_entity_on_an_ambiguous_name_refuses_and_lists_the_candidates() {
        let s = server_with_the_real_collision();
        let error = OpenMemoryForgetEntityTool::call(&s, json!({"entity": "ProjectAlpha"}))
            .expect_err("a destructive call must not guess");

        assert_eq!(error.code, -32005);
        let data = error.data.expect("structured disambiguation data");
        assert_eq!(data["reason"], json!("ambiguous_entity_name"));
        assert_eq!(data["name"], json!("ProjectAlpha"));
        assert_eq!(data["candidates"].as_array().unwrap().len(), 2);

        // Nothing was retired.
        let body =
            body_of(&OpenMemoryGetEntityTool::call(&s, json!({"entity": "ProjectAlpha"})).unwrap());
        assert_eq!(body["candidates"].as_array().unwrap().len(), 2);
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
