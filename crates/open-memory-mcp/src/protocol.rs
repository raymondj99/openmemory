//! JSON-RPC 2.0 framing + the small slice of MCP-specific data types we
//! emit and consume.
//!
//! Why hand-roll instead of using `rmcp`? Every published `rmcp` requires
//! rustc 1.88+; open-memory is pinned to MSRV 1.85. The wire format is
//! small enough to encode directly — JSON-RPC 2.0 + the four MCP methods
//! we serve. This module owns the wire types only; dispatch lives in
//! [`crate::tools`] and transport in [`crate::stdio`] / [`crate::http`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC request identifier. The spec allows string, integer, or null —
/// we represent it as a `serde_json::Value` so callers don't have to plumb
/// every variant through.
pub type RequestId = Value;

/// Inbound JSON-RPC 2.0 request. `id` is `None` when the message is a
/// notification.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Outbound JSON-RPC 2.0 response. Either `result` or `error` is set.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    /// `id` is always serialised (as `null` when absent) — the JSON-RPC 2.0
    /// spec mandates `id` on every response, and many parsers reject the
    /// missing-`id` shape.
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    #[must_use]
    pub fn success(id: Option<RequestId>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: id.unwrap_or(Value::Null),
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub fn error(id: Option<RequestId>, err: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id: id.unwrap_or(Value::Null),
            result: None,
            error: Some(err),
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    /// `INVALID_PARAMS = -32602`. Use when the request reaches the right
    /// method but its arguments are wrong.
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    /// `METHOD_NOT_FOUND = -32601`. Use when the method name isn't in the
    /// dispatch table.
    #[must_use]
    pub fn method_not_found(name: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {name}"),
            data: None,
        }
    }

    /// `INTERNAL_ERROR = -32603`. Catch-all for handler failures.
    #[must_use]
    pub fn internal_error(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }

    /// `PARSE_ERROR = -32700`. Use when the wire payload isn't valid JSON.
    #[must_use]
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: message.into(),
            data: None,
        }
    }
}

// ---------------------------------------------------------------------------
// MCP-specific data types
// ---------------------------------------------------------------------------

/// `serverInfo` block in the `initialize` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// `capabilities` block in the `initialize` response. We only advertise
/// `tools` — resources and prompts land in v0.2.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
}

impl ServerCapabilities {
    /// Standard tools-only capability set.
    #[must_use]
    pub fn with_tools() -> Self {
        Self {
            tools: Some(ToolsCapability {
                list_changed: Some(false),
            }),
        }
    }
}

/// Tool capability block. `list_changed` says whether the server fires
/// `notifications/tools/list_changed` after a tool list mutation. We never
/// add or remove tools at runtime, so it's always `false`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// One row in a `tools/list` response. Matches the MCP shape exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// MCP "tool annotations" hints surfaced by some clients. We populate
/// `read_only` for graph reads and `destructive` for `forget`/`forget_entity`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolAnnotations {
    #[serde(rename = "readOnlyHint", skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(rename = "destructiveHint", skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
    #[serde(rename = "idempotentHint", skip_serializing_if = "Option::is_none")]
    pub idempotent: Option<bool>,
}

/// One content block in a `tools/call` response. We only emit `text`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Content {
    Text { text: String },
}

impl Content {
    #[must_use]
    pub fn text(s: impl Into<String>) -> Self {
        Content::Text { text: s.into() }
    }
}

/// Standard `tools/call` success payload. Wrap it in
/// `serde_json::to_value(...)` to feed the caller's JsonRpcResponse.
#[derive(Debug, Clone, Serialize)]
pub struct CallToolResult {
    pub content: Vec<Content>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl CallToolResult {
    #[must_use]
    pub fn success(content: Vec<Content>) -> Self {
        Self {
            content,
            is_error: None,
        }
    }

    #[must_use]
    pub fn text(s: impl Into<String>) -> Self {
        Self::success(vec![Content::text(s)])
    }
}

/// `tools/call` request parameters.
#[derive(Debug, Clone, Deserialize)]
pub struct CallToolRequestParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonrpc_response_success_serialises() {
        let r =
            JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!({"ok": true}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"result\":{\"ok\":true}"));
        assert!(!s.contains("\"error\""));
    }

    #[test]
    fn jsonrpc_response_error_serialises() {
        let r = JsonRpcResponse::error(
            Some(serde_json::json!(1)),
            JsonRpcError::method_not_found("foo"),
        );
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"error\":{"));
        assert!(s.contains("-32601"));
        assert!(!s.contains("\"result\""));
    }

    #[test]
    fn parse_request_with_no_params() {
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).unwrap();
        assert_eq!(req.method, "initialize");
        assert!(req.params.is_null());
    }

    #[test]
    fn parse_notification_has_no_id() {
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(req.id.is_none());
    }

    #[test]
    fn content_serialises_with_type_tag() {
        let c = Content::text("hello");
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"type\":\"text\""));
        assert!(s.contains("\"text\":\"hello\""));
    }

    #[test]
    fn call_tool_result_success_omits_is_error() {
        let r = CallToolResult::text("hi");
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("isError"));
    }

    #[test]
    fn capabilities_with_tools_includes_list_changed() {
        let c = ServerCapabilities::with_tools();
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"listChanged\":false"));
    }
}
