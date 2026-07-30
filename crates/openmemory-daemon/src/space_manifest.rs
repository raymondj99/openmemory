//! Managed-root derivation, manifest validation, and legacy binding.
//!
//! Catalog rows persist only [`RootKey`] selectors.  This module is the sole
//! owner that turns one into an OS path and it validates every existing
//! component before touching a manifest or lock file.

// The managed-space creation service is private until Phase 5 routes exist.
#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use blake3::Hasher;
use openmemory_core::config::Config;
use openmemory_core::space::{ProfileName, SpaceContext, SpaceId, SpaceOwner, SpaceRef};
use openmemory_engine::{
    partition::DomainStore,
    portability::{FilesystemCapabilities, PortabilityError},
};
use openmemory_graph::MemoryError;
use rusqlite::{OpenFlags, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::product_store::{
    CatalogSpace, CatalogSpaceState, ProductStore, ProductStoreError, RootKey,
};

pub(crate) const LEGACY_ID_FILE: &str = ".space-id";
pub(crate) const LEGACY_LOCK_FILE: &str = ".personal-global.lock";
pub(crate) const MANAGED_SPACES_DIR: &str = "spaces";
pub(crate) const MANIFEST_FILE: &str = "space.toml";
pub(crate) const SPACE_LOCK_FILE: &str = ".space.lock";
const MANIFEST_FORMAT_VERSION: u16 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;

#[derive(Debug, Error)]
pub(crate) enum ManifestError {
    #[error("managed-root filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("managed root contains a symlink or non-directory component")]
    UnsafePath,
    #[error("managed root escapes the active profile")]
    RootEscape,
    #[error("managed root has an unexpected hard-link count")]
    HardLinked,
    #[error("space manifest is invalid: {0}")]
    Invalid(String),
    #[error("product catalog operation failed: {0}")]
    Product(#[from] ProductStoreError),
    #[error("filesystem capability inspection failed: {0}")]
    Portability(#[from] PortabilityError),
    #[error("space store operation failed: {0}")]
    Store(#[from] MemoryError),
}

/// Bounded versioned manifest persisted in each logical space root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpaceManifest {
    format_version: u16,
    space_id: SpaceId,
    profile: ProfileName,
    owner_kind: ManifestOwnerKind,
    owner_id: String,
    context_kind: ManifestContextKind,
    project_id: String,
    domain_count: u8,
    created_at_unix_secs: i64,
    catalog_binding_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ManifestOwnerKind {
    User,
    Team,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ManifestContextKind {
    Global,
    Project,
}

impl SpaceManifest {
    fn from_catalog(space: &CatalogSpace, created_at_unix_secs: i64) -> Self {
        let (owner_kind, owner_id) = match space.space.owner() {
            SpaceOwner::User(id) => (ManifestOwnerKind::User, id.to_string()),
            SpaceOwner::Team(id) => (ManifestOwnerKind::Team, id.to_string()),
        };
        let (context_kind, project_id) = match space.space.context() {
            SpaceContext::Global => (ManifestContextKind::Global, String::new()),
            SpaceContext::Project(id) => (ManifestContextKind::Project, id.to_string()),
        };
        let mut manifest = Self {
            format_version: MANIFEST_FORMAT_VERSION,
            space_id: space.space.id(),
            profile: space.profile.clone(),
            owner_kind,
            owner_id,
            context_kind,
            project_id,
            domain_count: space.domain_count,
            created_at_unix_secs,
            catalog_binding_hash: String::new(),
        };
        manifest.catalog_binding_hash = manifest.binding_hash(&space.root_key);
        manifest
    }

    pub(crate) fn space_id(&self) -> SpaceId {
        self.space_id
    }

    pub(crate) fn domain_count(&self) -> u8 {
        self.domain_count
    }

    pub(crate) fn binding_hash(&self, root_key: &RootKey) -> String {
        let mut hash = Hasher::new();
        hash.update(b"openmemory/space-manifest-binding/v1");
        frame(&mut hash, &self.format_version.to_le_bytes());
        frame(&mut hash, self.space_id.to_string().as_bytes());
        frame(&mut hash, self.profile.as_str().as_bytes());
        frame(
            &mut hash,
            match self.owner_kind {
                ManifestOwnerKind::User => b"user",
                ManifestOwnerKind::Team => b"team",
            },
        );
        frame(&mut hash, self.owner_id.as_bytes());
        frame(
            &mut hash,
            match self.context_kind {
                ManifestContextKind::Global => b"global",
                ManifestContextKind::Project => b"project",
            },
        );
        frame(&mut hash, self.project_id.as_bytes());
        frame(&mut hash, &[self.domain_count]);
        frame(&mut hash, root_key.as_str().as_bytes());
        format!("blake3:{}", hash.finalize().to_hex())
    }

    fn validate_against(&self, space: &CatalogSpace) -> Result<(), ManifestError> {
        if self.format_version != MANIFEST_FORMAT_VERSION
            || self.space_id != space.space.id()
            || self.profile != space.profile
            || self.domain_count != space.domain_count
            || self.catalog_binding_hash != self.binding_hash(&space.root_key)
            || self.catalog_binding_hash != space.manifest_hash
        {
            return Err(ManifestError::Invalid(
                "manifest does not match its catalog identity".to_owned(),
            ));
        }
        match (space.space.owner(), self.owner_kind, self.owner_id.as_str()) {
            (SpaceOwner::User(id), ManifestOwnerKind::User, owner) if id.as_str() == owner => {}
            (SpaceOwner::Team(id), ManifestOwnerKind::Team, owner) if id.as_str() == owner => {}
            _ => {
                return Err(ManifestError::Invalid(
                    "manifest owner does not match catalog".to_owned(),
                ));
            }
        }
        match (
            space.space.context(),
            self.context_kind,
            self.project_id.as_str(),
        ) {
            (SpaceContext::Global, ManifestContextKind::Global, "") => {}
            (SpaceContext::Project(id), ManifestContextKind::Project, project)
                if id.to_string() == project => {}
            _ => {
                return Err(ManifestError::Invalid(
                    "manifest context does not match catalog".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Fully validated legacy binding facts.  A caller may use the derived root
/// only while retaining its associated lifetime lock.
#[derive(Debug, Clone)]
pub(crate) struct LegacyBinding {
    pub(crate) catalog: CatalogSpace,
    pub(crate) root: PathBuf,
    pub(crate) manifest: SpaceManifest,
    pub(crate) filesystem: FilesystemCapabilities,
}

pub(crate) fn profile_root(home: &Path, profile: &ProfileName) -> PathBuf {
    home.join("data").join(profile.as_str())
}

pub(crate) fn root_for(profile_root: &Path, root_key: &RootKey) -> PathBuf {
    match root_key {
        RootKey::LegacyRoot => profile_root.to_path_buf(),
        RootKey::Space(id) => profile_root.join(MANAGED_SPACES_DIR).join(id.to_string()),
    }
}

pub(crate) fn lock_path(profile_root: &Path, root_key: &RootKey) -> PathBuf {
    match root_key {
        RootKey::LegacyRoot => profile_root.join(LEGACY_LOCK_FILE),
        RootKey::Space(_) => root_for(profile_root, root_key).join(SPACE_LOCK_FILE),
    }
}

/// Validate the selected root without resolving a caller-provided path.  The
/// ID and fixed ASCII directory names are the complete derivation input.
pub(crate) fn validate_catalog_root(
    profile_root: &Path,
    space: &CatalogSpace,
) -> Result<PathBuf, ManifestError> {
    ensure_directory(profile_root, false)?;
    let root = root_for(profile_root, &space.root_key);
    match space.root_key {
        RootKey::LegacyRoot => {}
        RootKey::Space(_) => {
            let spaces = profile_root.join(MANAGED_SPACES_DIR);
            ensure_directory(&spaces, false)?;
            ensure_directory(&root, false)?;
        }
    }
    ensure_below(profile_root, &root)?;
    Ok(root)
}

/// Atomically read or create the compatibility ID before any graph database
/// is opened.  A concurrent winner is read and validated, never replaced.
pub(crate) fn read_or_create_legacy_id(profile_root: &Path) -> Result<SpaceId, ManifestError> {
    ensure_directory(profile_root, false)?;
    let path = profile_root.join(LEGACY_ID_FILE);
    match read_bounded(&path, 256) {
        Ok(text) => parse_legacy_id(&text),
        Err(ManifestError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = existing_graph_space_id(profile_root)?.unwrap_or_else(SpaceId::new);
            match write_new_synced(&path, format!("{id}\n").as_bytes()) {
                Ok(()) => Ok(id),
                Err(ManifestError::Io(error))
                    if error.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    parse_legacy_id(&read_bounded(&path, 256)?)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn existing_graph_space_id(profile_root: &Path) -> Result<Option<SpaceId>, ManifestError> {
    let path = profile_root.join(openmemory_graph::MEMORY_DB_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let conn = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        ManifestError::Invalid(format!("legacy graph database is invalid: {error}"))
    })?;
    let has_meta: bool = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_master
                 WHERE type = 'table' AND name = 'memory_meta'
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| {
            ManifestError::Invalid(format!("legacy graph metadata is invalid: {error}"))
        })?;
    if !has_meta {
        return Ok(None);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM memory_meta WHERE key = 'space_id'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            ManifestError::Invalid(format!("legacy graph space identity is invalid: {error}"))
        })?;
    value
        .map(|value| {
            SpaceId::from_str(&value).map_err(|error| ManifestError::Invalid(error.to_string()))
        })
        .transpose()
}

/// Bind the legacy personal-global root.  The catalog's `creating` state is
/// intentional: a restart after any durable boundary reuses the same UUID and
/// completes the manifest/catalog pair instead of minting another root.
pub(crate) fn bind_legacy_personal_global(
    store: &ProductStore,
    config: &Config,
    home: &Path,
    profile: &ProfileName,
    domain_count: u8,
    now_unix_secs: i64,
) -> Result<LegacyBinding, ManifestError> {
    let root = profile_root(home, profile);
    let id = read_or_create_legacy_id(&root)?;
    let principal = store.ensure_installation_principal(now_unix_secs)?;
    let expected = CatalogSpace {
        space: SpaceRef::new(
            id,
            SpaceOwner::User(principal.clone()),
            SpaceContext::Global,
        ),
        profile: profile.clone(),
        root_key: RootKey::LegacyRoot,
        domain_count,
        // The manifest constructor fills this exact canonical hash.
        manifest_hash: String::new(),
        state: CatalogSpaceState::Creating,
        catalog_generation: 0,
    };
    let manifest = SpaceManifest::from_catalog(&expected, now_unix_secs);
    let catalog = store.bind_legacy_personal_global(
        profile,
        &principal,
        id,
        domain_count,
        &manifest.catalog_binding_hash,
        now_unix_secs,
    )?;
    if catalog.manifest_hash != manifest.catalog_binding_hash {
        return Err(ManifestError::Invalid(
            "legacy catalog binding hash differs from manifest identity".to_owned(),
        ));
    }
    let manifest_path = root.join(MANIFEST_FILE);
    match read_manifest(&manifest_path) {
        Ok(on_disk) => on_disk.validate_against(&catalog)?,
        Err(ManifestError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            write_manifest(&manifest_path, &manifest)?;
        }
        Err(error) => return Err(error),
    }
    let domain_store =
        DomainStore::open_scoped(config, &root, usize::from(domain_count), catalog.space.id())?;
    verify_domain_store(&domain_store)?;
    drop(domain_store);
    let catalog = store.activate_space(id, &manifest.catalog_binding_hash, now_unix_secs)?;
    let filesystem = FilesystemCapabilities::inspect(&root)?;
    Ok(LegacyBinding {
        catalog,
        root,
        manifest,
        filesystem,
    })
}

/// Create a non-legacy root from a catalog identity.  The random UUID is the
/// sole variable component; callers cannot provide a filesystem path.
pub(crate) fn create_managed_root(
    store: &ProductStore,
    config: &Config,
    home: &Path,
    space: SpaceRef,
    profile: &ProfileName,
    domain_count: u8,
    now_unix_secs: i64,
) -> Result<CatalogSpace, ManifestError> {
    let root_key = RootKey::Space(space.id());
    let provisional = CatalogSpace {
        space: space.clone(),
        profile: profile.clone(),
        root_key: root_key.clone(),
        domain_count,
        manifest_hash: String::new(),
        state: CatalogSpaceState::Creating,
        catalog_generation: 0,
    };
    let manifest = SpaceManifest::from_catalog(&provisional, now_unix_secs);
    let profile_root = profile_root(home, profile);
    ensure_managed_profile_root(home, &profile_root)?;
    let spaces = profile_root.join(MANAGED_SPACES_DIR);
    ensure_directory(&spaces, true)?;
    let root = root_for(&profile_root, &root_key);
    ensure_directory(&root, true)?;
    ensure_below(&profile_root, &root)?;
    write_manifest(&root.join(MANIFEST_FILE), &manifest)?;
    store.insert_space(
        &space,
        profile,
        &root_key,
        domain_count,
        &manifest.catalog_binding_hash,
        CatalogSpaceState::Creating,
        now_unix_secs,
    )?;
    let domain_store = DomainStore::open_scoped(
        config,
        &root.join("store"),
        usize::from(domain_count),
        space.id(),
    )?;
    verify_domain_store(&domain_store)?;
    drop(domain_store);
    // Do not discard the physical root after a late catalog failure: it
    // remains a safely inspectable, unreferenced artifact.
    store
        .activate_space(space.id(), &manifest.catalog_binding_hash, now_unix_secs)
        .map_err(Into::into)
}

fn verify_domain_store(store: &DomainStore) -> Result<(), ManifestError> {
    store.status()?;
    for domain in store.stores() {
        domain.persist_search_index()?;
        if !domain.wal_checkpoint()?.complete {
            return Err(ManifestError::Store(MemoryError::InvalidInput(
                "space WAL could not be fully checkpointed before activation".to_owned(),
            )));
        }
    }
    Ok(())
}

pub(crate) fn load_manifest_for_catalog(
    profile_root: &Path,
    catalog: &CatalogSpace,
) -> Result<SpaceManifest, ManifestError> {
    let root = validate_catalog_root(profile_root, catalog)?;
    let manifest = read_manifest(&root.join(MANIFEST_FILE))?;
    manifest.validate_against(catalog)?;
    Ok(manifest)
}

fn read_manifest(path: &Path) -> Result<SpaceManifest, ManifestError> {
    let text = read_bounded(path, MAX_MANIFEST_BYTES)?;
    toml::from_str(&text).map_err(|error| ManifestError::Invalid(error.to_string()))
}

fn write_manifest(path: &Path, manifest: &SpaceManifest) -> Result<(), ManifestError> {
    let text = toml::to_string_pretty(manifest)
        .map_err(|error| ManifestError::Invalid(error.to_string()))?;
    if text.len() > MAX_MANIFEST_BYTES as usize {
        return Err(ManifestError::Invalid(
            "manifest exceeds its bound".to_owned(),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| ManifestError::Invalid("manifest has no parent".to_owned()))?;
    ensure_directory(parent, false)?;
    if matches!(std::fs::symlink_metadata(path), Ok(metadata) if metadata.file_type().is_symlink())
    {
        return Err(ManifestError::UnsafePath);
    }
    let temp = path.with_file_name(format!(".space.toml.{}.tmp", SpaceId::new()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    match std::fs::rename(&temp, path) {
        Ok(()) => sync_directory(parent),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(ManifestError::Io(error))
        }
    }
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), ManifestError> {
    let parent = path
        .parent()
        .ok_or_else(|| ManifestError::Invalid("file has no parent".to_owned()))?;
    ensure_directory(parent, false)?;
    if matches!(std::fs::symlink_metadata(path), Ok(metadata) if metadata.file_type().is_symlink())
    {
        return Err(ManifestError::UnsafePath);
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    sync_directory(parent)
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<String, ManifestError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(ManifestError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() > 1 {
            return Err(ManifestError::HardLinked);
        }
    }
    let mut text = String::new();
    File::open(path)?
        .take(max_bytes + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > max_bytes {
        return Err(ManifestError::Invalid("file exceeds its bound".to_owned()));
    }
    Ok(text)
}

fn parse_legacy_id(text: &str) -> Result<SpaceId, ManifestError> {
    SpaceId::from_str(text.trim()).map_err(|error| ManifestError::Invalid(error.to_string()))
}

fn ensure_directory(path: &Path, create: bool) -> Result<(), ManifestError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ManifestError::UnsafePath);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            std::fs::create_dir(path)?;
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ManifestError::UnsafePath);
            }
        }
        Err(error) => return Err(ManifestError::Io(error)),
    }
    Ok(())
}

fn ensure_managed_profile_root(home: &Path, profile_root: &Path) -> Result<(), ManifestError> {
    let data_root = home.join("data");
    std::fs::create_dir_all(&data_root)?;
    ensure_directory(&data_root, false)?;
    ensure_directory(profile_root, true)
}

fn ensure_below(profile_root: &Path, root: &Path) -> Result<(), ManifestError> {
    let profile = std::fs::canonicalize(profile_root)?;
    let candidate = std::fs::canonicalize(root)?;
    if candidate.starts_with(&profile) {
        Ok(())
    } else {
        Err(ManifestError::RootEscape)
    }
}

fn sync_directory(path: &Path) -> Result<(), ManifestError> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn frame(hash: &mut Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}
