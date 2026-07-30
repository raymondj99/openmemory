//! `openmemory_supersede` — the correction write surface.
//!
//! Corrections are supersessions, never deletions
//! (`plan/17-trilayer-memory-principles.md` section 5, measured in the
//! T15 correction experiments): the superseded fact keeps its content
//! and gets a closed validity window, the superseding fact starts its
//! own window, and a `supersedes` relation records lineage. After this
//! call, current-truth retrieval (`valid_at` = now) skips the old
//! fact, `as_of`-pinned retrieval still reaches it, and
//! `openmemory_retrieve`'s successor promotion reroutes any surviving
//! stale ranking to the new fact. Nothing is lost: correction-as-
//! deletion measured current/history MRR 1.00/0.38; supersession
//! measures 1.00/1.00 (T15 F-15, F-21).
//!
//! ## Atomicity honesty
//!
//! Today's store has no in-place validity update, so the closed window
//! is expressed append-only: write the windowed copy, then tombstone
//! the open original. The steps are ordered so that **no partial
//! failure loses information** — a crash mid-sequence leaves at worst
//! a visible duplicate of the old fact (one open, one windowed), which
//! a retry heals. The single-transaction form (in-place stamp + edge +
//! audit revision in one commit) is specified in plan/18 section 3.2
//! and lands with the plan/16 changeset machinery.
//!
//! Step order: (1) write the new fact, (2) write the supersedes edge,
//! (3) write the windowed copy of each superseded observation,
//! (4) tombstone the open originals.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use openmemory_graph::{EntityType, MemoryError, Observation, ObservationInput};

use crate::params::EntityTypeParam;
use crate::protocol::{CallToolResult, JsonRpcError, ToolDescriptor};
use crate::OpenMemoryMcpServer;

use super::{json_text_result, schema_for, write_annotations, Tool, ToolGroup};

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, JsonRpcError> {
    serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("invalid arguments: {e}")))
}

fn map_memory_err(e: MemoryError) -> JsonRpcError {
    JsonRpcError::internal_error(format!("memory error: {e}"))
}

/// Input for `openmemory_supersede`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SupersedeInput {
    /// Entity whose open observation(s) are being superseded.
    pub old_entity: String,
    /// Type of the superseded entity. Defaults to `concept`.
    #[serde(default)]
    pub old_entity_type: Option<EntityTypeParam>,
    /// Supersede only this observation. When omitted, every open
    /// (non-tombstoned, no `valid_until`) observation on `old_entity`
    /// is superseded — the common "this fact is outdated" case.
    #[serde(default)]
    pub old_observation_id: Option<String>,
    /// Entity carrying the superseding fact. May equal `old_entity`
    /// (updating a fact in place on one entity).
    pub new_entity: String,
    /// Type of the superseding entity. Defaults to `concept`.
    #[serde(default)]
    pub new_entity_type: Option<EntityTypeParam>,
    /// The superseding fact's content.
    pub new_content: String,
    /// Optional title for the superseding fact.
    #[serde(default)]
    pub new_title: Option<String>,
    /// Unix seconds at which the old fact stopped being true and the
    /// new one started. Defaults to now.
    #[serde(default)]
    pub superseded_at: Option<i64>,
    /// Origin tag for audit. Defaults to `"supersession"`.
    #[serde(default)]
    pub source: Option<String>,
    /// Memory space holding both facts. Omit (or pass `default`) for
    /// the personal-global default store. Supersession never spans
    /// spaces.
    #[serde(default)]
    pub space: Option<String>,
}

/// Rebuild an [`ObservationInput`] carrying an observation's content
/// and metadata with a closed validity window. `valid_from` defaults
/// to the original's own start, falling back to when it was observed.
fn windowed_copy(obs: &Observation, superseded_at: i64) -> ObservationInput {
    let mut inp = ObservationInput::new(&obs.content)
        .with_confidence(obs.confidence)
        .with_source(&obs.source)
        .with_memory_tier(obs.memory_tier)
        .with_validity(
            // Clamp: if the caller backdates the supersession to before
            // the fact was recorded, the window degenerates rather than
            // inverting (the store rejects inverted windows).
            Some(obs.valid_from.unwrap_or(obs.observed_at).min(superseded_at)),
            Some(superseded_at),
        );
    if let Some(t) = obs.title.as_deref() {
        inp = inp.with_title(t);
    }
    if let Some(s) = obs.summary.as_deref() {
        inp = inp.with_summary(s);
    }
    if let Some(i) = obs.importance {
        inp = inp.with_importance(i);
    }
    if !obs.concepts.is_empty() {
        inp = inp.with_concepts(obs.concepts.clone());
    }
    if !obs.source_files.is_empty() {
        inp = inp.with_source_files(obs.source_files.clone());
    }
    inp
}

const SUPERSEDE_DESC: &str = "Correct a fact by supersession, never deletion: the old \
     observation keeps its content under a closed validity window (still reachable via \
     `as_of` on openmemory_retrieve / `valid_at` on openmemory_recall), the new fact starts \
     its own window, and a `supersedes` relation records lineage so retrieval reroutes stale \
     rankings to the successor. Omit `old_observation_id` to supersede every open observation \
     on the entity. Use this instead of openmemory_forget whenever a fact changed rather than \
     never having been true.";

/// Handler for the `openmemory_supersede` MCP tool.
pub struct OpenMemorySupersedeTool;
impl Tool for OpenMemorySupersedeTool {
    const NAME: &'static str = "openmemory_supersede";
    const SUMMARY: &'static str =
        "Correct a fact: close the old observation's validity window, add the successor.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: SUPERSEDE_DESC.into(),
            input_schema: schema_for::<SupersedeInput>(),
            annotations: Some(write_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: SupersedeInput = parse_args(args)?;
        if req.old_entity.trim().is_empty() || req.new_entity.trim().is_empty() {
            return Err(JsonRpcError::invalid_params(
                "old_entity and new_entity must not be empty",
            ));
        }
        if req.new_content.trim().is_empty() {
            return Err(JsonRpcError::invalid_params(
                "new_content must not be empty",
            ));
        }
        let superseded_at = match req.superseded_at {
            Some(t) if t <= 0 => {
                return Err(JsonRpcError::invalid_params(
                    "superseded_at must be a positive Unix timestamp",
                ));
            }
            Some(t) => t,
            None => now_secs(),
        };
        let old_type = req
            .old_entity_type
            .map_or(EntityType::Concept, |p| p.to_entity_type());
        let new_type = req
            .new_entity_type
            .map_or(EntityType::Concept, |p| p.to_entity_type());
        let source = req.source.as_deref().unwrap_or("supersession").to_string();

        let memory = server.store_for(req.space.as_deref())?;

        // Resolve the old entity and collect its open observations
        // BEFORE writing anything, so the new fact can never appear in
        // its own supersede set (old_entity may equal new_entity).
        let old = memory
            .get_entity_by_name_and_type(&req.old_entity, old_type)
            .map_err(map_memory_err)?
            .ok_or_else(|| JsonRpcError {
                code: -32004,
                message: format!("old_entity not found: {:?} ({})", req.old_entity, old_type),
                data: None,
            })?;
        let targets: Vec<Observation> = memory
            .get_entity_observations(&old.id)
            .map_err(map_memory_err)?
            .into_iter()
            .filter(|o| !o.tombstoned && o.valid_until.is_none())
            .filter(|o| {
                req.old_observation_id
                    .as_deref()
                    .is_none_or(|id| o.id == id)
            })
            .collect();
        if targets.is_empty() {
            return Err(JsonRpcError::invalid_params(
                "nothing to supersede: no open observation matched (already superseded, \
                 tombstoned, or wrong old_observation_id)",
            ));
        }

        // Step 1: the superseding fact, valid from the supersession
        // instant onward.
        let mut new_obs = ObservationInput::new(&req.new_content)
            .with_source(&source)
            .with_validity(Some(superseded_at), None);
        if let Some(t) = req.new_title.as_deref() {
            new_obs = new_obs.with_title(t);
        }
        let outcome = memory
            .remember(&req.new_entity, new_type, &[new_obs], &[], &source)
            .map_err(map_memory_err)?;

        // Step 2: lineage. Self-supersession (old == new entity) is a
        // valid in-place fact update; the store rejects self-relations,
        // so the edge is recorded only across distinct entities.
        let relation_id = if outcome.entity_id != old.id {
            Some(
                memory
                    .add_relation(&outcome.entity_id, &old.id, "supersedes", None, &source)
                    .map_err(map_memory_err)?,
            )
        } else {
            None
        };

        // Steps 3 and 4 per superseded observation: windowed copy
        // first, then tombstone the open original. A crash between the
        // two leaves a visible duplicate, never a gap.
        let mut superseded_ids = Vec::with_capacity(targets.len());
        for obs in &targets {
            memory
                .remember(
                    &req.old_entity,
                    old_type,
                    &[windowed_copy(obs, superseded_at)],
                    &[],
                    &source,
                )
                .map_err(map_memory_err)?;
            memory.forget(&obs.id).map_err(map_memory_err)?;
            superseded_ids.push(obs.id.clone());
        }

        json_text_result(&json!({
            "old_entity": req.old_entity,
            "new_entity": req.new_entity,
            "new_entity_id": outcome.entity_id,
            "new_observation_ids": outcome.observation_ids,
            "superseded_observation_ids": superseded_ids,
            "supersedes_relation_id": relation_id,
            "superseded_at": superseded_at,
            "atomic": false,
        }))
    }
}

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Register this module's tools (memory group).
pub(crate) fn register_all(v: &mut Vec<super::Entry>) {
    v.push(super::entry::<OpenMemorySupersedeTool>());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use openmemory_core::config::Config;
    use openmemory_graph::{MemoryStore, RecallFilters, SearchMode};

    fn server() -> OpenMemoryMcpServer {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store))
    }

    fn recall_contents(s: &OpenMemoryMcpServer, q: &str, valid_at: Option<i64>) -> Vec<String> {
        let mut f = RecallFilters::new();
        f.mode = Some(SearchMode::KeywordOnly);
        f.record_access = false;
        f.valid_at = valid_at;
        s.memory()
            .recall(q, 10, &f)
            .unwrap()
            .into_iter()
            .map(|r| r.observation.content)
            .collect()
    }

    #[test]
    fn supersede_preserves_history_and_serves_current_truth() {
        let s = server();
        s.memory()
            .remember(
                "db-decision",
                EntityType::Concept,
                &[ObservationInput::new("the datastore is Postgres")
                    .with_validity(Some(500_000), None)],
                &[],
                "test",
            )
            .unwrap();
        let r = OpenMemorySupersedeTool::call(
            &s,
            json!({
                "old_entity": "db-decision",
                "new_entity": "db-decision-v2",
                "new_content": "the datastore is SQLite per tenant",
                "superseded_at": 1_000_000
            }),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        assert!(body.contains("\"supersedes_relation_id\""));

        // Current truth: only the successor.
        let now = recall_contents(&s, "datastore", None);
        assert!(now.iter().any(|c| c.contains("SQLite per tenant")));
        assert!(!now.iter().any(|c| c.contains("is Postgres")));

        // Pinned before the supersession instant: only the old fact.
        let then = recall_contents(&s, "datastore", Some(999_999));
        assert!(then.iter().any(|c| c.contains("is Postgres")));
        assert!(!then.iter().any(|c| c.contains("SQLite per tenant")));
    }

    #[test]
    fn self_supersession_updates_a_fact_in_place() {
        let s = server();
        s.memory()
            .remember(
                "team-size",
                EntityType::Fact,
                &[ObservationInput::new("the team has four engineers")
                    .with_validity(Some(1_500_000), None)],
                &[],
                "test",
            )
            .unwrap();
        let r = OpenMemorySupersedeTool::call(
            &s,
            json!({
                "old_entity": "team-size", "old_entity_type": "fact",
                "new_entity": "team-size", "new_entity_type": "fact",
                "new_content": "the team has six engineers",
                "superseded_at": 2_000_000
            }),
        )
        .unwrap();
        let body = match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        };
        // No self-relation is written; the validity windows carry it.
        assert!(body.contains("\"supersedes_relation_id\": null"));
        let now = recall_contents(&s, "team engineers", None);
        assert!(now.iter().any(|c| c.contains("six engineers")));
        assert!(!now.iter().any(|c| c.contains("four engineers")));
        let then = recall_contents(&s, "team engineers", Some(1_999_999));
        assert!(then.iter().any(|c| c.contains("four engineers")));
    }

    #[test]
    fn nothing_open_to_supersede_is_a_clear_error() {
        let s = server();
        s.memory()
            .remember(
                "done-deal",
                EntityType::Concept,
                &[{
                    let mut o = ObservationInput::new("already closed fact");
                    o.valid_until = Some(5);
                    o
                }],
                &[],
                "test",
            )
            .unwrap();
        let err = OpenMemorySupersedeTool::call(
            &s,
            json!({
                "old_entity": "done-deal",
                "new_entity": "irrelevant",
                "new_content": "x"
            }),
        )
        .unwrap_err();
        assert!(
            err.message.contains("nothing to supersede"),
            "{}",
            err.message
        );
    }

    #[test]
    fn missing_old_entity_is_not_created_implicitly() {
        let s = server();
        let err = OpenMemorySupersedeTool::call(
            &s,
            json!({
                "old_entity": "ghost",
                "new_entity": "new-thing",
                "new_content": "content"
            }),
        )
        .unwrap_err();
        assert!(
            err.message.contains("old_entity not found"),
            "{}",
            err.message
        );
    }
}
