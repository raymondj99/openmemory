//! Product policy normalization for catalog and context admission.
//!
//! Raw daemon configuration is converted once into immutable typed facts.  No
//! lower layer rereads the current directory, environment, profile string, or
//! filesystem probe after this boundary.

// The normalized policy is intentionally private until resolved-context routes
// are introduced in Phase 4.
#![allow(dead_code)]

use openmemory_core::space::{ProfileName, SelectionProvenance, SelectionSource, WorkspacePathKey};
use openmemory_engine::portability::FilesystemCapabilities;
use thiserror::Error;

use crate::space_registry::RegistryLimits;

/// Which layer supplied one independently resolved context choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionInput {
    ProductDefault,
    WorkspaceMapping,
    Explicit,
}

impl SelectionInput {
    const fn source(self) -> SelectionSource {
        match self {
            Self::ProductDefault => SelectionSource::ProductDefault,
            Self::WorkspaceMapping => SelectionSource::WorkspaceMapping,
            Self::Explicit => SelectionSource::Explicit,
        }
    }
}

/// Raw context choices at the product boundary.  An explicit invalid choice
/// is rejected by the context service; it is never downgraded here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionInputs {
    pub(crate) project: SelectionInput,
    pub(crate) team: SelectionInput,
    pub(crate) read_scope: SelectionInput,
    pub(crate) write_target: SelectionInput,
}

impl Default for SelectionInputs {
    fn default() -> Self {
        Self {
            project: SelectionInput::ProductDefault,
            team: SelectionInput::ProductDefault,
            read_scope: SelectionInput::ProductDefault,
            write_target: SelectionInput::ProductDefault,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum PolicyError {
    #[error("active profile is invalid: {0}")]
    Profile(String),
}

/// Immutable inputs accepted by catalog/authority/registry services.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedProductPolicy {
    profile: ProfileName,
    workspace: Option<WorkspacePathKey>,
    selection: SelectionProvenance,
    filesystem: FilesystemCapabilities,
    registry_limits: RegistryLimits,
}

impl ResolvedProductPolicy {
    pub(crate) fn resolve(
        profile: &str,
        workspace: Option<WorkspacePathKey>,
        selection: SelectionInputs,
        filesystem: FilesystemCapabilities,
        registry_limits: RegistryLimits,
    ) -> Result<Self, PolicyError> {
        let profile =
            ProfileName::new(profile).map_err(|error| PolicyError::Profile(error.to_string()))?;
        Ok(Self {
            profile,
            workspace,
            selection: SelectionProvenance::new(
                selection.project.source(),
                selection.team.source(),
                selection.read_scope.source(),
                selection.write_target.source(),
            ),
            filesystem,
            registry_limits,
        })
    }

    #[must_use]
    pub(crate) fn profile(&self) -> &ProfileName {
        &self.profile
    }

    #[must_use]
    pub(crate) fn workspace(&self) -> Option<&WorkspacePathKey> {
        self.workspace.as_ref()
    }

    #[must_use]
    pub(crate) const fn selection(&self) -> SelectionProvenance {
        self.selection
    }

    #[must_use]
    pub(crate) const fn filesystem(&self) -> FilesystemCapabilities {
        self.filesystem
    }

    #[must_use]
    pub(crate) const fn registry_limits(&self) -> RegistryLimits {
        self.registry_limits
    }
}
