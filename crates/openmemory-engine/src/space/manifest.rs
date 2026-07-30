//! Durable per-space manifest.
//!
//! Every managed space root carries a `space.toml` binding the directory to
//! one immutable [`SpaceId`] and its human-facing name. The manifest is the
//! identity anchor for reopen: a root whose manifest disagrees with the
//! caller's expectation (name mismatch, unsupported format version) fails
//! closed instead of silently opening someone else's data. Store-level
//! identity is additionally verified by `DomainStore::bind_space_id`, which
//! checks the `space_id` persisted inside every member database, so a copied
//! or spliced store cannot masquerade under another manifest.

use std::path::Path;
use std::str::FromStr;

use openmemory_core::space::SpaceId;
use openmemory_graph::{MemoryError, MemoryResult};
use serde::{Deserialize, Serialize};

/// Manifest file name inside a managed space root.
pub const SPACE_MANIFEST_FILE: &str = "space.toml";

/// Only supported manifest format version.
pub const SPACE_MANIFEST_FORMAT: u32 = 1;

/// Upper bound on manifest bytes; a larger file is treated as corrupt
/// rather than parsed.
const MANIFEST_MAX_BYTES: u64 = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestFile {
    format_version: u32,
    space_id: String,
    name: String,
    created_at: i64,
}

/// Validated identity of one managed space root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceManifest {
    /// Immutable space identity, persisted in the manifest and bound into
    /// every member database.
    pub space_id: SpaceId,
    /// Human-facing space name; always equals the validated directory name.
    pub name: String,
    /// Creation instant (Unix seconds) recorded when the space was created.
    pub created_at: i64,
}

impl SpaceManifest {
    /// Read and validate the manifest at `root`, requiring `expected_name`.
    pub fn load(root: &Path, expected_name: &str) -> MemoryResult<Self> {
        let path = root.join(SPACE_MANIFEST_FILE);
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(MemoryError::InvalidInput(format!(
                "space manifest {} is not a regular file",
                path.display()
            )));
        }
        if metadata.len() > MANIFEST_MAX_BYTES {
            return Err(MemoryError::InvalidInput(format!(
                "space manifest {} exceeds its size bound",
                path.display()
            )));
        }
        let text = std::fs::read_to_string(&path)?;
        let file: ManifestFile = toml::from_str(&text).map_err(|e| {
            MemoryError::InvalidInput(format!("corrupt space manifest {}: {e}", path.display()))
        })?;
        if file.format_version != SPACE_MANIFEST_FORMAT {
            return Err(MemoryError::InvalidInput(format!(
                "space manifest {} has unsupported format version {}",
                path.display(),
                file.format_version
            )));
        }
        if file.name != expected_name {
            return Err(MemoryError::InvalidInput(format!(
                "space manifest {} names '{}' but the root directory is '{expected_name}'",
                path.display(),
                file.name
            )));
        }
        let space_id = SpaceId::from_str(&file.space_id).map_err(|e| {
            MemoryError::InvalidInput(format!(
                "space manifest {} has an invalid space_id: {e}",
                path.display()
            ))
        })?;
        Ok(Self {
            space_id,
            name: file.name,
            created_at: file.created_at,
        })
    }

    /// Atomically write a new manifest into `root`. Fails if one already
    /// exists: manifests are written exactly once, at creation.
    pub fn create(
        root: &Path,
        space_id: SpaceId,
        name: &str,
        created_at: i64,
    ) -> MemoryResult<Self> {
        let path = root.join(SPACE_MANIFEST_FILE);
        if path.exists() {
            return Err(MemoryError::InvalidInput(format!(
                "space manifest {} already exists",
                path.display()
            )));
        }
        let file = ManifestFile {
            format_version: SPACE_MANIFEST_FORMAT,
            space_id: space_id.to_string(),
            name: name.to_owned(),
            created_at,
        };
        let text = toml::to_string_pretty(&file)
            .map_err(|e| MemoryError::InvalidInput(format!("encoding space manifest: {e}")))?;

        // Atomic publication: temp file in the same directory, fsync,
        // rename, then fsync the directory so the rename is durable.
        let tmp = root.join(".space.toml.tmp");
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            use std::io::Write as _;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        if let Ok(dir) = std::fs::File::open(root) {
            let _ = dir.sync_all();
        }
        Ok(Self {
            space_id,
            name: name.to_owned(),
            created_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let id = SpaceId::new();
        let created = SpaceManifest::create(dir.path(), id, "research", 1_700_000_000).unwrap();
        let loaded = SpaceManifest::load(dir.path(), "research").unwrap();
        assert_eq!(created, loaded);
        assert_eq!(loaded.space_id, id);
        assert_eq!(loaded.created_at, 1_700_000_000);
    }

    #[test]
    fn create_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        SpaceManifest::create(dir.path(), SpaceId::new(), "a", 0).unwrap();
        let err = SpaceManifest::create(dir.path(), SpaceId::new(), "a", 0).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn load_rejects_name_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        SpaceManifest::create(dir.path(), SpaceId::new(), "alpha", 0).unwrap();
        let err = SpaceManifest::load(dir.path(), "beta").unwrap_err();
        assert!(err.to_string().contains("root directory is 'beta'"));
    }

    #[test]
    fn load_rejects_unsupported_format_version() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SPACE_MANIFEST_FILE),
            "format_version = 99\nspace_id = \"00000000-0000-0000-0000-000000000000\"\nname = \"x\"\ncreated_at = 0\n",
        )
        .unwrap();
        let err = SpaceManifest::load(dir.path(), "x").unwrap_err();
        assert!(err.to_string().contains("unsupported format version"));
    }

    #[test]
    fn load_rejects_corrupt_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SPACE_MANIFEST_FILE), "not toml [[").unwrap();
        let err = SpaceManifest::load(dir.path(), "x").unwrap_err();
        assert!(err.to_string().contains("corrupt space manifest"));
    }
}
