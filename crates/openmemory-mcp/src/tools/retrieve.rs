//! `openmemory_retrieve` — the tri-layer retrieval surface.
//!
//! One orchestration tool over the three stores the server already owns:
//! the concept/gloss layer (graph observations), the free-text index
//! (URI chunks), and the typed relation graph. The pipeline follows
//! `plan/18-trilayer-retrieval-production.md` and the measured findings
//! it cites (T15 v1–v6 in `experiments/trilayer/RESULTS.md`):
//!
//! 1. **Classify** the query into an intent (caller override wins; the
//!    calling agent is the first classifier).
//! 2. **Route** to exactly one primary: gloss recall for name-shaped
//!    queries, index search for content questions, typed edge traversal
//!    for relational questions.
//! 3. **Fill, never fuse**: secondary routes append after the primary,
//!    deduplicated. No score mixing and no multiplicative boosts —
//!    every fused-prior variant measured to date lost (T6, T15 F-4).
//! 4. **Supersession post-pass**: a result whose entity carries an
//!    incoming `supersedes` edge yields its rank to the successor and
//!    is annotated `superseded_by`. Skipped when `as_of` is pinned,
//!    because as-of questions want the superseded fact.
//! 5. **Scale gate**: routing engages only past a store-size threshold;
//!    below it flat index search is at the measured ceiling and
//!    structure adds only risk (T15 F-12, four corpora).
//!
//! Retrieval here never records access feedback (`record_access =
//! false`): repeated identical calls return identical rankings, unlike
//! `openmemory_recall` (T3 measured 67% top-1 churn from the feedback
//! loop).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use openmemory_graph::{EntityType, MemoryError, RecallFilters, SearchMode};

use crate::protocol::{CallToolResult, JsonRpcError, ToolDescriptor};
use crate::OpenMemoryMcpServer;

use super::{json_text_result, read_only_annotations, round2, schema_for, Tool, ToolGroup};

/// Routing engages at or above this many live observations. Below it,
/// flat hybrid index search was at the quality ceiling on every corpus
/// measured (T15 F-12); the threshold is a placeholder until the plan/18
/// T6 scale-bend experiment replaces it with a measured value.
const ENGAGE_MIN_OBSERVATIONS: u64 = 200;

/// How many candidates each route fetches before the fill/truncate pass.
const OVERFETCH_FACTOR: usize = 3;

/// Traversal breadth caps: seeds consulted and neighbors emitted. Small
/// and deterministic on purpose; graph value comes from edge coverage,
/// not walk depth (T6 round 2).
const TRAVERSAL_SEEDS: usize = 4;
const TRAVERSAL_NEIGHBORS: usize = 6;

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, JsonRpcError> {
    serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("invalid arguments: {e}")))
}

fn map_memory_err(e: MemoryError) -> JsonRpcError {
    JsonRpcError::internal_error(format!("memory error: {e}"))
}

/// Query intent. `auto` lets the server classify; the other values are
/// caller overrides and always win. In MCP deployments the calling
/// agent is the best classifier — pass the intent when you know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetrieveIntentParam {
    /// Classify server-side from query shape.
    #[default]
    Auto,
    /// Name-shaped lookup (identifier, title): gloss layer first.
    Lookup,
    /// Content question: free-text index first.
    Content,
    /// Relationship question: typed edge traversal first.
    Relational,
}

/// Input for `openmemory_retrieve`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RetrieveInput {
    /// Natural-language query.
    pub query: String,
    /// Maximum results. Defaults to 10; clamped to [1, 50].
    #[serde(default)]
    pub limit: Option<u32>,
    /// Answer as of this Unix timestamp: observations whose validity
    /// window excludes the instant are filtered out and supersession
    /// promotion is disabled (as-of questions want the superseded
    /// fact). Omit for current truth.
    #[serde(default)]
    pub as_of: Option<i64>,
    /// Intent override. Defaults to `auto` (server-side classification).
    #[serde(default)]
    pub intent: Option<RetrieveIntentParam>,
    /// Force the routing pipeline on (`true`) or off (`false`)
    /// regardless of store size. Omit to use the store-size gate.
    #[serde(default)]
    pub engage: Option<bool>,
}

/// Resolved intent after classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    Lookup,
    Content,
    Relational,
}

impl Intent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lookup => "lookup",
            Self::Content => "content",
            Self::Relational => "relational",
        }
    }
}

/// True when the query contains an identifier-shaped token: snake_case,
/// an internal lowercase-to-uppercase transition (camelCase and
/// PascalCase — the latter was the measured T15 F-10 classifier miss),
/// a path separator inside a token, `::`, `!`, or a backtick span.
fn looks_like_identifier(query: &str) -> bool {
    if query.contains("::") || query.contains('!') || query.contains('`') {
        return true;
    }
    for token in query.split_whitespace() {
        let bytes: Vec<char> = token.chars().collect();
        for w in bytes.windows(3) {
            if w[0].is_ascii_alphanumeric() && w[1] == '_' && w[2].is_ascii_alphanumeric() {
                return true;
            }
        }
        for w in bytes.windows(2) {
            if w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase() {
                return true;
            }
            if w[0].is_ascii_alphanumeric() && w[1] == '/' {
                return true;
            }
        }
    }
    false
}

/// Conservative relational-cue check. Deliberately narrow: cue lists
/// misfire across domains (T15 F-13), so anything not clearly
/// relational falls through to the content route, and callers who know
/// better pass `intent: relational`.
fn looks_relational(query: &str) -> bool {
    const CUES: &[&str] = &[
        "depends on",
        "depend on",
        "relies on",
        "what does",
        "which crate",
        "which project",
        "which component",
        "which module",
        "who maintains",
        "part of",
        "supersedes",
        "superseded",
    ];
    let lower = query.to_lowercase();
    CUES.iter().any(|c| lower.contains(c))
}

fn classify(query: &str, requested: RetrieveIntentParam) -> Intent {
    match requested {
        RetrieveIntentParam::Lookup => Intent::Lookup,
        RetrieveIntentParam::Content => Intent::Content,
        RetrieveIntentParam::Relational => Intent::Relational,
        RetrieveIntentParam::Auto => {
            if looks_like_identifier(query) {
                Intent::Lookup
            } else if looks_relational(query) {
                Intent::Relational
            } else {
                Intent::Content
            }
        }
    }
}

/// One ranked result before serialization. Graph results carry entity
/// identity; index results carry a URI. The two never merge.
#[derive(Debug, Clone)]
struct Item {
    /// `Some` for graph-layer results.
    entity: Option<(String, String)>, // (entity_id, entity_name)
    entity_type: Option<EntityType>,
    /// `Some` for index-layer results.
    uri: Option<String>,
    snippet: String,
    score: Option<f32>,
    route: &'static str,
    /// Relation annotation for traversal results, e.g.
    /// `"via depends_on from codex"`.
    via: Option<String>,
    superseded_by: Option<String>,
}

impl Item {
    fn key(&self) -> String {
        match (&self.entity, &self.uri) {
            (Some((_, name)), _) => format!("entity:{name}"),
            (_, Some(uri)) => format!("uri:{uri}"),
            _ => format!("snippet:{}", self.snippet),
        }
    }
}

fn snippet_of(text: &str) -> String {
    const MAX: usize = 240;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let cut: String = text.chars().take(MAX).collect();
    format!("{cut}…")
}

/// Gloss route: hybrid recall over graph observations. Deterministic
/// (`record_access = false`) and validity-aware via `as_of`.
fn gloss_route(
    server: &OpenMemoryMcpServer,
    query: &str,
    fetch: usize,
    as_of: Option<i64>,
) -> Result<Vec<Item>, JsonRpcError> {
    gloss_route_with(server, query, fetch, as_of, true)
}

/// Gloss recall with explicit control over spreading activation. The
/// traversal route disables spreading for its seeds: it performs its
/// own typed one-hop expansion, and spread results would surface
/// neighbors as unannotated gloss rows before the edge walk can label
/// them.
fn gloss_route_with(
    server: &OpenMemoryMcpServer,
    query: &str,
    fetch: usize,
    as_of: Option<i64>,
    spreading: bool,
) -> Result<Vec<Item>, JsonRpcError> {
    let mut filters = RecallFilters::new();
    filters.record_access = false;
    filters.valid_at = as_of;
    filters.spreading_activation = spreading;
    let hits = server
        .memory()
        .recall(query, fetch, &filters)
        .map_err(map_memory_err)?;
    Ok(hits
        .into_iter()
        .map(|h| Item {
            entity: Some((h.observation.entity_id.clone(), h.entity_name.clone())),
            entity_type: Some(h.entity_type),
            uri: None,
            snippet: snippet_of(&h.observation.content),
            score: Some(h.score),
            route: "gloss",
            via: None,
            superseded_by: None,
        })
        .collect())
}

/// Content route: hybrid search over the free-text index. Chunks carry
/// no validity metadata yet (plan/18 3.3), so `as_of` does not filter
/// here.
fn content_route(
    server: &OpenMemoryMcpServer,
    query: &str,
    fetch: usize,
) -> Result<Vec<Item>, JsonRpcError> {
    let vector = server.memory().embed_query(query);
    let results = server
        .memory()
        .index_search(&vector, query, fetch, SearchMode::Hybrid, 0)
        .map_err(|e| JsonRpcError::internal_error(format!("search failed: {e}")))?;
    Ok(results
        .into_iter()
        .map(|r| Item {
            entity: None,
            entity_type: None,
            uri: Some(r.uri),
            snippet: snippet_of(&r.text),
            score: Some(r.score),
            route: "content",
            via: None,
            superseded_by: None,
        })
        .collect())
}

/// Traversal route: recall seeds, then one deterministic hop along
/// typed edges. Non-structural edges (`references`, `depends_on`, …)
/// outrank `part_of`; neighbors surface with their first live
/// observation as the snippet and a `via` annotation naming the edge.
fn traversal_route(
    server: &OpenMemoryMcpServer,
    query: &str,
    as_of: Option<i64>,
) -> Result<Vec<Item>, JsonRpcError> {
    let seeds = gloss_route_with(server, query, TRAVERSAL_SEEDS * 2, as_of, false)?;
    let mut out: Vec<Item> = Vec::new();
    let mut seen_entities: Vec<String> = Vec::new();

    for seed in seeds.iter().take(TRAVERSAL_SEEDS * 2) {
        if let Some((_, name)) = &seed.entity {
            if !seen_entities.contains(name) {
                seen_entities.push(name.clone());
                out.push(seed.clone());
            }
        }
    }

    // (priority, neighbor_id, via) — lower priority sorts first.
    let mut neighbors: Vec<(u8, String, String)> = Vec::new();
    for seed in seeds.iter().take(TRAVERSAL_SEEDS) {
        let Some((seed_id, seed_name)) = &seed.entity else {
            continue;
        };
        let relations = server
            .memory()
            .get_entity_relations(seed_id)
            .map_err(map_memory_err)?;
        for rel in relations {
            let (other_id, direction) = if rel.from_entity == *seed_id {
                (rel.to_entity.clone(), "to")
            } else {
                (rel.from_entity.clone(), "from")
            };
            let priority = u8::from(rel.relation_type == "part_of");
            let via = format!("via {} ({direction}) {seed_name}", rel.relation_type);
            if !neighbors.iter().any(|(_, id, _)| *id == other_id) {
                neighbors.push((priority, other_id, via));
            }
        }
    }
    neighbors.sort_by_key(|(p, _, _)| *p);

    for (_, neighbor_id, via) in neighbors.into_iter().take(TRAVERSAL_NEIGHBORS) {
        let Some(entity) = server
            .memory()
            .get_entity_by_id(&neighbor_id)
            .map_err(map_memory_err)?
        else {
            continue;
        };
        if seen_entities.contains(&entity.name) {
            continue;
        }
        seen_entities.push(entity.name.clone());
        let gloss = server
            .memory()
            .get_entity_observations(&neighbor_id)
            .map_err(map_memory_err)?
            .into_iter()
            .find(|o| !o.tombstoned)
            .map(|o| o.content)
            .unwrap_or_default();
        out.push(Item {
            entity: Some((neighbor_id, entity.name)),
            entity_type: Some(entity.entity_type),
            uri: None,
            snippet: snippet_of(&gloss),
            score: None,
            route: "traversal",
            via: Some(via),
            superseded_by: None,
        });
    }
    Ok(out)
}

/// Successor promotion (T15 F-19): any graph result whose entity has an
/// incoming `supersedes` edge yields its rank to the successor and is
/// annotated. Returns the number of promotions performed.
fn promote_successors(
    server: &OpenMemoryMcpServer,
    items: &mut Vec<Item>,
) -> Result<usize, JsonRpcError> {
    let mut promotions = 0;
    let mut i = 0;
    while i < items.len() {
        let Some((entity_id, _)) = items[i].entity.clone() else {
            i += 1;
            continue;
        };
        if items[i].superseded_by.is_some() {
            i += 1;
            continue;
        }
        let relations = server
            .memory()
            .get_entity_relations(&entity_id)
            .map_err(map_memory_err)?;
        let successor_id = relations
            .iter()
            .find(|r| r.relation_type == "supersedes" && r.to_entity == entity_id)
            .map(|r| r.from_entity.clone());
        let Some(successor_id) = successor_id else {
            i += 1;
            continue;
        };
        let Some(successor) = server
            .memory()
            .get_entity_by_id(&successor_id)
            .map_err(map_memory_err)?
        else {
            i += 1;
            continue;
        };
        items[i].superseded_by = Some(successor.name.clone());
        let already_ranked_higher = items[..i]
            .iter()
            .any(|it| matches!(&it.entity, Some((id, _)) if *id == successor_id));
        if !already_ranked_higher {
            // Pull the successor up to this rank: reuse its later entry
            // if present, otherwise materialize it from the store.
            let later = items[i + 1..]
                .iter()
                .position(|it| matches!(&it.entity, Some((id, _)) if *id == successor_id));
            let promoted = if let Some(offset) = later {
                items.remove(i + 1 + offset)
            } else {
                let gloss = server
                    .memory()
                    .get_entity_observations(&successor_id)
                    .map_err(map_memory_err)?
                    .into_iter()
                    .find(|o| !o.tombstoned)
                    .map(|o| o.content)
                    .unwrap_or_default();
                Item {
                    entity: Some((successor_id.clone(), successor.name.clone())),
                    entity_type: Some(successor.entity_type),
                    uri: None,
                    snippet: snippet_of(&gloss),
                    score: None,
                    route: "supersession",
                    via: Some(format!("supersedes {}", items[i].key())),
                    superseded_by: None,
                }
            };
            items.insert(i, promoted);
            promotions += 1;
            i += 1; // step past the promoted successor onto the stale row
        }
        i += 1;
    }
    Ok(promotions)
}

fn append_dedup(out: &mut Vec<Item>, extra: Vec<Item>) {
    for item in extra {
        if !out.iter().any(|existing| existing.key() == item.key()) {
            out.push(item);
        }
    }
}

const RETRIEVE_DESC: &str = "Tri-layer retrieval over the knowledge graph, the free-text index, \
     and typed relations in one call. Classifies the query (or takes an explicit `intent`), \
     routes it to the best layer, appends the other layers as fallback, and applies \
     supersession-aware post-processing: superseded facts yield their rank to their successor \
     and are annotated, unless `as_of` pins a past instant (then validity filtering applies \
     instead). Pass `intent` and `as_of` when you know them — the caller is the best \
     classifier. Deterministic: never records access feedback.";

/// Handler for the `openmemory_retrieve` MCP tool.
pub struct OpenMemoryRetrieveTool;
impl Tool for OpenMemoryRetrieveTool {
    const NAME: &'static str = "openmemory_retrieve";
    const SUMMARY: &'static str =
        "Routed tri-layer retrieval: gloss, content, and relation traversal with supersession.";
    const GROUP: ToolGroup = ToolGroup::Memory;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: Self::NAME.into(),
            description: RETRIEVE_DESC.into(),
            input_schema: schema_for::<RetrieveInput>(),
            annotations: Some(read_only_annotations()),
        }
    }

    fn call(server: &OpenMemoryMcpServer, args: Value) -> Result<CallToolResult, JsonRpcError> {
        let req: RetrieveInput = parse_args(args)?;
        if req.query.trim().is_empty() {
            return Err(JsonRpcError::invalid_params("query must not be empty"));
        }
        let limit = req.limit.unwrap_or(10).clamp(1, 50) as usize;
        let fetch = limit.saturating_mul(OVERFETCH_FACTOR);

        let engaged = if let Some(explicit) = req.engage {
            explicit
        } else {
            let status = server.memory().status().map_err(map_memory_err)?;
            status.total_observations >= ENGAGE_MIN_OBSERVATIONS
        };
        // An as-of instant restricts retrieval to the validity-aware
        // graph layers: free-text chunks (and the index copies of
        // observation text) carry no validity metadata, so the content
        // route cannot answer "as of March" and serving it would leak
        // present-day facts into past-truth answers. Content questions
        // pinned to an instant therefore answer through the gloss layer.
        let pinned = req.as_of.is_some();
        let mut intent = if engaged {
            classify(&req.query, req.intent.unwrap_or_default())
        } else {
            Intent::Content
        };
        if pinned && intent == Intent::Content {
            intent = Intent::Lookup;
        }

        let mut items = match intent {
            Intent::Lookup => gloss_route(server, &req.query, fetch, req.as_of)?,
            Intent::Content => content_route(server, &req.query, fetch)?,
            Intent::Relational => traversal_route(server, &req.query, req.as_of)?,
        };
        // Fill: the other layers append after the primary (never
        // interleave — measured at -0.065 MRR vs +0.065 for append,
        // T15 F-4 vs F-9).
        if intent != Intent::Content && !pinned {
            append_dedup(&mut items, content_route(server, &req.query, fetch)?);
        }
        if intent != Intent::Lookup && (engaged || pinned) {
            append_dedup(
                &mut items,
                gloss_route(server, &req.query, fetch, req.as_of)?,
            );
        }

        let promotions = if req.as_of.is_none() {
            promote_successors(server, &mut items)?
        } else {
            0
        };
        items.truncate(limit);

        let results: Vec<Value> = items
            .iter()
            .map(|it| {
                let mut row = json!({
                    "route": it.route,
                    "snippet": it.snippet,
                });
                let obj = row.as_object_mut().expect("object literal");
                if let Some((_, name)) = &it.entity {
                    obj.insert("entity_name".into(), json!(name));
                }
                if let Some(t) = it.entity_type {
                    obj.insert("entity_type".into(), json!(t.as_str()));
                }
                if let Some(uri) = &it.uri {
                    obj.insert("uri".into(), json!(uri));
                }
                if let Some(score) = it.score {
                    obj.insert("score".into(), json!(round2(score)));
                }
                if let Some(via) = &it.via {
                    obj.insert("via".into(), json!(via));
                }
                if let Some(s) = &it.superseded_by {
                    obj.insert("superseded_by".into(), json!(s));
                }
                row
            })
            .collect();

        json_text_result(&json!({
            "results": results,
            "limit": limit,
            "trace": {
                "engaged": engaged,
                "intent": intent.as_str(),
                "as_of": req.as_of,
                "promotions": promotions,
            },
        }))
    }
}

/// Register this module's tools. Called from the memory-group section of
/// the registry so the instructions block keeps one contiguous
/// `MEMORY TOOLS:` section.
pub(crate) fn register_all(v: &mut Vec<super::Entry>) {
    v.push(super::entry::<OpenMemoryRetrieveTool>());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use openmemory_core::config::Config;
    use openmemory_graph::{EntityType, MemoryStore, ObservationInput, RelationInput};

    fn server() -> OpenMemoryMcpServer {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store))
    }

    fn text_of(r: &CallToolResult) -> String {
        match &r.content[0] {
            crate::protocol::Content::Text { text } => text.clone(),
        }
    }

    fn parsed(r: &CallToolResult) -> Value {
        serde_json::from_str(&text_of(r)).unwrap()
    }

    fn remember(s: &OpenMemoryMcpServer, name: &str, ty: EntityType, obs: ObservationInput) {
        s.memory().remember(name, ty, &[obs], &[], "test").unwrap();
    }

    #[test]
    fn classifier_recognizes_identifier_shapes() {
        for q in [
            "generate_pkce",
            "ValueHint enum",
            "camelCase helper",
            "std::mem::swap",
            "the bail! macro",
            "src/lib.rs",
        ] {
            assert!(looks_like_identifier(q), "should be identifier: {q}");
        }
        for q in ["how does error handling work", "the deploy policy"] {
            assert!(!looks_like_identifier(q), "not an identifier: {q}");
        }
    }

    #[test]
    fn scale_gate_defaults_to_content_route_on_small_stores() {
        let s = server();
        remember(
            &s,
            "alpha",
            EntityType::Concept,
            ObservationInput::new("alpha gloss"),
        );
        let r = OpenMemoryRetrieveTool::call(&s, json!({"query": "alpha_thing lookup"})).unwrap();
        let v = parsed(&r);
        assert_eq!(v["trace"]["engaged"], json!(false));
        assert_eq!(v["trace"]["intent"], json!("content"));
    }

    #[test]
    fn engaged_lookup_routes_to_gloss_layer() {
        let s = server();
        remember(
            &s,
            "widget",
            EntityType::Concept,
            ObservationInput::new("defines the frobnicate_widget function"),
        );
        let r =
            OpenMemoryRetrieveTool::call(&s, json!({"query": "frobnicate_widget", "engage": true}))
                .unwrap();
        let v = parsed(&r);
        assert_eq!(v["trace"]["intent"], json!("lookup"));
        assert_eq!(v["results"][0]["route"], json!("gloss"));
        assert_eq!(v["results"][0]["entity_name"], json!("widget"));
    }

    #[test]
    fn relational_intent_walks_typed_edges() {
        let s = server();
        s.memory()
            .remember(
                "libfoo",
                EntityType::Project,
                &[ObservationInput::new("a parsing library")],
                &[],
                "test",
            )
            .unwrap();
        s.memory()
            .remember(
                "appbar",
                EntityType::Project,
                &[ObservationInput::new("the appbar application binary")],
                &[RelationInput::new(
                    "depends_on",
                    "libfoo",
                    EntityType::Project,
                )],
                "test",
            )
            .unwrap();
        let r = OpenMemoryRetrieveTool::call(
            &s,
            json!({"query": "what does appbar depend on", "engage": true}),
        )
        .unwrap();
        let v = parsed(&r);
        assert_eq!(v["trace"]["intent"], json!("relational"));
        let names: Vec<String> = v["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["entity_name"].as_str().map(String::from))
            .collect();
        assert!(names.contains(&"libfoo".to_string()), "got {names:?}");
        let via = v["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["entity_name"] == json!("libfoo"))
            .and_then(|row| row["via"].as_str().map(String::from));
        assert!(
            via.is_some_and(|s| s.contains("depends_on")),
            "traversal result should carry the edge annotation"
        );
    }

    #[test]
    fn superseded_results_yield_rank_to_their_successor() {
        let s = server();
        remember(
            &s,
            "policy-v1",
            EntityType::Concept,
            ObservationInput::new("deploy freeze policy: no Friday deploys ever allowed"),
        );
        s.memory()
            .remember(
                "policy-v2",
                EntityType::Concept,
                &[ObservationInput::new(
                    "deploy cadence: continuous cohort deploys with canary",
                )],
                &[RelationInput::new(
                    "supersedes",
                    "policy-v1",
                    EntityType::Concept,
                )],
                "test",
            )
            .unwrap();
        // The query matches the stale doc's vocabulary, not the new one's.
        let r = OpenMemoryRetrieveTool::call(
            &s,
            json!({"query": "Friday deploy freeze policy", "engage": true, "intent": "lookup"}),
        )
        .unwrap();
        let v = parsed(&r);
        assert!(v["trace"]["promotions"].as_u64().unwrap() >= 1);
        let rows = v["results"].as_array().unwrap();
        let pos = |name: &str| {
            rows.iter()
                .position(|row| row["entity_name"] == json!(name))
                .unwrap_or(usize::MAX)
        };
        assert!(
            pos("policy-v2") < pos("policy-v1"),
            "successor must outrank the superseded entity: {rows:?}"
        );
        let stale = &rows[pos("policy-v1")];
        assert_eq!(stale["superseded_by"], json!("policy-v2"));
    }

    #[test]
    fn as_of_pins_validity_and_disables_promotion() {
        let s = server();
        let mut old = ObservationInput::new("the datastore is Postgres");
        old.valid_from = Some(100);
        old.valid_until = Some(1_000);
        remember(&s, "datastore-decision", EntityType::Concept, old);
        let mut new = ObservationInput::new("the datastore is SQLite per tenant");
        new.valid_from = Some(2_000);
        remember(&s, "datastore-decision-v2", EntityType::Concept, new);
        s.memory()
            .remember(
                "datastore-decision-v2",
                EntityType::Concept,
                &[],
                &[RelationInput::new(
                    "supersedes",
                    "datastore-decision",
                    EntityType::Concept,
                )],
                "test",
            )
            .unwrap();

        // Pinned inside the old window: the old fact answers, the new
        // fact (valid_from = write time) is filtered, and no promotion
        // rewrites history.
        let r = OpenMemoryRetrieveTool::call(
            &s,
            json!({
                "query": "datastore", "engage": true,
                "intent": "lookup", "as_of": 500
            }),
        )
        .unwrap();
        let v = parsed(&r);
        assert_eq!(v["trace"]["promotions"], json!(0));
        let body = text_of(&r);
        assert!(body.contains("Postgres"));
        assert!(!body.contains("SQLite per tenant"));

        // Unpinned: the validity-aware gloss layer serves current truth
        // only. (The content-route fill may still carry the index's copy
        // of the stale text, because free-text chunks have no validity
        // metadata yet — the plan/18 layer-separation item.)
        let r = OpenMemoryRetrieveTool::call(
            &s,
            json!({"query": "datastore", "engage": true, "intent": "lookup"}),
        )
        .unwrap();
        let v = parsed(&r);
        let rows = v["results"].as_array().unwrap();
        assert!(rows.iter().any(|row| row["snippet"]
            .as_str()
            .unwrap_or("")
            .contains("SQLite per tenant")));
        for row in rows {
            if row["route"] == json!("gloss") {
                assert!(
                    !row["snippet"]
                        .as_str()
                        .unwrap_or("")
                        .contains("is Postgres"),
                    "graph layer must filter the expired fact: {row}"
                );
            }
        }
    }

    #[test]
    fn identical_calls_return_identical_rankings() {
        let s = server();
        for i in 0..8 {
            remember(
                &s,
                &format!("doc-{i}"),
                EntityType::Concept,
                ObservationInput::new(format!("shared vocabulary document number {i}")),
            );
        }
        let call = || {
            text_of(
                &OpenMemoryRetrieveTool::call(
                    &s,
                    json!({"query": "shared vocabulary document", "engage": true}),
                )
                .unwrap(),
            )
        };
        let first = call();
        for _ in 0..3 {
            assert_eq!(first, call(), "retrieve must not mutate its own ranking");
        }
    }

    #[test]
    fn empty_query_is_rejected() {
        let s = server();
        let err = OpenMemoryRetrieveTool::call(&s, json!({"query": "  "})).unwrap_err();
        assert!(err.message.contains("query must not be empty"));
    }
}
