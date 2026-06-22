//! Maintenance tools — `openmemory_consolidate`.
//!
//! Periodic dedup + decay-prune. Idempotent: running twice in a row reports
//! zero merges and zero prunes on the second call. Thin wrapper over
//! [`openmemory_graph::MemoryStore::consolidate`].

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use openmemory_graph::ConsolidateConfig;

use crate::protocol::{CallToolResult, JsonRpcError, ToolDescriptor};
use crate::tools::{
    entry, json_text_result, schema_for, write_annotations, Entry, Tool, ToolGroup,
};
use crate::OpenMemoryMcpServer;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ConsolidateInput {
    /// Override the dedup Jaccard text-similarity threshold. Range
    /// [0.0, 1.0]; default 0.95.
    #[serde(default)]
    pub dedup_threshold: Option<f32>,
    /// Override the decay-prune score floor. Default 0.05.
    #[serde(default)]
    pub prune_floor: Option<f32>,
    /// Minimum age (seconds) for an observation to be eligible for
    /// decay-prune. Default 86400 (1 day).
    #[serde(default)]
    pub min_age_secs: Option<i64>,
}

const CONSOLIDATE_DESC: &str =
    "Run the dedup + decay/prune consolidation pipeline once. Phase 1 merges near-duplicate \
     observations within an entity (Jaccard text similarity >= dedup_threshold). Phase 2 \
     scores every observation with the same Ebbinghaus formula recall uses and tombstones \
     anything below prune_floor, then sweeps tombstones older than the configured TTL and \
     entities that became orphaned. Idempotent: running twice in a row reports zero merges \
     and zero prunes on the second call.";

/// Handler for the `openmemory_consolidate` MCP tool. Runs the
/// dedup + decay-prune sweep over the knowledge graph; idempotent.
pub struct OpenMemoryConsolidateTool;
impl Tool for OpenMemoryConsolidateTool {
    const NAME: &'static str = "openmemory_consolidate";
    const SUMMARY: &'static str = "Dedup + decay-prune the knowledge graph. Idempotent.";
    const GROUP: ToolGroup = ToolGroup::Maintenance;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: CONSOLIDATE_DESC.into(),
            input_schema: schema_for::<ConsolidateInput>(),
            annotations: Some(write_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: ConsolidateInput = serde_json::from_value(args)
            .map_err(|e| JsonRpcError::invalid_params(format!("invalid arguments: {e}")))?;

        let mut cfg = ConsolidateConfig {
            decay_rate: server.memory().decay_rate(),
            ..ConsolidateConfig::default()
        };
        if let Some(t) = req.dedup_threshold {
            cfg.dedup_text_threshold = t.clamp(0.0, 1.0);
        }
        if let Some(p) = req.prune_floor {
            cfg.prune_floor = p;
        }
        if let Some(a) = req.min_age_secs {
            cfg.min_age_secs = a.max(0);
        }

        let report = server
            .memory()
            .consolidate(&cfg)
            .map_err(|e| JsonRpcError::internal_error(e.to_string()))?;

        json_text_result(&json!({
            "duplicates_merged": report.duplicates_merged,
            "observations_pruned": report.observations_pruned,
            "entities_pruned": report.entities_pruned,
            "dedup_threshold": cfg.dedup_text_threshold,
            "prune_floor": cfg.prune_floor,
            "min_age_secs": cfg.min_age_secs,
        }))
    }
}

pub(crate) fn register_all(out: &mut Vec<Entry>) {
    out.push(entry::<OpenMemoryConsolidateTool>());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use openmemory_core::config::Config;
    use openmemory_graph::{EntityType, MemoryStore, ObservationInput};
    use serde_json::json;

    fn server() -> OpenMemoryMcpServer {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store))
    }

    #[test]
    fn descriptor_uses_openmemory_prefix() {
        let d = OpenMemoryConsolidateTool::descriptor();
        assert_eq!(d.name, "openmemory_consolidate");
        assert!(d.description.contains("dedup"));
    }

    #[test]
    fn consolidate_dedups_then_idempotent() {
        let s = server();
        s.memory()
            .remember(
                "X",
                EntityType::Fact,
                &[
                    ObservationInput::new("hello world"),
                    ObservationInput::new("hello world"),
                    ObservationInput::new("hello world"),
                ],
                &[],
                "t",
            )
            .unwrap();
        let r1 = OpenMemoryConsolidateTool::call(&s, json!({})).unwrap();
        let body1 = match &r1.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body1.contains("\"duplicates_merged\": 2"));
        let r2 = OpenMemoryConsolidateTool::call(&s, json!({})).unwrap();
        let body2 = match &r2.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body2.contains("\"duplicates_merged\": 0"));
    }

    #[test]
    fn consolidate_accepts_threshold_overrides() {
        let s = server();
        let r = OpenMemoryConsolidateTool::call(
            &s,
            json!({"dedup_threshold": 0.8, "prune_floor": 0.1, "min_age_secs": 0}),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"dedup_threshold\": 0.8"));
        assert!(body.contains("\"prune_floor\": 0.1"));
        assert!(body.contains("\"min_age_secs\": 0"));
    }
}
