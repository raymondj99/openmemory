//! Tool registry for the open-memory MCP server.
//!
//! Each MCP tool is a zero-sized unit struct implementing the [`Tool`] trait.
//! `Tool::descriptor()` produces the wire-level [`ToolDescriptor`] (used for
//! `tools/list` and JSON-Schema validation), and `Tool::call()` handles the
//! actual invocation. Both operations live next to the input type so schema
//! and dispatch can never drift.
//!
//! [`build_router`] is the **only** place tools are registered, and
//! [`server_instructions`] derives the human-readable tool index from the
//! same registration list. Adding a new tool is a one-line change.
//!
//! Submodule layout (filled in across commits 32–34):
//! - [`memory`] — knowledge-graph tools
//! - [`index`] — free-text indexing tools
//! - [`maintenance`] — `open_memory_consolidate`

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::protocol::{
    CallToolRequestParams, CallToolResult, JsonRpcError, ToolAnnotations, ToolDescriptor,
};
use crate::OpenMemoryMcpServer;

pub mod index;
pub mod maintenance;
pub mod memory;

/// MCP-tool trait. Each tool is a zero-sized unit struct implementing this
/// trait. The trait keeps three concerns colocated:
///
/// 1. The MCP descriptor ([`Self::descriptor`]) — name, description, JSON
///    Schema, annotations.
/// 2. The handler ([`Self::call`]) that takes deserialised arguments and
///    returns a [`CallToolResult`].
/// 3. The instructions metadata ([`Self::SUMMARY`]) used when rendering
///    the server's `instructions` block.
///
/// Because both `descriptor()` and `call()` come from the same impl block,
/// an agent can never see a tool advertised in `tools/list` that the
/// router can't actually dispatch (or the other way round).
pub trait Tool: Send + Sync + 'static {
    /// Tool name as advertised over MCP (e.g. `"open_memory_remember"`).
    const NAME: &'static str;
    /// One-line summary used in the `get_info` instructions block.
    const SUMMARY: &'static str;
    /// Tool group for the instructions block.
    const GROUP: ToolGroup;

    /// Build the wire-level descriptor.
    fn descriptor() -> ToolDescriptor;

    /// Invoke the tool. `args` is the raw `arguments` field from
    /// `tools/call`. Implementations should deserialise it into their
    /// strongly-typed input struct and translate any business-logic errors
    /// to a [`JsonRpcError`].
    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError>;
}

/// Logical grouping for the instructions block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolGroup {
    Memory,
    Index,
    Maintenance,
}

impl ToolGroup {
    fn header(self) -> &'static str {
        match self {
            Self::Memory => "MEMORY TOOLS:",
            Self::Index => "INDEX TOOLS:",
            Self::Maintenance => "MAINTENANCE TOOLS:",
        }
    }
}

/// Boxed-trait alias for the per-tool handler signature.
pub(crate) type DynHandler = dyn Fn(&OpenMemoryMcpServer, Value) -> Result<CallToolResult, JsonRpcError>
    + Send
    + Sync;

/// Single registry entry. The entries are kept in registration order so the
/// instructions block matches the dispatch table.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) group: ToolGroup,
    pub(crate) name: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) descriptor: ToolDescriptor,
    pub(crate) handler: Arc<DynHandler>,
}

#[allow(dead_code)]
pub(crate) fn entry<T: Tool>() -> Entry {
    Entry {
        group: T::GROUP,
        name: T::NAME,
        summary: T::SUMMARY,
        descriptor: T::descriptor(),
        handler: Arc::new(T::call),
    }
}

/// Tool router — owns the dispatch table from name -> handler and the list
/// of descriptors served by `tools/list`.
#[derive(Clone)]
pub struct ToolRouter {
    entries: Vec<Entry>,
    by_name: HashMap<&'static str, usize>,
}

impl ToolRouter {
    pub(crate) fn new(entries: Vec<Entry>) -> Self {
        let by_name = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name, i))
            .collect();
        Self { entries, by_name }
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no tools are registered. Used by the scaffolding test.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All descriptors in registration order — what `tools/list` returns.
    pub fn list_descriptors(&self) -> Vec<&ToolDescriptor> {
        self.entries.iter().map(|e| &e.descriptor).collect()
    }

    /// Names of registered tools, registration order.
    pub fn names(&self) -> Vec<&'static str> {
        self.entries.iter().map(|e| e.name).collect()
    }

    /// Dispatch a `tools/call` payload through the registered handler.
    pub fn call(
        &self,
        server: &OpenMemoryMcpServer,
        params: Value,
    ) -> Result<Value, JsonRpcError> {
        let parsed: CallToolRequestParams = serde_json::from_value(params)
            .map_err(|e| JsonRpcError::invalid_params(format!("invalid tools/call params: {e}")))?;
        let idx = self
            .by_name
            .get(parsed.name.as_str())
            .copied()
            .ok_or_else(|| JsonRpcError::method_not_found(&parsed.name))?;
        let handler = &self.entries[idx].handler;
        let result = handler(server, parsed.arguments)?;
        serde_json::to_value(&result)
            .map_err(|e| JsonRpcError::internal_error(format!("encode tools/call result: {e}")))
    }
}

/// Build the [`ToolRouter`] populated with every tool the server exposes.
/// The matching list in [`registry`] keeps [`server_instructions`] in sync.
pub fn build_router() -> ToolRouter {
    ToolRouter::new(registry())
}

/// Render the human-readable tool index used by
/// [`crate::OpenMemoryMcpServer`]'s `initialize` instructions block.
pub fn server_instructions() -> String {
    let mut out = String::from(
        "open-memory is a local persistent agent memory + hybrid text search engine.\n\n",
    );
    let entries = registry();
    let mut current_group: Option<ToolGroup> = None;
    for e in &entries {
        if Some(e.group) != current_group {
            if current_group.is_some() {
                out.push('\n');
            }
            out.push_str(e.group.header());
            out.push('\n');
            current_group = Some(e.group);
        }
        out.push_str("- ");
        out.push_str(e.name);
        out.push_str(": ");
        out.push_str(e.summary);
        out.push('\n');
    }
    out.push_str(
        "\nWORKFLOW:\n\
         1. Use open_memory_remember to store facts about named entities.\n\
         2. Use open_memory_recall to find facts by natural-language query.\n\
         3. Use open_memory_index_text + open_memory_search for free-text content.\n\
         4. Run open_memory_consolidate periodically to dedup + decay-prune.\n",
    );
    out
}

/// Flat ordered list of every tool exposed by the server. Memory first,
/// then index, then maintenance. Each submodule contributes via
/// `register_all`.
fn registry() -> Vec<Entry> {
    let mut v = Vec::new();
    memory::register_all(&mut v);
    index::register_all(&mut v);
    maintenance::register_all(&mut v);
    v
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Helper for tools that emit a JSON object as their result. Renders pretty
/// JSON in a single text content block.
#[allow(dead_code)]
pub(crate) fn json_text_result(value: &Value) -> Result<CallToolResult, JsonRpcError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| JsonRpcError::internal_error(format!("encode json: {e}")))?;
    Ok(CallToolResult::text(text))
}

/// Round to two decimal places — keeps tool output compact.
#[allow(dead_code)]
pub(crate) fn round2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

/// Build the standard `inputSchema` for a tool's input struct via
/// [`schemars`]. Wraps the schema as a JSON `Value` so it can ride along
/// with the descriptor.
#[allow(dead_code)]
pub(crate) fn schema_for<T: schemars::JsonSchema>() -> Value {
    let schema = schemars::schema_for!(T);
    serde_json::to_value(&schema).unwrap_or(serde_json::json!({"type": "object"}))
}

/// Build an empty-input JSON Schema for tools that take no arguments.
#[allow(dead_code)]
pub(crate) fn empty_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    })
}

/// Annotation for read-only tools.
#[allow(dead_code)]
pub(crate) fn read_only_annotations() -> ToolAnnotations {
    ToolAnnotations {
        read_only: Some(true),
        destructive: Some(false),
        idempotent: Some(true),
    }
}

/// Annotation for mutating but idempotent tools.
#[allow(dead_code)]
pub(crate) fn write_annotations() -> ToolAnnotations {
    ToolAnnotations {
        read_only: Some(false),
        destructive: Some(false),
        idempotent: Some(false),
    }
}

/// Annotation for destructive (delete-style) tools.
#[allow(dead_code)]
pub(crate) fn destructive_annotations() -> ToolAnnotations {
    ToolAnnotations {
        read_only: Some(false),
        destructive: Some(true),
        idempotent: Some(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_starts_empty_until_tools_land() {
        let names: Vec<_> = registry().iter().map(|e| e.name).collect();
        assert!(names.is_empty(), "expected empty pre-tool registry: {names:?}");
    }

    #[test]
    fn instructions_block_has_workflow_section() {
        let text = server_instructions();
        assert!(text.contains("open-memory is a local persistent agent memory"));
        assert!(text.contains("WORKFLOW:"));
    }

    #[test]
    fn router_is_empty_at_scaffold() {
        let r = build_router();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }
}
