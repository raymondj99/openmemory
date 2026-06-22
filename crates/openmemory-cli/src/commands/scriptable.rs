//! Scriptable CLI surface for the knowledge graph: `remember`, `recall`,
//! `list-entities`, `forget-entity`. Optimised for shell pipelines, not
//! interactive use — every command opens the store, performs one
//! transaction, prints the result, and exits.

use std::io::Write;

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_engine::partition::DomainStore;
use openmemory_graph::{EntityType, ObservationInput, RecallFilters, RelationInput};
#[cfg(feature = "embeddings")]
use std::sync::Arc;

use crate::cli::{ForgetEntityArgs, ListEntitiesArgs, RecallArgs, RememberArgs};
use crate::ui::glyph::Glyph;
use crate::ui::{card, paint, stdout_stream, style};

/// Open the profile with whatever partitioning it already has (the
/// manifest's domain count when present, single-store otherwise).
/// Scriptable commands follow the on-disk layout; the `mcp` and
/// `ingest` surfaces are the ones that materialise partitioning from
/// the `[engine] domains` config.
fn open(profile: &str, attach_embedder: bool) -> Result<DomainStore> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        anyhow::bail!(
            "openmemory profile {profile:?} not initialised (no data directory at {}). \
             Run `openmemory init` first.",
            data_dir.display()
        );
    }
    let _ = attach_embedder;

    #[cfg(feature = "embeddings")]
    if attach_embedder {
        let models_dir = Config::models_dir().context("resolving models directory")?;
        if let Some(embedder) = openmemory_embed::load_embedder(&models_dir) {
            return DomainStore::open_existing_with_embedder(
                &config,
                &data_dir,
                Arc::new(embedder),
            )
            .with_context(|| format!("opening memory store at {}", data_dir.display()));
        }
    }

    DomainStore::open_existing(&config, &data_dir)
        .with_context(|| format!("opening memory store at {}", data_dir.display()))
}

fn parse_entity_type(s: &str) -> Result<EntityType> {
    EntityType::parse(s)
        .ok_or_else(|| anyhow::anyhow!("unknown entity type {s:?}; valid: person, project, concept, tool, preference, fact, event, location, organization"))
}

// ------------------------- remember -------------------------

pub fn remember(profile: &str, args: RememberArgs) -> Result<()> {
    if args.observations.is_empty() {
        anyhow::bail!("at least one --observation is required");
    }
    let store = open(profile, true)?;
    let entity_type = parse_entity_type(&args.entity_type)?;
    let observations: Vec<ObservationInput> = args
        .observations
        .iter()
        .map(|s| ObservationInput::new(s).with_source(args.source.as_deref().unwrap_or("cli")))
        .collect();

    let mut relations: Vec<RelationInput> = Vec::new();
    for spec in &args.relation {
        relations.push(parse_relation(spec)?);
    }

    let outcome = store
        .remember(
            &args.entity,
            entity_type,
            &observations,
            &relations,
            args.source.as_deref().unwrap_or("cli"),
        )
        .context("remember failed")?;

    if args.json {
        let payload = serde_json::json!({
            "entity_id": outcome.entity_id,
            "entity_existed": outcome.entity_existed,
            "observation_ids": outcome.observation_ids,
            "relation_ids": outcome.relation_ids,
        });
        println!("{}", serde_json::to_string(&payload)?);
    } else {
        let mut stream = stdout_stream();
        let glyph = paint(style::SUCCESS, Glyph::Ok.as_str());
        let count = outcome.observation_ids.len();
        let noun = if count == 1 {
            "observation"
        } else {
            "observations"
        };
        let entity = paint(style::SECTION, &args.entity);
        let etype = paint(style::MUTED, &format!("({})", entity_type.as_str()));
        let _ = writeln!(
            &mut stream,
            "  {glyph} remembered {count} {noun} for {entity}  {etype}"
        );
    }
    Ok(())
}

fn parse_relation(spec: &str) -> Result<RelationInput> {
    // Format: TYPE=NAME[:ENTITY_TYPE]
    let (relation_type, target_spec) = spec
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("relation must be TYPE=NAME[:ENTITY_TYPE], got {spec:?}"))?;
    if relation_type.trim().is_empty() {
        anyhow::bail!("relation type missing in {spec:?}");
    }
    let (target_name, target_type) = target_spec
        .split_once(':')
        .map_or((target_spec, None), |(n, t)| (n, Some(t)));
    if target_name.trim().is_empty() {
        anyhow::bail!("relation target name missing in {spec:?}");
    }
    let target_type = target_type
        .map(parse_entity_type)
        .transpose()?
        .unwrap_or(EntityType::Concept);
    Ok(RelationInput::new(relation_type, target_name, target_type))
}

// ------------------------- recall -------------------------

pub fn recall(profile: &str, args: RecallArgs) -> Result<()> {
    let store = open(profile, true)?;
    let limit = args.limit.unwrap_or(10).max(1) as usize;
    let mut filters = RecallFilters::new();
    if let Some(et) = args.entity_type {
        filters.entity_type = Some(parse_entity_type(&et)?);
    }
    filters.source = args.source;
    filters.min_confidence = args.min_confidence;

    let hits = store
        .recall(&args.query, limit, &filters)
        .context("recall failed")?;
    if args.json {
        let json: Vec<serde_json::Value> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "observation_id": h.observation.id,
                    "entity_name": h.entity_name,
                    "entity_type": h.entity_type.as_str(),
                    "content": h.observation.content,
                    "score": h.score,
                    "raw_score": h.raw_score,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&json)?);
    } else {
        let mut stream = stdout_stream();
        if hits.is_empty() {
            let muted = paint(style::MUTED, "no results.");
            let _ = writeln!(&mut stream, "  {muted}");
        } else {
            for (i, h) in hits.iter().enumerate() {
                if i > 0 {
                    card::separator(&mut stream);
                }
                let header = format!(
                    "{} {} {}",
                    h.entity_name,
                    Glyph::Separator.as_str(),
                    h.entity_type.as_str()
                );
                let suffix = format!("{:.3}", h.score);
                card::render(&mut stream, &header, &suffix, &h.observation.content);
            }
        }
    }
    Ok(())
}

// ------------------------- list-entities -------------------------

pub fn list_entities(profile: &str, args: ListEntitiesArgs) -> Result<()> {
    let store = open(profile, false)?;
    let limit = args.limit.unwrap_or(50).max(1) as usize;
    let offset = args.offset.unwrap_or(0) as usize;
    let entity_type = args
        .entity_type
        .as_deref()
        .map(parse_entity_type)
        .transpose()?;
    let rows = store
        .list_entities(entity_type, limit, offset)
        .context("list_entities failed")?;
    if args.json {
        let json: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.entity.id,
                    "name": r.entity.name,
                    "entity_type": r.entity.entity_type.as_str(),
                    "observation_count": r.observation_count,
                    "updated_at": r.entity.updated_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&json)?);
    } else {
        let mut stream = stdout_stream();
        if rows.is_empty() {
            let muted = paint(style::MUTED, "no entities.");
            let _ = writeln!(&mut stream, "  {muted}");
        } else {
            let name_w = rows
                .iter()
                .map(|r| r.entity.name.chars().count())
                .max()
                .unwrap_or(0)
                .max(4);
            let type_w = rows
                .iter()
                .map(|r| r.entity.entity_type.as_str().len())
                .max()
                .unwrap_or(0)
                .max(4);
            let header = format!(
                "  {:name_w$}  {:type_w$}  {:>12}",
                "name",
                "type",
                "observations",
                name_w = name_w,
                type_w = type_w,
            );
            let _ = writeln!(&mut stream, "{}", paint(style::SECTION, &header));
            let rule = format!(
                "  {} ",
                Glyph::EdgeHorizontal
                    .as_str()
                    .repeat(name_w + type_w + 12 + 4)
            );
            let _ = writeln!(&mut stream, "{}", paint(style::MUTED, rule.trim_end()));
            for r in &rows {
                let line = format!(
                    "  {:name_w$}  {:type_w$}  {:>12}",
                    r.entity.name,
                    r.entity.entity_type.as_str(),
                    r.observation_count,
                    name_w = name_w,
                    type_w = type_w,
                );
                let _ = writeln!(&mut stream, "{line}");
            }
        }
    }
    Ok(())
}

// ------------------------- forget-entity -------------------------

pub fn forget_entity(profile: &str, args: ForgetEntityArgs) -> Result<()> {
    if !args.yes {
        anyhow::bail!(
            "refusing to hard-delete {:?}: re-run with --yes to confirm. \
             this removes the entity and every observation and relation attached to it.",
            args.entity
        );
    }
    let store = open(profile, false)?;
    let removed = store
        .forget_entity(&args.entity)
        .context("forget_entity failed")?;
    let mut stream = stdout_stream();
    let glyph = paint(style::SUCCESS, Glyph::Ok.as_str());
    let entity = paint(style::SECTION, &args.entity);
    let suffix = paint(style::MUTED, &format!("{removed} observation(s) cascaded"));
    let _ = writeln!(&mut stream, "  {glyph} removed {entity}  {suffix}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;

    fn init(profile: &str) {
        crate::commands::init::run(profile, crate::cli::InitArgs { force: false }).unwrap();
    }

    #[test]
    fn remember_then_recall_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            init("default");
            remember(
                "default",
                RememberArgs {
                    entity: "Raymond".into(),
                    entity_type: "person".into(),
                    observations: vec!["prefers Rust".into()],
                    relation: vec![],
                    source: None,
                    json: false,
                },
            )
            .unwrap();
            recall(
                "default",
                RecallArgs {
                    query: "Rust".into(),
                    limit: Some(5),
                    entity_type: None,
                    source: None,
                    min_confidence: None,
                    json: true,
                },
            )
            .unwrap();
        });
    }

    #[test]
    fn list_entities_runs() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            init("default");
            list_entities(
                "default",
                ListEntitiesArgs {
                    entity_type: None,
                    limit: None,
                    offset: None,
                    json: false,
                },
            )
            .unwrap();
        });
    }

    #[test]
    fn forget_entity_requires_yes() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            init("default");
            let err = forget_entity(
                "default",
                ForgetEntityArgs {
                    entity: "Missing".into(),
                    yes: false,
                },
            )
            .unwrap_err();
            assert!(err.to_string().contains("--yes"));
        });
    }

    #[test]
    fn parse_relation_with_explicit_type() {
        let r = parse_relation("maintains=openmemory:project").unwrap();
        assert_eq!(r.relation_type, "maintains");
        assert_eq!(r.target_name, "openmemory");
        assert_eq!(r.target_type, EntityType::Project);
    }

    #[test]
    fn parse_relation_defaults_to_concept() {
        let r = parse_relation("uses=Rust").unwrap();
        assert_eq!(r.target_type, EntityType::Concept);
    }

    #[test]
    fn parse_relation_rejects_invalid_format() {
        assert!(parse_relation("nopair").is_err());
        assert!(parse_relation("=name").is_err());
        assert!(parse_relation("type=").is_err());
    }
}
