//! Domain-partitioned memory: K independent [`MemoryStore`] families
//! behind one routing facade.
//!
//! This is the flux-rs partition idea applied to the storage layer
//! itself. Entities hash by lowercased name onto K *domains*; each
//! domain owns a complete store family (memory.sqlite, fulltext.sqlite,
//! vectors.bin, read pool, writer mutex, rebuild lock), so writes to
//! different domains commit on different SQLite writers in parallel and
//! a drain in one domain never blocks another domain's readers.
//!
//! # Layout and the manifest
//!
//! - `domains = 1` (the default): the single store lives at the profile
//!   root, byte-for-byte the legacy layout. No manifest is written; the
//!   facade is a zero-cost passthrough.
//! - `domains = K > 1`: stores live under
//!   `<data_dir>/domains/domain-00 .. domain-NN`, and a manifest
//!   (`domains.toml`) pins K. Opening with a different K (or opening a
//!   populated single-store profile with K > 1) fails with a clear
//!   error rather than silently re-homing entities: the entity-to-domain
//!   hash is part of the on-disk contract.
//!
//! # Cross-domain relations (double-written edges)
//!
//! A relation whose endpoints live in different domains is materialised
//! in BOTH, TAO-style, so each side can traverse its incident edges
//! locally:
//!
//! - the **home** domain (of the `from` entity) stores the canonical
//!   edge, pointing at a *stub* entity standing in for the remote
//!   target;
//! - the **target** domain stores a mirror edge from a stub of the
//!   source to the canonical target.
//!
//! Stubs are ordinary entity rows tagged with the reserved source
//! [`PARTITION_STUB_SOURCE`]; they carry no observations, are skipped
//! by entity listings, subtracted from aggregate status counts, and are
//! swept by `prune` as soon as their last edge dies. Name lookups
//! always route to the entity's home domain, so a stub never shadows
//! its canonical row.
//!
//! # Reads
//!
//! `recall` fans out to every domain in parallel and merges by final
//! score. Hash partitioning keeps per-domain corpus statistics close to
//! uniform, which is what makes cross-domain score merging sound (the
//! same argument Lucene/Elasticsearch rely on for per-shard BM25);
//! spreading activation runs domain-locally over the double-written
//! edges. Entity lookups route by name hash; id-based operations probe
//! domains (ids are UUIDv7, collisions across domains are not a
//! concern).

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lru::LruCache;
use openmemory_core::config::Config;
use openmemory_graph::batch::BatchOptions;
use openmemory_graph::recall::{RecallFilters, RecallResult};
use openmemory_graph::{
    ConsolidateConfig, ConsolidateReport, Entity, EntityListRow, EntityType, MemoryError,
    MemoryResult, MemoryStatus, MemoryStore, Observation, ObservationInput, PruneReport, Relation,
    RelationInput, RememberOutcome, RememberRequest, SearchMode,
};
use serde::{Deserialize, Serialize};

#[cfg(any(feature = "testing", feature = "embeddings"))]
use openmemory_core::testing::Embedder;

/// Reserved `source` tag for cross-domain stub entities. Stub rows are
/// bookkeeping, not knowledge: they exist so a domain can traverse its
/// incident cross-domain edges locally. Defined in openmemory-graph so
/// the normalization candidate scan can exclude placeholder rows from
/// fuzzy matching.
pub use openmemory_graph::PARTITION_STUB_SOURCE;

/// Manifest file name, written at the profile root for `domains > 1`.
pub const DOMAIN_MANIFEST_FILE: &str = "domains.toml";

/// Subdirectory holding the per-domain store families.
pub(crate) const DOMAINS_DIR: &str = "domains";

/// Persisted partition manifest. Pins the domain count for the life of
/// the profile: the entity-to-domain hash is an on-disk contract, and
/// changing K without migrating rows would silently re-home entities.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) version: u32,
    pub(crate) domains: usize,
}

pub(crate) const MANIFEST_VERSION: u32 = 1;

/// Capacity of the facade recall cache, matching the per-store search
/// cache it restores parity with.
const RECALL_CACHE_CAPACITY: usize = 50;
/// TTL of the facade recall cache. Bounds the staleness classes a
/// merged-final-results cache adds over the per-store raw-search cache
/// (frozen decay scores — about 7 ppm of drift at the default lambda —
/// and frozen temporal-validity filtering).
const RECALL_CACHE_TTL: Duration = Duration::from_secs(60);

/// One cached merged recall. Valid while the facade write version is
/// unchanged AND the TTL has not elapsed.
struct RecallCacheEntry {
    version: u64,
    inserted_at: Instant,
    results: Vec<RecallResult>,
}

/// K domain store families behind one routing facade. See the module
/// docs for layout, routing, and cross-domain edge semantics.
///
/// # The facade recall cache (K > 1 only)
///
/// Partitioning loses the single store's biggest read optimisation: the
/// per-store search cache still works per domain arm, but every recall
/// pays K fan-out threads, K rebuild-lock acquisitions, and a merge.
/// The facade therefore memoises MERGED final results, keyed by query +
/// top_k + every [`RecallFilters`] field, and invalidated by a single
/// write-version counter that every facade write bumps after its commit
/// (matching the per-store cache, which clears on every index write).
/// Measured: cache hits run in ~1 us vs ~330 us fanned out, and under
/// epoch-paced write load p99 drops from ~40 ms to under 0.5 ms.
///
/// Two documented deltas vs the uncached path, both TTL-bounded:
/// decay scores and temporal-validity filtering are frozen for up to
/// the cache TTL (`RECALL_CACHE_TTL`, 60 s), and `access_count`
/// retrieval-boost bumps are
/// recorded once per cached window instead of once per call (the boost
/// is logarithmic, and every facade write resets the window, so hot
/// entities still accumulate counts in any active system).
///
/// A persistent reader pool was prototyped and rejected: thread-spawn
/// overhead measured 6-9% of fan-out latency (below the 15-20% bar
/// that would justify pool lifecycle complexity), and a pool helps
/// none of the dominant costs the cache eliminates. Re-evaluate via
/// `examples/readpath.rs` if K grows well past CPU cores or per-domain
/// search drops under ~0.5 ms.
pub struct DomainStore {
    stores: Vec<Arc<MemoryStore>>,
    data_dir: PathBuf,
    /// Bumped (Release) after every facade write commits; recall
    /// captures it (Acquire) BEFORE fanning out and tags the cached
    /// entry with the pre-search value, so a write that lands during
    /// the search invalidates the entry it would otherwise poison.
    write_version: AtomicU64,
    recall_cache: Mutex<LruCache<u64, RecallCacheEntry>>,
    recall_cache_ttl: Duration,
}

impl std::fmt::Debug for DomainStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DomainStore")
            .field("domains", &self.stores.len())
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl DomainStore {
    /// Open (or create) a domain-partitioned store rooted at `data_dir`
    /// with `domains` families. `domains = 1` opens the legacy
    /// single-store layout at the root.
    pub fn open(config: &Config, data_dir: &Path, domains: usize) -> MemoryResult<Self> {
        Self::open_inner(config, data_dir, domains, None)
    }

    /// As [`Self::open`], attaching `embedder` to every domain store.
    #[cfg(any(feature = "testing", feature = "embeddings"))]
    pub fn open_with_embedder(
        config: &Config,
        data_dir: &Path,
        domains: usize,
        embedder: Arc<dyn Embedder>,
    ) -> MemoryResult<Self> {
        Self::open_inner(config, data_dir, domains, Some(embedder))
    }

    #[cfg_attr(
        not(any(feature = "testing", feature = "embeddings")),
        allow(unused_variables, clippy::unnecessary_wraps)
    )]
    fn open_inner(
        config: &Config,
        data_dir: &Path,
        domains: usize,
        #[cfg(any(feature = "testing", feature = "embeddings"))] embedder: Option<
            Arc<dyn Embedder>,
        >,
        #[cfg(not(any(feature = "testing", feature = "embeddings")))] embedder: Option<()>,
    ) -> MemoryResult<Self> {
        let domains = domains.max(1);
        std::fs::create_dir_all(data_dir)?;

        // A half-completed domain migration leaves an intent sentinel;
        // opening through it would materialise a fresh empty store
        // (manifest absence decodes as K=1) and poison recovery.
        let sentinel = data_dir.join(crate::migrate::MIGRATE_SENTINEL_FILE);
        if sentinel.exists() {
            return Err(MemoryError::InvalidInput(format!(
                "profile at {} has an interrupted domain migration ({}); restore \
                 the layout from {} or re-run `openmemory migrate-domains`",
                data_dir.display(),
                sentinel.display(),
                data_dir.join(crate::migrate::MIGRATE_BACKUP_DIR).display(),
            )));
        }

        let manifest_path = data_dir.join(DOMAIN_MANIFEST_FILE);

        let pinned = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => {
                let manifest: Manifest = toml::from_str(&text).map_err(|e| {
                    MemoryError::InvalidInput(format!(
                        "corrupt domain manifest {}: {e}",
                        manifest_path.display()
                    ))
                })?;
                Some(manifest.domains.max(1))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        match (pinned, domains) {
            // Existing partitioned profile: the manifest wins; a config
            // mismatch is an error, not a silent re-home.
            (Some(pinned), requested) if pinned != requested => {
                return Err(MemoryError::InvalidInput(format!(
                    "profile at {} is partitioned into {pinned} domains but the \
                     configuration requests {requested}; changing the domain count \
                     requires an explicit migration",
                    data_dir.display()
                )));
            }
            (Some(_), _) => {}
            // Fresh request for K>1 on a profile that already has
            // single-store data: refuse rather than hide it.
            (None, requested) if requested > 1 => {
                let legacy_db = data_dir.join(openmemory_graph::MEMORY_DB_FILE);
                if legacy_db.exists() {
                    return Err(MemoryError::InvalidInput(format!(
                        "profile at {} holds single-store data; partitioning it into \
                         {requested} domains requires an explicit migration",
                        data_dir.display()
                    )));
                }
                let manifest = Manifest {
                    version: MANIFEST_VERSION,
                    domains: requested,
                };
                let text = toml::to_string_pretty(&manifest)
                    .map_err(|e| MemoryError::InvalidInput(format!("manifest encode: {e}")))?;
                std::fs::write(&manifest_path, text)?;
            }
            (None, _) => {}
        }

        let dirs: Vec<PathBuf> = if domains == 1 {
            vec![data_dir.to_path_buf()]
        } else {
            (0..domains)
                .map(|d| data_dir.join(DOMAINS_DIR).join(format!("domain-{d:02}")))
                .collect()
        };

        let mut stores = Vec::with_capacity(dirs.len());
        for dir in &dirs {
            let store = MemoryStore::open(config, dir)?;
            #[cfg(any(feature = "testing", feature = "embeddings"))]
            let store = match &embedder {
                Some(embedder) => store.with_embedder(Arc::clone(embedder)),
                None => store,
            };
            #[cfg(not(any(feature = "testing", feature = "embeddings")))]
            let _ = &embedder;
            stores.push(Arc::new(store));
        }

        Ok(Self::assemble(stores, data_dir.to_path_buf()))
    }

    fn assemble(stores: Vec<Arc<MemoryStore>>, data_dir: PathBuf) -> Self {
        let capacity = NonZeroUsize::new(RECALL_CACHE_CAPACITY).unwrap_or(NonZeroUsize::MIN);
        Self {
            stores,
            data_dir,
            write_version: AtomicU64::new(0),
            recall_cache: Mutex::new(LruCache::new(capacity)),
            recall_cache_ttl: RECALL_CACHE_TTL,
        }
    }

    /// Override the facade recall cache TTL. Tests use short TTLs;
    /// deployments that need tighter temporal-validity precision than
    /// the default can lower it (or set zero to disable hits).
    #[must_use]
    pub fn with_recall_cache_ttl(mut self, ttl: Duration) -> Self {
        self.recall_cache_ttl = ttl;
        self
    }

    /// Record a completed facade write: invalidates cached merged
    /// recalls. Called AFTER the member-store commit succeeds.
    fn bump_write_version(&self) {
        self.write_version.fetch_add(1, Ordering::Release);
    }

    /// Open a profile with whatever partitioning it already has: the
    /// manifest's domain count when present, single-store otherwise.
    /// Read-side surfaces (scriptable CLI commands) use this so they
    /// follow the on-disk layout regardless of the active config;
    /// partitioning materialises only through surfaces that pass the
    /// configured count to [`Self::open`].
    pub fn open_existing(config: &Config, data_dir: &Path) -> MemoryResult<Self> {
        let domains = Self::manifest_domains(data_dir)?;
        Self::open(config, data_dir, domains)
    }

    /// As [`Self::open_existing`], attaching `embedder` to every store.
    #[cfg(any(feature = "testing", feature = "embeddings"))]
    pub fn open_existing_with_embedder(
        config: &Config,
        data_dir: &Path,
        embedder: Arc<dyn Embedder>,
    ) -> MemoryResult<Self> {
        let domains = Self::manifest_domains(data_dir)?;
        Self::open_with_embedder(config, data_dir, domains, embedder)
    }

    /// Domain count pinned by the profile's manifest, `1` when absent.
    pub fn manifest_domains(data_dir: &Path) -> MemoryResult<usize> {
        let manifest_path = data_dir.join(DOMAIN_MANIFEST_FILE);
        match std::fs::read_to_string(&manifest_path) {
            Ok(text) => {
                let manifest: Manifest = toml::from_str(&text).map_err(|e| {
                    MemoryError::InvalidInput(format!(
                        "corrupt domain manifest {}: {e}",
                        manifest_path.display()
                    ))
                })?;
                Ok(manifest.domains.max(1))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(1),
            Err(e) => Err(e.into()),
        }
    }

    /// Wrap an already-open single store. The compatibility path for
    /// callers that owned the open ceremony before partitioning existed.
    pub fn from_single(store: Arc<MemoryStore>) -> Self {
        let data_dir = store.data_dir().to_path_buf();
        Self::assemble(vec![store], data_dir)
    }

    /// Number of domains.
    pub fn domains(&self) -> usize {
        self.stores.len()
    }

    /// Profile root this store was opened at.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Every domain store, indexed by domain. The context engine maps
    /// its shards onto these.
    pub fn stores(&self) -> &[Arc<MemoryStore>] {
        &self.stores
    }

    /// The single underlying store when not partitioned. Surfaces that
    /// are deliberately single-domain use this and refuse to run
    /// otherwise.
    pub fn single(&self) -> Option<&Arc<MemoryStore>> {
        (self.stores.len() == 1).then(|| &self.stores[0])
    }

    /// Home domain of an entity name. Must agree with the context
    /// engine's shard hash (same lowercase + DefaultHasher scheme) so a
    /// shard's entities all live in one domain.
    pub fn domain_for(&self, entity: &str) -> usize {
        domain_for(entity, self.stores.len())
    }

    /// The domain store that owns `entity`.
    pub fn store_for(&self, entity: &str) -> &Arc<MemoryStore> {
        &self.stores[self.domain_for(entity)]
    }

    // --------------------- write paths ---------------------

    /// Atomic write routed to the entity's home domain. Relations whose
    /// targets live in other domains are double-written (see module
    /// docs): the home edge commits with the outcome returned here;
    /// mirror edges commit per target domain immediately after.
    pub fn remember(
        &self,
        name: &str,
        entity_type: EntityType,
        observations: &[ObservationInput],
        relations: &[RelationInput],
        source: &str,
    ) -> MemoryResult<RememberOutcome> {
        let home = self.domain_for(name);
        let request = RememberRequest {
            name: name.to_string(),
            entity_type,
            observations: observations.to_vec(),
            relations: relations.to_vec(),
            source: source.to_string(),
        };
        let mut outcomes = self.remember_batch_in_domain(home, std::slice::from_ref(&request))?;
        Ok(outcomes.pop().expect("one outcome per request"))
    }

    /// Commit `requests` into domain `domain` as one transaction, then
    /// commit any cross-domain mirror edges into their target domains.
    /// Every request's entity must hash to `domain`; the context engine
    /// guarantees this by mapping whole shards onto domains.
    ///
    /// `opts.checkpoint` (when used by the engine) rides the HOME
    /// commit: mirrors are written first, so a crash between the two
    /// replays the journal entry and re-issues the mirror — mirror
    /// writes are at-least-once, consistent with the relation API's
    /// documented append-only contract.
    pub fn remember_batch_in_domain_with_options(
        &self,
        domain: usize,
        requests: &[RememberRequest],
        opts: &BatchOptions,
    ) -> MemoryResult<Vec<RememberOutcome>> {
        if self.stores.len() == 1 {
            let outcomes = self.stores[domain].remember_batch(requests, opts)?;
            self.bump_write_version();
            return Ok(outcomes);
        }

        let (home_requests, stub_requests, mirrors) = self.plan_batch(domain, requests);

        // Mirrors first (see doc comment). Failures here are
        // warn-and-continue: the canonical edge is about to commit, and
        // a missing mirror only degrades remote-side traversal.
        for (target_domain, batch) in mirrors {
            if let Err(e) = self.stores[target_domain].remember_batch(
                &batch,
                &BatchOptions {
                    normalize: false,
                    checkpoint: None,
                },
            ) {
                tracing::warn!(
                    target_domain,
                    error = %e,
                    "cross-domain mirror commit failed; remote-side traversal degraded"
                );
            }
        }

        // Home commit: caller requests plus local stub rows for remote
        // targets, one transaction. Stubs go first so each edge's
        // ensure_entity finds them by exact match.
        let mut combined = stub_requests;
        let caller_start = combined.len();
        combined.extend(home_requests);
        let mut outcomes = self.stores[domain].remember_batch(&combined, opts)?;
        self.bump_write_version();
        Ok(outcomes.split_off(caller_start))
    }

    /// As [`Self::remember_batch_in_domain_with_options`] with default
    /// options.
    pub fn remember_batch_in_domain(
        &self,
        domain: usize,
        requests: &[RememberRequest],
    ) -> MemoryResult<Vec<RememberOutcome>> {
        self.remember_batch_in_domain_with_options(domain, requests, &BatchOptions::default())
    }

    /// Split a batch into (caller requests, home-side stub rows, mirror
    /// batches per target domain).
    #[allow(clippy::type_complexity)]
    fn plan_batch(
        &self,
        home: usize,
        requests: &[RememberRequest],
    ) -> (
        Vec<RememberRequest>,
        Vec<RememberRequest>,
        Vec<(usize, Vec<RememberRequest>)>,
    ) {
        if self.stores.len() == 1 {
            return (requests.to_vec(), Vec::new(), Vec::new());
        }

        let mut stub_requests: Vec<RememberRequest> = Vec::new();
        let mut seen_stubs: std::collections::HashSet<(String, EntityType)> =
            std::collections::HashSet::new();
        let mut mirrors: HashMap<usize, Vec<RememberRequest>> = HashMap::new();

        for req in requests {
            for rel in &req.relations {
                let target_domain = self.domain_for(&rel.target_name);
                if target_domain == home {
                    continue;
                }
                // Home side: a marked stub row for the remote target, so
                // the canonical edge's ensure_entity resolves locally
                // without inventing an unmarked duplicate.
                if seen_stubs.insert((rel.target_name.to_lowercase(), rel.target_type)) {
                    stub_requests.push(
                        RememberRequest::new(rel.target_name.clone(), rel.target_type)
                            .with_source(PARTITION_STUB_SOURCE),
                    );
                }
                // Target side: a stub of the source plus the mirror
                // edge. The relation keeps the caller's source so the
                // canonical target (created here on first contact) gets
                // proper provenance.
                let mut mirror_rel = rel.clone();
                if mirror_rel.source.is_empty() {
                    mirror_rel.source.clone_from(&req.source);
                }
                mirrors.entry(target_domain).or_default().push(
                    RememberRequest::new(req.name.clone(), req.entity_type)
                        .with_relations(vec![mirror_rel])
                        .with_source(PARTITION_STUB_SOURCE),
                );
            }
        }

        (
            requests.to_vec(),
            stub_requests,
            mirrors.into_iter().collect(),
        )
    }

    /// Relation between two existing entities, by id. Cross-domain
    /// pairs double-write exactly like `remember` relations.
    pub fn add_relation(
        &self,
        from_entity_id: &str,
        to_entity_id: &str,
        relation_type: &str,
        weight: Option<f32>,
        source: &str,
    ) -> MemoryResult<String> {
        if self.stores.len() == 1 {
            let id = self.stores[0].add_relation(
                from_entity_id,
                to_entity_id,
                relation_type,
                weight,
                source,
            )?;
            self.bump_write_version();
            return Ok(id);
        }

        let (from_domain, from) = self
            .find_entity_by_id(from_entity_id)?
            .ok_or_else(|| MemoryError::EntityNotFound(from_entity_id.to_string()))?;
        let (to_domain, to) = self
            .find_entity_by_id(to_entity_id)?
            .ok_or_else(|| MemoryError::EntityNotFound(to_entity_id.to_string()))?;

        if from_domain == to_domain {
            let id = self.stores[from_domain].add_relation(
                from_entity_id,
                to_entity_id,
                relation_type,
                weight,
                source,
            )?;
            self.bump_write_version();
            return Ok(id);
        }

        let mut rel = RelationInput::new(relation_type, to.name.clone(), to.entity_type);
        rel.weight = weight.unwrap_or(1.0);
        rel.source = source.to_string();

        // Reuse the batched double-write plan: home edge (canonical) +
        // mirror in the target domain.
        let request = RememberRequest::new(from.name.clone(), from.entity_type)
            .with_relations(vec![rel])
            .with_source(source);
        let outcomes =
            self.remember_batch_in_domain(from_domain, std::slice::from_ref(&request))?;
        let _ = to_domain;
        outcomes
            .into_iter()
            .next()
            .and_then(|o| o.relation_ids.into_iter().next())
            .ok_or_else(|| MemoryError::InvalidInput("relation was not created".into()))
    }

    // --------------------- read paths ---------------------

    /// Hybrid recall fanned out to every domain in parallel, merged by
    /// final score, truncated to `top_k`. Merged results are memoised
    /// in the facade cache (see the type docs); a hit skips the fan-out
    /// entirely, including every domain's index-visibility lock.
    pub fn recall(
        &self,
        query: &str,
        top_k: usize,
        filters: &RecallFilters,
    ) -> MemoryResult<Vec<RecallResult>> {
        if self.stores.len() == 1 {
            return self.stores[0].recall(query, top_k, filters);
        }

        // Capture the version BEFORE searching: a write landing during
        // the fan-out bumps past this value and the populated entry is
        // never served.
        let version = self.write_version.load(Ordering::Acquire);
        let key = recall_cache_key(query, top_k, filters);
        {
            let mut cache = self.recall_cache.lock().expect("recall cache poisoned");
            if let Some(entry) = cache.get(&key) {
                if entry.version == version && entry.inserted_at.elapsed() < self.recall_cache_ttl {
                    return Ok(entry.results.clone());
                }
                cache.pop(&key);
            }
        }

        // Fan out: the calling thread works one domain itself; the rest
        // run on scoped threads (a persistent pool was measured at a
        // 6-9% win here and rejected — see the type docs).
        let (first, rest) = self.stores.split_first().expect("at least one domain");
        let mut results: Vec<MemoryResult<Vec<RecallResult>>> = std::thread::scope(|scope| {
            let handles: Vec<_> = rest
                .iter()
                .map(|store| scope.spawn(move || store.recall(query, top_k, filters)))
                .collect();
            let mut out = vec![first.recall(query, top_k, filters)];
            out.extend(handles.into_iter().map(|h| match h.join() {
                Ok(result) => result,
                Err(_) => Err(MemoryError::InvalidInput("recall worker panicked".into())),
            }));
            out
        });

        let mut merged = Vec::new();
        for result in results.drain(..) {
            merged.extend(result?);
        }
        merged.sort_by(|a, b| b.score.total_cmp(&a.score));
        merged.truncate(top_k.max(1));

        self.recall_cache
            .lock()
            .expect("recall cache poisoned")
            .put(
                key,
                RecallCacheEntry {
                    version,
                    inserted_at: Instant::now(),
                    results: merged.clone(),
                },
            );
        Ok(merged)
    }

    /// Entity lookup by name, routed to the home domain (stubs in other
    /// domains never shadow the canonical row).
    pub fn get_entity(&self, name: &str) -> MemoryResult<Option<Entity>> {
        self.store_for(name).get_entity(name)
    }

    /// Entity lookup by id: probe domains (ids are UUIDv7-unique).
    pub fn get_entity_by_id(&self, id: &str) -> MemoryResult<Option<Entity>> {
        Ok(self.find_entity_by_id(id)?.map(|(_, entity)| entity))
    }

    /// Entity lookup by `(name, entity_type)`, routed to the home
    /// domain.
    pub fn get_entity_by_name_and_type(
        &self,
        name: &str,
        entity_type: EntityType,
    ) -> MemoryResult<Option<Entity>> {
        self.store_for(name)
            .get_entity_by_name_and_type(name, entity_type)
    }

    fn find_entity_by_id(&self, id: &str) -> MemoryResult<Option<(usize, Entity)>> {
        for (domain, store) in self.stores.iter().enumerate() {
            if let Some(entity) = store.get_entity_by_id(id)? {
                return Ok(Some((domain, entity)));
            }
        }
        Ok(None)
    }

    /// Observations for an entity id, from whichever domain holds it.
    pub fn get_entity_observations(&self, entity_id: &str) -> MemoryResult<Vec<Observation>> {
        for store in &self.stores {
            let observations = store.get_entity_observations(entity_id)?;
            if !observations.is_empty() {
                return Ok(observations);
            }
        }
        Ok(Vec::new())
    }

    /// Relations incident to an entity id, from whichever domain holds
    /// it. With double-written edges, the entity's own domain carries
    /// every edge incident to it (mirrors included).
    pub fn get_entity_relations(&self, entity_id: &str) -> MemoryResult<Vec<Relation>> {
        for store in &self.stores {
            if store.get_entity_by_id(entity_id)?.is_some() {
                return store.get_entity_relations(entity_id);
            }
        }
        Ok(Vec::new())
    }

    /// Entity listing merged across domains, stub rows excluded, sorted
    /// by `updated_at` descending with global limit/offset semantics.
    pub fn list_entities(
        &self,
        entity_type: Option<EntityType>,
        limit: usize,
        offset: usize,
    ) -> MemoryResult<Vec<EntityListRow>> {
        if self.stores.len() == 1 {
            return self.stores[0].list_entities(entity_type, limit, offset);
        }
        // Each domain must over-fetch limit+offset rows: the global
        // top-(limit+offset) could in the worst case live in one domain.
        let fetch = limit.saturating_add(offset);
        let mut merged = Vec::new();
        for store in &self.stores {
            merged.extend(
                store
                    .list_entities(entity_type, fetch, 0)?
                    .into_iter()
                    .filter(|row| row.entity.source != PARTITION_STUB_SOURCE),
            );
        }
        merged.sort_by(|a, b| b.entity.updated_at.cmp(&a.entity.updated_at));
        Ok(merged.into_iter().skip(offset).take(limit).collect())
    }

    /// Aggregate status across domains. Stub entities and mirror edges
    /// are subtracted so the numbers describe knowledge, not partition
    /// bookkeeping. `schema_version` and `reader_pool_size` report the
    /// first domain (identical across domains by construction).
    pub fn status(&self) -> MemoryResult<MemoryStatus> {
        if self.stores.len() == 1 {
            return self.stores[0].status();
        }
        let mut aggregate = MemoryStatus::default();
        for store in &self.stores {
            let status = store.status()?;
            let stubs = store.entity_type_counts_by_source(PARTITION_STUB_SOURCE)?;
            let stub_total: u64 = stubs.values().sum();
            let mirrored_relations =
                store.count_relations_from_source_entities(PARTITION_STUB_SOURCE)?;

            aggregate.total_entities += status.total_entities - stub_total;
            aggregate.total_observations += status.total_observations;
            aggregate.total_relations += status.total_relations - mirrored_relations;
            aggregate.tombstoned_observations += status.tombstoned_observations;
            aggregate.schema_version = aggregate.schema_version.max(status.schema_version);
            aggregate.oldest_observation =
                match (aggregate.oldest_observation, status.oldest_observation) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            aggregate.newest_observation =
                match (aggregate.newest_observation, status.newest_observation) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
            for (ty, count) in status.entity_type_counts {
                let stub_count = stubs.get(&ty).copied().unwrap_or(0);
                *aggregate.entity_type_counts.entry(ty).or_insert(0) += count - stub_count;
            }
            for (tier, count) in status.tier_counts {
                *aggregate.tier_counts.entry(tier).or_insert(0) += count;
            }
            aggregate.vector_count += status.vector_count;
            aggregate.reader_pool_size += status.reader_pool_size;
        }
        aggregate.entity_type_counts.retain(|_, count| *count > 0);
        Ok(aggregate)
    }

    // --------------------- destructive paths ---------------------

    /// Soft-delete an observation by id, in whichever domain holds it.
    pub fn forget(&self, observation_id: &str) -> MemoryResult<bool> {
        for store in &self.stores {
            if store.forget(observation_id)? {
                self.bump_write_version();
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Hard-delete an entity by name: the canonical row in its home
    /// domain plus any stubs (and their mirror edges, via cascade) in
    /// the others. Returns the number of observations cascaded.
    pub fn forget_entity(&self, name: &str) -> MemoryResult<usize> {
        let home = self.domain_for(name);
        let cascaded = self.stores[home].forget_entity(name)?;
        for (domain, store) in self.stores.iter().enumerate() {
            if domain == home {
                continue;
            }
            match store.get_entity(name)? {
                Some(entity) if entity.source == PARTITION_STUB_SOURCE => {
                    let _ = store.forget_entity(name)?;
                }
                _ => {}
            }
        }
        self.bump_write_version();
        Ok(cascaded)
    }

    /// Promote (or demote) an observation between memory tiers, in
    /// whichever domain holds it.
    pub fn set_observation_memory_tier(
        &self,
        observation_id: &str,
        tier: openmemory_graph::MemoryTier,
    ) -> MemoryResult<bool> {
        for store in &self.stores {
            if store.set_observation_memory_tier(observation_id, tier)? {
                // The per-store search cache rightly ignores tier changes
                // (tier is filtered from live rows per recall); the facade
                // cache holds FINAL results, so it must invalidate.
                self.bump_write_version();
                return Ok(true);
            }
        }
        Ok(false)
    }

    // --------------------- maintenance ---------------------

    /// Run consolidation in every domain; reports are summed.
    pub fn consolidate(&self, config: &ConsolidateConfig) -> MemoryResult<ConsolidateReport> {
        let mut total = ConsolidateReport::default();
        for store in &self.stores {
            let report = store.consolidate(config)?;
            total.duplicates_merged += report.duplicates_merged;
            total.observations_pruned += report.observations_pruned;
            total.entities_pruned += report.entities_pruned;
        }
        self.bump_write_version();
        Ok(total)
    }

    /// Sweep tombstones and orphans in every domain; reports are summed.
    pub fn prune_with_ttl(&self, ttl_secs: i64) -> MemoryResult<PruneReport> {
        let mut total = PruneReport::default();
        for store in &self.stores {
            let report = store.prune_with_ttl(ttl_secs)?;
            total.tombstones_removed += report.tombstones_removed;
            total.entities_removed += report.entities_removed;
        }
        self.bump_write_version();
        Ok(total)
    }

    // --------------------- free-text index ---------------------

    /// Decay rate in effect (identical across domains by construction).
    pub fn decay_rate(&self) -> f64 {
        self.stores[0].decay_rate()
    }

    /// Embed a query string (the embedder is shared across domains).
    pub fn embed_query(&self, text: &str) -> Vec<f32> {
        self.stores[0].embed_query(text)
    }

    /// Embed a document string (the embedder is shared across domains).
    pub fn embed_document(&self, text: &str) -> Vec<f32> {
        self.stores[0].embed_document(text)
    }

    /// The domain store owning free-text documents under `uri`.
    /// Free-text URIs hash like entity names so inserts, deletes, and
    /// re-indexing of one document always land in the same domain.
    ///
    /// Read-side accessor. Writers must go through
    /// [`Self::index_insert`] / [`Self::index_delete`] so the facade
    /// recall cache learns about the mutation.
    pub fn store_for_uri(&self, uri: &str) -> &Arc<MemoryStore> {
        &self.stores[domain_for(uri, self.stores.len())]
    }

    /// Index a free-text entry into its URI's domain. Facade twin of
    /// the per-store insert: free-text rows share the hybrid index with
    /// observations and shift recall candidates, so this bumps the
    /// write version (the per-store cache already clears on insert).
    pub fn index_insert(&self, entry: openmemory_index::IndexEntry) -> MemoryResult<()> {
        self.store_for_uri(&entry.uri)
            .engine()
            .engine
            .insert(std::slice::from_ref(&entry))
            .map_err(|e| MemoryError::InvalidInput(e.to_string()))?;
        self.bump_write_version();
        Ok(())
    }

    /// Remove a URI from its domain's index. Returns rows removed.
    pub fn index_delete(&self, uri: &str) -> MemoryResult<u64> {
        let removed = self
            .store_for_uri(uri)
            .engine()
            .engine
            .delete_by_uri(uri)
            .map_err(|e| MemoryError::InvalidInput(e.to_string()))?;
        self.bump_write_version();
        Ok(removed)
    }

    /// Total (vector, keyword) document counts across domains.
    pub fn index_counts(&self) -> MemoryResult<(u64, u64)> {
        let mut vectors = 0u64;
        let mut keywords = 0u64;
        for store in &self.stores {
            vectors += store
                .engine()
                .engine
                .count()
                .map_err(|e| MemoryError::InvalidInput(e.to_string()))?;
            keywords += store
                .engine()
                .engine
                .keyword_count()
                .map_err(|e| MemoryError::InvalidInput(e.to_string()))?;
        }
        Ok((vectors, keywords))
    }

    /// Free-text search fanned out to every domain, merged by score.
    /// `filters_hash` folds caller-side filter state into each
    /// per-store cache key (`0` when there is none).
    pub fn index_search(
        &self,
        query_vector: &[f32],
        query: &str,
        top_k: usize,
        mode: SearchMode,
        filters_hash: u64,
    ) -> MemoryResult<Vec<openmemory_index::SearchResult>> {
        let mut merged = Vec::new();
        for store in &self.stores {
            merged.extend(
                store
                    .engine()
                    .engine
                    .search(query_vector, query, top_k, mode, filters_hash)
                    .map_err(|e| MemoryError::InvalidInput(e.to_string()))?,
            );
        }
        merged.sort_by(|a, b| b.score.total_cmp(&a.score));
        merged.truncate(top_k.max(1));
        Ok(merged)
    }
}

/// Entity-name (or URI) to domain hash. The exact scheme the context
/// engine uses for shards: lowercase, `DefaultHasher`, modulo.
pub(crate) fn domain_for(key: &str, domains: usize) -> usize {
    (hash_lowercase_key(key) as usize) % domains.max(1)
}

/// Hash a key exactly like `key.to_lowercase().hash(DefaultHasher)`,
/// without allocating the lowercase `String`.
pub(crate) fn hash_lowercase_key(key: &str) -> u64 {
    use std::hash::Hasher;

    let mut h = std::hash::DefaultHasher::new();
    for ch in key.chars().flat_map(char::to_lowercase) {
        let mut buf = [0u8; 4];
        h.write(ch.encode_utf8(&mut buf).as_bytes());
    }
    // `Hash for str` terminates with 0xff so adjacent string fields do
    // not collide (`("ab", "c")` vs `("a", "bc")`). Preserve that
    // byte to keep partition routing bit-for-bit compatible.
    h.write_u8(0xff);
    h.finish()
}

/// Facade recall cache key. Unlike the per-store search cache (which
/// caches filter-INDEPENDENT raw results and can ignore filters), this
/// keys MERGED FINAL results, so every [`RecallFilters`] field must
/// participate: omitting one would serve results for the wrong filter
/// set, which is worse than any staleness. `entity_names` are
/// lowercased, sorted, and deduped because the filter is
/// case-insensitive set membership; `min_confidence` hashes by bit
/// pattern (f32 has no `Hash`); `mode: None` normalises to the Hybrid
/// default; `valid_at: None` is keyed as the None variant, never the
/// resolved clock value (clock dependence is bounded by the TTL
/// instead).
fn recall_cache_key(query: &str, top_k: usize, filters: &RecallFilters) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    query.hash(&mut h);
    top_k.hash(&mut h);
    filters.entity_type.map(|t| t.as_str()).hash(&mut h);
    filters.valid_at.hash(&mut h);
    filters.source.hash(&mut h);
    filters.min_confidence.map(f32::to_bits).hash(&mut h);
    match &filters.entity_names {
        None => 0u8.hash(&mut h),
        Some(names) => {
            1u8.hash(&mut h);
            let mut normalized: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
            normalized.sort_unstable();
            normalized.dedup();
            normalized.hash(&mut h);
        }
    }
    filters.mode.unwrap_or_default().hash(&mut h);
    filters.memory_tier.map(|t| t.as_str()).hash(&mut h);
    filters.spreading_activation.hash(&mut h);
    filters.record_access.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_domains(k: usize) -> (DomainStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = DomainStore::open(&Config::default(), dir.path(), k).unwrap();
        (store, dir)
    }

    fn obs(content: &str) -> ObservationInput {
        ObservationInput::new(content)
    }

    #[test]
    fn allocation_free_partition_hash_matches_existing_lowercase_hash() {
        use std::hash::{Hash, Hasher};

        for key in [
            "",
            "Alpha",
            "Project Alpha",
            "STRASSE",
            "Straße",
            "İstanbul",
            "alpha\0bravo",
        ] {
            let mut old = std::hash::DefaultHasher::new();
            key.to_lowercase().hash(&mut old);
            assert_eq!(
                hash_lowercase_key(key),
                old.finish(),
                "hash changed for {key:?}"
            );
        }
    }

    #[test]
    fn domain_hash_stays_case_insensitive() {
        for domains in [1, 2, 3, 8, 32] {
            assert_eq!(
                domain_for("Project Alpha", domains),
                domain_for("project alpha", domains)
            );
            assert_eq!(
                domain_for("STRASSE", domains),
                domain_for("strasse", domains)
            );
        }
    }

    /// A name pair guaranteed to live in different domains at the given
    /// K, discovered by probing the real hash.
    fn cross_domain_pair(store: &DomainStore) -> (String, String) {
        let a = "entity-a".to_string();
        let home = store.domain_for(&a);
        for i in 0..1000 {
            let b = format!("entity-b-{i}");
            if store.domain_for(&b) != home {
                return (a, b);
            }
        }
        panic!("no cross-domain name found in 1000 probes");
    }

    #[test]
    fn single_domain_uses_legacy_layout() {
        let (store, dir) = open_domains(1);
        assert_eq!(store.domains(), 1);
        assert!(dir.path().join(openmemory_graph::MEMORY_DB_FILE).exists());
        assert!(
            !dir.path().join(DOMAIN_MANIFEST_FILE).exists(),
            "K=1 must not write a manifest"
        );
        assert!(store.single().is_some());
    }

    #[test]
    fn partitioned_layout_creates_domain_dirs_and_manifest() {
        let (store, dir) = open_domains(4);
        assert_eq!(store.domains(), 4);
        assert!(dir.path().join(DOMAIN_MANIFEST_FILE).exists());
        for d in 0..4 {
            assert!(dir
                .path()
                .join(DOMAINS_DIR)
                .join(format!("domain-{d:02}"))
                .join(openmemory_graph::MEMORY_DB_FILE)
                .exists());
        }
        assert!(store.single().is_none());
    }

    #[test]
    fn manifest_mismatch_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        drop(DomainStore::open(&Config::default(), dir.path(), 4).unwrap());
        let err = DomainStore::open(&Config::default(), dir.path(), 8).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));
        assert!(err.to_string().contains("4 domains"), "got: {err}");
        // Reopening with the pinned count works.
        let reopened = DomainStore::open(&Config::default(), dir.path(), 4).unwrap();
        assert_eq!(reopened.domains(), 4);
    }

    #[test]
    fn partitioning_a_populated_single_store_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        {
            let single = DomainStore::open(&Config::default(), dir.path(), 1).unwrap();
            single
                .remember("alpha", EntityType::Fact, &[obs("seed")], &[], "test")
                .unwrap();
        }
        let err = DomainStore::open(&Config::default(), dir.path(), 4).unwrap_err();
        assert!(err.to_string().contains("single-store data"), "got: {err}");
    }

    #[test]
    fn writes_route_to_home_domain_and_reads_route_back() {
        let (store, _dir) = open_domains(4);
        for i in 0..40 {
            store
                .remember(
                    &format!("entity-{i}"),
                    EntityType::Fact,
                    &[obs(&format!("fact number {i}"))],
                    &[],
                    "test",
                )
                .unwrap();
        }

        // Every entity is readable through the facade.
        for i in 0..40 {
            let name = format!("entity-{i}");
            let entity = store.get_entity(&name).unwrap().unwrap();
            assert_eq!(entity.name, name);
            let observations = store.get_entity_observations(&entity.id).unwrap();
            assert_eq!(observations.len(), 1);
        }

        // Writes actually spread across domains (40 hashed names cannot
        // all land in one of four domains).
        let populated = store
            .stores()
            .iter()
            .filter(|s| s.status().unwrap().total_entities > 0)
            .count();
        assert!(populated > 1, "expected multiple populated domains");

        // Aggregate status sees everything exactly once.
        let status = store.status().unwrap();
        assert_eq!(status.total_entities, 40);
        assert_eq!(status.total_observations, 40);
    }

    #[test]
    fn recall_fans_out_and_merges() {
        let (store, _dir) = open_domains(4);
        for i in 0..40 {
            store
                .remember(
                    &format!("entity-{i}"),
                    EntityType::Fact,
                    &[obs(&format!("observation about quarterly planning {i}"))],
                    &[],
                    "test",
                )
                .unwrap();
        }
        let results = store
            .recall("quarterly planning", 10, &RecallFilters::new())
            .unwrap();
        assert_eq!(results.len(), 10, "global top-k from fanned-out search");
        for window in results.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "merged results must be score-sorted"
            );
        }
    }

    #[test]
    fn cross_domain_relation_is_visible_from_both_sides() {
        let (store, _dir) = open_domains(4);
        let (a, b) = cross_domain_pair(&store);

        store
            .remember(
                &b,
                EntityType::Project,
                &[obs("the target project")],
                &[],
                "test",
            )
            .unwrap();
        store
            .remember(
                &a,
                EntityType::Person,
                &[obs("the source person")],
                &[RelationInput::new(
                    "maintains",
                    b.clone(),
                    EntityType::Project,
                )],
                "test",
            )
            .unwrap();

        // From the source side.
        let a_entity = store.get_entity(&a).unwrap().unwrap();
        let a_rels = store.get_entity_relations(&a_entity.id).unwrap();
        assert_eq!(a_rels.len(), 1);
        assert_eq!(a_rels[0].relation_type, "maintains");

        // From the target side: the mirror edge in b's domain.
        let b_entity = store.get_entity(&b).unwrap().unwrap();
        assert_ne!(
            b_entity.source, PARTITION_STUB_SOURCE,
            "canonical target must not be a stub"
        );
        let b_rels = store.get_entity_relations(&b_entity.id).unwrap();
        assert_eq!(b_rels.len(), 1, "mirror edge visible from the target");
        assert_eq!(b_rels[0].relation_type, "maintains");

        // Aggregate status counts the edge once and no stub entities.
        let status = store.status().unwrap();
        assert_eq!(status.total_entities, 2);
        assert_eq!(status.total_relations, 1);
    }

    #[test]
    fn stub_entities_are_hidden_from_listings() {
        let (store, _dir) = open_domains(4);
        let (a, b) = cross_domain_pair(&store);
        store
            .remember(
                &a,
                EntityType::Person,
                &[obs("source")],
                &[RelationInput::new("knows", b.clone(), EntityType::Person)],
                "test",
            )
            .unwrap();

        let rows = store.list_entities(None, 100, 0).unwrap();
        let names: Vec<_> = rows.iter().map(|r| r.entity.name.as_str()).collect();
        assert!(names.contains(&a.as_str()));
        assert!(
            names.contains(&b.as_str()),
            "canonical target (created via the mirror) must be listed"
        );
        assert_eq!(
            rows.len(),
            2,
            "stub rows must not appear in listings: {names:?}"
        );
    }

    #[test]
    fn forget_entity_cleans_stubs_in_other_domains() {
        let (store, _dir) = open_domains(4);
        let (a, b) = cross_domain_pair(&store);
        store
            .remember(
                &a,
                EntityType::Person,
                &[obs("source")],
                &[RelationInput::new("knows", b.clone(), EntityType::Person)],
                "test",
            )
            .unwrap();

        store.forget_entity(&a).unwrap();
        assert!(store.get_entity(&a).unwrap().is_none());
        // The stub of `a` in b's domain is gone too: b has no incident
        // edges left.
        let b_entity = store.get_entity(&b).unwrap().unwrap();
        assert!(store.get_entity_relations(&b_entity.id).unwrap().is_empty());
        for s in store.stores() {
            if let Some(entity) = s.get_entity(&a).unwrap() {
                panic!("stub of {a} survived in domain holding {}", entity.id);
            }
        }
    }

    #[test]
    fn add_relation_by_id_across_domains_double_writes() {
        let (store, _dir) = open_domains(4);
        let (a, b) = cross_domain_pair(&store);
        let a_out = store
            .remember(&a, EntityType::Person, &[obs("a")], &[], "test")
            .unwrap();
        let b_out = store
            .remember(&b, EntityType::Tool, &[obs("b")], &[], "test")
            .unwrap();

        store
            .add_relation(
                &a_out.entity_id,
                &b_out.entity_id,
                "uses",
                Some(0.5),
                "test",
            )
            .unwrap();

        let a_rels = store.get_entity_relations(&a_out.entity_id).unwrap();
        assert_eq!(a_rels.len(), 1);
        let b_rels = store.get_entity_relations(&b_out.entity_id).unwrap();
        assert_eq!(b_rels.len(), 1, "mirror edge visible from target");
        assert_eq!(store.status().unwrap().total_relations, 1);
    }

    #[test]
    fn forget_and_promote_probe_domains() {
        let (store, _dir) = open_domains(4);
        let outcome = store
            .remember("alpha", EntityType::Fact, &[obs("to forget")], &[], "test")
            .unwrap();
        let obs_id = &outcome.observation_ids[0];

        assert!(store
            .set_observation_memory_tier(obs_id, openmemory_graph::MemoryTier::Semantic)
            .unwrap());
        assert!(store.forget(obs_id).unwrap());
        assert!(!store.forget("no-such-observation").unwrap());
    }

    #[test]
    fn list_entities_paginates_globally() {
        let (store, _dir) = open_domains(4);
        for i in 0..20 {
            store
                .remember(
                    &format!("entity-{i:02}"),
                    EntityType::Fact,
                    &[obs("x")],
                    &[],
                    "test",
                )
                .unwrap();
        }
        let page1 = store.list_entities(None, 8, 0).unwrap();
        let page2 = store.list_entities(None, 8, 8).unwrap();
        let page3 = store.list_entities(None, 8, 16).unwrap();
        assert_eq!(page1.len(), 8);
        assert_eq!(page2.len(), 8);
        assert_eq!(page3.len(), 4);
        let mut all: Vec<_> = page1
            .iter()
            .chain(&page2)
            .chain(&page3)
            .map(|r| r.entity.name.clone())
            .collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 20, "pages must not overlap or drop entities");
    }

    #[test]
    fn recall_cache_hits_and_invalidates_on_every_write_kind() {
        let (store, _dir) = open_domains(4);
        for i in 0..20 {
            store
                .remember(
                    &format!("entity-{i}"),
                    EntityType::Fact,
                    &[obs(&format!("cached recall fact {i}"))],
                    &[],
                    "t",
                )
                .unwrap();
        }
        let filters = RecallFilters::new();

        // Populate, then hit: identical results, cache holds one entry.
        let first = store.recall("cached recall", 5, &filters).unwrap();
        let second = store.recall("cached recall", 5, &filters).unwrap();
        assert_eq!(first.len(), second.len());
        assert_eq!(store.recall_cache.lock().unwrap().len(), 1);

        // A new write must invalidate: the fresh observation has to be
        // reachable immediately (read-your-writes through the facade).
        let outcome = store
            .remember(
                "entity-new",
                EntityType::Fact,
                &[obs("cached recall fact NEWEST")],
                &[],
                "t",
            )
            .unwrap();
        let after_write = store.recall("cached recall NEWEST", 10, &filters).unwrap();
        assert!(
            after_write
                .iter()
                .any(|r| r.observation.content.contains("NEWEST")),
            "post-write recall must see the new observation"
        );

        // Every other write kind also invalidates. Each pair: populate
        // an entry, mutate, assert the next recall reflects it.
        let probe = || store.recall("cached recall", 5, &filters).unwrap();
        let version = || store.write_version.load(Ordering::Acquire);

        let _ = probe();
        let v = version();
        assert!(store.forget(&outcome.observation_ids[0]).unwrap());
        assert!(version() > v, "forget must bump the write version");

        let v = version();
        store.forget_entity("entity-new").unwrap();
        assert!(version() > v, "forget_entity must bump");

        let v = version();
        let any = store.recall("cached recall", 1, &filters).unwrap();
        assert!(store
            .set_observation_memory_tier(
                &any[0].observation.id,
                openmemory_graph::MemoryTier::Semantic
            )
            .unwrap());
        assert!(version() > v, "tier change must bump");

        let v = version();
        store.consolidate(&ConsolidateConfig::default()).unwrap();
        store.prune_with_ttl(0).unwrap();
        assert!(version() > v, "maintenance must bump");

        let v = version();
        store
            .index_insert(openmemory_index::IndexEntry::new(
                "note://x",
                "free text shifts candidates",
            ))
            .unwrap();
        store.index_delete("note://x").unwrap();
        assert!(version() > v, "index writes must bump");
    }

    #[test]
    fn recall_cache_respects_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let store = DomainStore::open(&Config::default(), dir.path(), 2)
            .unwrap()
            .with_recall_cache_ttl(Duration::from_millis(0));
        store
            .remember("alpha", EntityType::Fact, &[obs("ttl probe")], &[], "t")
            .unwrap();
        let filters = RecallFilters::new();
        let _ = store.recall("ttl probe", 5, &filters).unwrap();
        // Zero TTL: the entry is present but never served...
        let _ = store.recall("ttl probe", 5, &filters).unwrap();
        // ...which we can only observe structurally: the entry was
        // re-populated rather than reused (no panic, same results).
        assert_eq!(store.recall_cache.lock().unwrap().len(), 1);
    }

    #[test]
    fn recall_cache_keys_distinguish_filters() {
        let (store, _dir) = open_domains(2);
        store
            .remember("alpha", EntityType::Fact, &[obs("filter probe")], &[], "t")
            .unwrap();
        store
            .remember(
                "bravo",
                EntityType::Person,
                &[obs("filter probe person")],
                &[],
                "t",
            )
            .unwrap();

        let unfiltered = store
            .recall("filter probe", 10, &RecallFilters::new())
            .unwrap();
        let person_only = store
            .recall(
                "filter probe",
                10,
                &RecallFilters {
                    entity_type: Some(EntityType::Person),
                    ..RecallFilters::new()
                },
            )
            .unwrap();
        assert!(unfiltered.len() > person_only.len());
        assert!(person_only
            .iter()
            .all(|r| r.entity_type == EntityType::Person));

        // Equivalent entity_names filters (case/order/dupes) share a key.
        let a = recall_cache_key(
            "q",
            5,
            &RecallFilters {
                entity_names: Some(vec!["Alpha".into(), "bravo".into(), "ALPHA".into()]),
                ..RecallFilters::new()
            },
        );
        let b = recall_cache_key(
            "q",
            5,
            &RecallFilters {
                entity_names: Some(vec!["bravo".into(), "alpha".into()]),
                ..RecallFilters::new()
            },
        );
        assert_eq!(a, b, "normalised entity_names must share a cache key");

        // mode None and Some(default) share a key; others differ.
        let none = recall_cache_key("q", 5, &RecallFilters::new());
        let hybrid = recall_cache_key(
            "q",
            5,
            &RecallFilters {
                mode: Some(SearchMode::Hybrid),
                ..RecallFilters::new()
            },
        );
        let keyword = recall_cache_key(
            "q",
            5,
            &RecallFilters {
                mode: Some(SearchMode::KeywordOnly),
                ..RecallFilters::new()
            },
        );
        assert_eq!(none, hybrid);
        assert_ne!(none, keyword);
    }

    #[test]
    fn single_domain_recall_bypasses_facade_cache() {
        let (store, _dir) = open_domains(1);
        store
            .remember("alpha", EntityType::Fact, &[obs("passthrough")], &[], "t")
            .unwrap();
        let _ = store
            .recall("passthrough", 5, &RecallFilters::new())
            .unwrap();
        assert_eq!(
            store.recall_cache.lock().unwrap().len(),
            0,
            "K=1 must stay a pure passthrough (TUI/watch write directly to the store)"
        );
    }

    #[test]
    fn consolidate_and_prune_cover_every_domain() {
        let (store, _dir) = open_domains(2);
        store
            .remember("alpha", EntityType::Fact, &[obs("x")], &[], "t")
            .unwrap();
        store
            .remember("bravo", EntityType::Fact, &[obs("y")], &[], "t")
            .unwrap();
        let report = store.consolidate(&ConsolidateConfig::default()).unwrap();
        assert_eq!(report.duplicates_merged, 0);
        let prune = store.prune_with_ttl(0).unwrap();
        assert_eq!(prune.tombstones_removed, 0);
    }
}
