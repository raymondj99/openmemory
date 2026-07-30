//! Managed space handles and the bounded lazy space manager.
//!
//! A memory space is an isolation silo owning one complete
//! [`DomainStore`]. Managed spaces live under `<profile>/spaces/<name>/`,
//! each root carrying a durable `space.toml` manifest ([`SpaceManifest`])
//! and a shared advisory lifetime lock. The personal-global profile store
//! is NOT managed here: it remains the semantic default that legacy
//! clients keep using; this module only adds named silos beside it.
//!
//! [`SpaceManager`] is deliberately small and conservative:
//! - names are validated before they touch the filesystem (lowercase
//!   ASCII alphanumerics and interior dashes only), so a space name can
//!   never traverse paths;
//! - opens are lazy and cached, bounded by [`MAX_OPEN_SPACES`]; when the
//!   bound is hit, only idle handles (no outside references) are evicted,
//!   and if none is idle the open fails with a typed error instead of
//!   evicting a store that is mid-request;
//! - a symlinked space root fails closed;
//! - reopen verifies the manifest and binds the persisted [`SpaceId`]
//!   into every member database via `DomainStore::bind_space_id`, so a
//!   copied store cannot silently answer for another space.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs4::{FileExt, TryLockError};
use openmemory_core::config::Config;
use openmemory_core::space::SpaceId;
use openmemory_graph::{MemoryError, MemoryResult};

#[cfg(any(feature = "testing", feature = "embeddings"))]
use openmemory_core::testing::Embedder;

use crate::partition::DomainStore;

use super::manifest::{SpaceManifest, SPACE_MANIFEST_FILE};

/// Directory under the profile root that holds managed space roots.
pub const SPACES_DIR: &str = "spaces";

/// Per-space shared advisory lifetime lock file.
pub const SPACE_LOCK_FILE: &str = ".space.lock";

/// Maximum concurrently open managed spaces per manager.
pub const MAX_OPEN_SPACES: usize = 8;

/// Maximum space name length in bytes.
pub const MAX_SPACE_NAME_LEN: usize = 64;

/// Reserved names that can never be created as managed spaces. `default`
/// addresses the personal-global profile store in read sets, so a managed
/// space by that name would be unaddressable.
const RESERVED_NAMES: &[&str] = &["default"];

/// Validate a managed space name. Returns the name unchanged on success.
///
/// Rules: 1..=64 bytes; lowercase ASCII alphanumerics and dashes; must
/// start and end with an alphanumeric; no consecutive dashes. The grammar
/// is deliberately path-safe: a valid name can never contain `/`, `\`,
/// `.` or NUL, so joining it under the spaces directory cannot escape it.
pub fn validate_space_name(name: &str) -> MemoryResult<&str> {
    if name.is_empty() || name.len() > MAX_SPACE_NAME_LEN {
        return Err(MemoryError::InvalidInput(format!(
            "space name must be 1..={MAX_SPACE_NAME_LEN} characters"
        )));
    }
    let bytes = name.as_bytes();
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if !alnum(bytes[0]) || !alnum(bytes[bytes.len() - 1]) {
        return Err(MemoryError::InvalidInput(
            "space name must start and end with a lowercase letter or digit".to_owned(),
        ));
    }
    let mut prev_dash = false;
    for &b in bytes {
        if b == b'-' {
            if prev_dash {
                return Err(MemoryError::InvalidInput(
                    "space name must not contain consecutive dashes".to_owned(),
                ));
            }
            prev_dash = true;
        } else if alnum(b) {
            prev_dash = false;
        } else {
            return Err(MemoryError::InvalidInput(
                "space name may only contain lowercase letters, digits, and dashes".to_owned(),
            ));
        }
    }
    if RESERVED_NAMES.contains(&name) {
        return Err(MemoryError::InvalidInput(format!(
            "space name '{name}' is reserved for the personal-global default"
        )));
    }
    Ok(name)
}

/// Shared advisory lifetime lock on one managed space root. Held for the
/// life of the open handle; an exclusive holder (future maintenance)
/// blocks new shared opens.
#[derive(Debug)]
struct SpaceRootLock {
    file: File,
}

impl SpaceRootLock {
    fn acquire_shared(root: &Path) -> MemoryResult<Self> {
        let path = root.join(SPACE_LOCK_FILE);
        if matches!(std::fs::symlink_metadata(&path), Ok(m) if m.file_type().is_symlink()) {
            return Err(MemoryError::InvalidInput(
                "space lifetime lock is a symlink".to_owned(),
            ));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match FileExt::try_lock_shared(&file) {
            Ok(()) => Ok(Self { file }),
            Err(TryLockError::WouldBlock) => Err(MemoryError::InvalidInput(
                "space_busy: space root is held exclusively".to_owned(),
            )),
            Err(TryLockError::Error(error)) => Err(MemoryError::Io(error)),
        }
    }
}

impl Drop for SpaceRootLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// One open managed space: validated identity plus its owning store.
#[derive(Debug)]
pub struct SpaceHandle {
    manifest: SpaceManifest,
    store: Arc<DomainStore>,
    _lock: SpaceRootLock,
}

impl SpaceHandle {
    /// The space's validated manifest identity.
    #[must_use]
    pub fn manifest(&self) -> &SpaceManifest {
        &self.manifest
    }

    /// The store owning this space's data.
    #[must_use]
    pub fn store(&self) -> &Arc<DomainStore> {
        &self.store
    }
}

/// Catalog row for one managed space, readable without opening its store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceInfo {
    pub name: String,
    pub space_id: SpaceId,
    pub created_at: i64,
}

/// Bounded, lazy manager for managed spaces under one profile root.
pub struct SpaceManager {
    config: Config,
    spaces_root: PathBuf,
    domains: usize,
    #[cfg(any(feature = "testing", feature = "embeddings"))]
    embedder: Option<Arc<dyn Embedder>>,
    open: Mutex<HashMap<String, Arc<SpaceHandle>>>,
}

impl std::fmt::Debug for SpaceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpaceManager")
            .field("spaces_root", &self.spaces_root)
            .field("domains", &self.domains)
            .finish_non_exhaustive()
    }
}

impl SpaceManager {
    /// Build a manager rooted at `<profile_root>/spaces`. Nothing is
    /// created or opened until a space is first used.
    #[must_use]
    pub fn new(config: Config, profile_root: &Path, domains: usize) -> Self {
        Self {
            config,
            spaces_root: profile_root.join(SPACES_DIR),
            domains: domains.max(1),
            #[cfg(any(feature = "testing", feature = "embeddings"))]
            embedder: None,
            open: Mutex::new(HashMap::new()),
        }
    }

    /// Attach an embedder used for every subsequently opened space store.
    #[cfg(any(feature = "testing", feature = "embeddings"))]
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Root directory that holds managed space roots.
    #[must_use]
    pub fn spaces_root(&self) -> &Path {
        &self.spaces_root
    }

    /// Create a new managed space. Fails if the name is invalid or the
    /// space already exists. The store itself is created lazily on first
    /// open; creation persists only the root directory and manifest, so a
    /// crash between the two leaves an inspectable directory without a
    /// manifest, which `create` repairs on retry and `list` ignores.
    pub fn create(&self, name: &str) -> MemoryResult<SpaceInfo> {
        validate_space_name(name)?;
        let root = self.spaces_root.join(name);
        if root.join(SPACE_MANIFEST_FILE).exists() {
            return Err(MemoryError::InvalidInput(format!(
                "space '{name}' already exists"
            )));
        }
        std::fs::create_dir_all(&root)?;
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let manifest = SpaceManifest::create(&root, SpaceId::new(), name, created_at)?;
        Ok(SpaceInfo {
            name: manifest.name,
            space_id: manifest.space_id,
            created_at: manifest.created_at,
        })
    }

    /// List every managed space by reading manifests only; no store is
    /// opened. Directories without a valid manifest are skipped.
    pub fn list(&self) -> MemoryResult<Vec<SpaceInfo>> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.spaces_root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if validate_space_name(&name).is_err() {
                continue;
            }
            if !entry.path().is_dir() {
                continue;
            }
            let Ok(manifest) = SpaceManifest::load(&entry.path(), &name) else {
                continue;
            };
            out.push(SpaceInfo {
                name: manifest.name,
                space_id: manifest.space_id,
                created_at: manifest.created_at,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Open (or return the cached handle for) one managed space.
    pub fn open(&self, name: &str) -> MemoryResult<Arc<SpaceHandle>> {
        validate_space_name(name)?;
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handle) = open.get(name) {
            return Ok(Arc::clone(handle));
        }

        // Bounded admission: evict idle handles (only the map holds a
        // reference) before refusing.
        if open.len() >= MAX_OPEN_SPACES {
            open.retain(|_, handle| Arc::strong_count(handle) > 1);
        }
        if open.len() >= MAX_OPEN_SPACES {
            return Err(MemoryError::InvalidInput(format!(
                "too many open spaces (bound {MAX_OPEN_SPACES}); close or finish \
                 in-flight requests before opening '{name}'"
            )));
        }

        let root = self.spaces_root.join(name);
        let metadata = std::fs::symlink_metadata(&root).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                MemoryError::InvalidInput(format!("space '{name}' does not exist; create it first"))
            } else {
                MemoryError::Io(e)
            }
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(MemoryError::InvalidInput(format!(
                "space root for '{name}' must be a real directory"
            )));
        }
        let manifest = SpaceManifest::load(&root, name)?;
        let lock = SpaceRootLock::acquire_shared(&root)?;
        let store = self.open_store(&root)?.bind_space_id(manifest.space_id)?;
        let handle = Arc::new(SpaceHandle {
            manifest,
            store: Arc::new(store),
            _lock: lock,
        });
        open.insert(name.to_owned(), Arc::clone(&handle));
        Ok(handle)
    }

    /// Resolve the store for one managed space (opening it if needed).
    pub fn store(&self, name: &str) -> MemoryResult<Arc<DomainStore>> {
        Ok(Arc::clone(self.open(name)?.store()))
    }

    #[cfg(any(feature = "testing", feature = "embeddings"))]
    fn open_store(&self, root: &Path) -> MemoryResult<DomainStore> {
        match &self.embedder {
            Some(embedder) => DomainStore::open_with_embedder(
                &self.config,
                root,
                self.domains,
                Arc::clone(embedder),
            ),
            None => DomainStore::open(&self.config, root, self.domains),
        }
    }

    #[cfg(not(any(feature = "testing", feature = "embeddings")))]
    fn open_store(&self, root: &Path) -> MemoryResult<DomainStore> {
        DomainStore::open(&self.config, root, self.domains)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_graph::{EntityType, ObservationInput, RecallFilters};

    fn manager(dir: &Path) -> SpaceManager {
        SpaceManager::new(Config::default(), dir, 1)
    }

    #[test]
    fn name_validation_accepts_and_rejects() {
        assert!(validate_space_name("research").is_ok());
        assert!(validate_space_name("proj-2026").is_ok());
        assert!(validate_space_name("a").is_ok());
        assert!(validate_space_name("").is_err());
        assert!(validate_space_name("Research").is_err());
        assert!(validate_space_name("has space").is_err());
        assert!(validate_space_name("-lead").is_err());
        assert!(validate_space_name("trail-").is_err());
        assert!(validate_space_name("dou--ble").is_err());
        assert!(validate_space_name("../escape").is_err());
        assert!(validate_space_name("a/b").is_err());
        assert!(validate_space_name("default").is_err());
        assert!(validate_space_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn create_list_open_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        assert!(mgr.list().unwrap().is_empty());

        let info = mgr.create("alpha").unwrap();
        mgr.create("beta").unwrap();
        let listed = mgr.list().unwrap();
        assert_eq!(
            listed.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );

        let handle = mgr.open("alpha").unwrap();
        assert_eq!(handle.manifest().space_id, info.space_id);
        assert_eq!(handle.store().space_id(), Some(info.space_id));
    }

    #[test]
    fn create_rejects_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        mgr.create("alpha").unwrap();
        let err = mgr.create("alpha").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn open_unknown_space_is_typed() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        let err = mgr.open("ghost").unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn spaces_are_physically_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        mgr.create("left").unwrap();
        mgr.create("right").unwrap();

        let left = mgr.store("left").unwrap();
        let right = mgr.store("right").unwrap();
        left.remember(
            "Shared Name",
            EntityType::Fact,
            &[ObservationInput::new("only in left")],
            &[],
            "test",
        )
        .unwrap();

        let filters = RecallFilters::new();
        let left_hits = left.recall("only in left", 5, &filters).unwrap();
        let right_hits = right.recall("only in left", 5, &filters).unwrap();
        assert!(!left_hits.is_empty());
        assert!(right_hits.is_empty());
        assert_ne!(left.space_id(), right.space_id());
    }

    #[test]
    fn reopen_preserves_space_identity() {
        let dir = tempfile::tempdir().unwrap();
        let info = {
            let mgr = manager(dir.path());
            let info = mgr.create("stable").unwrap();
            mgr.store("stable")
                .unwrap()
                .remember(
                    "Fact",
                    EntityType::Fact,
                    &[ObservationInput::new("durable row")],
                    &[],
                    "test",
                )
                .unwrap();
            info
        };
        let mgr = manager(dir.path());
        let handle = mgr.open("stable").unwrap();
        assert_eq!(handle.manifest().space_id, info.space_id);
        let hits = handle
            .store()
            .recall("durable row", 5, &RecallFilters::new())
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn symlinked_space_root_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        mgr.create("real").unwrap();
        std::fs::create_dir_all(mgr.spaces_root()).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                mgr.spaces_root().join("real"),
                mgr.spaces_root().join("evil"),
            )
            .unwrap();
            let err = mgr.open("evil").unwrap_err();
            assert!(err.to_string().contains("real directory"));
        }
    }

    #[test]
    fn open_bound_is_enforced_and_idle_handles_evict() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        for i in 0..MAX_OPEN_SPACES {
            mgr.create(&format!("s{i}")).unwrap();
        }
        mgr.create("overflow").unwrap();

        // Hold every handle: the bound must refuse the next open.
        let held: Vec<_> = (0..MAX_OPEN_SPACES)
            .map(|i| mgr.open(&format!("s{i}")).unwrap())
            .collect();
        let err = mgr.open("overflow").unwrap_err();
        assert!(err.to_string().contains("too many open spaces"));

        // Dropping outside references makes the cached handles idle, so
        // the next open evicts and succeeds.
        drop(held);
        mgr.open("overflow").unwrap();
    }

    #[test]
    fn list_skips_directories_without_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager(dir.path());
        mgr.create("good").unwrap();
        std::fs::create_dir_all(mgr.spaces_root().join("junk")).unwrap();
        let listed = mgr.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "good");
    }
}
