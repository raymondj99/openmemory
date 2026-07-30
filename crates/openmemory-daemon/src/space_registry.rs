//! Bounded lazy space-runtime registry ownership.
//!
//! The registry retains only open roots.  It never scans or eagerly opens the
//! catalog, so catalog cardinality cannot become a process-resource multiplier.

// Registry leases are private until context routing begins in Phase 4.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs4::{FileExt, TryLockError};
use openmemory_core::config::Config;
use openmemory_core::space::SpaceId;
use openmemory_engine::{partition::DomainStore, portability::FilesystemCapabilities};
use openmemory_graph::MemoryError;
use thiserror::Error;

use crate::product_store::{
    CatalogSpace, CatalogSpaceState, ProductStore, ProductStoreError, RootKey,
};
use crate::space_manifest::{
    load_manifest_for_catalog, lock_path, validate_catalog_root, ManifestError, SpaceManifest,
};

/// Conservative registry limits.  Values are validated before any runtime
/// opens, and the hard limits keep a malformed product configuration from
/// turning catalog growth into unbounded handles or connections.
// The `max_` prefix deliberately distinguishes configured upper bounds from
// the registry's current counters at every call site.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegistryLimits {
    pub(crate) max_open_spaces: usize,
    pub(crate) max_open_domains: usize,
    pub(crate) max_graph_connections: usize,
    pub(crate) max_index_handles: usize,
    pub(crate) max_context_engines: usize,
    pub(crate) max_flusher_threads: usize,
}

impl Default for RegistryLimits {
    fn default() -> Self {
        Self {
            max_open_spaces: 8,
            max_open_domains: 64,
            max_graph_connections: 192,
            max_index_handles: 64,
            max_context_engines: 2,
            max_flusher_threads: 4,
        }
    }
}

impl RegistryLimits {
    pub(crate) fn validate(self) -> Result<Self, RegistryError> {
        if self.max_open_spaces == 0
            || self.max_open_spaces > 64
            || self.max_open_domains == 0
            || self.max_open_domains > 512
            || self.max_graph_connections == 0
            || self.max_graph_connections > 1024
            || self.max_index_handles == 0
            || self.max_index_handles > 512
            || self.max_context_engines > 8
            || self.max_flusher_threads > 16
        {
            return Err(RegistryError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Debug, Error)]
pub(crate) enum RegistryError {
    #[error("registry limits are outside their hard bounds")]
    InvalidLimits,
    #[error("catalog operation failed: {0}")]
    Product(#[from] ProductStoreError),
    #[error("manifest validation failed: {0}")]
    Manifest(#[from] ManifestError),
    #[error("space store operation failed: {0}")]
    Store(#[from] MemoryError),
    #[error("space is not active in this profile")]
    Inactive,
    #[error("space is closing for exclusive maintenance")]
    Closing,
    #[error("registry resource budget is exhausted")]
    Capacity,
    #[error("space has outstanding registry leases")]
    Leased,
    #[error("space lock is held by another supported process")]
    Busy,
    #[error("space lock file is unsafe")]
    UnsafeLock,
    #[error("space lock filesystem operation failed: {0}")]
    LockIo(#[from] std::io::Error),
}

#[derive(Debug)]
enum LockMode {
    Shared,
    Exclusive,
}

/// A stable advisory lock retained for the complete runtime/maintenance
/// lifetime.  It intentionally lives next to a space root, outside a future
/// replaceable `store/` child.
#[derive(Debug)]
pub(crate) struct SpaceLock {
    file: File,
    path: PathBuf,
    mode: LockMode,
}

impl SpaceLock {
    pub(crate) fn shared(path: &Path) -> Result<Self, RegistryError> {
        Self::acquire(path, LockMode::Shared)
    }

    pub(crate) fn exclusive(path: &Path) -> Result<Self, RegistryError> {
        Self::acquire(path, LockMode::Exclusive)
    }

    fn acquire(path: &Path, mode: LockMode) -> Result<Self, RegistryError> {
        if matches!(std::fs::symlink_metadata(path), Ok(metadata) if metadata.file_type().is_symlink())
        {
            return Err(RegistryError::UnsafeLock);
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // A pre-existing lock contains no data and must not be erased.
            .truncate(false)
            .open(path)?;
        let locked = match mode {
            LockMode::Shared => FileExt::try_lock_shared(&file),
            LockMode::Exclusive => FileExt::try_lock(&file),
        };
        match locked {
            Ok(()) => Ok(Self {
                file,
                path: path.to_path_buf(),
                mode,
            }),
            Err(TryLockError::WouldBlock) => Err(RegistryError::Busy),
            Err(TryLockError::Error(error)) => Err(RegistryError::LockIo(error)),
        }
    }

    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub(crate) const fn is_exclusive(&self) -> bool {
        matches!(self.mode, LockMode::Exclusive)
    }
}

impl Drop for SpaceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Readiness facts that can be safely exposed to Phase 2 callers.  Graph and
/// index readiness remains unavailable until their owning phases implement it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpaceReadiness {
    StoreVerified,
}

/// Lazily resident runtime metadata.  The lifetime lock is intentionally kept
/// private so every future `DomainStore`/context-engine addition inherits the
/// same root lifetime rather than opening a parallel lock.
#[derive(Debug)]
pub(crate) struct SpaceRuntime {
    catalog: CatalogSpace,
    root: PathBuf,
    manifest: SpaceManifest,
    filesystem: FilesystemCapabilities,
    readiness: SpaceReadiness,
    store: Arc<DomainStore>,
    graph_connections: usize,
    index_handles: usize,
    _lock: SpaceLock,
}

impl SpaceRuntime {
    #[must_use]
    pub(crate) fn catalog(&self) -> &CatalogSpace {
        &self.catalog
    }

    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub(crate) fn manifest(&self) -> &SpaceManifest {
        &self.manifest
    }

    #[must_use]
    pub(crate) const fn filesystem(&self) -> FilesystemCapabilities {
        self.filesystem
    }

    #[must_use]
    pub(crate) const fn readiness(&self) -> SpaceReadiness {
        self.readiness
    }

    #[must_use]
    pub(crate) fn store(&self) -> &DomainStore {
        &self.store
    }
}

#[derive(Debug)]
struct RegistryInner {
    runtimes: HashMap<SpaceId, Arc<SpaceRuntime>>,
    leases: HashMap<SpaceId, usize>,
    last_used: HashMap<SpaceId, u64>,
    open_domains: usize,
    graph_connections: usize,
    index_handles: usize,
    context_engines: usize,
    flusher_threads: usize,
    tick: u64,
    closing: HashSet<SpaceId>,
}

/// Bounded, profile-local runtime registry.
pub(crate) struct SpaceRegistry {
    catalog: ProductStore,
    config: Config,
    #[cfg(feature = "embeddings")]
    embedder: Option<Arc<dyn openmemory_core::testing::Embedder>>,
    profile_root: PathBuf,
    limits: RegistryLimits,
    legacy_personal_global: SpaceId,
    inner: Arc<Mutex<RegistryInner>>,
}

impl std::fmt::Debug for SpaceRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpaceRegistry")
            .field("profile_root", &self.profile_root)
            .field("limits", &self.limits)
            .field("legacy_personal_global", &self.legacy_personal_global)
            .finish_non_exhaustive()
    }
}

impl SpaceRegistry {
    pub(crate) fn new(
        catalog: ProductStore,
        config: Config,
        profile_root: PathBuf,
        limits: RegistryLimits,
        legacy_personal_global: SpaceId,
    ) -> Result<Self, RegistryError> {
        Ok(Self {
            catalog,
            config,
            #[cfg(feature = "embeddings")]
            embedder: None,
            profile_root,
            limits: limits.validate()?,
            legacy_personal_global,
            inner: Arc::new(Mutex::new(RegistryInner {
                runtimes: HashMap::new(),
                leases: HashMap::new(),
                last_used: HashMap::new(),
                open_domains: 0,
                graph_connections: 0,
                index_handles: 0,
                context_engines: 0,
                flusher_threads: 0,
                tick: 0,
                closing: HashSet::new(),
            })),
        })
    }

    #[cfg(feature = "embeddings")]
    pub(crate) fn with_embedder(
        mut self,
        embedder: Option<Arc<dyn openmemory_core::testing::Embedder>>,
    ) -> Self {
        self.embedder = embedder;
        self
    }

    pub(crate) fn acquire(&self, id: SpaceId) -> Result<RuntimeLease, RegistryError> {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if inner.closing.contains(&id) {
            return Err(RegistryError::Closing);
        }
        if let Some(runtime) = inner.runtimes.get(&id).cloned() {
            increment_lease(&mut inner, id);
            return Ok(RuntimeLease {
                id,
                runtime,
                inner: Arc::clone(&self.inner),
            });
        }

        let catalog = self
            .catalog
            .space_by_id(id)?
            .ok_or(RegistryError::Inactive)?;
        if catalog.state != CatalogSpaceState::Active {
            return Err(RegistryError::Inactive);
        }
        let domains = catalog.domain_count as usize;
        let graph_connections = domains
            .checked_mul(self.config.num_jobs().saturating_add(1))
            .ok_or(RegistryError::Capacity)?;
        let index_handles = domains;
        self.reserve_capacity(&mut inner, domains, graph_connections, index_handles, 0, 0)?;
        let root = validate_catalog_root(&self.profile_root, &catalog)?;
        let manifest = load_manifest_for_catalog(&self.profile_root, &catalog)?;
        let filesystem = FilesystemCapabilities::inspect(&root).map_err(ManifestError::from)?;
        let lock = SpaceLock::shared(&lock_path(&self.profile_root, &catalog.root_key))?;
        let store_root = match catalog.root_key {
            RootKey::LegacyRoot => root.clone(),
            RootKey::Space(_) => root.join("store"),
        };
        #[cfg(feature = "embeddings")]
        let store = Arc::new(if let Some(embedder) = &self.embedder {
            DomainStore::open_with_embedder(
                &self.config,
                &store_root,
                domains,
                Arc::clone(embedder),
            )?
            .bind_space_id(id)?
        } else {
            DomainStore::open_scoped(&self.config, &store_root, domains, id)?
        });
        #[cfg(not(feature = "embeddings"))]
        let store = Arc::new(DomainStore::open_scoped(
            &self.config,
            &store_root,
            domains,
            id,
        )?);
        if store.space_id() != Some(id) || store.domains() != domains {
            return Err(RegistryError::Store(MemoryError::InvalidInput(
                "opened store does not match its catalog identity".to_owned(),
            )));
        }
        store.status()?;
        let runtime = Arc::new(SpaceRuntime {
            catalog,
            root,
            manifest,
            filesystem,
            readiness: SpaceReadiness::StoreVerified,
            store,
            graph_connections,
            index_handles,
            _lock: lock,
        });
        inner.open_domains += domains;
        inner.graph_connections += graph_connections;
        inner.index_handles += index_handles;
        inner.runtimes.insert(id, Arc::clone(&runtime));
        increment_lease(&mut inner, id);
        Ok(RuntimeLease {
            id,
            runtime,
            inner: Arc::clone(&self.inner),
        })
    }

    /// Prevent new leases, wait for existing callers to leave (the caller
    /// retries after draining), then retain an exclusive cross-process lock.
    pub(crate) fn close_for_maintenance(
        &self,
        id: SpaceId,
    ) -> Result<ExclusiveSpaceLease, RegistryError> {
        let catalog = self
            .catalog
            .space_by_id(id)?
            .ok_or(RegistryError::Inactive)?;
        if catalog.state != CatalogSpaceState::Active {
            return Err(RegistryError::Inactive);
        }
        let runtime = {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            if inner.closing.contains(&id) {
                return Err(RegistryError::Closing);
            }
            if inner.leases.get(&id).copied().unwrap_or(0) != 0 {
                return Err(RegistryError::Leased);
            }
            if let Some(runtime) = inner.runtimes.get(&id) {
                flush_runtime(runtime)?;
            }
            inner.closing.insert(id);
            let runtime = inner.runtimes.remove(&id);
            if let Some(runtime) = &runtime {
                release_resources(&mut inner, runtime);
                inner.last_used.remove(&id);
            }
            runtime
        };
        drop(runtime);
        let root = match validate_catalog_root(&self.profile_root, &catalog) {
            Ok(root) => root,
            Err(error) => {
                self.finish_closing(id);
                return Err(error.into());
            }
        };
        let lock = match SpaceLock::exclusive(&lock_path(&self.profile_root, &catalog.root_key)) {
            Ok(lock) => lock,
            Err(error) => {
                self.finish_closing(id);
                return Err(error);
            }
        };
        Ok(ExclusiveSpaceLease {
            id,
            root,
            lock_path: lock.path().to_path_buf(),
            lock: Some(lock),
            inner: Arc::clone(&self.inner),
        })
    }

    #[must_use]
    pub(crate) fn open_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .runtimes
            .len()
    }

    #[must_use]
    pub(crate) fn open_domain_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .open_domains
    }

    fn reserve_capacity(
        &self,
        inner: &mut RegistryInner,
        requested_domains: usize,
        requested_graph_connections: usize,
        requested_index_handles: usize,
        requested_context_engines: usize,
        requested_flusher_threads: usize,
    ) -> Result<(), RegistryError> {
        if requested_domains > self.limits.max_open_domains
            || requested_graph_connections > self.limits.max_graph_connections
            || requested_index_handles > self.limits.max_index_handles
            || requested_context_engines > self.limits.max_context_engines
            || requested_flusher_threads > self.limits.max_flusher_threads
        {
            return Err(RegistryError::Capacity);
        }
        while inner.runtimes.len() >= self.limits.max_open_spaces
            || inner.open_domains + requested_domains > self.limits.max_open_domains
            || inner.graph_connections + requested_graph_connections
                > self.limits.max_graph_connections
            || inner.index_handles + requested_index_handles > self.limits.max_index_handles
            || inner.context_engines + requested_context_engines > self.limits.max_context_engines
            || inner.flusher_threads + requested_flusher_threads > self.limits.max_flusher_threads
        {
            let candidate = inner
                .last_used
                .iter()
                .filter(|(id, _)| {
                    **id != self.legacy_personal_global
                        && inner.leases.get(id).copied().unwrap_or(0) == 0
                })
                .min_by_key(|(_, tick)| *tick)
                .map(|(id, _)| *id);
            let Some(candidate) = candidate else {
                return Err(RegistryError::Capacity);
            };
            if let Some(runtime) = inner.runtimes.get(&candidate) {
                flush_runtime(runtime)?;
            }
            if let Some(runtime) = inner.runtimes.remove(&candidate) {
                release_resources(inner, &runtime);
            }
            inner.last_used.remove(&candidate);
            inner.leases.remove(&candidate);
        }
        Ok(())
    }

    fn finish_closing(&self, id: SpaceId) {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closing
            .remove(&id);
    }
}

fn flush_runtime(runtime: &SpaceRuntime) -> Result<(), RegistryError> {
    for domain in runtime.store.stores() {
        domain.persist_search_index()?;
        let report = domain.wal_checkpoint()?;
        if !report.complete {
            return Err(RegistryError::Store(MemoryError::InvalidInput(
                "space WAL could not be fully checkpointed".to_owned(),
            )));
        }
    }
    Ok(())
}

fn release_resources(inner: &mut RegistryInner, runtime: &SpaceRuntime) {
    inner.open_domains = inner
        .open_domains
        .saturating_sub(runtime.catalog.domain_count as usize);
    inner.graph_connections = inner
        .graph_connections
        .saturating_sub(runtime.graph_connections);
    inner.index_handles = inner.index_handles.saturating_sub(runtime.index_handles);
}

fn increment_lease(inner: &mut RegistryInner, id: SpaceId) {
    *inner.leases.entry(id).or_insert(0) += 1;
    inner.tick = inner.tick.saturating_add(1);
    inner.last_used.insert(id, inner.tick);
}

/// RAII request/job lease over a resident runtime.
#[derive(Debug)]
pub(crate) struct RuntimeLease {
    id: SpaceId,
    runtime: Arc<SpaceRuntime>,
    inner: Arc<Mutex<RegistryInner>>,
}

impl RuntimeLease {
    #[must_use]
    pub(crate) fn runtime(&self) -> &SpaceRuntime {
        &self.runtime
    }

    pub(crate) fn store(&self) -> Arc<DomainStore> {
        Arc::clone(&self.runtime.store)
    }
}

impl Drop for RuntimeLease {
    fn drop(&mut self) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(leases) = inner.leases.get_mut(&self.id) {
            *leases = leases.saturating_sub(1);
        }
        inner.tick = inner.tick.saturating_add(1);
        let tick = inner.tick;
        inner.last_used.insert(self.id, tick);
    }
}

/// RAII exclusive-maintenance guard.  Its drop releases both the process lock
/// and the registry close gate exactly once.
#[derive(Debug)]
pub(crate) struct ExclusiveSpaceLease {
    id: SpaceId,
    root: PathBuf,
    lock_path: PathBuf,
    lock: Option<SpaceLock>,
    inner: Arc<Mutex<RegistryInner>>,
}

impl ExclusiveSpaceLease {
    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub(crate) fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

impl Drop for ExclusiveSpaceLease {
    fn drop(&mut self) {
        self.lock.take();
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closing
            .remove(&self.id);
    }
}
