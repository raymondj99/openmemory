//! Validated identifiers and authorization context for semantic memory spaces.
//!
//! A memory space is a user-visible semantic silo. It is deliberately distinct
//! from an engine domain, which is only a performance partition inside a space.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::Uuid;

/// Maximum number of spaces an interactive request may read.
pub const MAX_READ_SET: usize = 4;
/// Maximum encoded length of an opaque principal or team identifier.
pub const MAX_PRINCIPAL_BYTES: usize = 128;

/// Validation failure for a memory-space contract value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpaceValidationError {
    #[error("{kind} must be a canonical UUIDv7")]
    InvalidUuid { kind: &'static str },
    #[error("{kind} must contain 1..={max} path-safe ASCII bytes")]
    InvalidOpaqueId { kind: &'static str, max: usize },
    #[error("read set must contain 1..={MAX_READ_SET} grants")]
    InvalidReadSetSize,
    #[error("read set contains duplicate space {0}")]
    DuplicateSpace(SpaceId),
    #[error("space {0} has no current authority generation")]
    UnauthorizedSpace(SpaceId),
    #[error("memory context profile is not a path-safe profile name")]
    InvalidProfile,
    #[error("memory context has no current authorization generation")]
    InvalidAuthorizationGeneration,
    #[error("default write target is absent from the read set")]
    WriteTargetMissing,
    #[error("default write target does not grant contributor access")]
    WriteTargetReadOnly,
}

macro_rules! uuid_id {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("A canonical UUIDv7-backed ", $kind, ".")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint a time-ordered UUIDv7 identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Return the underlying UUID value.
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

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.hyphenated().fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = SpaceValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let parsed = Uuid::parse_str(value)
                    .map_err(|_| SpaceValidationError::InvalidUuid { kind: $kind })?;
                let canonical = parsed.hyphenated().to_string();
                if parsed.get_version_num() != 7 || canonical != value {
                    return Err(SpaceValidationError::InvalidUuid { kind: $kind });
                }
                Ok(Self(parsed))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_str(self)
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

uuid_id!(SpaceId, "space ID");
uuid_id!(ProjectId, "project ID");
uuid_id!(ChangeSetId, "changeset ID");
uuid_id!(RevisionId, "revision ID");
uuid_id!(MergeJobId, "merge-job ID");

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PRINCIPAL_BYTES
        && value != "."
        && value != ".."
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b':' | b'@' | b'+' | b'=' | b'-')
        })
}

fn valid_profile(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

macro_rules! opaque_id {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("A validated opaque ", $kind, ".")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Box<str>);

        impl $name {
            /// Return the validated external identifier.
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
            type Err = SpaceValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if !valid_opaque_id(value) {
                    return Err(SpaceValidationError::InvalidOpaqueId {
                        kind: $kind,
                        max: MAX_PRINCIPAL_BYTES,
                    });
                }
                Ok(Self(value.into()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = SpaceValidationError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                value.parse()
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
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

opaque_id!(PrincipalId, "principal ID");
opaque_id!(TeamId, "team ID");

/// The authority which owns one semantic memory space.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum SpaceOwner {
    /// A personal space owned by one installation principal.
    User(PrincipalId),
    /// A shared space owned by one locally authorized team.
    Team(TeamId),
}

/// Stable semantic context for a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "project_id")]
pub enum SpaceContext {
    /// Owner-wide context.
    Global,
    /// One stable project, independent of workspace path.
    Project(ProjectId),
}

/// Local authorization role, ordered from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceRole {
    Reader,
    Contributor,
    Reviewer,
    Maintainer,
}

impl SpaceRole {
    /// Whether this role contains the requested role's authority.
    #[must_use]
    pub fn allows(self, required: Self) -> bool {
        self >= required
    }
}

/// Kind of actor using an authorized context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
    System,
}

/// Catalog identity and semantic ownership of a space.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceRef {
    pub id: SpaceId,
    pub owner: SpaceOwner,
    pub context: SpaceContext,
}

/// One current authorization grant in an ordered read set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceGrant {
    pub space: SpaceRef,
    pub role: SpaceRole,
    pub authority_generation: u64,
}

/// A validated, explicitly ordered set of one to four authorized spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadSet(Vec<SpaceGrant>);

impl ReadSet {
    /// Validate cardinality, current authority, and unique space identity.
    pub fn new(grants: Vec<SpaceGrant>) -> Result<Self, SpaceValidationError> {
        if grants.is_empty() || grants.len() > MAX_READ_SET {
            return Err(SpaceValidationError::InvalidReadSetSize);
        }
        let mut seen = BTreeSet::new();
        for grant in &grants {
            if grant.authority_generation == 0 {
                return Err(SpaceValidationError::UnauthorizedSpace(grant.space.id));
            }
            if !seen.insert(grant.space.id) {
                return Err(SpaceValidationError::DuplicateSpace(grant.space.id));
            }
        }
        Ok(Self(grants))
    }

    /// Grants in exact recall precedence order.
    #[must_use]
    pub fn as_slice(&self) -> &[SpaceGrant] {
        &self.0
    }

    /// Iterate in exact recall precedence order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &SpaceGrant> {
        self.0.iter()
    }

    /// Number of selected spaces.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The validated set is never empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Find one grant without changing precedence order.
    #[must_use]
    pub fn grant(&self, id: SpaceId) -> Option<&SpaceGrant> {
        self.0.iter().find(|grant| grant.space.id == id)
    }
}

impl Serialize for ReadSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
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

impl IntoIterator for ReadSet {
    type Item = SpaceGrant;
    type IntoIter = std::vec::IntoIter<SpaceGrant>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Immutable authorization result carried by a memory request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryContext {
    pub principal: PrincipalId,
    pub actor_kind: ActorKind,
    pub profile: String,
    pub project: Option<ProjectId>,
    pub active_team: Option<TeamId>,
    pub read_set: ReadSet,
    pub default_write: SpaceId,
    pub authorization_generation: u64,
}

impl MemoryContext {
    /// Validate profile, generation, and default write authority.
    ///
    /// Read-only contexts do not require the nominal default target to be in
    /// the read set. Mutable contexts require a current Contributor grant.
    pub fn validate(&self, read_only: bool) -> Result<(), SpaceValidationError> {
        if !valid_profile(&self.profile) {
            return Err(SpaceValidationError::InvalidProfile);
        }
        if self.authorization_generation == 0 {
            return Err(SpaceValidationError::InvalidAuthorizationGeneration);
        }
        if read_only {
            return Ok(());
        }
        let grant = self
            .read_set
            .grant(self.default_write)
            .ok_or(SpaceValidationError::WriteTargetMissing)?;
        if !grant.role.allows(SpaceRole::Contributor) {
            return Err(SpaceValidationError::WriteTargetReadOnly);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn principal() -> PrincipalId {
        "local:owner".parse().unwrap()
    }

    fn grant(id: SpaceId, role: SpaceRole) -> SpaceGrant {
        SpaceGrant {
            space: SpaceRef {
                id,
                owner: SpaceOwner::User(principal()),
                context: SpaceContext::Global,
            },
            role,
            authority_generation: 1,
        }
    }

    #[test]
    fn uuid_ids_are_v7_canonical_and_serde_as_strings() {
        let id = SpaceId::new();
        let encoded = id.to_string();
        assert_eq!(encoded.parse::<SpaceId>().unwrap(), id);
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            format!("\"{encoded}\"")
        );
        assert_eq!(
            serde_json::from_str::<SpaceId>(&format!("\"{encoded}\"")).unwrap(),
            id
        );
        assert!("550e8400-e29b-41d4-a716-446655440000"
            .parse::<SpaceId>()
            .is_err());
        assert!(encoded.to_uppercase().parse::<SpaceId>().is_err());
    }

    #[test]
    fn opaque_ids_are_bounded_and_path_safe() {
        for valid in ["local:owner", "team.alpha-1", "user@example.com"] {
            assert_eq!(valid.parse::<PrincipalId>().unwrap().as_str(), valid);
        }
        for invalid in ["", ".", "..", "a/b", "a\\b", " has-space", "snowman-☃"] {
            assert!(
                invalid.parse::<PrincipalId>().is_err(),
                "accepted {invalid:?}"
            );
        }
        assert!("x"
            .repeat(MAX_PRINCIPAL_BYTES + 1)
            .parse::<TeamId>()
            .is_err());
    }

    #[test]
    fn read_set_enforces_bounds_authority_and_order() {
        let first = SpaceId::new();
        let second = SpaceId::new();
        let set = ReadSet::new(vec![
            grant(first, SpaceRole::Contributor),
            grant(second, SpaceRole::Reader),
        ])
        .unwrap();
        assert_eq!(
            set.iter().map(|item| item.space.id).collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(ReadSet::new(Vec::new()).is_err());
        assert!(ReadSet::new(vec![
            grant(first, SpaceRole::Reader),
            grant(first, SpaceRole::Reader)
        ])
        .is_err());
        let mut unauthorized = grant(first, SpaceRole::Reader);
        unauthorized.authority_generation = 0;
        assert!(ReadSet::new(vec![unauthorized]).is_err());
        assert!(ReadSet::new(
            (0..=MAX_READ_SET)
                .map(|_| grant(SpaceId::new(), SpaceRole::Reader))
                .collect()
        )
        .is_err());
    }

    #[test]
    fn context_requires_contributor_write_grant() {
        let id = SpaceId::new();
        let mut context = MemoryContext {
            principal: principal(),
            actor_kind: ActorKind::Agent,
            profile: "default".to_string(),
            project: None,
            active_team: None,
            read_set: ReadSet::new(vec![grant(id, SpaceRole::Reader)]).unwrap(),
            default_write: id,
            authorization_generation: 1,
        };
        assert_eq!(
            context.validate(false),
            Err(SpaceValidationError::WriteTargetReadOnly)
        );
        assert!(context.validate(true).is_ok());
        context.read_set = ReadSet::new(vec![grant(id, SpaceRole::Contributor)]).unwrap();
        assert!(context.validate(false).is_ok());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn arbitrary_ids_never_panic_and_accepted_values_are_path_safe(value in any::<String>()) {
            if let Ok(id) = value.parse::<PrincipalId>() {
                prop_assert!(!id.as_str().contains('/'));
                prop_assert!(!id.as_str().contains('\\'));
                prop_assert!(id.as_str().is_ascii());
                prop_assert!(id.as_str().len() <= MAX_PRINCIPAL_BYTES);
            }
            if let Ok(id) = value.parse::<SpaceId>() {
                prop_assert_eq!(id.to_string(), value);
                prop_assert_eq!(id.as_uuid().get_version_num(), 7);
            }
        }
    }
}
