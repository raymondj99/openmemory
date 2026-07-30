//! Immutable platform and filesystem capability facts.
//!
//! Callers resolve these facts once, before admission.  The current release
//! deliberately does not claim race-safe managed-path traversal because the
//! workspace has no reviewed directory-relative no-follow dependency yet.
//! Material promotion therefore remains unavailable rather than emulating a
//! security primitive with path canonicalization.

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Failure while inspecting a managed root before it enters a product policy.
#[derive(Debug, Error)]
pub enum PortabilityError {
    #[error("managed root inspection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("managed root must be a directory")]
    NotDirectory,
}

/// Versioned, immutable facts about one managed-root filesystem.
// These independent facts are intentionally not collapsed into one enum: the
// normalized policy records each eligibility decision and later phases can
// enable one capability without accidentally implying another.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemCapabilities {
    format_version: u16,
    advisory_locks: bool,
    same_filesystem_atomic_rename: bool,
    directory_sync: bool,
    managed_path_race_hardening: bool,
    material_promotion: bool,
}

impl FilesystemCapabilities {
    /// Inspect a root using only portable, conservative facts.  `false` means
    /// unavailable, never that a weaker path may be used.
    pub fn inspect(root: &Path) -> Result<Self, PortabilityError> {
        let metadata = std::fs::symlink_metadata(root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(PortabilityError::NotDirectory);
        }
        Ok(Self {
            format_version: 1,
            advisory_locks: true,
            same_filesystem_atomic_rename: true,
            directory_sync: cfg!(unix),
            // This is intentionally false until directory-relative no-follow
            // operations are provided by a reviewed safe dependency.
            managed_path_race_hardening: false,
            material_promotion: false,
        })
    }

    #[must_use]
    pub const fn format_version(self) -> u16 {
        self.format_version
    }

    #[must_use]
    pub const fn advisory_locks(self) -> bool {
        self.advisory_locks
    }

    #[must_use]
    pub const fn same_filesystem_atomic_rename(self) -> bool {
        self.same_filesystem_atomic_rename
    }

    #[must_use]
    pub const fn directory_sync(self) -> bool {
        self.directory_sync
    }

    #[must_use]
    pub const fn managed_path_race_hardening(self) -> bool {
        self.managed_path_race_hardening
    }

    #[must_use]
    pub const fn material_promotion(self) -> bool {
        self.material_promotion
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_path_hardening_is_not_overclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let capabilities = FilesystemCapabilities::inspect(dir.path()).unwrap();
        assert!(capabilities.advisory_locks());
        assert!(!capabilities.managed_path_race_hardening());
        assert!(!capabilities.material_promotion());
    }
}
