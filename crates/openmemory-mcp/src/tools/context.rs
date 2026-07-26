//! Agent-safe context, history, and proposal tools.

use openmemory_graph::{ChangeOperation, ChangeSetDraft, ObjectKind, ObjectRef, SubmitMode};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::protocol::{CallToolResult, JsonRpcError, ToolDescriptor};
use crate::tools::{
    empty_schema, entry, json_text_result, read_only_annotations, schema_for, write_annotations,
    Entry, Tool, ToolGroup,
};
use crate::OpenMemoryMcpServer;

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, JsonRpcError> {
    serde_json::from_value(args)
        .map_err(|error| JsonRpcError::invalid_params(format!("invalid arguments: {error}")))
}

pub struct OpenMemoryContextTool;

impl Tool for OpenMemoryContextTool {
    const NAME: &'static str = "openmemory_context";
    const SUMMARY: &'static str =
        "Show the authorized read layers, write target, roles, and generations.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: "Return the immutable memory context bound to this request. Direct \
                          compatibility servers report one default space."
                .into(),
            input_schema: empty_schema(),
            annotations: Some(read_only_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, _args: Value) -> Result<CallToolResult, JsonRpcError> {
        if let Some(resolved) = server.resolved_context() {
            let grants = resolved
                .context
                .read_set
                .iter()
                .enumerate()
                .map(|(priority, grant)| {
                    json!({
                        "space_id": grant.space.id.to_string(),
                        "owner": grant.space.owner,
                        "context": grant.space.context,
                        "role": format!("{:?}", grant.role).to_lowercase(),
                        "authority_generation": grant.authority_generation,
                        "read_priority": priority,
                    })
                })
                .collect::<Vec<_>>();
            return json_text_result(&json!({
                "profile": resolved.context.profile,
                "principal_id": resolved.context.principal.to_string(),
                "actor_kind": format!("{:?}", resolved.context.actor_kind).to_lowercase(),
                "project_id": resolved.context.project.map(|id| id.to_string()),
                "active_team_id": resolved.context.active_team.as_ref().map(ToString::to_string),
                "read_set": grants,
                "write_target": resolved.context.default_write.to_string(),
                "authorization_generation": resolved.context.authorization_generation,
                "proposal_required": resolved.proposal_required,
            }));
        }
        json_text_result(&json!({
            "profile": "default",
            "read_set": [{
                "space_id": server.memory().space_id().to_string(),
                "read_priority": 0,
            }],
            "write_target": server.memory().space_id().to_string(),
            "compatibility_default": true,
        }))
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HistoryInput {
    pub logical_id: String,
    #[serde(default = "default_kind")]
    pub object_kind: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

fn default_kind() -> String {
    "observation".to_string()
}

pub struct OpenMemoryHistoryTool;

impl Tool for OpenMemoryHistoryTool {
    const NAME: &'static str = "openmemory_history";
    const SUMMARY: &'static str = "Read bounded immutable revision history for one memory object.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: "Return immutable semantic revision metadata for an object in the \
                          request's concrete write space."
                .into(),
            input_schema: schema_for::<HistoryInput>(),
            annotations: Some(read_only_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let request: HistoryInput = parse_args(args)?;
        let kind = parse_kind(&request.object_kind)?;
        let space_id = server.memory().space_id();
        let history = server
            .memory()
            .object_history(
                ObjectRef {
                    kind,
                    logical_id: request.logical_id.clone(),
                },
                None,
                request.limit.unwrap_or(50).clamp(1, 256) as usize,
            )
            .map_err(|error| JsonRpcError::internal_error(error.to_string()))?;
        json_text_result(&json!({
            "space_id": space_id.to_string(),
            "logical_id": request.logical_id,
            "object_kind": request.object_kind,
            "revisions": history.revisions.into_iter().map(|revision| json!({
                "revision_id": revision.revision_id.to_string(),
                "parent_revision_id": revision.parent_revision_id.map(|id| id.to_string()),
                "semantic_hash": format!("blake3:{}", hex(&revision.semantic_hash)),
                "created_by_changeset": revision.created_by_change_set.to_string(),
                "created_at": revision.created_at,
            })).collect::<Vec<_>>(),
            "next_cursor": history.next_cursor.map(|(time, id)| format!("{time}:{id}")),
        }))
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeChangeInput {
    pub idempotency_key: String,
    pub reason: String,
    /// Serialized `ChangeOperation` objects. Unknown fields and operations are
    /// rejected by the graph's strict schema before any proposal is stored.
    pub operations: Vec<Value>,
}

pub struct OpenMemoryProposeChangeTool;

impl Tool for OpenMemoryProposeChangeTool {
    const NAME: &'static str = "openmemory_propose_change";
    const SUMMARY: &'static str =
        "Submit a non-canonical proposal; agents can never approve their own changes.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        let mut annotations = write_annotations();
        annotations.destructive = Some(false);
        ToolDescriptor {
            name: Self::NAME.into(),
            description: "Create a reviewable proposal in exactly one authorized space. This \
                          tool never applies, approves, rejects, or destroys canonical memory."
                .into(),
            input_schema: schema_for::<ProposeChangeInput>(),
            annotations: Some(annotations),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let request: ProposeChangeInput = parse_args(args)?;
        let context = server.resolved_context().ok_or_else(|| {
            JsonRpcError::invalid_params(
                "openmemory_propose_change requires a daemon context capability",
            )
        })?;
        let operations = request
            .operations
            .into_iter()
            .map(|operation| {
                serde_json::from_value::<ChangeOperation>(operation).map_err(|error| {
                    JsonRpcError::invalid_params(format!("invalid change operation: {error}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let draft = ChangeSetDraft {
            idempotency_key: request.idempotency_key,
            space_id: context.context.default_write,
            actor_principal: context.context.principal.clone(),
            actor_kind: context.context.actor_kind,
            authorization_generation: context.context.authorization_generation,
            reason: request.reason,
            source: "mcp:proposal".to_string(),
            operations,
        };
        let receipt = server
            .memory()
            .submit_changeset(&draft, SubmitMode::Propose)
            .map_err(|error| JsonRpcError::internal_error(error.to_string()))?;
        json_text_result(&json!({
            "changeset_id": receipt.id.to_string(),
            "state": "proposed",
            "space_id": context.context.default_write.to_string(),
            "semantic_generation": receipt.semantic_generation,
        }))
    }
}

fn parse_kind(value: &str) -> Result<ObjectKind, JsonRpcError> {
    match value {
        "entity" => Ok(ObjectKind::Entity),
        "observation" => Ok(ObjectKind::Observation),
        "relation" => Ok(ObjectKind::Relation),
        _ => Err(JsonRpcError::invalid_params(
            "object_kind must be entity, observation, or relation",
        )),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn register_all(entries: &mut Vec<Entry>) {
    entries.push(entry::<OpenMemoryContextTool>());
    entries.push(entry::<OpenMemoryHistoryTool>());
    entries.push(entry::<OpenMemoryProposeChangeTool>());
}
