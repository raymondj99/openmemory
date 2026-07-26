//! Durable `space.toml` bindings.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use openmemory_core::space::{SpaceContext, SpaceId, SpaceOwner};
use openmemory_graph::{MemoryError, MemoryResult};
use serde::{Deserialize, Serialize};

/// File which binds a catalog space to a physical store root.
pub const SPACE_MANIFEST_FILE: &str = "space.toml";
/// Current manifest encoding.
pub const SPACE_MANIFEST_FORMAT_VERSION: u32 = 1;

/// Durable, display-name-independent binding for one semantic space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceManifest {
    pub format_version: u32,
    pub space_id: SpaceId,
    pub profile: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub context_kind: String,
    pub project_id: String,
    pub domain_count: usize,
    pub created_at_unix_secs: i64,
    pub catalog_binding_hash: String,
}

impl SpaceManifest {
    /// Build a manifest and bind it to the catalog's immutable root key.
    pub fn new(
        space_id: SpaceId,
        profile: &str,
        owner: &SpaceOwner,
        context: SpaceContext,
        domain_count: usize,
        created_at_unix_secs: i64,
        catalog_root_key: &str,
    ) -> MemoryResult<Self> {
        validate_profile(profile)?;
        validate_root_key(catalog_root_key)?;
        if domain_count == 0 {
            return Err(MemoryError::InvalidInput(
                "space domain count must be at least one".to_string(),
            ));
        }
        let (owner_kind, owner_id) = match owner {
            SpaceOwner::User(id) => ("user".to_string(), id.to_string()),
            SpaceOwner::Team(id) => ("team".to_string(), id.to_string()),
        };
        let (context_kind, project_id) = match context {
            SpaceContext::Global => ("global".to_string(), String::new()),
            SpaceContext::Project(id) => ("project".to_string(), id.to_string()),
        };
        let mut manifest = Self {
            format_version: SPACE_MANIFEST_FORMAT_VERSION,
            space_id,
            profile: profile.to_string(),
            owner_kind,
            owner_id,
            context_kind,
            project_id,
            domain_count,
            created_at_unix_secs,
            catalog_binding_hash: String::new(),
        };
        manifest.catalog_binding_hash = manifest.binding_hash(catalog_root_key);
        Ok(manifest)
    }

    /// Parse and structurally validate a manifest.
    pub fn load(path: &Path) -> MemoryResult<Self> {
        let encoded = std::fs::read_to_string(path)?;
        let manifest: Self = toml::from_str(&encoded).map_err(|error| {
            MemoryError::InvalidInput(format!(
                "invalid space manifest {}: {error}",
                path.display()
            ))
        })?;
        manifest.validate_shape()?;
        Ok(manifest)
    }

    /// Atomically replace a manifest and durably persist its directory entry.
    pub fn write_atomic(&self, path: &Path) -> MemoryResult<()> {
        self.validate_shape()?;
        let parent = path.parent().ok_or_else(|| {
            MemoryError::InvalidInput("space manifest path has no parent".to_string())
        })?;
        std::fs::create_dir_all(parent)?;
        let encoded = toml::to_string_pretty(self).map_err(|error| {
            MemoryError::InvalidInput(format!("space manifest encode failed: {error}"))
        })?;
        let temp = parent.join(format!(
            ".{}.tmp-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(SPACE_MANIFEST_FILE),
            SpaceId::new()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(encoded.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temp, path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    /// Verify both catalog identity and physical-domain binding.
    pub fn verify(
        &self,
        expected_space_id: SpaceId,
        expected_profile: &str,
        expected_root_key: &str,
        expected_domain_count: usize,
    ) -> MemoryResult<()> {
        self.validate_shape()?;
        if self.space_id != expected_space_id
            || self.profile != expected_profile
            || self.domain_count != expected_domain_count
        {
            return Err(MemoryError::InvalidInput(
                "space manifest catalog binding mismatch".to_string(),
            ));
        }
        let expected = self.binding_hash(expected_root_key);
        if !constant_time_eq(self.catalog_binding_hash.as_bytes(), expected.as_bytes()) {
            return Err(MemoryError::InvalidInput(
                "space manifest binding hash mismatch".to_string(),
            ));
        }
        Ok(())
    }

    /// Recover the typed owner encoded by the manifest.
    pub fn owner(&self) -> MemoryResult<SpaceOwner> {
        match self.owner_kind.as_str() {
            "user" => self
                .owner_id
                .parse()
                .map(SpaceOwner::User)
                .map_err(|error| MemoryError::InvalidInput(format!("{error}"))),
            "team" => self
                .owner_id
                .parse()
                .map(SpaceOwner::Team)
                .map_err(|error| MemoryError::InvalidInput(format!("{error}"))),
            _ => Err(MemoryError::InvalidInput(
                "invalid space owner kind".to_string(),
            )),
        }
    }

    /// Recover the typed context encoded by the manifest.
    pub fn context(&self) -> MemoryResult<SpaceContext> {
        match self.context_kind.as_str() {
            "global" if self.project_id.is_empty() => Ok(SpaceContext::Global),
            "project" if !self.project_id.is_empty() => self
                .project_id
                .parse()
                .map(SpaceContext::Project)
                .map_err(|error| MemoryError::InvalidInput(format!("{error}"))),
            _ => Err(MemoryError::InvalidInput(
                "invalid space context binding".to_string(),
            )),
        }
    }

    fn validate_shape(&self) -> MemoryResult<()> {
        if self.format_version != SPACE_MANIFEST_FORMAT_VERSION {
            return Err(MemoryError::InvalidInput(format!(
                "unsupported space manifest version {}",
                self.format_version
            )));
        }
        validate_profile(&self.profile)?;
        if self.domain_count == 0 {
            return Err(MemoryError::InvalidInput(
                "space domain count must be at least one".to_string(),
            ));
        }
        let _ = self.owner()?;
        let _ = self.context()?;
        let hash = self
            .catalog_binding_hash
            .strip_prefix("blake3:")
            .ok_or_else(|| {
                MemoryError::InvalidInput("space manifest binding hash must use blake3".to_string())
            })?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(MemoryError::InvalidInput(
                "invalid space manifest binding hash".to_string(),
            ));
        }
        Ok(())
    }

    fn binding_hash(&self, catalog_root_key: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"openmemory/space-manifest/v1\0");
        hash_field(
            &mut hasher,
            b"format_version",
            &self.format_version.to_le_bytes(),
        );
        hash_field(
            &mut hasher,
            b"space_id",
            self.space_id.to_string().as_bytes(),
        );
        hash_field(&mut hasher, b"profile", self.profile.as_bytes());
        hash_field(&mut hasher, b"owner_kind", self.owner_kind.as_bytes());
        hash_field(&mut hasher, b"owner_id", self.owner_id.as_bytes());
        hash_field(&mut hasher, b"context_kind", self.context_kind.as_bytes());
        hash_field(&mut hasher, b"project_id", self.project_id.as_bytes());
        hash_field(
            &mut hasher,
            b"domain_count",
            &self.domain_count.to_le_bytes(),
        );
        hash_field(
            &mut hasher,
            b"catalog_root_key",
            catalog_root_key.as_bytes(),
        );
        format!("blake3:{}", hasher.finalize().to_hex())
    }
}

/// Resolve an opaque space root and reject traversal or symlink escape.
pub fn resolve_space_root(profile_root: &Path, space_id: SpaceId) -> MemoryResult<PathBuf> {
    reject_non_normal_path(profile_root)?;
    let canonical_profile = profile_root.canonicalize()?;
    let spaces = canonical_profile.join("spaces");
    std::fs::create_dir_all(&spaces)?;
    let canonical_spaces = spaces.canonicalize()?;
    if !canonical_spaces.starts_with(&canonical_profile) {
        return Err(MemoryError::InvalidInput(
            "spaces directory escapes profile root".to_string(),
        ));
    }
    let candidate = canonical_spaces.join(space_id.to_string());
    if candidate.exists() {
        let canonical = candidate.canonicalize()?;
        if !canonical.starts_with(&canonical_spaces) {
            return Err(MemoryError::InvalidInput(
                "space root escapes profile root".to_string(),
            ));
        }
        Ok(canonical)
    } else {
        Ok(candidate)
    }
}

fn validate_profile(value: &str) -> MemoryResult<()> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(MemoryError::InvalidInput(
            "invalid profile in space manifest".to_string(),
        ))
    }
}

fn validate_root_key(value: &str) -> MemoryResult<()> {
    if !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('/')
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        Ok(())
    } else {
        Err(MemoryError::InvalidInput(
            "catalog root key is not a safe relative path".to_string(),
        ))
    }
}

fn reject_non_normal_path(path: &Path) -> MemoryResult<()> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(MemoryError::InvalidInput(
            "profile root contains parent traversal".to_string(),
        ));
    }
    Ok(())
}

fn hash_field(hasher: &mut blake3::Hasher, name: &[u8], value: &[u8]) {
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name);
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_core::space::PrincipalId;

    fn manifest() -> SpaceManifest {
        SpaceManifest::new(
            SpaceId::new(),
            "default",
            &SpaceOwner::User("local:user".parse::<PrincipalId>().unwrap()),
            SpaceContext::Global,
            2,
            123,
            "spaces/root-key/store",
        )
        .unwrap()
    }

    #[test]
    fn atomic_round_trip_and_binding_verification() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(SPACE_MANIFEST_FILE);
        let expected = manifest();
        expected.write_atomic(&path).unwrap();
        let actual = SpaceManifest::load(&path).unwrap();
        assert_eq!(actual, expected);
        actual
            .verify(expected.space_id, "default", "spaces/root-key/store", 2)
            .unwrap();
    }

    #[test]
    fn copied_catalog_binding_fails_closed() {
        let item = manifest();
        assert!(item
            .verify(item.space_id, "default", "spaces/another/store", 2)
            .is_err());
    }

    #[test]
    fn malicious_root_key_is_rejected() {
        let mut item = manifest();
        item.catalog_binding_hash = item.binding_hash("spaces/ok/store");
        assert!(SpaceManifest::new(
            item.space_id,
            "default",
            &item.owner().unwrap(),
            SpaceContext::Global,
            1,
            0,
            "../escape"
        )
        .is_err());
    }
}
