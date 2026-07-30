//! Space tools — `openmemory_space`.
//!
//! Managed memory spaces are isolation silos beside the personal-global
//! default: each space owns a complete, physically separate store under
//! `<profile>/spaces/<name>/`. Every memory and index tool accepts an
//! optional `space` parameter naming its target; recall/retrieve
//! additionally accept a `read_spaces` ordered read set (at most four)
//! whose results are fused by deterministic rank interleaving. Omitting
//! `space` always addresses the personal-global default, so legacy
//! callers are unaffected.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::protocol::{CallToolResult, JsonRpcError, ToolDescriptor};
use crate::tools::{
    entry, json_text_result, schema_for, write_annotations, Entry, Tool, ToolGroup,
};
use crate::OpenMemoryMcpServer;

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, JsonRpcError> {
    serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("invalid arguments: {e}")))
}

/// Action selector for `openmemory_space`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpaceAction {
    /// Create a new managed space.
    Create,
    /// List managed spaces (the personal-global default is implicit).
    List,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SpaceInput {
    /// What to do: `create` a named space, or `list` existing spaces.
    pub action: SpaceAction,
    /// Space name for `create`. Lowercase letters, digits, and dashes;
    /// 1..=64 characters. `default` is reserved for the personal-global
    /// store.
    #[serde(default)]
    pub name: Option<String>,
}

const SPACE_DESC: &str =
    "Manage memory spaces: isolated silos beside the personal-global default store. Each \
     space owns a physically separate store; entities, observations, relations, and indexed \
     text never leak across spaces. `create` makes a new named space; `list` shows existing \
     ones. Address a space by passing `space` to the memory/index tools, and compose \
     cross-space reads with `read_spaces` on recall/retrieve (results are fused by rank \
     interleaving, never by comparing scores across spaces).";

/// Handler for the `openmemory_space` MCP tool.
pub struct OpenMemorySpaceTool;
impl Tool for OpenMemorySpaceTool {
    const NAME: &'static str = "openmemory_space";
    const SUMMARY: &'static str =
        "Create or list memory spaces: isolated stores beside the default.";
    const GROUP: ToolGroup = ToolGroup::Spaces;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: SPACE_DESC.into(),
            input_schema: schema_for::<SpaceInput>(),
            annotations: Some(write_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: SpaceInput = parse_args(args)?;
        let manager = server.space_manager().ok_or_else(|| {
            JsonRpcError::invalid_params("memory spaces are not available on this server instance")
        })?;
        match req.action {
            SpaceAction::Create => {
                let name = req.name.as_deref().ok_or_else(|| {
                    JsonRpcError::invalid_params("`name` is required for action=create")
                })?;
                let info = manager
                    .create(name)
                    .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
                json_text_result(&json!({
                    "created": true,
                    "name": info.name,
                    "space_id": info.space_id.to_string(),
                    "created_at": info.created_at,
                }))
            }
            SpaceAction::List => {
                let spaces: Vec<Value> = manager
                    .list()
                    .map_err(|e| JsonRpcError::internal_error(e.to_string()))?
                    .into_iter()
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "space_id": s.space_id.to_string(),
                            "created_at": s.created_at,
                        })
                    })
                    .collect();
                json_text_result(&json!({
                    "default": "default",
                    "spaces": spaces,
                }))
            }
        }
    }
}

pub(crate) fn register_all(out: &mut Vec<Entry>) {
    out.push(entry::<OpenMemorySpaceTool>());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use openmemory_core::config::Config;
    use openmemory_engine::space::SpaceManager;
    use openmemory_graph::MemoryStore;
    use serde_json::json;

    fn server_with_spaces(dir: &std::path::Path) -> OpenMemoryMcpServer {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store))
            .with_space_manager(SpaceManager::new(Config::default(), dir, 1))
    }

    fn text_of(result: &CallToolResult) -> String {
        match &result.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        }
    }

    #[test]
    fn descriptor_uses_openmemory_prefix() {
        let d = OpenMemorySpaceTool::descriptor();
        assert_eq!(d.name, "openmemory_space");
        assert!(d.description.contains("isolated"));
    }

    #[test]
    fn create_then_list_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let s = server_with_spaces(dir.path());

        let created =
            OpenMemorySpaceTool::call(&s, json!({"action": "create", "name": "research"})).unwrap();
        let body = text_of(&created);
        assert!(body.contains("\"created\": true"));
        assert!(body.contains("\"research\""));

        let listed = OpenMemorySpaceTool::call(&s, json!({"action": "list"})).unwrap();
        let body = text_of(&listed);
        assert!(body.contains("\"research\""));
        assert!(body.contains("\"default\": \"default\""));
    }

    #[test]
    fn create_requires_name() {
        let dir = tempfile::tempdir().unwrap();
        let s = server_with_spaces(dir.path());
        let err = OpenMemorySpaceTool::call(&s, json!({"action": "create"})).unwrap_err();
        assert!(err.message.contains("`name` is required"));
    }

    #[test]
    fn invalid_name_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let s = server_with_spaces(dir.path());
        let err = OpenMemorySpaceTool::call(&s, json!({"action": "create", "name": "../escape"}))
            .unwrap_err();
        assert!(err.message.contains("lowercase"));
    }

    #[test]
    fn missing_manager_is_typed() {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        let s = OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store));
        let err = OpenMemorySpaceTool::call(&s, json!({"action": "list"})).unwrap_err();
        assert!(err.message.contains("not available"));
    }

    #[test]
    fn remember_and_recall_are_space_isolated_through_the_tools() {
        use crate::tools::memory::{OpenMemoryRecallTool, OpenMemoryRememberTool};

        let dir = tempfile::tempdir().unwrap();
        let s = server_with_spaces(dir.path());
        OpenMemorySpaceTool::call(&s, json!({"action": "create", "name": "work"})).unwrap();

        OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "Client Deadline",
                "observations": ["ship the report by friday"],
                "space": "work",
            }),
        )
        .unwrap();
        OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "Grocery List",
                "observations": ["buy oat milk and coffee"],
            }),
        )
        .unwrap();

        // The space sees its own fact and not the default's.
        let in_space = text_of(
            &OpenMemoryRecallTool::call(&s, json!({"query": "ship the report", "space": "work"}))
                .unwrap(),
        );
        assert!(in_space.contains("Client Deadline"));
        let cross = text_of(
            &OpenMemoryRecallTool::call(&s, json!({"query": "buy oat milk", "space": "work"}))
                .unwrap(),
        );
        assert!(!cross.contains("Grocery List"));

        // The default sees its own fact and not the space's.
        let in_default =
            text_of(&OpenMemoryRecallTool::call(&s, json!({"query": "buy oat milk"})).unwrap());
        assert!(in_default.contains("Grocery List"));
        let cross =
            text_of(&OpenMemoryRecallTool::call(&s, json!({"query": "ship the report"})).unwrap());
        assert!(!cross.contains("Client Deadline"));
    }

    #[test]
    fn layered_recall_interleaves_across_spaces() {
        use crate::tools::memory::{OpenMemoryRecallTool, OpenMemoryRememberTool};

        let dir = tempfile::tempdir().unwrap();
        let s = server_with_spaces(dir.path());
        OpenMemorySpaceTool::call(&s, json!({"action": "create", "name": "left"})).unwrap();
        OpenMemorySpaceTool::call(&s, json!({"action": "create", "name": "right"})).unwrap();

        for (space, entity) in [("left", "Left Fact"), ("right", "Right Fact")] {
            OpenMemoryRememberTool::call(
                &s,
                json!({
                    "entity": entity,
                    "observations": ["the shared unusual phrase xylophone"],
                    "space": space,
                }),
            )
            .unwrap();
        }

        let r = OpenMemoryRecallTool::call(
            &s,
            json!({
                "query": "shared unusual phrase xylophone",
                "read_spaces": ["left", "right"],
            }),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&text_of(&r)).unwrap();
        assert_eq!(v["fusion"], json!("rank_interleave"));
        let rows = v["results"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{rows:?}");
        // Rank interleaving in read-set order: left's rank-0 first.
        assert_eq!(rows[0]["space"], json!("left"));
        assert_eq!(rows[0]["entity_name"], json!("Left Fact"));
        assert_eq!(rows[1]["space"], json!("right"));
        assert_eq!(rows[1]["entity_name"], json!("Right Fact"));
    }

    #[test]
    fn read_spaces_validation_is_enforced() {
        use crate::tools::memory::OpenMemoryRecallTool;

        let dir = tempfile::tempdir().unwrap();
        let s = server_with_spaces(dir.path());
        OpenMemorySpaceTool::call(&s, json!({"action": "create", "name": "a"})).unwrap();

        let err = OpenMemoryRecallTool::call(&s, json!({"query": "q", "read_spaces": ["a", "a"]}))
            .unwrap_err();
        assert!(err.message.contains("more than once"));

        let err = OpenMemoryRecallTool::call(
            &s,
            json!({"query": "q", "read_spaces": ["a", "b", "c", "d", "e"]}),
        )
        .unwrap_err();
        assert!(err.message.contains("at most"));

        let err = OpenMemoryRecallTool::call(
            &s,
            json!({"query": "q", "space": "a", "read_spaces": ["a"]}),
        )
        .unwrap_err();
        assert!(err.message.contains("mutually exclusive"));

        let err = OpenMemoryRecallTool::call(&s, json!({"query": "q", "read_spaces": ["ghost"]}))
            .unwrap_err();
        assert!(err.message.contains("does not exist"));
    }

    #[test]
    fn retrieve_supports_spaces_and_layered_reads() {
        use crate::tools::memory::OpenMemoryRememberTool;
        use crate::tools::retrieve::OpenMemoryRetrieveTool;

        let dir = tempfile::tempdir().unwrap();
        let s = server_with_spaces(dir.path());
        OpenMemorySpaceTool::call(&s, json!({"action": "create", "name": "proj"})).unwrap();
        OpenMemoryRememberTool::call(
            &s,
            json!({
                "entity": "parse_config",
                "observations": ["parse_config reads the TOML settings file"],
                "space": "proj",
            }),
        )
        .unwrap();

        // Single-space retrieve resolves inside the space.
        let r = OpenMemoryRetrieveTool::call(
            &s,
            json!({"query": "parse_config", "space": "proj", "engage": true}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&text_of(&r)).unwrap();
        assert_eq!(v["results"][0]["entity_name"], json!("parse_config"));

        // Layered retrieve carries per-space traces and space labels.
        let r = OpenMemoryRetrieveTool::call(
            &s,
            json!({
                "query": "parse_config",
                "read_spaces": ["default", "proj"],
                "engage": true,
            }),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&text_of(&r)).unwrap();
        assert_eq!(v["trace"]["fusion"], json!("rank_interleave"));
        assert_eq!(v["trace"]["read_spaces"], json!(["default", "proj"]));
        let rows = v["results"].as_array().unwrap();
        assert!(rows.iter().any(
            |row| row["space"] == json!("proj") && row["entity_name"] == json!("parse_config")
        ));
    }
}
