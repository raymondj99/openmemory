//! Physical memory roots and bounded, deterministic layered recall.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openmemory_core::config::Config;
use openmemory_engine::partition::DomainStore;
use openmemory_graph::{RecallFilters, RecallResult};
use serde::{Deserialize, Serialize};

use crate::{PocError, PocResult};

pub const MAX_READ_SET: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SpaceId(String);

impl SpaceId {
    /// Parse a path-safe catalog identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, overlong, or path-like identifiers.
    pub fn parse(value: impl Into<String>) -> PocResult<Self> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 128 {
            return Err(PocError::Invalid(
                "space ID must contain 1..=128 characters".to_string(),
            ));
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(PocError::Invalid(format!(
                "space ID contains a path or unsupported character: {value:?}"
            )));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SpaceOwner {
    User(String),
    Team(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SpaceContext {
    Global,
    Project(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceDescriptor {
    pub id: SpaceId,
    pub owner: SpaceOwner,
    pub context: SpaceContext,
    pub root: PathBuf,
}

/// One independently persisted production graph root.
pub struct MemorySpace {
    descriptor: SpaceDescriptor,
    store: Arc<DomainStore>,
}

impl std::fmt::Debug for MemorySpace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemorySpace")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

impl MemorySpace {
    /// Open the independent production graph root described by `descriptor`.
    ///
    /// # Errors
    ///
    /// Returns an error when the graph root cannot be opened.
    pub fn open(config: &Config, descriptor: SpaceDescriptor) -> PocResult<Self> {
        let store = DomainStore::open(config, &descriptor.root, config.engine.domains)?;
        Ok(Self {
            descriptor,
            store: Arc::new(store),
        })
    }

    #[must_use]
    pub fn descriptor(&self) -> &SpaceDescriptor {
        &self.descriptor
    }

    #[must_use]
    pub fn store(&self) -> &Arc<DomainStore> {
        &self.store
    }
}

#[derive(Debug)]
enum CatalogEntry {
    Closed(SpaceDescriptor),
    Open(Arc<MemorySpace>),
}

/// Catalog metadata is cheap; only selected spaces need an open graph handle.
#[derive(Debug, Default)]
pub struct SpaceRegistry {
    entries: HashMap<SpaceId, CatalogEntry>,
}

impl SpaceRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register catalog metadata without opening its graph root.
    ///
    /// # Errors
    ///
    /// Returns an error when the space ID is already registered.
    pub fn register_closed(&mut self, descriptor: SpaceDescriptor) -> PocResult<()> {
        let id = descriptor.id.clone();
        if self
            .entries
            .insert(id.clone(), CatalogEntry::Closed(descriptor))
            .is_some()
        {
            return Err(PocError::Conflict(format!(
                "space {:?} is already registered",
                id.as_str()
            )));
        }
        Ok(())
    }

    /// Register an already-open graph root.
    ///
    /// # Errors
    ///
    /// Returns an error when the space ID is already registered.
    pub fn insert_open(&mut self, space: MemorySpace) -> PocResult<()> {
        let id = space.descriptor.id.clone();
        if self
            .entries
            .insert(id.clone(), CatalogEntry::Open(Arc::new(space)))
            .is_some()
        {
            return Err(PocError::Conflict(format!(
                "space {:?} is already registered",
                id.as_str()
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn registered_count(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn descriptor(&self, id: &SpaceId) -> Option<&SpaceDescriptor> {
        self.entries.get(id).map(|entry| match entry {
            CatalogEntry::Closed(descriptor) => descriptor,
            CatalogEntry::Open(space) => space.descriptor(),
        })
    }

    fn open_space(&self, id: &SpaceId) -> PocResult<&Arc<MemorySpace>> {
        match self.entries.get(id) {
            Some(CatalogEntry::Open(space)) => Ok(space),
            Some(CatalogEntry::Closed(_)) => Err(PocError::Invalid(format!(
                "space {:?} is registered but not open",
                id.as_str()
            ))),
            None => Err(PocError::NotFound(format!("space {:?}", id.as_str()))),
        }
    }

    /// Query selected spaces on the caller thread and fuse their results.
    ///
    /// # Errors
    ///
    /// Returns an error when a selected space is absent, closed, or its
    /// recall fails.
    pub fn recall_sequential(
        &self,
        read_set: &ReadSet,
        query: &str,
        top_k: usize,
        filters: &RecallFilters,
    ) -> PocResult<Vec<LayeredRecallHit>> {
        let mut candidates = Vec::new();
        for (priority, id) in read_set.ids.iter().enumerate() {
            let space = self.open_space(id)?;
            let hits = space.store.recall(query, top_k, filters)?;
            candidates.push((priority, id.clone(), hits));
        }
        Ok(merge_results(candidates, top_k))
    }

    /// Query selected spaces concurrently and fuse their results.
    ///
    /// # Errors
    ///
    /// Returns an error when a selected space is absent, closed, its recall
    /// fails, or a scoped worker panics.
    pub fn recall_parallel(
        &self,
        read_set: &ReadSet,
        query: &str,
        top_k: usize,
        filters: &RecallFilters,
    ) -> PocResult<Vec<LayeredRecallHit>> {
        let mut results = std::thread::scope(|scope| -> PocResult<Vec<_>> {
            let mut handles = Vec::with_capacity(read_set.ids.len());
            for (priority, id) in read_set.ids.iter().enumerate() {
                let space = self.open_space(id)?;
                handles.push(scope.spawn(move || {
                    space
                        .store
                        .recall(query, top_k, filters)
                        .map(|hits| (priority, id.clone(), hits))
                }));
            }
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| {
                            PocError::Invalid("layered recall worker panicked".to_string())
                        })?
                        .map_err(PocError::from)
                })
                .collect()
        })?;
        results.sort_by_key(|(priority, _, _)| *priority);
        Ok(merge_results(results, top_k))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadSet {
    ids: Vec<SpaceId>,
}

impl ReadSet {
    /// Build an ordered, bounded, duplicate-free read set.
    ///
    /// # Errors
    ///
    /// Returns an error unless the set contains one through
    /// [`MAX_READ_SET`] unique IDs.
    pub fn new(ids: Vec<SpaceId>) -> PocResult<Self> {
        if ids.is_empty() || ids.len() > MAX_READ_SET {
            return Err(PocError::Invalid(format!(
                "read set must contain 1..={MAX_READ_SET} spaces"
            )));
        }
        let unique: HashSet<_> = ids.iter().collect();
        if unique.len() != ids.len() {
            return Err(PocError::Invalid(
                "read set must not contain duplicate spaces".to_string(),
            ));
        }
        Ok(Self { ids })
    }

    #[must_use]
    pub fn ids(&self) -> &[SpaceId] {
        &self.ids
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayeredRecallHit {
    pub hit: RecallResult,
    /// Ordered provenance. The first space won deterministic ranking.
    pub origins: Vec<SpaceId>,
    pub priority: usize,
}

fn merge_results(
    candidates: Vec<(usize, SpaceId, Vec<RecallResult>)>,
    top_k: usize,
) -> Vec<LayeredRecallHit> {
    let flat = candidates
        .into_iter()
        .flat_map(|(priority, space_id, hits)| {
            hits.into_iter().map(move |hit| LayeredRecallHit {
                hit,
                origins: vec![space_id.clone()],
                priority,
            })
        })
        .collect::<Vec<_>>();
    merge_layered_hits(flat, top_k)
}

/// Deterministic candidate fusion, exposed so the isolated performance runner
/// measures the exact implementation used by layered recall.
#[must_use]
pub fn merge_layered_hits(mut flat: Vec<LayeredRecallHit>, top_k: usize) -> Vec<LayeredRecallHit> {
    flat.sort_by(|left, right| {
        right
            .hit
            .score
            .total_cmp(&left.hit.score)
            .then_with(|| left.priority.cmp(&right.priority))
            .then_with(|| left.origins[0].cmp(&right.origins[0]))
            .then_with(|| left.hit.observation.id.cmp(&right.hit.observation.id))
    });

    let mut positions: HashMap<(String, String), usize> = HashMap::new();
    let mut merged: Vec<LayeredRecallHit> = Vec::with_capacity(flat.len().min(top_k));
    for candidate in flat {
        let key = (
            candidate.hit.entity_name.to_lowercase(),
            candidate.hit.observation.content.clone(),
        );
        if let Some(position) = positions.get(&key).copied() {
            let origin = candidate.origins[0].clone();
            if !merged[position].origins.contains(&origin) {
                merged[position].origins.push(origin);
            }
            continue;
        }
        if merged.len() == top_k {
            continue;
        }
        positions.insert(key, merged.len());
        merged.push(candidate);
    }
    merged
}

#[must_use]
pub fn descriptor(
    id: SpaceId,
    owner: SpaceOwner,
    context: SpaceContext,
    root: impl AsRef<Path>,
) -> SpaceDescriptor {
    SpaceDescriptor {
        id,
        owner,
        context,
        root: root.as_ref().to_path_buf(),
    }
}
