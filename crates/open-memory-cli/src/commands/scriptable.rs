//! Scriptable CLI surface for the knowledge graph: `remember`, `recall`,
//! `list-entities`, `forget-entity`. Optimised for shell pipelines, not
//! interactive use — every command opens the store, performs one
//! transaction, prints the result, and exits.

use anyhow::{Context, Result};
use open_memory_core::config::Config;
use open_memory_graph::{
    EntityType, MemoryStore, ObservationInput, RecallFilters, RelationInput,
};

use crate::cli::{
    ForgetEntityArgs, ListEntitiesArgs, RecallArgs, RememberArgs,
};

fn open(profile: &str) -> Result<MemoryStore> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    if !data_dir.exists() {
        anyhow::bail!(
            "open-memory profile {profile:?} not initialised (no data directory at {}). \
             Run `open-memory init` first.",
            data_dir.display()
        );
    }
    MemoryStore::open(&config, &data_dir)
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
    let store = open(profile)?;
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
        println!(
            "remembered {} observation(s) for {} (entity_id={})",
            outcome.observation_ids.len(),
            args.entity,
            outcome.entity_id
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
    let store = open(profile)?;
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
    } else if hits.is_empty() {
        println!("(no results)");
    } else {
        for h in &hits {
            println!(
                "{:.3}  [{}/{}]  {}",
                h.score,
                h.entity_name,
                h.entity_type.as_str(),
                h.observation.content
            );
        }
    }
    Ok(())
}

// ------------------------- list-entities -------------------------

pub fn list_entities(profile: &str, args: ListEntitiesArgs) -> Result<()> {
    let store = open(profile)?;
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
    } else if rows.is_empty() {
        println!("(no entities)");
    } else {
        for r in &rows {
            println!(
                "{:>3}  {:>14}  {}",
                r.observation_count,
                r.entity.entity_type.as_str(),
                r.entity.name
            );
        }
    }
    Ok(())
}

// ------------------------- forget-entity -------------------------

pub fn forget_entity(profile: &str, args: ForgetEntityArgs) -> Result<()> {
    if !args.yes {
        anyhow::bail!(
            "this command hard-deletes {:?} and all of its observations / relations. \
             Re-run with --yes to confirm.",
            args.entity
        );
    }
    let store = open(profile)?;
    let removed = store
        .forget_entity(&args.entity)
        .context("forget_entity failed")?;
    println!(
        "removed entity {:?} ({removed} observation(s) cascaded)",
        args.entity
    );
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
        let r = parse_relation("maintains=open-memory:project").unwrap();
        assert_eq!(r.relation_type, "maintains");
        assert_eq!(r.target_name, "open-memory");
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
