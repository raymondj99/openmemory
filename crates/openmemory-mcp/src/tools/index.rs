//! Free-text index tools.
//!
//! Three tools wrap the [`openmemory_index::engine::OpenEngine`] that the
//! [`openmemory_graph::MemoryStore`] holds:
//!
//! - `openmemory_index_text` — store arbitrary text under a caller-supplied
//!   URI for keyword-and-semantic recall via `openmemory_search`.
//! - `openmemory_search` — hybrid (vector + keyword) search across every
//!   indexed source.
//! - `openmemory_delete` — drop a URI from the index.
//!
//! These talk to the same engine the graph crate's recall path uses, so
//! agents can mix structured memories (entities + observations) with
//! free-text content under the same hybrid-search surface.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use openmemory_index::traits::IndexEntry;

use crate::params::SearchModeParam;
use crate::protocol::{CallToolResult, JsonRpcError, ToolDescriptor};
use crate::tools::{
    destructive_annotations, entry, json_text_result, read_only_annotations, schema_for,
    write_annotations, Entry, Tool, ToolGroup,
};
use crate::OpenMemoryMcpServer;

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, JsonRpcError> {
    serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("invalid arguments: {e}")))
}

// ---------------------------------------------------------------------------
// openmemory_index_text
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct IndexTextInput {
    /// Stable identifier for this content. URI shape recommended:
    /// `note://2026-05-04/architecture-notes`. URIs starting with
    /// `memory://observation/` are reserved for the graph store and
    /// rejected here.
    pub uri: String,
    /// The text to index. Empty content is rejected.
    pub text: String,
    /// Caller-defined chunk index. Defaults to 0.
    #[serde(default)]
    pub chunk_index: Option<u32>,
}

const INDEX_TEXT_DESC: &str =
    "Index a piece of text under a caller-supplied URI for hybrid (vector + keyword) recall \
     via openmemory_search. Embeds the text with the active embedding model when one is \
     loaded; otherwise stores keyword-only. URIs starting with `memory://observation/` are \
     reserved for the graph store.";

/// Handler for the `openmemory_index_text` MCP tool. Indexes raw text
/// under a caller-supplied URI for hybrid recall.
pub struct OpenMemoryIndexTextTool;
impl Tool for OpenMemoryIndexTextTool {
    const NAME: &'static str = "openmemory_index_text";
    const SUMMARY: &'static str =
        "Index arbitrary text under a caller-supplied URI for hybrid recall.";
    const GROUP: ToolGroup = ToolGroup::Index;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: INDEX_TEXT_DESC.into(),
            input_schema: schema_for::<IndexTextInput>(),
            annotations: Some(write_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: IndexTextInput = parse_args(args)?;
        if req.uri.trim().is_empty() {
            return Err(JsonRpcError::invalid_params("uri must not be empty"));
        }
        if req.uri.starts_with("memory://observation/") {
            return Err(JsonRpcError::invalid_params(
                "the memory://observation/ URI prefix is reserved for the graph store",
            ));
        }
        if req.text.trim().is_empty() {
            return Err(JsonRpcError::invalid_params("text must not be empty"));
        }

        let vector = server.memory().embed_document(&req.text);
        let mut entry = IndexEntry::new(req.uri.clone(), req.text.clone())
            .with_chunk_index(req.chunk_index.unwrap_or(0));
        if !vector.is_empty() {
            entry = entry.with_vector(vector);
        }
        // The facade routes by URI hash AND invalidates its recall
        // cache; never write to a member store directly.
        server
            .memory()
            .index_insert(entry)
            .map_err(|e| JsonRpcError::internal_error(format!("index insert failed: {e}")))?;
        json_text_result(&json!({ "uri": req.uri, "indexed": true }))
    }
}

// ---------------------------------------------------------------------------
// openmemory_search
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchInput {
    /// Search query. Empty string rejected.
    pub query: String,
    /// Maximum results to return. Defaults to 10; clamped to [1, 50].
    #[serde(default)]
    pub limit: Option<u32>,
    /// Optional URI prefix filter — drops results whose URI does not start
    /// with this string. Useful for scoping a query to a sub-tree of
    /// indexed content (`note://`, `doc://`, etc.).
    #[serde(default)]
    pub uri_prefix: Option<String>,
    /// Minimum score threshold in [0.0, 1.0]. Default 0.0 (no floor).
    #[serde(default)]
    pub min_score: Option<f32>,
    /// Search mode. Defaults to `hybrid`.
    #[serde(default)]
    pub mode: Option<SearchModeParam>,
}

const SEARCH_DESC: &str =
    "Hybrid (vector + keyword) search across every indexed source. Returns results sorted by \
     relevance with optional URI-prefix scoping and a minimum-score floor. Set `mode` to \
     `keyword` to skip the vector path; `vector` to skip keyword. With no embedding model \
     loaded, hybrid falls through to keyword-only and vector returns no results.";

/// Handler for the `openmemory_search` MCP tool. Hybrid search across
/// every indexed source with URI-prefix scoping and score floor.
pub struct OpenMemorySearchTool;
impl Tool for OpenMemorySearchTool {
    const NAME: &'static str = "openmemory_search";
    const SUMMARY: &'static str =
        "Hybrid search across indexed text. Filter by URI prefix or score floor.";
    const GROUP: ToolGroup = ToolGroup::Index;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: SEARCH_DESC.into(),
            input_schema: schema_for::<SearchInput>(),
            annotations: Some(read_only_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: SearchInput = parse_args(args)?;
        if req.query.trim().is_empty() {
            return Err(JsonRpcError::invalid_params("query must not be empty"));
        }
        let limit = req.limit.unwrap_or(10).clamp(1, 50) as usize;
        let mode = req.mode.unwrap_or_default().to_mode();

        let vector = server.memory().embed_query(&req.query);
        let scoped_by_prefix = req.uri_prefix.as_deref().is_some_and(|p| !p.is_empty());
        let has_min_score = req.min_score.is_some_and(|s| s > 0.0);
        let needs_overfetch = scoped_by_prefix || has_min_score;
        let fetch = if needs_overfetch {
            let (vector_count, keyword_count) = server
                .memory()
                .index_counts()
                .map_err(|e| JsonRpcError::internal_error(format!("count failed: {e}")))?;
            usize::try_from(vector_count.max(keyword_count))
                .unwrap_or(4_096)
                .min(limit.saturating_mul(32))
                .min(4_096)
        } else {
            limit
        }
        .max(limit);
        let mut results = server
            .memory()
            .index_search(&vector, &req.query, fetch, mode, 0)
            .map_err(|e| JsonRpcError::internal_error(format!("search failed: {e}")))?;

        if let Some(prefix) = req.uri_prefix.as_deref().filter(|p| !p.is_empty()) {
            results.retain(|r| r.uri.starts_with(prefix));
        }
        if let Some(floor) = req.min_score {
            results.retain(|r| r.score >= floor);
        }
        results.truncate(limit);

        let payload: Vec<Value> = results
            .into_iter()
            .map(|r| {
                json!({
                    "uri": r.uri,
                    "text": r.text,
                    "chunk_index": r.chunk_index,
                    "score": super::round2(r.score),
                })
            })
            .collect();
        json_text_result(&json!({
            "results": payload,
            "limit": limit,
        }))
    }
}

// ---------------------------------------------------------------------------
// openmemory_delete
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DeleteInput {
    /// URI to remove from the index.
    pub uri: String,
}

const DELETE_DESC: &str =
    "Remove a URI from the search index. Drops the entry from both the vector and full-text \
     stores. Returns the number of records actually removed (0 if the URI wasn't indexed).";

/// Handler for the `openmemory_delete` MCP tool. Drops a URI from
/// both the vector and full-text stores.
pub struct OpenMemoryDeleteTool;
impl Tool for OpenMemoryDeleteTool {
    const NAME: &'static str = "openmemory_delete";
    const SUMMARY: &'static str = "Drop a URI from the search index.";
    const GROUP: ToolGroup = ToolGroup::Index;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: DELETE_DESC.into(),
            input_schema: schema_for::<DeleteInput>(),
            annotations: Some(destructive_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: DeleteInput = parse_args(args)?;
        if req.uri.trim().is_empty() {
            return Err(JsonRpcError::invalid_params("uri must not be empty"));
        }
        let removed = server
            .memory()
            .index_delete(&req.uri)
            .map_err(|e| JsonRpcError::internal_error(format!("delete failed: {e}")))?;
        json_text_result(&json!({
            "uri": req.uri,
            "removed": removed,
        }))
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub(crate) fn register_all(out: &mut Vec<Entry>) {
    out.push(entry::<OpenMemoryIndexTextTool>());
    out.push(entry::<OpenMemorySearchTool>());
    out.push(entry::<OpenMemoryDeleteTool>());
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

    fn body(r: &CallToolResult) -> String {
        match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        }
    }

    #[test]
    fn descriptors_use_openmemory_prefix() {
        for d in [
            OpenMemoryIndexTextTool::descriptor(),
            OpenMemorySearchTool::descriptor(),
            OpenMemoryDeleteTool::descriptor(),
        ] {
            assert!(d.name.starts_with("openmemory_"));
        }
    }

    #[test]
    fn index_then_search_round_trip() {
        let s = server();
        OpenMemoryIndexTextTool::call(&s, json!({"uri": "note://hello", "text": "hello world"}))
            .unwrap();
        let r = OpenMemorySearchTool::call(
            &s,
            json!({"query": "hello", "limit": 5, "mode": "keyword"}),
        )
        .unwrap();
        let text = body(&r);
        assert!(text.contains("note://hello"));
        assert!(text.contains("hello world"));
    }

    #[test]
    fn search_uri_prefix_filter() {
        let s = server();
        OpenMemoryIndexTextTool::call(&s, json!({"uri": "note://a", "text": "alpha mention"}))
            .unwrap();
        OpenMemoryIndexTextTool::call(&s, json!({"uri": "doc://a", "text": "alpha mention"}))
            .unwrap();
        let r = OpenMemorySearchTool::call(
            &s,
            json!({
                "query": "alpha",
                "uri_prefix": "note://",
                "mode": "keyword",
            }),
        )
        .unwrap();
        let text = body(&r);
        assert!(text.contains("note://a"));
        assert!(!text.contains("doc://a"));
    }

    #[test]
    fn search_uri_prefix_overfetches_before_filtering() {
        use openmemory_core::testing::{Embedder, FakeEmbedder};
        let store = MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_embedder(Arc::new(FakeEmbedder::new(32)) as Arc<dyn Embedder>);
        let s = OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store));

        OpenMemoryIndexTextTool::call(
            &s,
            json!({"uri": "doc://top", "text": "alpha bravo charlie"}),
        )
        .unwrap();
        OpenMemoryIndexTextTool::call(
            &s,
            json!({"uri": "note://second", "text": "alpha bravo charlie extra"}),
        )
        .unwrap();

        let r = OpenMemorySearchTool::call(
            &s,
            json!({
                "query": "alpha bravo charlie",
                "limit": 1,
                "uri_prefix": "note://",
                "mode": "vector",
            }),
        )
        .unwrap();
        let text = body(&r);
        assert!(text.contains("note://second"), "{text}");
        assert!(!text.contains("doc://top"), "{text}");
    }

    #[test]
    fn delete_drops_entry_from_results() {
        let s = server();
        OpenMemoryIndexTextTool::call(&s, json!({"uri": "note://gone", "text": "to be deleted"}))
            .unwrap();
        let r = OpenMemoryDeleteTool::call(&s, json!({"uri": "note://gone"})).unwrap();
        let text = body(&r);
        assert!(text.contains("\"removed\": 1") || text.contains("\"removed\": 0"));

        let r =
            OpenMemorySearchTool::call(&s, json!({"query": "deleted", "mode": "keyword"})).unwrap();
        assert!(!body(&r).contains("note://gone"));
    }

    #[test]
    fn index_text_rejects_reserved_observation_prefix() {
        let s = server();
        let err = OpenMemoryIndexTextTool::call(
            &s,
            json!({"uri": "memory://observation/abc", "text": "x"}),
        )
        .unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn index_text_rejects_empty_input() {
        let s = server();
        for args in [
            json!({"uri": "", "text": "x"}),
            json!({"uri": "note://x", "text": ""}),
        ] {
            let err = OpenMemoryIndexTextTool::call(&s, args).unwrap_err();
            assert_eq!(err.code, -32602);
        }
    }

    #[test]
    fn search_rejects_empty_query() {
        let s = server();
        let err = OpenMemorySearchTool::call(&s, json!({"query": ""})).unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn index_text_with_embedder_stores_vector() {
        use openmemory_core::testing::{Embedder, FakeEmbedder};
        let store = MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_embedder(Arc::new(FakeEmbedder::new(16)) as Arc<dyn Embedder>);
        let s = OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store));

        OpenMemoryIndexTextTool::call(&s, json!({"uri": "note://vec", "text": "lorem ipsum"}))
            .unwrap();

        let (vector_count, _) = s.memory().index_counts().unwrap();
        assert_eq!(vector_count, 1, "vector backend must see the new row");
    }

    #[test]
    fn search_returns_vector_only_hit_when_no_keyword_overlap() {
        use openmemory_core::testing::{Embedder, FakeEmbedder};
        let store = MemoryStore::open_in_memory(&Config::default())
            .unwrap()
            .with_embedder(Arc::new(FakeEmbedder::new(16)) as Arc<dyn Embedder>);
        let s = OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store));

        OpenMemoryIndexTextTool::call(
            &s,
            json!({"uri": "note://nokw", "text": "alpha bravo charlie"}),
        )
        .unwrap();

        // Identical text on both sides matches via the deterministic
        // FakeEmbedder regardless of the keyword backend; selecting
        // mode=vector proves the vector arm actually runs.
        let r = OpenMemorySearchTool::call(
            &s,
            json!({"query": "alpha bravo charlie", "mode": "vector"}),
        )
        .unwrap();
        assert!(body(&r).contains("note://nokw"));
    }

    #[test]
    fn search_min_score_filters_low_results() {
        let s = server();
        OpenMemoryIndexTextTool::call(&s, json!({"uri": "note://a", "text": "alpha"})).unwrap();
        // A min_score above any plausible BM25 keyword score should drop
        // every result. We don't assume an exact upper bound (FTS5 BM25
        // scores aren't normalised) — pick a value that's plainly higher
        // than what any single-term match returns.
        let r = OpenMemorySearchTool::call(
            &s,
            json!({
                "query": "alpha",
                "min_score": 1_000.0,
                "mode": "keyword",
            }),
        )
        .unwrap();
        assert!(body(&r).contains("\"results\": []"));
    }
}
