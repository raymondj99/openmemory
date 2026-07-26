//! Bounded, lease-pinned cache of open semantic space runtimes.

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use openmemory_core::space::SpaceId;
use openmemory_core::space::{ActorKind, MemoryContext, SpaceOwner};
use openmemory_engine::partition::DomainStore;
use openmemory_engine::space::{SpaceManifest, SpaceReadHandle};
use thiserror::Error;

use crate::spaces::{LocalSpaceService, SpaceServiceError};

const HARD_MAX_OPEN_SPACES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceReadiness {
    Ready,
    DegradedIndex,
    DegradedMirrors,
}

#[derive(Debug)]
pub struct SpaceRuntime {
    pub id: SpaceId,
    pub manifest: SpaceManifest,
    pub domains: Arc<DomainStore>,
    pub readiness: SpaceReadiness,
    catalog_generation: u64,
}

#[derive(Debug)]
struct Entry {
    runtime: Arc<SpaceRuntime>,
    leases: usize,
    last_used: Instant,
    pinned: bool,
    closing: bool,
}

#[derive(Debug, Default)]
struct RegistryState {
    entries: HashMap<SpaceId, Entry>,
    opening: HashSet<SpaceId>,
    promotion_blocks: HashSet<SpaceId>,
}

#[derive(Debug)]
struct RegistryInner {
    service: LocalSpaceService,
    max_open: usize,
    idle_ttl: Duration,
    state: Mutex<RegistryState>,
    changed: Condvar,
}

#[derive(Debug, Error)]
pub enum SpaceRegistryError {
    #[error("space registry capacity is exhausted by leased runtimes")]
    Capacity,
    #[error("space runtime is closing for maintenance")]
    Closing,
    #[error("space runtime catalog generation is stale")]
    StaleCatalog,
    #[error("space runtime failed to open: {0}")]
    Open(String),
}

impl From<SpaceServiceError> for SpaceRegistryError {
    fn from(error: SpaceServiceError) -> Self {
        Self::Open(error.to_string())
    }
}

/// A runtime lease. Dropping it makes the entry eligible for idle eviction.
#[derive(Debug)]
pub struct SpaceLease {
    inner: Arc<RegistryInner>,
    runtime: Arc<SpaceRuntime>,
}

impl Deref for SpaceLease {
    type Target = SpaceRuntime;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

impl Drop for SpaceLease {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = state.entries.get_mut(&self.runtime.id) {
            entry.leases = entry.leases.saturating_sub(1);
            entry.last_used = Instant::now();
        }
        self.inner.changed.notify_all();
    }
}

/// Synchronous registry with one opener per `SpaceId` and no polling thread.
#[derive(Debug, Clone)]
pub struct SpaceRegistry {
    inner: Arc<RegistryInner>,
}

impl SpaceRegistry {
    pub fn new(
        service: LocalSpaceService,
        max_open_spaces: usize,
        idle_ttl: Duration,
    ) -> Result<Self, SpaceRegistryError> {
        if max_open_spaces == 0 || max_open_spaces > HARD_MAX_OPEN_SPACES {
            return Err(SpaceRegistryError::Capacity);
        }
        Ok(Self {
            inner: Arc::new(RegistryInner {
                service,
                max_open: max_open_spaces,
                idle_ttl,
                state: Mutex::new(RegistryState::default()),
                changed: Condvar::new(),
            }),
        })
    }

    /// Lease an open runtime, opening it once if absent.
    pub fn lease(
        &self,
        id: SpaceId,
        catalog_generation: u64,
        pinned: bool,
    ) -> Result<SpaceLease, SpaceRegistryError> {
        loop {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.promotion_blocks.contains(&id) {
                return Err(SpaceRegistryError::Closing);
            }
            if let Some(entry) = state.entries.get_mut(&id) {
                if entry.closing {
                    return Err(SpaceRegistryError::Closing);
                }
                if entry.runtime.catalog_generation != catalog_generation {
                    if entry.leases != 0 {
                        return Err(SpaceRegistryError::StaleCatalog);
                    }
                    state.entries.remove(&id);
                    continue;
                }
                entry.leases += 1;
                entry.last_used = Instant::now();
                let runtime = Arc::clone(&entry.runtime);
                return Ok(SpaceLease {
                    inner: Arc::clone(&self.inner),
                    runtime,
                });
            }
            if state.opening.contains(&id) {
                state = self
                    .inner
                    .changed
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
                drop(state);
                continue;
            }
            evict_locked(&mut state, self.inner.max_open, self.inner.idle_ttl);
            if state.entries.len() >= self.inner.max_open {
                return Err(SpaceRegistryError::Capacity);
            }
            state.opening.insert(id);
            drop(state);

            let opened = self.open_runtime(id, catalog_generation);
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.opening.remove(&id);
            match opened {
                Ok(runtime) => {
                    state.entries.insert(
                        id,
                        Entry {
                            runtime: Arc::clone(&runtime),
                            leases: 1,
                            last_used: Instant::now(),
                            pinned,
                            closing: false,
                        },
                    );
                    self.inner.changed.notify_all();
                    return Ok(SpaceLease {
                        inner: Arc::clone(&self.inner),
                        runtime,
                    });
                }
                Err(error) => {
                    self.inner.changed.notify_all();
                    return Err(error);
                }
            }
        }
    }

    fn open_runtime(
        &self,
        id: SpaceId,
        catalog_generation: u64,
    ) -> Result<Arc<SpaceRuntime>, SpaceRegistryError> {
        let summary = self.inner.service.get(id)?;
        if summary.catalog_generation != catalog_generation {
            return Err(SpaceRegistryError::StaleCatalog);
        }
        let handle = self.inner.service.open_handle(id)?;
        Ok(Arc::new(SpaceRuntime {
            id,
            manifest: handle.manifest().clone(),
            domains: Arc::clone(handle.domains()),
            readiness: SpaceReadiness::Ready,
            catalog_generation,
        }))
    }

    /// Close an unleased runtime and prevent new leases during promotion.
    /// A caller-provided deadline bounds waiting for in-flight requests.
    pub fn close_for_promotion(
        &self,
        id: SpaceId,
        timeout: Duration,
    ) -> Result<(), SpaceRegistryError> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !state.promotion_blocks.insert(id) {
            return Err(SpaceRegistryError::Closing);
        }
        if let Some(entry) = state.entries.get_mut(&id) {
            entry.closing = true;
        }
        while state
            .entries
            .get(&id)
            .is_some_and(|entry| entry.leases != 0)
        {
            let now = Instant::now();
            if now >= deadline {
                if let Some(entry) = state.entries.get_mut(&id) {
                    entry.closing = false;
                }
                state.promotion_blocks.remove(&id);
                self.inner.changed.notify_all();
                return Err(SpaceRegistryError::Closing);
            }
            let duration = deadline.saturating_duration_since(now);
            let (next, _) = self
                .inner
                .changed
                .wait_timeout(state, duration)
                .unwrap_or_else(|error| error.into_inner());
            state = next;
        }
        state.entries.remove(&id);
        self.inner.changed.notify_all();
        Ok(())
    }

    /// Re-enable leases after promotion or a failed precondition. The next
    /// lease opens and verifies the current catalog generation from disk.
    pub fn reopen_after_promotion(&self, id: SpaceId) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.entries.remove(&id);
        state.promotion_blocks.remove(&id);
        self.inner.changed.notify_all();
    }

    /// Bind an authorized immutable context to concrete registry leases for
    /// one MCP request. Catalog and authority generations were validated by
    /// the context service immediately before this call.
    pub fn mcp_context(
        &self,
        context: MemoryContext,
    ) -> Result<openmemory_mcp::McpResolvedContext, SpaceRegistryError> {
        let mut reads = Vec::with_capacity(context.read_set.len());
        let mut guards = Vec::<Arc<dyn Send + Sync>>::with_capacity(context.read_set.len());
        let mut write_store = None;
        for (priority, grant) in context.read_set.iter().enumerate() {
            let summary = self.inner.service.get(grant.space.id)?;
            let lease = Arc::new(self.lease(
                grant.space.id,
                summary.catalog_generation,
                false,
            )?);
            let domains = Arc::clone(&lease.domains);
            if grant.space.id == context.default_write {
                write_store = Some(Arc::clone(&domains));
            }
            reads.push(SpaceReadHandle {
                space: grant.space.clone(),
                read_priority: u8::try_from(priority).unwrap_or(u8::MAX),
                domains,
            });
            guards.push(lease);
        }
        let write_store = write_store.ok_or_else(|| {
            SpaceRegistryError::Open("context write target is outside its read set".to_string())
        })?;
        let proposal_required = context.actor_kind == ActorKind::Agent
            && context
                .read_set
                .iter()
                .find(|grant| grant.space.id == context.default_write)
                .is_some_and(|grant| matches!(grant.space.owner, SpaceOwner::Team(_)));
        openmemory_mcp::McpResolvedContext::new(
            context,
            reads,
            write_store,
            proposal_required,
            guards,
        )
        .map_err(|error| SpaceRegistryError::Open(error.to_string()))
    }

    /// Maintenance-cadence eviction; pinned and leased entries stay open.
    pub fn evict_idle(&self) -> usize {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let before = state.entries.len();
        let now = Instant::now();
        state.entries.retain(|_, entry| {
            entry.pinned
                || entry.leases != 0
                || now.duration_since(entry.last_used) < self.inner.idle_ttl
        });
        before - state.entries.len()
    }

    #[must_use]
    pub fn open_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .len()
    }
}

fn evict_locked(state: &mut RegistryState, max_open: usize, idle_ttl: Duration) {
    if state.entries.len() < max_open {
        return;
    }
    let now = Instant::now();
    let victim = state
        .entries
        .iter()
        .filter(|(_, entry)| {
            !entry.pinned
                && !entry.closing
                && entry.leases == 0
                && now.duration_since(entry.last_used) >= idle_ttl
        })
        .min_by_key(|(_, entry)| entry.last_used)
        .map(|(id, _)| *id);
    if let Some(victim) = victim {
        state.entries.remove(&victim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::CreateSpace;
    use openmemory_core::config::Config;
    use openmemory_core::space::{PrincipalId, SpaceContext, SpaceOwner};

    fn setup() -> (
        SpaceRegistry,
        crate::spaces::SpaceSummary,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let home = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let service =
            LocalSpaceService::open(home.path(), profile.path(), Config::default()).unwrap();
        let owner: PrincipalId = "local:registry".parse().unwrap();
        let space = service
            .create(
                &CreateSpace {
                    profile: "default".to_string(),
                    owner: SpaceOwner::User(owner),
                    context: SpaceContext::Global,
                    display_name: "Registry".to_string(),
                    domain_count: 1,
                },
                1,
            )
            .unwrap();
        let registry = SpaceRegistry::new(service, 1, Duration::ZERO).unwrap();
        (registry, space, home, profile)
    }

    #[test]
    fn leases_pin_and_release_one_runtime() {
        let (registry, space, _home, _profile) = setup();
        let first = registry
            .lease(space.id, space.catalog_generation, false)
            .unwrap();
        let second = registry
            .lease(space.id, space.catalog_generation, false)
            .unwrap();
        assert!(Arc::ptr_eq(&first.domains, &second.domains));
        assert_eq!(registry.open_count(), 1);
        assert_eq!(registry.evict_idle(), 0);
        drop(first);
        drop(second);
        assert_eq!(registry.evict_idle(), 1);
    }

    #[test]
    fn promotion_close_waits_for_lease_and_times_out() {
        let (registry, space, _home, _profile) = setup();
        let lease = registry
            .lease(space.id, space.catalog_generation, false)
            .unwrap();
        assert!(registry
            .close_for_promotion(space.id, Duration::ZERO)
            .is_err());
        drop(lease);
        registry
            .close_for_promotion(space.id, Duration::from_secs(1))
            .unwrap();
        assert_eq!(registry.open_count(), 0);
    }

    #[test]
    fn promotion_block_prevents_reopen_until_explicit_release() {
        let (registry, space, _home, _profile) = setup();
        registry
            .close_for_promotion(space.id, Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            registry.lease(space.id, space.catalog_generation, false),
            Err(SpaceRegistryError::Closing)
        ));
        registry.reopen_after_promotion(space.id);
        let _lease = registry
            .lease(space.id, space.catalog_generation, false)
            .unwrap();
    }
}
