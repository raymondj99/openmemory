//! Verified handle for one semantic space root.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use openmemory_core::config::Config;
use openmemory_core::space::SpaceId;
use openmemory_graph::{MemoryError, MemoryResult};

use crate::partition::DomainStore;

use super::{SpaceManifest, SPACE_MANIFEST_FILE};

/// An opened semantic space. Its store is permanently bound to `id`.
#[derive(Debug)]
pub struct SpaceHandle {
    pub id: SpaceId,
    manifest: SpaceManifest,
    domains: Arc<DomainStore>,
    root: PathBuf,
}

impl SpaceHandle {
    /// Verify the manifest and open every performance domain under one ID.
    pub fn open(
        config: &Config,
        root: &Path,
        expected_profile: &str,
        expected_root_key: &str,
        expected_id: SpaceId,
        expected_domains: usize,
    ) -> MemoryResult<Self> {
        let manifest = SpaceManifest::load(&root.join(SPACE_MANIFEST_FILE))?;
        manifest.verify(
            expected_id,
            expected_profile,
            expected_root_key,
            expected_domains,
        )?;
        let store_root = if root.join("store").is_dir() {
            root.join("store")
        } else {
            root.to_path_buf()
        };
        let canonical_root = root.canonicalize()?;
        let canonical_store = store_root.canonicalize().map_err(|error| {
            MemoryError::InvalidInput(format!(
                "space store root {} cannot be opened: {error}",
                store_root.display()
            ))
        })?;
        if !canonical_store.starts_with(&canonical_root) {
            return Err(MemoryError::InvalidInput(
                "space store escapes its manifest root".to_string(),
            ));
        }
        let domains = Arc::new(DomainStore::open_scoped(
            config,
            &canonical_store,
            manifest.domain_count,
            manifest.space_id,
        )?);
        Ok(Self {
            id: manifest.space_id,
            manifest,
            domains,
            root: canonical_root,
        })
    }

    #[must_use]
    pub fn manifest(&self) -> &SpaceManifest {
        &self.manifest
    }

    #[must_use]
    pub fn domains(&self) -> &Arc<DomainStore> {
        &self.domains
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}
