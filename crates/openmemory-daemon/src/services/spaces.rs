//! Space, project, team, grant, and lifecycle service ownership.
//!
//! These are deliberately daemon-private contracts.  Phase 2 establishes the
//! policy/authority/catalog seam; human-admin DTOs and routes are added only
//! after their audit/history contract exists in Phase 5.

#![allow(dead_code)]

use openmemory_core::{
    config::Config,
    space::{ProfileName, SpaceRef},
};

use crate::product_store::{CatalogSpace, ProductStore, ProductStoreError};
use crate::space_manifest::{create_managed_root, ManifestError};

#[derive(Debug, Clone)]
pub(crate) struct CreateManagedSpace {
    pub(crate) space: SpaceRef,
    pub(crate) profile: ProfileName,
    pub(crate) domain_count: u8,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SpaceServiceError {
    #[error("catalog operation failed: {0}")]
    Product(#[from] ProductStoreError),
    #[error("managed-root operation failed: {0}")]
    Manifest(#[from] ManifestError),
}

/// Product service that owns the complete new-space pipeline.  It has no HTTP
/// fields and accepts no caller path, which keeps admin transport from
/// becoming a second catalog/root implementation.
#[derive(Debug, Clone)]
pub(crate) struct SpaceService {
    catalog: ProductStore,
    config: Config,
    home: std::path::PathBuf,
}

impl SpaceService {
    pub(crate) fn new(catalog: ProductStore, config: Config, home: std::path::PathBuf) -> Self {
        Self {
            catalog,
            config,
            home,
        }
    }

    pub(crate) fn create(
        &self,
        request: CreateManagedSpace,
        now_unix_secs: i64,
    ) -> Result<CatalogSpace, SpaceServiceError> {
        create_managed_root(
            &self.catalog,
            &self.config,
            &self.home,
            request.space,
            &request.profile,
            request.domain_count,
            now_unix_secs,
        )
        .map_err(Into::into)
    }
}
