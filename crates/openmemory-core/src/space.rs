//! Validated memory-space identity, authority, and context contracts.
//!
//! These types own the local invariants that every higher-level space,
//! identity, and merge operation relies on. Constructors validate once;
//! private fields prevent lower layers from reinterpreting raw input.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::Uuid;

const MAX_OPAQUE_ID_BYTES: usize = 128;
const MAX_PROFILE_BYTES: usize = 128;
const MAX_WORKSPACE_PATH_BYTES: usize = 16 * 1024;
const AUTHORITY_DIGEST_BYTES: usize = 32;
const READ_SET_MAX: usize = 4;

/// Validation failures for memory-space foundation types.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SpaceTypeError {
    #[error("{kind} must be a canonical lowercase hyphenated UUIDv7")]
    InvalidUuidV7 { kind: &'static str },
    #[error("{kind} must contain 1..={max} UTF-8 bytes without control characters")]
    InvalidOpaqueId { kind: &'static str, max: usize },
    #[error("profile name is not a portable canonical component")]
    InvalidProfileName,
    #[error("workspace path must be absolute, reversible UTF-8, and at most {max} bytes")]
    InvalidWorkspacePath { max: usize },
    #[error("workspace path normalizer version must be positive")]
    InvalidPathNormalizerVersion,
    #[error("workspace path platform does not match the current platform")]
    WorkspacePathPlatformMismatch,
    #[error("authority snapshot version must be positive")]
    InvalidAuthorityVersion,
    #[error("authority snapshot digest must be 64 lowercase hexadecimal characters")]
    InvalidAuthorityDigest,
    #[error("read set must contain 1..={READ_SET_MAX} unique spaces")]
    InvalidReadSet,
    #[error("default write target is absent from the read set")]
    WriteTargetMissing,
    #[error("default write target does not grant write authority")]
    WriteTargetUnauthorized,
}

macro_rules! uuid_v7_id {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Canonical UUIDv7 identifier for a ", $kind, ".")]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint a new time-ordered UUIDv7.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Return the underlying UUID without weakening validation.
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl FromStr for $name {
            type Err = SpaceTypeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let parsed = Uuid::parse_str(value)
                    .map_err(|_| SpaceTypeError::InvalidUuidV7 { kind: $kind })?;
                if parsed.get_version_num() != 7
                    || parsed.hyphenated().to_string() != value
                    || value.bytes().any(|byte| byte.is_ascii_uppercase())
                {
                    return Err(SpaceTypeError::InvalidUuidV7 { kind: $kind });
                }
                Ok(Self(parsed))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}", self.0.hyphenated())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.to_string())
                    .finish()
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(de::Error::custom)
            }
        }
    };
}

uuid_v7_id!(SpaceId, "space");
uuid_v7_id!(ProjectId, "project");
uuid_v7_id!(ChangeSetId, "changeset");
uuid_v7_id!(RevisionId, "revision");
uuid_v7_id!(MergeJobId, "merge job");
uuid_v7_id!(SnapshotId, "snapshot");
uuid_v7_id!(DecisionEventId, "decision event");

macro_rules! opaque_id {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Bounded opaque ", $kind, " identifier.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Validate a caller-supplied opaque identifier.
            pub fn new(value: impl Into<String>) -> Result<Self, SpaceTypeError> {
                let value = value.into();
                if value.is_empty()
                    || value.len() > MAX_OPAQUE_ID_BYTES
                    || value.chars().any(char::is_control)
                {
                    return Err(SpaceTypeError::InvalidOpaqueId {
                        kind: $kind,
                        max: MAX_OPAQUE_ID_BYTES,
                    });
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = SpaceTypeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

opaque_id!(PrincipalId, "principal");
opaque_id!(TeamId, "team");

/// Portable, canonical profile directory component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProfileName(String);

impl ProfileName {
    pub fn new(value: impl Into<String>) -> Result<Self, SpaceTypeError> {
        let value = value.into();
        if !valid_profile_name(&value) {
            return Err(SpaceTypeError::InvalidProfileName);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ProfileName {
    type Err = SpaceTypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for ProfileName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

fn valid_profile_name(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_PROFILE_BYTES
        || value == "."
        || value == ".."
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return false;
    }
    let stem = value.split('.').next().unwrap_or_default();
    !is_windows_device_name(stem)
}

fn is_windows_device_name(value: &str) -> bool {
    let lowercase = value.to_ascii_lowercase();
    matches!(lowercase.as_str(), "con" | "prn" | "aux" | "nul")
        || (lowercase.len() == 4
            && (lowercase.starts_with("com") || lowercase.starts_with("lpt"))
            && matches!(lowercase.as_bytes()[3], b'1'..=b'9'))
}

/// Platform whose native path representation produced a workspace key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathPlatform {
    Linux,
    MacOs,
    Windows,
}

impl PathPlatform {
    #[must_use]
    pub const fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            compile_error!("workspace path keys require an explicitly supported platform");
        }
    }
}

/// Lossless, versioned key for an already canonicalized workspace path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct WorkspacePathKey {
    platform: PathPlatform,
    normalizer_version: u16,
    path: String,
}

impl WorkspacePathKey {
    /// Capture an absolute canonical path without lossy display conversion.
    pub fn from_canonical_path(
        path: &Path,
        normalizer_version: u16,
    ) -> Result<Self, SpaceTypeError> {
        let value = path.to_str().ok_or(SpaceTypeError::InvalidWorkspacePath {
            max: MAX_WORKSPACE_PATH_BYTES,
        })?;
        Self::from_parts(PathPlatform::current(), normalizer_version, value)
    }

    /// Reconstitute a validated persisted key.
    pub fn from_parts(
        platform: PathPlatform,
        normalizer_version: u16,
        path: impl Into<String>,
    ) -> Result<Self, SpaceTypeError> {
        if normalizer_version == 0 {
            return Err(SpaceTypeError::InvalidPathNormalizerVersion);
        }
        let path = path.into();
        if path.is_empty()
            || path.len() > MAX_WORKSPACE_PATH_BYTES
            || path.chars().any(|character| character == '\0')
            || !is_absolute_for_platform(platform, &path)
        {
            return Err(SpaceTypeError::InvalidWorkspacePath {
                max: MAX_WORKSPACE_PATH_BYTES,
            });
        }
        Ok(Self {
            platform,
            normalizer_version,
            path,
        })
    }

    #[must_use]
    pub const fn platform(&self) -> PathPlatform {
        self.platform
    }

    #[must_use]
    pub const fn normalizer_version(&self) -> u16 {
        self.normalizer_version
    }

    #[must_use]
    pub fn path_text(&self) -> &str {
        &self.path
    }

    pub fn as_current_path(&self) -> Result<&Path, SpaceTypeError> {
        if self.platform != PathPlatform::current() {
            return Err(SpaceTypeError::WorkspacePathPlatformMismatch);
        }
        Ok(Path::new(&self.path))
    }

    pub fn to_current_path_buf(&self) -> Result<PathBuf, SpaceTypeError> {
        self.as_current_path().map(Path::to_path_buf)
    }
}

fn is_absolute_for_platform(platform: PathPlatform, path: &str) -> bool {
    match platform {
        PathPlatform::Linux | PathPlatform::MacOs => path.starts_with('/'),
        PathPlatform::Windows => {
            let bytes = path.as_bytes();
            (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/'))
                || path.starts_with(r"\\")
        }
    }
}

#[derive(Deserialize)]
struct RawWorkspacePathKey {
    platform: PathPlatform,
    normalizer_version: u16,
    path: String,
}

impl<'de> Deserialize<'de> for WorkspacePathKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawWorkspacePathKey::deserialize(deserializer)?;
        Self::from_parts(raw.platform, raw.normalizer_version, raw.path).map_err(de::Error::custom)
    }
}

/// Versioned digest of all authority generations relevant to an operation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthoritySnapshot {
    version: u16,
    digest: [u8; AUTHORITY_DIGEST_BYTES],
}

impl AuthoritySnapshot {
    pub fn new(version: u16, digest: [u8; AUTHORITY_DIGEST_BYTES]) -> Result<Self, SpaceTypeError> {
        if version == 0 {
            return Err(SpaceTypeError::InvalidAuthorityVersion);
        }
        Ok(Self { version, digest })
    }

    /// Derive a digest from already normalized generation values.
    pub fn from_generations(version: u16, generations: &[u64]) -> Result<Self, SpaceTypeError> {
        if version == 0 {
            return Err(SpaceTypeError::InvalidAuthorityVersion);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"openmemory/authority-snapshot/v1");
        hasher.update(&version.to_le_bytes());
        hasher.update(
            &u64::try_from(generations.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        for generation in generations {
            hasher.update(&generation.to_le_bytes());
        }
        Ok(Self {
            version,
            digest: *hasher.finalize().as_bytes(),
        })
    }

    #[must_use]
    pub const fn version(self) -> u16 {
        self.version
    }

    #[must_use]
    pub const fn digest(self) -> [u8; AUTHORITY_DIGEST_BYTES] {
        self.digest
    }
}

impl fmt::Display for AuthoritySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "v{}:", self.version)?;
        for byte in self.digest {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for AuthoritySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AuthoritySnapshot")
            .field(&self.to_string())
            .finish()
    }
}

impl FromStr for AuthoritySnapshot {
    type Err = SpaceTypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (version, digest) = value
            .split_once(':')
            .ok_or(SpaceTypeError::InvalidAuthorityDigest)?;
        let version = version
            .strip_prefix('v')
            .ok_or(SpaceTypeError::InvalidAuthorityDigest)?
            .parse::<u16>()
            .map_err(|_| SpaceTypeError::InvalidAuthorityDigest)?;
        if version == 0
            || digest.len() != AUTHORITY_DIGEST_BYTES * 2
            || digest
                .bytes()
                .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
        {
            return Err(SpaceTypeError::InvalidAuthorityDigest);
        }
        let mut bytes = [0_u8; AUTHORITY_DIGEST_BYTES];
        for (index, slot) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            *slot = u8::from_str_radix(&digest[offset..offset + 2], 16)
                .map_err(|_| SpaceTypeError::InvalidAuthorityDigest)?;
        }
        Self::new(version, bytes)
    }
}

impl Serialize for AuthoritySnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AuthoritySnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SpaceOwner {
    User(PrincipalId),
    Team(TeamId),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "project_id", rename_all = "snake_case")]
pub enum SpaceContext {
    Global,
    Project(ProjectId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceRole {
    Reader,
    Contributor,
    Reviewer,
    Maintainer,
}

impl SpaceRole {
    #[must_use]
    pub const fn can_write(self) -> bool {
        !matches!(self, Self::Reader)
    }

    #[must_use]
    pub const fn can_review(self) -> bool {
        matches!(self, Self::Reviewer | Self::Maintainer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionSource {
    Explicit,
    WorkspaceMapping,
    ProductDefault,
}

/// Provenance for each independently resolved context choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SelectionProvenance {
    project: SelectionSource,
    team: SelectionSource,
    read_scope: SelectionSource,
    write_target: SelectionSource,
}

impl SelectionProvenance {
    #[must_use]
    pub const fn new(
        project: SelectionSource,
        team: SelectionSource,
        read_scope: SelectionSource,
        write_target: SelectionSource,
    ) -> Self {
        Self {
            project,
            team,
            read_scope,
            write_target,
        }
    }

    #[must_use]
    pub const fn project(self) -> SelectionSource {
        self.project
    }

    #[must_use]
    pub const fn team(self) -> SelectionSource {
        self.team
    }

    #[must_use]
    pub const fn read_scope(self) -> SelectionSource {
        self.read_scope
    }

    #[must_use]
    pub const fn write_target(self) -> SelectionSource {
        self.write_target
    }
}

/// Catalog identity and ownership of one complete memory space.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SpaceRef {
    id: SpaceId,
    owner: SpaceOwner,
    context: SpaceContext,
}

impl SpaceRef {
    #[must_use]
    pub const fn new(id: SpaceId, owner: SpaceOwner, context: SpaceContext) -> Self {
        Self { id, owner, context }
    }

    #[must_use]
    pub const fn id(&self) -> SpaceId {
        self.id
    }

    #[must_use]
    pub fn owner(&self) -> &SpaceOwner {
        &self.owner
    }

    #[must_use]
    pub fn context(&self) -> &SpaceContext {
        &self.context
    }
}

/// Current role and authority snapshot for one space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceGrant {
    space: SpaceRef,
    role: SpaceRole,
    authority: AuthoritySnapshot,
}

impl SpaceGrant {
    #[must_use]
    pub const fn new(space: SpaceRef, role: SpaceRole, authority: AuthoritySnapshot) -> Self {
        Self {
            space,
            role,
            authority,
        }
    }

    #[must_use]
    pub fn space(&self) -> &SpaceRef {
        &self.space
    }

    #[must_use]
    pub const fn role(&self) -> SpaceRole {
        self.role
    }

    #[must_use]
    pub const fn authority(&self) -> AuthoritySnapshot {
        self.authority
    }
}

/// Ordered, unique, bounded authorized recall layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ReadSet(Vec<SpaceGrant>);

impl ReadSet {
    pub fn new(grants: Vec<SpaceGrant>) -> Result<Self, SpaceTypeError> {
        if grants.is_empty() || grants.len() > READ_SET_MAX {
            return Err(SpaceTypeError::InvalidReadSet);
        }
        let mut seen = BTreeSet::new();
        if grants.iter().any(|grant| !seen.insert(grant.space().id())) {
            return Err(SpaceTypeError::InvalidReadSet);
        }
        Ok(Self(grants))
    }

    #[must_use]
    pub fn grants(&self) -> &[SpaceGrant] {
        &self.0
    }

    #[must_use]
    pub fn get(&self, id: SpaceId) -> Option<&SpaceGrant> {
        self.0.iter().find(|grant| grant.space().id() == id)
    }
}

impl<'de> Deserialize<'de> for ReadSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let grants = Vec::<SpaceGrant>::deserialize(deserializer)?;
        Self::new(grants).map_err(de::Error::custom)
    }
}

/// Fully resolved, immutable context consumed by operation hot paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryContext {
    principal: PrincipalId,
    actor_kind: ActorKind,
    profile: ProfileName,
    project: Option<ProjectId>,
    active_team: Option<TeamId>,
    read_set: ReadSet,
    default_write: SpaceId,
    authority: AuthoritySnapshot,
    selection: SelectionProvenance,
}

#[derive(Deserialize)]
struct RawMemoryContext {
    principal: PrincipalId,
    actor_kind: ActorKind,
    profile: ProfileName,
    project: Option<ProjectId>,
    active_team: Option<TeamId>,
    read_set: ReadSet,
    default_write: SpaceId,
    authority: AuthoritySnapshot,
    selection: SelectionProvenance,
}

impl<'de> Deserialize<'de> for MemoryContext {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawMemoryContext::deserialize(deserializer)?;
        Self::new(
            raw.principal,
            raw.actor_kind,
            raw.profile,
            raw.project,
            raw.active_team,
            raw.read_set,
            raw.default_write,
            raw.authority,
            raw.selection,
        )
        .map_err(de::Error::custom)
    }
}

impl MemoryContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        principal: PrincipalId,
        actor_kind: ActorKind,
        profile: ProfileName,
        project: Option<ProjectId>,
        active_team: Option<TeamId>,
        read_set: ReadSet,
        default_write: SpaceId,
        authority: AuthoritySnapshot,
        selection: SelectionProvenance,
    ) -> Result<Self, SpaceTypeError> {
        let write_grant = read_set
            .get(default_write)
            .ok_or(SpaceTypeError::WriteTargetMissing)?;
        if !write_grant.role().can_write() {
            return Err(SpaceTypeError::WriteTargetUnauthorized);
        }
        Ok(Self {
            principal,
            actor_kind,
            profile,
            project,
            active_team,
            read_set,
            default_write,
            authority,
            selection,
        })
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    pub const fn actor_kind(&self) -> ActorKind {
        self.actor_kind
    }

    #[must_use]
    pub fn profile(&self) -> &ProfileName {
        &self.profile
    }

    #[must_use]
    pub const fn project(&self) -> Option<ProjectId> {
        self.project
    }

    #[must_use]
    pub fn active_team(&self) -> Option<&TeamId> {
        self.active_team.as_ref()
    }

    #[must_use]
    pub fn read_set(&self) -> &ReadSet {
        &self.read_set
    }

    #[must_use]
    pub const fn default_write(&self) -> SpaceId {
        self.default_write
    }

    #[must_use]
    pub const fn authority(&self) -> AuthoritySnapshot {
        self.authority
    }

    #[must_use]
    pub const fn selection(&self) -> SelectionProvenance {
        self.selection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED_V7: &str = "018f6b7a-4d3c-7abc-8def-000000000001";

    fn authority() -> AuthoritySnapshot {
        AuthoritySnapshot::from_generations(1, &[4, 9, 12]).unwrap()
    }

    fn grant(id: &str, role: SpaceRole) -> SpaceGrant {
        SpaceGrant::new(
            SpaceRef::new(
                id.parse().unwrap(),
                SpaceOwner::User(PrincipalId::new("principal").unwrap()),
                SpaceContext::Global,
            ),
            role,
            authority(),
        )
    }

    #[test]
    fn uuid_ids_require_canonical_v7_and_round_trip_serde() {
        let id: SpaceId = FIXED_V7.parse().unwrap();
        assert_eq!(id.to_string(), FIXED_V7);
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            format!("\"{FIXED_V7}\"")
        );
        assert_eq!(
            serde_json::from_str::<SpaceId>(&format!("\"{FIXED_V7}\"")).unwrap(),
            id
        );
        assert!("550e8400-e29b-41d4-a716-446655440000"
            .parse::<SpaceId>()
            .is_err());
        assert!(FIXED_V7.to_ascii_uppercase().parse::<SpaceId>().is_err());
    }

    #[test]
    fn opaque_and_profile_boundaries_are_distinct() {
        assert!(PrincipalId::new("external/team:id").is_ok());
        assert!(PrincipalId::new("\n").is_err());
        assert!(ProfileName::new("team/default").is_err());
        assert!(ProfileName::new(".hidden").is_err());
        assert!(ProfileName::new("CON").is_err());
        assert_eq!(ProfileName::new("default").unwrap().as_str(), "default");
    }

    #[test]
    fn workspace_key_requires_absolute_reversible_versioned_path() {
        let key = WorkspacePathKey::from_parts(PathPlatform::Linux, 1, "/work/α").unwrap();
        let encoded = serde_json::to_string(&key).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkspacePathKey>(&encoded).unwrap(),
            key
        );
        assert!(WorkspacePathKey::from_parts(PathPlatform::Linux, 0, "/work").is_err());
        assert!(WorkspacePathKey::from_parts(PathPlatform::Linux, 1, "relative").is_err());
        let windows =
            WorkspacePathKey::from_parts(PathPlatform::Windows, 1, r"C:\work\memory").unwrap();
        assert_eq!(
            serde_json::from_str::<WorkspacePathKey>(&serde_json::to_string(&windows).unwrap())
                .unwrap(),
            windows
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_workspace_path_is_rejected_without_lossy_aliasing() {
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
        assert!(matches!(
            WorkspacePathKey::from_canonical_path(&path, 1),
            Err(SpaceTypeError::InvalidWorkspacePath { .. })
        ));
    }

    #[test]
    fn authority_snapshot_is_versioned_and_canonical() {
        let value = authority();
        let text = value.to_string();
        assert_eq!(text.parse::<AuthoritySnapshot>().unwrap(), value);
        assert!(text
            .to_ascii_uppercase()
            .parse::<AuthoritySnapshot>()
            .is_err());
        assert_ne!(
            AuthoritySnapshot::from_generations(1, &[1, 23]).unwrap(),
            AuthoritySnapshot::from_generations(1, &[12, 3]).unwrap()
        );
    }

    #[test]
    fn read_set_preserves_order_and_rejects_duplicates_and_bounds() {
        let first = grant(FIXED_V7, SpaceRole::Contributor);
        let duplicate = first.clone();
        assert!(ReadSet::new(vec![first.clone(), duplicate]).is_err());
        let set = ReadSet::new(vec![first]).unwrap();
        assert_eq!(set.grants()[0].space().id().to_string(), FIXED_V7);
        assert!(ReadSet::new(Vec::new()).is_err());
    }

    #[test]
    fn context_requires_authorized_write_target_and_retains_intent() {
        let read_only = grant(FIXED_V7, SpaceRole::Reader);
        let id = read_only.space().id();
        let result = MemoryContext::new(
            PrincipalId::new("principal").unwrap(),
            ActorKind::Human,
            ProfileName::new("default").unwrap(),
            None,
            None,
            ReadSet::new(vec![read_only]).unwrap(),
            id,
            authority(),
            SelectionProvenance::new(
                SelectionSource::Explicit,
                SelectionSource::ProductDefault,
                SelectionSource::Explicit,
                SelectionSource::Explicit,
            ),
        );
        assert_eq!(result.unwrap_err(), SpaceTypeError::WriteTargetUnauthorized);

        let writable = grant(FIXED_V7, SpaceRole::Contributor);
        let context = MemoryContext::new(
            PrincipalId::new("principal").unwrap(),
            ActorKind::Agent,
            ProfileName::new("default").unwrap(),
            None,
            None,
            ReadSet::new(vec![writable]).unwrap(),
            id,
            authority(),
            SelectionProvenance::new(
                SelectionSource::Explicit,
                SelectionSource::ProductDefault,
                SelectionSource::WorkspaceMapping,
                SelectionSource::Explicit,
            ),
        )
        .unwrap();
        assert_eq!(
            context.selection().write_target(),
            SelectionSource::Explicit
        );
        assert_eq!(
            serde_json::from_str::<MemoryContext>(&serde_json::to_string(&context).unwrap())
                .unwrap(),
            context
        );
    }
}
