//! Domain-count migration: re-home a profile from K_old to K_new
//! domains (including 1 → K and K → 1), offline, with a staging build
//! and a two-phase swap.
//!
//! # Why this is not a copy
//!
//! Modulo hashing reshuffles nearly every entity for any K change, so
//! "move the changed rows" does not exist as an optimisation. And the
//! normal write APIs are unusable: `remember_batch` mints new ids and
//! timestamps and runs normalization. Migration therefore uses the raw
//! export/import surface (`openmemory_graph::export`), which preserves
//! ids, timestamps, tombstones, access counts, tiers, and bi-temporal
//! validity byte-exactly, and carries search-index entries (including
//! embedding vectors) verbatim so nothing is re-embedded.
//!
//! # What is deliberately NOT carried
//!
//! - **Engine checkpoints** (`engine:journal:<shard>` rows): per-shard
//!   journal watermarks bound to the old shard-to-domain map. A copied
//!   stale-high watermark would make replay silently skip future
//!   journal entries. Fresh stores start clean, which is correct given
//!   the journals-empty precondition.
//! - **Journals**: required empty. A non-empty journal holds
//!   acknowledged writes whose committed-vs-pending status is only
//!   decidable against the checkpoints being discarded.
//! - **Stubs and mirror edges**: partition bookkeeping for the OLD
//!   boundaries. They are dropped and re-derived for the new ones,
//!   re-pointing canonical edges whose `to_entity` was a stub id.
//! - **Orphaned `memory://observation/` index rows** (observation
//!   missing or tombstoned): `forget` already intended to delete them;
//!   migration drops the drift and reports the count.
//! - **`last_consolidation`** in `memory_meta`: the next consolidation
//!   re-stamps it.
//!
//! # Crash safety (two-phase swap)
//!
//! The staging build happens in `.migrate-staging/` (invisible to every
//! open path) and is verified by raw-count reconciliation before
//! anything destructive happens. The swap itself is guarded by an
//! fsynced intent sentinel: without it, a crash between "old layout
//! moved to backup" and "new layout moved in" would decode as a fresh
//! empty profile (manifest absence means K=1) and self-poison on first
//! write. [`super::partition::DomainStore`] refuses to open while the
//! sentinel exists. The old layout survives in `.migrate-backup/` and
//! is never deleted by the migration itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use openmemory_core::config::Config;
use openmemory_graph::{
    Entity, EntityType, MemoryError, MemoryResult, MemoryStore, Observation, Relation,
};
use openmemory_index::traits::{ExportEntry, FullTextStore, IndexEntry, VectorIndex, VectorStore};

use crate::partition::{
    domain_for, DomainStore, Manifest, DOMAINS_DIR, DOMAIN_MANIFEST_FILE, MANIFEST_VERSION,
    PARTITION_STUB_SOURCE,
};

/// Intent sentinel: present while a swap is in flight. The partition
/// layer refuses to open a profile carrying it.
pub const MIGRATE_SENTINEL_FILE: &str = ".migrate-intent";
/// Staging directory for the new layout, inside the profile root.
pub const MIGRATE_STAGING_DIR: &str = ".migrate-staging";
/// Backup directory holding the pre-migration layout.
pub const MIGRATE_BACKUP_DIR: &str = ".migrate-backup";

/// URI prefix of graph observations in the search index.
const OBSERVATION_URI_PREFIX: &str = "memory://observation/";

/// Store files of a single-store (K=1) layout, moved during swaps.
/// `engine-journal` (required empty) and non-store state (`tui/`,
/// config) deliberately stay in place.
const SINGLE_STORE_ENTRIES: &[&str] = &[
    "memory.sqlite",
    "fulltext.sqlite",
    "bm25.json",
    "metadata.sqlite",
    "vectors.bin",
    "vectors.usearch",
    "vectors.usearch.meta.json",
    "embeddings",
];

/// Outcome of a completed migration. Every count is reconciled before
/// the swap: `new == old - dropped + synthesized`, per table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub from_domains: usize,
    pub to_domains: usize,
    /// Canonical (non-stub) entities moved.
    pub entities: usize,
    pub observations: usize,
    /// Canonical relations moved (mirrors excluded).
    pub relations: usize,
    pub stubs_dropped: usize,
    pub mirrors_dropped: usize,
    pub stubs_created: usize,
    pub mirrors_created: usize,
    /// Vector + full-text index entries carried.
    pub index_entries: usize,
    /// Drifted `memory://observation/` rows dropped (observation
    /// missing or tombstoned).
    pub orphaned_index_entries_dropped: usize,
    /// Watcher / index_text source records carried.
    pub source_records: usize,
    /// Where the pre-migration layout was parked. Never deleted by the
    /// migration; remove it manually once satisfied.
    pub backup_dir: PathBuf,
}

/// Everything exported from the old layout, merged across domains.
struct Exported {
    entities: Vec<Entity>,
    observations: Vec<Observation>,
    relations: Vec<Relation>,
    vectors: Vec<ExportEntry>,
    fts: Vec<ExportEntry>,
    #[cfg(feature = "fts5")]
    sources: Vec<openmemory_index::SourceRecord>,
}

/// Per-new-domain build plan.
#[derive(Default)]
struct DomainPlan {
    entities: Vec<Entity>,
    observations: Vec<Observation>,
    relations: Vec<Relation>,
    vectors: Vec<IndexEntry>,
    fts: Vec<IndexEntry>,
    #[cfg(feature = "fts5")]
    sources: Vec<openmemory_index::SourceRecord>,
}

/// Re-home `data_dir` from its current domain count to `to_domains`.
///
/// Offline only: the caller must guarantee no other process holds the
/// profile open (a running MCP server or watcher reads a moving target
/// and has files yanked mid-swap). The engine's journals must be empty
/// — shut it down cleanly first.
pub fn migrate_domains(
    config: &Config,
    data_dir: &Path,
    to_domains: usize,
) -> MemoryResult<MigrationReport> {
    let to_domains = to_domains.max(1);

    // ---- Preconditions ------------------------------------------------
    let sentinel = data_dir.join(MIGRATE_SENTINEL_FILE);
    if sentinel.exists() {
        return Err(MemoryError::InvalidInput(format!(
            "a previous migration of {} was interrupted mid-swap; restore the \
             layout from {} before retrying",
            data_dir.display(),
            data_dir.join(MIGRATE_BACKUP_DIR).display(),
        )));
    }
    let backup_dir = data_dir.join(MIGRATE_BACKUP_DIR);
    if backup_dir.exists() {
        return Err(MemoryError::InvalidInput(format!(
            "backup from a previous migration exists at {}; remove it first",
            backup_dir.display(),
        )));
    }
    let from_domains = DomainStore::manifest_domains(data_dir)?;
    if from_domains == 1 && !data_dir.join(openmemory_graph::MEMORY_DB_FILE).exists() {
        return Err(MemoryError::InvalidInput(format!(
            "no store found at {}; migrating a profile that does not exist              would create an empty one",
            data_dir.display(),
        )));
    }
    if from_domains == to_domains {
        return Err(MemoryError::InvalidInput(format!(
            "profile already has {to_domains} domain(s); nothing to migrate",
        )));
    }
    require_empty_journals(data_dir)?;

    let staging_dir = data_dir.join(MIGRATE_STAGING_DIR);
    if staging_dir.exists() {
        std::fs::remove_dir_all(&staging_dir)?;
    }
    std::fs::create_dir_all(&staging_dir)?;

    // ---- Export from the old layout ------------------------------------
    let old = DomainStore::open_existing(config, data_dir)?;
    let exported = export_all(&old)?;

    // ---- Transform + route ---------------------------------------------
    let (plans, mut report) = route(&exported, to_domains)?;
    report.from_domains = from_domains;
    report.to_domains = to_domains;
    report.backup_dir.clone_from(&backup_dir);

    // ---- Build + verify staging ----------------------------------------
    let staged_dirs = staging_domain_dirs(&staging_dir, to_domains);
    for (dir, plan) in staged_dirs.iter().zip(&plans) {
        build_staged_domain(config, dir, plan)?;
    }
    if to_domains > 1 {
        write_manifest(&staging_dir.join(DOMAIN_MANIFEST_FILE), to_domains)?;
    }
    verify_staging(config, &staged_dirs, &plans, &exported)?;

    // Make the old layout's WALs self-contained before parking it as a
    // backup (a stray -wal separated from its database is data loss),
    // then close every old handle before any rename.
    for store in old.stores() {
        let _ = store.wal_checkpoint()?;
    }
    drop(old);

    // ---- Two-phase swap --------------------------------------------------
    write_sentinel(&sentinel, from_domains, to_domains)?;

    std::fs::create_dir_all(&backup_dir)?;
    move_layout_to_backup(data_dir, &backup_dir, from_domains)?;
    move_staging_into_place(data_dir, &staging_dir, to_domains)?;
    let _ = std::fs::remove_dir_all(&staging_dir);
    fsync_dir(data_dir)?;

    std::fs::remove_file(&sentinel)?;
    fsync_dir(data_dir)?;

    Ok(report)
}

/// Every shard journal must be absent or empty: non-empty journals hold
/// acknowledged writes whose status is only decidable against the
/// per-shard checkpoints that migration discards.
fn require_empty_journals(data_dir: &Path) -> MemoryResult<()> {
    let journal_dir = data_dir.join("engine-journal");
    if !journal_dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&journal_dir)? {
        let entry = entry?;
        if entry.metadata()?.len() > 0 {
            return Err(MemoryError::InvalidInput(format!(
                "engine journal {} is not empty; shut the engine down cleanly \
                 (it drains and checkpoints on shutdown) before migrating",
                entry.path().display(),
            )));
        }
    }
    Ok(())
}

fn export_all(old: &DomainStore) -> MemoryResult<Exported> {
    let mut exported = Exported {
        entities: Vec::new(),
        observations: Vec::new(),
        relations: Vec::new(),
        vectors: Vec::new(),
        fts: Vec::new(),
        #[cfg(feature = "fts5")]
        sources: Vec::new(),
    };
    for store in old.stores() {
        exported.entities.extend(store.export_entities_raw()?);
        exported
            .observations
            .extend(store.export_observations_raw()?);
        exported.relations.extend(store.export_relations_raw()?);

        let engine = store.engine();
        exported.vectors.extend(
            engine
                .engine
                .inner()
                .vector_store()
                .export_all()
                .map_err(index_err)?,
        );
        exported.fts.extend(
            engine
                .engine
                .inner()
                .fulltext_store()
                .export_all()
                .map_err(index_err)?,
        );
        #[cfg(feature = "fts5")]
        exported
            .sources
            .extend(engine.metadata.list(None).map_err(index_err)?);
    }
    Ok(exported)
}

/// Route every exported row to its new domain, dropping old partition
/// bookkeeping and synthesizing the new boundaries' stubs and mirrors.
#[allow(clippy::too_many_lines)]
fn route(
    exported: &Exported,
    to_domains: usize,
) -> MemoryResult<(Vec<DomainPlan>, MigrationReport)> {
    let mut plans: Vec<DomainPlan> = (0..to_domains).map(|_| DomainPlan::default()).collect();
    let mut report = MigrationReport {
        from_domains: 0,
        to_domains,
        entities: 0,
        observations: 0,
        relations: 0,
        stubs_dropped: 0,
        mirrors_dropped: 0,
        stubs_created: 0,
        mirrors_created: 0,
        index_entries: 0,
        orphaned_index_entries_dropped: 0,
        source_records: 0,
        backup_dir: PathBuf::new(),
    };

    // Old stubs: dropped, but remembered so edges pointing at them can
    // be re-resolved by (name, type).
    let mut old_stub_meta: HashMap<&str, (&str, EntityType)> = HashMap::new();
    // Canonical entities by id and by (lowercase name, type).
    let mut canonical_by_id: HashMap<&str, &Entity> = HashMap::new();
    let mut canonical_id_by_key: HashMap<(String, EntityType), &Entity> = HashMap::new();
    for entity in &exported.entities {
        if entity.source == PARTITION_STUB_SOURCE {
            old_stub_meta.insert(&entity.id, (&entity.name, entity.entity_type));
            report.stubs_dropped += 1;
        } else {
            canonical_by_id.insert(&entity.id, entity);
            canonical_id_by_key.insert((entity.name.to_lowercase(), entity.entity_type), entity);
        }
    }

    // Canonical entities and their observations follow the name hash.
    let mut entity_name_by_id: HashMap<&str, &str> = HashMap::new();
    for entity in canonical_by_id.values() {
        entity_name_by_id.insert(&entity.id, &entity.name);
        plans[domain_for(&entity.name, to_domains)]
            .entities
            .push((*entity).clone());
        report.entities += 1;
    }
    let mut observation_meta: HashMap<&str, (&str, bool)> = HashMap::new();
    for obs in &exported.observations {
        let Some(name) = entity_name_by_id.get(obs.entity_id.as_str()) else {
            return Err(MemoryError::InvalidInput(format!(
                "observation {} belongs to stub or missing entity {}; the \
                 profile is inconsistent — run `openmemory consolidate` and retry",
                obs.id, obs.entity_id,
            )));
        };
        observation_meta.insert(&obs.id, (name, obs.tombstoned));
        plans[domain_for(name, to_domains)]
            .observations
            .push(obs.clone());
        report.observations += 1;
    }

    // Relations: drop mirrors, re-point canonical edges, synthesize the
    // new boundaries' stubs and mirrors.
    let mut new_stub_ids: HashMap<(usize, String, EntityType), String> = HashMap::new();
    let mut synthesize_stub = |plans: &mut Vec<DomainPlan>,
                               report: &mut MigrationReport,
                               domain: usize,
                               of: &Entity|
     -> String {
        let key = (domain, of.name.to_lowercase(), of.entity_type);
        if let Some(id) = new_stub_ids.get(&key) {
            return id.clone();
        }
        let stub = Entity {
            id: openmemory_graph::new_id(),
            name: of.name.clone(),
            entity_type: of.entity_type,
            created_at: of.created_at,
            updated_at: of.updated_at,
            confidence: 1.0,
            source: PARTITION_STUB_SOURCE.to_string(),
        };
        let id = stub.id.clone();
        plans[domain].entities.push(stub);
        report.stubs_created += 1;
        new_stub_ids.insert(key, id.clone());
        id
    };

    for rel in &exported.relations {
        if old_stub_meta.contains_key(rel.from_entity.as_str()) {
            report.mirrors_dropped += 1;
            continue;
        }
        let Some(from) = canonical_by_id.get(rel.from_entity.as_str()) else {
            return Err(MemoryError::InvalidInput(format!(
                "relation {} originates from missing entity {}",
                rel.id, rel.from_entity,
            )));
        };
        // Resolve the target to its canonical row, looking through old
        // stub ids. A stub without any canonical row (a mirror commit
        // that never landed) is promoted to a canonical entity so the
        // edge survives.
        let target: &Entity = if let Some(entity) = canonical_by_id.get(rel.to_entity.as_str()) {
            entity
        } else if let Some((name, ty)) = old_stub_meta.get(rel.to_entity.as_str()) {
            match canonical_id_by_key.get(&(name.to_lowercase(), *ty)) {
                Some(entity) => entity,
                None => {
                    return Err(MemoryError::InvalidInput(format!(
                        "relation {} points at stub-only entity {name:?} with no \
                         canonical row; run `openmemory consolidate` and retry",
                        rel.id,
                    )))
                }
            }
        } else {
            return Err(MemoryError::InvalidInput(format!(
                "relation {} points at missing entity {}",
                rel.id, rel.to_entity,
            )));
        };

        let from_domain = domain_for(&from.name, to_domains);
        let target_domain = domain_for(&target.name, to_domains);
        let mut canonical_edge = rel.clone();
        if from_domain == target_domain {
            canonical_edge.to_entity.clone_from(&target.id);
        } else {
            canonical_edge.to_entity =
                synthesize_stub(&mut plans, &mut report, from_domain, target);
            let mirror = Relation {
                id: openmemory_graph::new_id(),
                from_entity: synthesize_stub(&mut plans, &mut report, target_domain, from),
                to_entity: target.id.clone(),
                relation_type: rel.relation_type.clone(),
                weight: rel.weight,
                created_at: rel.created_at,
                valid_from: rel.valid_from,
                valid_until: rel.valid_until,
                source: rel.source.clone(),
            };
            plans[target_domain].relations.push(mirror);
            report.mirrors_created += 1;
        }
        plans[from_domain].relations.push(canonical_edge);
        report.relations += 1;
    }

    // Index entries: observation rows follow their entity; everything
    // else routes by URI hash (matching store_for_uri). Drifted
    // observation rows are dropped.
    let mut route_entry = |entry: &ExportEntry, vector_arm: bool, report: &mut MigrationReport| {
        let domain = if let Some(obs_id) = entry.uri.strip_prefix(OBSERVATION_URI_PREFIX) {
            if let Some((name, false)) = observation_meta.get(obs_id) {
                domain_for(name, to_domains)
            } else {
                report.orphaned_index_entries_dropped += 1;
                return;
            }
        } else {
            domain_for(&entry.uri, to_domains)
        };
        report.index_entries += 1;
        let target = if vector_arm {
            &mut plans[domain].vectors
        } else {
            &mut plans[domain].fts
        };
        target.push(IndexEntry::from(entry.clone()));
    };
    for entry in &exported.vectors {
        route_entry(entry, true, &mut report);
    }
    for entry in &exported.fts {
        route_entry(entry, false, &mut report);
    }

    #[cfg(feature = "fts5")]
    for record in &exported.sources {
        plans[domain_for(&record.uri, to_domains)]
            .sources
            .push(record.clone());
        report.source_records += 1;
    }

    Ok((plans, report))
}

fn staging_domain_dirs(staging_dir: &Path, to_domains: usize) -> Vec<PathBuf> {
    if to_domains == 1 {
        vec![staging_dir.to_path_buf()]
    } else {
        (0..to_domains)
            .map(|d| staging_dir.join(DOMAINS_DIR).join(format!("domain-{d:02}")))
            .collect()
    }
}

fn build_staged_domain(config: &Config, dir: &Path, plan: &DomainPlan) -> MemoryResult<()> {
    let store = MemoryStore::open(config, dir)?;
    store.import_raw(&plan.entities, &plan.observations, &plan.relations)?;

    let engine = store.engine();
    engine
        .engine
        .inner()
        .vector_store()
        .insert(&plan.vectors)
        .map_err(index_err)?;
    engine
        .engine
        .inner()
        .fulltext_store()
        .insert(&plan.fts)
        .map_err(index_err)?;
    #[cfg(feature = "fts5")]
    engine
        .metadata
        .upsert_batch(&plan.sources)
        .map_err(index_err)?;

    store.persist_search_index()?;
    let _ = store.wal_checkpoint()?;
    Ok(())
}

/// Raw-count reconciliation: every staged domain must hold exactly its
/// plan, and the totals must match the routing report. `MemoryStatus`
/// cannot be the verifier (it filters tombstoned and expired rows), so
/// the staged stores are re-exported raw. Routing is spot-checked by
/// resolving sampled canonical names through the new layout.
fn verify_staging(
    config: &Config,
    staged_dirs: &[PathBuf],
    plans: &[DomainPlan],
    exported: &Exported,
) -> MemoryResult<()> {
    let mut sampled = 0usize;
    for (dir, plan) in staged_dirs.iter().zip(plans) {
        let store = MemoryStore::open(config, dir)?;
        let checks: [(&str, usize, usize); 5] = [
            (
                "entities",
                store.export_entities_raw()?.len(),
                plan.entities.len(),
            ),
            (
                "observations",
                store.export_observations_raw()?.len(),
                plan.observations.len(),
            ),
            (
                "relations",
                store.export_relations_raw()?.len(),
                plan.relations.len(),
            ),
            (
                "vector entries",
                store.engine().engine.count().map_err(index_err)? as usize,
                plan.vectors.len(),
            ),
            (
                "fulltext entries",
                store.engine().engine.keyword_count().map_err(index_err)? as usize,
                plan.fts.len(),
            ),
        ];
        for (what, staged, planned) in checks {
            if staged != planned {
                return Err(MemoryError::InvalidInput(format!(
                    "staging verification failed in {}: {what} staged {staged} != planned {planned}; \
                     the original layout is untouched",
                    dir.display(),
                )));
            }
        }

        // Spot-check routing: sampled canonical entities resolve by
        // name in the domain that will own them.
        for entity in plan
            .entities
            .iter()
            .filter(|e| e.source != PARTITION_STUB_SOURCE)
            .take(32)
        {
            if store.get_entity(&entity.name)?.is_none() {
                return Err(MemoryError::InvalidInput(format!(
                    "staging verification failed: entity {:?} not resolvable in {}",
                    entity.name,
                    dir.display(),
                )));
            }
            sampled += 1;
        }
    }

    // Cross-domain reconciliation: canonical totals survive routing.
    let canonical_entities = exported
        .entities
        .iter()
        .filter(|e| e.source != PARTITION_STUB_SOURCE)
        .count();
    let routed_canonical: usize = plans
        .iter()
        .map(|p| {
            p.entities
                .iter()
                .filter(|e| e.source != PARTITION_STUB_SOURCE)
                .count()
        })
        .sum();
    if canonical_entities != routed_canonical {
        return Err(MemoryError::InvalidInput(format!(
            "staging verification failed: {canonical_entities} canonical entities \
             exported but {routed_canonical} routed",
        )));
    }
    let _ = sampled;
    Ok(())
}

fn write_manifest(path: &Path, domains: usize) -> MemoryResult<()> {
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        domains,
    };
    let text = toml::to_string_pretty(&manifest)
        .map_err(|e| MemoryError::InvalidInput(format!("manifest encode: {e}")))?;
    std::fs::write(path, text)?;
    Ok(())
}

/// Write and fsync the intent sentinel, then fsync the profile root so
/// the sentinel's existence is durable before the first destructive
/// rename.
fn write_sentinel(path: &Path, from: usize, to: usize) -> MemoryResult<()> {
    use std::io::Write as IoWrite;
    let mut file = std::fs::File::create(path)?;
    writeln!(file, "migrating domains: {from} -> {to}")?;
    file.sync_all()?;
    fsync_dir(path.parent().unwrap_or_else(|| Path::new(".")))?;
    Ok(())
}

fn move_layout_to_backup(
    data_dir: &Path,
    backup_dir: &Path,
    from_domains: usize,
) -> MemoryResult<()> {
    if from_domains > 1 {
        std::fs::rename(data_dir.join(DOMAINS_DIR), backup_dir.join(DOMAINS_DIR))?;
        std::fs::rename(
            data_dir.join(DOMAIN_MANIFEST_FILE),
            backup_dir.join(DOMAIN_MANIFEST_FILE),
        )?;
    } else {
        for name in SINGLE_STORE_ENTRIES {
            let source = data_dir.join(name);
            if source.exists() {
                std::fs::rename(&source, backup_dir.join(name))?;
            }
        }
    }
    Ok(())
}

fn move_staging_into_place(
    data_dir: &Path,
    staging_dir: &Path,
    to_domains: usize,
) -> MemoryResult<()> {
    if to_domains > 1 {
        std::fs::rename(staging_dir.join(DOMAINS_DIR), data_dir.join(DOMAINS_DIR))?;
        std::fs::rename(
            staging_dir.join(DOMAIN_MANIFEST_FILE),
            data_dir.join(DOMAIN_MANIFEST_FILE),
        )?;
    } else {
        for name in SINGLE_STORE_ENTRIES {
            let source = staging_dir.join(name);
            if source.exists() {
                std::fs::rename(&source, data_dir.join(name))?;
            }
        }
    }
    Ok(())
}

/// fsync a directory so completed renames inside it survive power loss.
fn fsync_dir(dir: &Path) -> MemoryResult<()> {
    let handle = std::fs::File::open(dir)?;
    handle.sync_all()?;
    Ok(())
}

fn index_err(e: openmemory_index::IndexError) -> MemoryError {
    MemoryError::InvalidInput(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_graph::recall::RecallFilters;
    use openmemory_graph::{ObservationInput, RelationInput, SearchMode};

    fn cfg() -> Config {
        Config::default()
    }

    /// Seed a profile exercising everything migration must preserve:
    /// tombstones, tiers, v2 fields, cross-entity relations, explicit
    /// vectors, and free-text index entries.
    fn seed(store: &DomainStore) -> (String, String) {
        let a = "alpha-project".to_string();
        let b = (0..1000)
            .map(|i| format!("teammate-{i}"))
            .find(|b| store.domains() == 1 || store.domain_for(b) != store.domain_for(&a))
            .expect("name pair");

        store
            .remember(
                &b,
                EntityType::Person,
                &[ObservationInput::new("works on the project")],
                &[],
                "seed",
            )
            .unwrap();
        let outcome = store
            .remember(
                &a,
                EntityType::Project,
                &[
                    ObservationInput::new("ships the context engine")
                        .with_title("headline")
                        .with_concepts(vec!["engine".into()]),
                    ObservationInput::new("to be tombstoned"),
                ],
                &[RelationInput::new(
                    "staffed_by",
                    b.clone(),
                    EntityType::Person,
                )],
                "seed",
            )
            .unwrap();
        store.forget(&outcome.observation_ids[1]).unwrap();
        store
            .set_observation_memory_tier(
                &outcome.observation_ids[0],
                openmemory_graph::MemoryTier::Semantic,
            )
            .unwrap();

        // Free-text doc with an explicit vector: must carry without an
        // embedder being attached.
        store
            .index_insert(
                IndexEntry::new("note://meeting", "quarterly planning notes")
                    .with_vector(vec![0.5, 0.25, 0.25]),
            )
            .unwrap();

        for i in 0..30 {
            store
                .remember(
                    &format!("filler-{i}"),
                    EntityType::Fact,
                    &[ObservationInput::new(format!("filler fact {i}"))],
                    &[],
                    "seed",
                )
                .unwrap();
        }
        (a, b)
    }

    fn snapshot(store: &DomainStore) -> (usize, usize, usize) {
        let status = store.status().unwrap();
        (
            status.total_entities as usize,
            status.total_observations as usize,
            status.total_relations as usize,
        )
    }

    fn assert_migrated_profile(
        store: &DomainStore,
        a: &str,
        b: &str,
        before: (usize, usize, usize),
    ) {
        assert_eq!(snapshot(store), before, "knowledge counts must survive");

        // Identity preserved: ids, timestamps, tombstones, tiers.
        let alpha = store.get_entity(a).unwrap().expect("alpha resolvable");
        let observations: Vec<_> = store
            .stores()
            .iter()
            .flat_map(|s| s.export_observations_raw().unwrap())
            .collect();
        assert!(
            observations.iter().any(|o| o.tombstoned),
            "tombstoned observation must survive migration"
        );
        assert!(observations
            .iter()
            .any(|o| o.memory_tier == openmemory_graph::MemoryTier::Semantic));
        assert!(observations
            .iter()
            .any(|o| o.title.as_deref() == Some("headline")));

        // Relation visible from both endpoints.
        let rels = store.get_entity_relations(&alpha.id).unwrap();
        assert_eq!(rels.len(), 1);
        let bravo = store.get_entity(b).unwrap().expect("b resolvable");
        assert_eq!(store.get_entity_relations(&bravo.id).unwrap().len(), 1);

        // Search works end to end: graph recall and the carried
        // free-text doc (vector intact, searchable by keyword).
        let results = store
            .recall("context engine", 5, &RecallFilters::new())
            .unwrap();
        assert!(!results.is_empty());
        let hits = store
            .index_search(&[], "quarterly planning", 5, SearchMode::KeywordOnly, 0)
            .unwrap();
        assert!(hits.iter().any(|h| h.uri == "note://meeting"));
        let vector_count: u64 = store
            .stores()
            .iter()
            .map(|s| {
                use openmemory_index::traits::VectorStore;
                s.engine().engine.inner().vector_store().count().unwrap()
            })
            .sum();
        assert_eq!(vector_count, 1, "explicit vector must carry");
    }

    #[test]
    fn migrate_single_store_to_four_domains_and_back() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, before) = {
            let store = DomainStore::open(&cfg(), dir.path(), 1).unwrap();
            let (a, b) = seed(&store);
            let before = snapshot(&store);
            (a, b, before)
        };

        let report = migrate_domains(&cfg(), dir.path(), 4).unwrap();
        assert_eq!(report.from_domains, 1);
        assert_eq!(report.to_domains, 4);
        assert_eq!(report.entities, before.0);
        // status counts live observations only; the raw report carries
        // the tombstoned one too.
        assert_eq!(report.observations, before.1 + 1);
        assert_eq!(report.stubs_dropped, 0, "single store has no stubs");

        let migrated = DomainStore::open_existing(&cfg(), dir.path()).unwrap();
        assert_eq!(migrated.domains(), 4);
        assert_migrated_profile(&migrated, &a, &b, before);
        assert!(dir.path().join(MIGRATE_BACKUP_DIR).exists(), "backup kept");
        drop(migrated);

        // And back down to a single store: stubs and mirrors melt away.
        std::fs::remove_dir_all(dir.path().join(MIGRATE_BACKUP_DIR)).unwrap();
        let report = migrate_domains(&cfg(), dir.path(), 1).unwrap();
        assert_eq!(report.to_domains, 1);
        assert_eq!(report.stubs_created, 0);
        assert_eq!(report.mirrors_created, 0);

        let single = DomainStore::open_existing(&cfg(), dir.path()).unwrap();
        assert_eq!(single.domains(), 1);
        assert!(
            !dir.path().join(DOMAIN_MANIFEST_FILE).exists(),
            "K=1 writes no manifest"
        );
        assert_migrated_profile(&single, &a, &b, before);
    }

    #[test]
    fn migrate_repartitions_between_domain_counts() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, before) = {
            let store = DomainStore::open(&cfg(), dir.path(), 4).unwrap();
            let (a, b) = seed(&store);
            (a, b, snapshot(&store))
        };

        let report = migrate_domains(&cfg(), dir.path(), 2).unwrap();
        assert_eq!(report.from_domains, 4);
        assert_eq!(report.to_domains, 2);
        // The seeded cross-domain relation had a stub + mirror at K=4.
        assert!(report.stubs_dropped >= 1, "old stubs dropped: {report:?}");
        assert!(report.mirrors_dropped >= 1);

        let migrated = DomainStore::open_existing(&cfg(), dir.path()).unwrap();
        assert_eq!(migrated.domains(), 2);
        assert_migrated_profile(&migrated, &a, &b, before);
    }

    #[test]
    fn migrate_rejects_same_domain_count_and_nonempty_journal() {
        let dir = tempfile::tempdir().unwrap();
        drop(DomainStore::open(&cfg(), dir.path(), 2).unwrap());

        let err = migrate_domains(&cfg(), dir.path(), 2).unwrap_err();
        assert!(err.to_string().contains("nothing to migrate"));

        let journal_dir = dir.path().join("engine-journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        std::fs::write(journal_dir.join("shard-0.jsonl"), "{\"seq\":1}\n").unwrap();
        let err = migrate_domains(&cfg(), dir.path(), 4).unwrap_err();
        assert!(
            err.to_string().contains("not empty"),
            "non-empty journal must block migration: {err}"
        );
    }

    #[test]
    fn sentinel_blocks_open_and_migration() {
        let dir = tempfile::tempdir().unwrap();
        drop(DomainStore::open(&cfg(), dir.path(), 2).unwrap());
        std::fs::write(dir.path().join(MIGRATE_SENTINEL_FILE), "intent").unwrap();

        let err = DomainStore::open_existing(&cfg(), dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("interrupted domain migration"),
            "open must refuse a half-swapped profile: {err}"
        );
        let err = migrate_domains(&cfg(), dir.path(), 4).unwrap_err();
        assert!(err.to_string().contains("interrupted"));
    }
}
