//! Validated canonical records and snapshot-reader seam.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use openmemory_core::space::{RevisionId, SnapshotId, SpaceId};
use serde::{de, Deserialize, Deserializer, Serialize};

use crate::canonical::CanonicalHasher;
use crate::hash::{SemanticHash, SnapshotHash};
use crate::{MergeError, MergeErrorCode, MergeResult};

pub const MAX_LOGICAL_ID_BYTES: usize = 256;
pub const MAX_LABEL_BYTES: usize = 512;
pub const MAX_KIND_BYTES: usize = 128;
pub const MAX_ALIASES: usize = 64;
pub const MAX_IDENTIFIERS: usize = 32;
pub const MAX_PROPERTIES: usize = 64;
pub const MAX_PROPERTY_NAME_BYTES: usize = 128;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_ORIGINS: usize = 1_024;
pub const MAX_SNAPSHOT_RECORDS: usize = 1_000_000;
pub const QUALIFIED_ID_PREFIX: &str = "omq1:";

/// Validated local object identifier. The qualified import namespace is
/// reserved and cannot be supplied as a local ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LogicalId(String);

impl LogicalId {
    pub fn new(value: impl Into<String>) -> MergeResult<Self> {
        let value = value.into();
        if !valid_token(&value, MAX_LOGICAL_ID_BYTES) || value.starts_with(QUALIFIED_ID_PREFIX) {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "logical ID is empty, oversized, contains controls, or uses the reserved namespace",
            ));
        }
        Ok(Self(value))
    }

    /// Validate an ID read from a canonical snapshot projection. This accepts
    /// the reserved qualified namespace only when its full versioned hash
    /// shape is valid; ordinary creation must use `new`.
    pub fn from_projection(value: impl Into<String>) -> MergeResult<Self> {
        let value = value.into();
        if let Some(digest) = value.strip_prefix(QUALIFIED_ID_PREFIX) {
            if digest.len() != 64
                || digest
                    .bytes()
                    .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
            {
                return Err(MergeError::new(
                    MergeErrorCode::InvalidInput,
                    "qualified projection ID has an invalid canonical hash",
                ));
            }
            return Ok(Self(value));
        }
        Self::new(value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LogicalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for LogicalId {
    type Err = MergeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for LogicalId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// Collision-resistant projection ID for an imported source object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct QualifiedObjectId(String);

impl QualifiedObjectId {
    #[must_use]
    pub fn derive(space: SpaceId, logical_id: &LogicalId) -> Self {
        let mut hash = CanonicalHasher::new(b"openmemory/qualified-object-id/v1");
        hash.space_id(space);
        hash.text(logical_id.as_str());
        Self(format!("{QUALIFIED_ID_PREFIX}{}", hash.finish_semantic()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Result object identity is either an existing target-local ID or a derived
/// qualified import ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ResultObjectId {
    Target(LogicalId),
    Qualified(QualifiedObjectId),
}

impl ResultObjectId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Target(value) => value.as_str(),
            Self::Qualified(value) => value.as_str(),
        }
    }
}

/// Space-qualified local object address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ObjectAddress {
    space: SpaceId,
    logical_id: LogicalId,
}

impl ObjectAddress {
    #[must_use]
    pub const fn new(space: SpaceId, logical_id: LogicalId) -> Self {
        Self { space, logical_id }
    }

    #[must_use]
    pub const fn space(&self) -> SpaceId {
        self.space
    }

    #[must_use]
    pub fn logical_id(&self) -> &LogicalId {
        &self.logical_id
    }
}

pub type EntityAddress = ObjectAddress;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Active,
    Retired,
    Deleted,
}

/// Trust attached by ingestion to an external identifier assertion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssertionTrust {
    Claimed,
    SourceVerified {
        source_snapshot: SnapshotId,
        verifier_version: String,
        resolver_generation: u64,
    },
}

/// One namespace-qualified identifier assertion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct IdentifierAssertion {
    namespace: String,
    value: String,
    trust: AssertionTrust,
}

impl IdentifierAssertion {
    pub fn claimed(namespace: impl Into<String>, value: impl Into<String>) -> MergeResult<Self> {
        Self::new(namespace.into(), value.into(), AssertionTrust::Claimed)
    }

    pub fn source_verified(
        namespace: impl Into<String>,
        value: impl Into<String>,
        source_snapshot: SnapshotId,
        verifier_version: impl Into<String>,
        resolver_generation: u64,
    ) -> MergeResult<Self> {
        let verifier_version = verifier_version.into();
        if !valid_token(&verifier_version, MAX_KIND_BYTES) || resolver_generation == 0 {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "verified assertion requires a verifier version and positive resolver generation",
            ));
        }
        Self::new(
            namespace.into(),
            value.into(),
            AssertionTrust::SourceVerified {
                source_snapshot,
                verifier_version,
                resolver_generation,
            },
        )
    }

    fn new(namespace: String, value: String, trust: AssertionTrust) -> MergeResult<Self> {
        if !valid_token(&namespace, MAX_KIND_BYTES) || !valid_text(&value, MAX_LABEL_BYTES) {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "identifier namespace or value is invalid",
            ));
        }
        Ok(Self {
            namespace,
            value,
            trust,
        })
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn trust(&self) -> &AssertionTrust {
        &self.trust
    }
}

/// Immutable provenance contribution retained by a result object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct OriginContribution {
    origin: ObjectAddress,
    revision: RevisionId,
}

impl OriginContribution {
    #[must_use]
    pub const fn new(origin: ObjectAddress, revision: RevisionId) -> Self {
        Self { origin, revision }
    }

    #[must_use]
    pub fn origin(&self) -> &ObjectAddress {
        &self.origin
    }

    #[must_use]
    pub const fn revision(&self) -> RevisionId {
        self.revision
    }
}

/// Canonical entity projection plus identity context and provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntityRecord {
    address: EntityAddress,
    revision: RevisionId,
    lifecycle: Lifecycle,
    label: String,
    kind: Option<String>,
    lineage: Option<String>,
    aliases: BTreeSet<String>,
    identifiers: BTreeSet<IdentifierAssertion>,
    properties: BTreeMap<String, String>,
    origins: BTreeSet<OriginContribution>,
}

impl EntityRecord {
    pub fn new(
        address: EntityAddress,
        revision: RevisionId,
        label: impl Into<String>,
        kind: Option<impl Into<String>>,
    ) -> MergeResult<Self> {
        let label = label.into();
        let kind = kind.map(Into::into);
        if !valid_text(&label, MAX_LABEL_BYTES)
            || kind
                .as_deref()
                .is_some_and(|value| !valid_token(value, MAX_KIND_BYTES))
        {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "entity label or kind is invalid",
            ));
        }
        let origin = OriginContribution::new(address.clone(), revision);
        Ok(Self {
            address,
            revision,
            lifecycle: Lifecycle::Active,
            label,
            kind,
            lineage: None,
            aliases: BTreeSet::new(),
            identifiers: BTreeSet::new(),
            properties: BTreeMap::new(),
            origins: BTreeSet::from([origin]),
        })
    }

    pub fn with_lifecycle(mut self, lifecycle: Lifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    pub fn with_lineage(mut self, lineage: Option<impl Into<String>>) -> MergeResult<Self> {
        let lineage = lineage.map(Into::into);
        if lineage
            .as_deref()
            .is_some_and(|value| !valid_token(value, MAX_LOGICAL_ID_BYTES))
        {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "entity lineage is invalid",
            ));
        }
        self.lineage = lineage;
        Ok(self)
    }

    pub fn with_aliases(mut self, aliases: impl IntoIterator<Item = String>) -> MergeResult<Self> {
        let aliases = aliases.into_iter().collect::<BTreeSet<_>>();
        if aliases.len() > MAX_ALIASES
            || aliases
                .iter()
                .any(|alias| !valid_text(alias, MAX_LABEL_BYTES))
        {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "entity aliases exceed bounds or contain invalid text",
            ));
        }
        self.aliases = aliases;
        Ok(self)
    }

    pub fn with_identifiers(
        mut self,
        identifiers: impl IntoIterator<Item = IdentifierAssertion>,
    ) -> MergeResult<Self> {
        let identifiers = identifiers.into_iter().collect::<BTreeSet<_>>();
        if identifiers.len() > MAX_IDENTIFIERS {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "entity identifier count exceeds the hard limit",
            ));
        }
        self.identifiers = identifiers;
        Ok(self)
    }

    pub fn with_properties(
        mut self,
        properties: impl IntoIterator<Item = (String, String)>,
    ) -> MergeResult<Self> {
        let mut result = BTreeMap::new();
        for (name, value) in properties {
            if !valid_token(&name, MAX_PROPERTY_NAME_BYTES)
                || !valid_text(&value, MAX_TEXT_BYTES)
                || result.insert(name, value).is_some()
            {
                return Err(MergeError::new(
                    MergeErrorCode::InvalidInput,
                    "entity property is invalid or duplicated",
                ));
            }
        }
        if result.len() > MAX_PROPERTIES {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "entity property count exceeds the hard limit",
            ));
        }
        self.properties = result;
        Ok(self)
    }

    pub fn with_origins(
        mut self,
        origins: impl IntoIterator<Item = OriginContribution>,
    ) -> MergeResult<Self> {
        let origins = origins.into_iter().collect::<BTreeSet<_>>();
        if origins.is_empty() || origins.len() > MAX_ORIGINS {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "entity origin count must be bounded and non-empty",
            ));
        }
        self.origins = origins;
        Ok(self)
    }

    #[must_use]
    pub fn address(&self) -> &EntityAddress {
        &self.address
    }

    #[must_use]
    pub const fn revision(&self) -> RevisionId {
        self.revision
    }

    #[must_use]
    pub const fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    #[must_use]
    pub fn lineage(&self) -> Option<&str> {
        self.lineage.as_deref()
    }

    #[must_use]
    pub fn aliases(&self) -> &BTreeSet<String> {
        &self.aliases
    }

    #[must_use]
    pub fn identifiers(&self) -> &BTreeSet<IdentifierAssertion> {
        &self.identifiers
    }

    #[must_use]
    pub fn properties(&self) -> &BTreeMap<String, String> {
        &self.properties
    }

    #[must_use]
    pub fn origins(&self) -> &BTreeSet<OriginContribution> {
        &self.origins
    }

    #[must_use]
    pub fn semantic_hash(&self) -> SemanticHash {
        let mut hash = CanonicalHasher::new(b"openmemory/entity/v1");
        encode_entity(&mut hash, self);
        hash.finish_semantic()
    }
}

/// Canonical semantic observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservationRecord {
    address: ObjectAddress,
    entity: EntityAddress,
    revision: RevisionId,
    lifecycle: Lifecycle,
    content: String,
    origins: BTreeSet<OriginContribution>,
}

impl ObservationRecord {
    pub fn new(
        address: ObjectAddress,
        entity: EntityAddress,
        revision: RevisionId,
        content: impl Into<String>,
    ) -> MergeResult<Self> {
        let content = content.into();
        if !valid_text(&content, MAX_TEXT_BYTES) {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "observation content is empty, oversized, or contains controls",
            ));
        }
        let origin = OriginContribution::new(address.clone(), revision);
        Ok(Self {
            address,
            entity,
            revision,
            lifecycle: Lifecycle::Active,
            content,
            origins: BTreeSet::from([origin]),
        })
    }

    pub fn with_lifecycle(mut self, lifecycle: Lifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    pub fn with_origins(
        mut self,
        origins: impl IntoIterator<Item = OriginContribution>,
    ) -> MergeResult<Self> {
        let origins = origins.into_iter().collect::<BTreeSet<_>>();
        if origins.is_empty() || origins.len() > MAX_ORIGINS {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "observation origin count must be bounded and non-empty",
            ));
        }
        self.origins = origins;
        Ok(self)
    }

    #[must_use]
    pub fn address(&self) -> &ObjectAddress {
        &self.address
    }

    #[must_use]
    pub fn entity(&self) -> &EntityAddress {
        &self.entity
    }

    #[must_use]
    pub const fn revision(&self) -> RevisionId {
        self.revision
    }

    #[must_use]
    pub const fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }

    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    #[must_use]
    pub fn origins(&self) -> &BTreeSet<OriginContribution> {
        &self.origins
    }

    #[must_use]
    pub fn semantic_hash(&self) -> SemanticHash {
        let mut hash = CanonicalHasher::new(b"openmemory/observation/v1");
        encode_observation(&mut hash, self);
        hash.finish_semantic()
    }
}

/// Canonical directional relation assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationRecord {
    address: ObjectAddress,
    subject: EntityAddress,
    predicate: String,
    object: EntityAddress,
    revision: RevisionId,
    lifecycle: Lifecycle,
    origins: BTreeSet<OriginContribution>,
}

impl RelationRecord {
    pub fn new(
        address: ObjectAddress,
        subject: EntityAddress,
        predicate: impl Into<String>,
        object: EntityAddress,
        revision: RevisionId,
    ) -> MergeResult<Self> {
        let predicate = predicate.into();
        if !valid_token(&predicate, MAX_KIND_BYTES) {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "relation predicate is invalid",
            ));
        }
        let origin = OriginContribution::new(address.clone(), revision);
        Ok(Self {
            address,
            subject,
            predicate,
            object,
            revision,
            lifecycle: Lifecycle::Active,
            origins: BTreeSet::from([origin]),
        })
    }

    pub fn with_lifecycle(mut self, lifecycle: Lifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    pub fn with_origins(
        mut self,
        origins: impl IntoIterator<Item = OriginContribution>,
    ) -> MergeResult<Self> {
        let origins = origins.into_iter().collect::<BTreeSet<_>>();
        if origins.is_empty() || origins.len() > MAX_ORIGINS {
            return Err(MergeError::new(
                MergeErrorCode::BoundExceeded,
                "relation origin count must be bounded and non-empty",
            ));
        }
        self.origins = origins;
        Ok(self)
    }

    #[must_use]
    pub fn address(&self) -> &ObjectAddress {
        &self.address
    }

    #[must_use]
    pub fn subject(&self) -> &EntityAddress {
        &self.subject
    }

    #[must_use]
    pub fn predicate(&self) -> &str {
        &self.predicate
    }

    #[must_use]
    pub fn object(&self) -> &EntityAddress {
        &self.object
    }

    #[must_use]
    pub const fn revision(&self) -> RevisionId {
        self.revision
    }

    #[must_use]
    pub const fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }

    #[must_use]
    pub fn origins(&self) -> &BTreeSet<OriginContribution> {
        &self.origins
    }

    #[must_use]
    pub fn semantic_hash(&self) -> SemanticHash {
        let mut hash = CanonicalHasher::new(b"openmemory/relation/v1");
        encode_relation(&mut hash, self);
        hash.finish_semantic()
    }
}

/// Per-domain semantic and derived-state generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DomainGeneration {
    domain: u16,
    semantic: u64,
    indexed: u64,
    mirrored: u64,
}

impl DomainGeneration {
    #[must_use]
    pub const fn new(domain: u16, semantic: u64, indexed: u64, mirrored: u64) -> Self {
        Self {
            domain,
            semantic,
            indexed,
            mirrored,
        }
    }

    #[must_use]
    pub const fn domain(self) -> u16 {
        self.domain
    }

    #[must_use]
    pub const fn semantic(self) -> u64 {
        self.semantic
    }

    #[must_use]
    pub const fn indexed(self) -> u64 {
        self.indexed
    }

    #[must_use]
    pub const fn mirrored(self) -> u64 {
        self.mirrored
    }
}

/// Numeric-domain-ordered version vector plus canonical live semantic hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpaceVersion {
    domains: Vec<DomainGeneration>,
    combined_hash: SemanticHash,
}

impl SpaceVersion {
    pub fn new(domains: Vec<DomainGeneration>, combined_hash: SemanticHash) -> MergeResult<Self> {
        if domains.is_empty()
            || domains.len() > 64
            || domains
                .iter()
                .enumerate()
                .any(|(index, value)| usize::from(value.domain()) != index)
        {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                "space version domains must be non-empty, bounded, and numerically contiguous",
            ));
        }
        Ok(Self {
            domains,
            combined_hash,
        })
    }

    #[must_use]
    pub fn domains(&self) -> &[DomainGeneration] {
        &self.domains
    }

    #[must_use]
    pub const fn combined_hash(&self) -> SemanticHash {
        self.combined_hash
    }
}

/// Immutable validated snapshot header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotHeader {
    format_version: u16,
    snapshot_id: SnapshotId,
    space_id: SpaceId,
    space_version: SpaceVersion,
    entity_count: u64,
    observation_count: u64,
    relation_count: u64,
    hash: SnapshotHash,
}

impl SnapshotHeader {
    #[must_use]
    pub const fn format_version(&self) -> u16 {
        self.format_version
    }

    #[must_use]
    pub const fn snapshot_id(&self) -> SnapshotId {
        self.snapshot_id
    }

    #[must_use]
    pub const fn space_id(&self) -> SpaceId {
        self.space_id
    }

    #[must_use]
    pub fn space_version(&self) -> &SpaceVersion {
        &self.space_version
    }

    #[must_use]
    pub const fn entity_count(&self) -> u64 {
        self.entity_count
    }

    #[must_use]
    pub const fn observation_count(&self) -> u64 {
        self.observation_count
    }

    #[must_use]
    pub const fn relation_count(&self) -> u64 {
        self.relation_count
    }

    #[must_use]
    pub const fn hash(&self) -> SnapshotHash {
        self.hash
    }
}

/// Resettable semantic record streams. A production file reader can implement
/// this trait without retaining a graph; `MemorySnapshot` is the bounded test
/// and fixture adapter.
pub trait SnapshotSource {
    fn header(&self) -> &SnapshotHeader;

    fn visit_entities(
        &mut self,
        visitor: &mut dyn FnMut(EntityRecord) -> MergeResult<()>,
    ) -> MergeResult<()>;

    fn visit_observations(
        &mut self,
        visitor: &mut dyn FnMut(ObservationRecord) -> MergeResult<()>,
    ) -> MergeResult<()>;

    fn visit_relations(
        &mut self,
        visitor: &mut dyn FnMut(RelationRecord) -> MergeResult<()>,
    ) -> MergeResult<()>;
}

/// In-memory validated snapshot used by pure tests, permanent fixtures, and
/// callers that already own bounded canonical records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySnapshot {
    header: SnapshotHeader,
    entities: Vec<EntityRecord>,
    observations: Vec<ObservationRecord>,
    relations: Vec<RelationRecord>,
    canonical_bytes: usize,
}

impl MemorySnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        snapshot_id: SnapshotId,
        space_id: SpaceId,
        space_version: SpaceVersion,
        mut entities: Vec<EntityRecord>,
        mut observations: Vec<ObservationRecord>,
        mut relations: Vec<RelationRecord>,
    ) -> MergeResult<Self> {
        validate_count("entities", entities.len())?;
        validate_count("observations", observations.len())?;
        validate_count("relations", relations.len())?;

        entities.sort_by(|left, right| left.address().cmp(right.address()));
        observations.sort_by(|left, right| left.address().cmp(right.address()));
        relations.sort_by(|left, right| left.address().cmp(right.address()));

        validate_unique_addresses(
            "entity",
            entities.iter().map(EntityRecord::address),
            space_id,
        )?;
        validate_unique_addresses(
            "observation",
            observations.iter().map(ObservationRecord::address),
            space_id,
        )?;
        validate_unique_addresses(
            "relation",
            relations.iter().map(RelationRecord::address),
            space_id,
        )?;

        let entity_ids = entities
            .iter()
            .map(|entity| entity.address().clone())
            .collect::<BTreeSet<_>>();
        if observations
            .iter()
            .any(|observation| !entity_ids.contains(observation.entity()))
            || relations.iter().any(|relation| {
                !entity_ids.contains(relation.subject()) || !entity_ids.contains(relation.object())
            })
        {
            return Err(MergeError::new(
                MergeErrorCode::DanglingEndpoint,
                "snapshot observation or relation references an absent entity",
            ));
        }

        let (hash, canonical_bytes) = compute_snapshot_hash(
            snapshot_id,
            space_id,
            &space_version,
            &entities,
            &observations,
            &relations,
        );
        let header = SnapshotHeader {
            format_version: 1,
            snapshot_id,
            space_id,
            space_version,
            entity_count: u64::try_from(entities.len()).unwrap_or(u64::MAX),
            observation_count: u64::try_from(observations.len()).unwrap_or(u64::MAX),
            relation_count: u64::try_from(relations.len()).unwrap_or(u64::MAX),
            hash,
        };
        Ok(Self {
            header,
            entities,
            observations,
            relations,
            canonical_bytes,
        })
    }

    #[must_use]
    pub fn entities(&self) -> &[EntityRecord] {
        &self.entities
    }

    #[must_use]
    pub fn observations(&self) -> &[ObservationRecord] {
        &self.observations
    }

    #[must_use]
    pub fn relations(&self) -> &[RelationRecord] {
        &self.relations
    }

    #[must_use]
    pub const fn canonical_bytes(&self) -> usize {
        self.canonical_bytes
    }
}

impl SnapshotSource for MemorySnapshot {
    fn header(&self) -> &SnapshotHeader {
        &self.header
    }

    fn visit_entities(
        &mut self,
        visitor: &mut dyn FnMut(EntityRecord) -> MergeResult<()>,
    ) -> MergeResult<()> {
        for record in &self.entities {
            visitor(record.clone())?;
        }
        Ok(())
    }

    fn visit_observations(
        &mut self,
        visitor: &mut dyn FnMut(ObservationRecord) -> MergeResult<()>,
    ) -> MergeResult<()> {
        for record in &self.observations {
            visitor(record.clone())?;
        }
        Ok(())
    }

    fn visit_relations(
        &mut self,
        visitor: &mut dyn FnMut(RelationRecord) -> MergeResult<()>,
    ) -> MergeResult<()> {
        for record in &self.relations {
            visitor(record.clone())?;
        }
        Ok(())
    }
}

fn validate_count(name: &str, value: usize) -> MergeResult<()> {
    if value > MAX_SNAPSHOT_RECORDS {
        return Err(MergeError::new(
            MergeErrorCode::BoundExceeded,
            format!("{name} exceeds the snapshot hard limit"),
        ));
    }
    Ok(())
}

fn validate_unique_addresses<'a>(
    kind: &str,
    values: impl Iterator<Item = &'a ObjectAddress>,
    expected_space: SpaceId,
) -> MergeResult<()> {
    let mut previous: Option<&ObjectAddress> = None;
    for value in values {
        if value.space() != expected_space {
            return Err(MergeError::new(
                MergeErrorCode::InvalidInput,
                format!("{kind} belongs to another space"),
            ));
        }
        if previous.is_some_and(|prior| prior >= value) {
            return Err(MergeError::new(
                MergeErrorCode::DuplicateInput,
                format!("{kind} addresses must be unique"),
            ));
        }
        previous = Some(value);
    }
    Ok(())
}

pub(crate) fn compute_snapshot_hash(
    snapshot_id: SnapshotId,
    space_id: SpaceId,
    space_version: &SpaceVersion,
    entities: &[EntityRecord],
    observations: &[ObservationRecord],
    relations: &[RelationRecord],
) -> (SnapshotHash, usize) {
    let mut hash = CanonicalHasher::new(b"openmemory/space-snapshot/v1");
    hash.u16(1);
    hash.snapshot_id(snapshot_id);
    hash.space_id(space_id);
    encode_space_version(&mut hash, space_version);
    hash.usize(entities.len());
    for entity in entities {
        encode_entity(&mut hash, entity);
    }
    hash.usize(observations.len());
    for observation in observations {
        encode_observation(&mut hash, observation);
    }
    hash.usize(relations.len());
    for relation in relations {
        encode_relation(&mut hash, relation);
    }
    let canonical_bytes = hash.encoded_len();
    (hash.finish_snapshot(), canonical_bytes)
}

pub(crate) fn encode_address(hash: &mut CanonicalHasher, value: &ObjectAddress) {
    hash.space_id(value.space());
    hash.text(value.logical_id().as_str());
}

pub(crate) fn encode_result_id(hash: &mut CanonicalHasher, value: &ResultObjectId) {
    match value {
        ResultObjectId::Target(value) => {
            hash.u16(1);
            hash.text(value.as_str());
        }
        ResultObjectId::Qualified(value) => {
            hash.u16(2);
            hash.text(value.as_str());
        }
    }
}

pub(crate) fn encode_origin(hash: &mut CanonicalHasher, value: &OriginContribution) {
    encode_address(hash, value.origin());
    hash.revision_id(value.revision());
}

pub(crate) fn encode_trust(hash: &mut CanonicalHasher, value: &AssertionTrust) {
    match value {
        AssertionTrust::Claimed => hash.u16(1),
        AssertionTrust::SourceVerified {
            source_snapshot,
            verifier_version,
            resolver_generation,
        } => {
            hash.u16(2);
            hash.snapshot_id(*source_snapshot);
            hash.text(verifier_version);
            hash.u64(*resolver_generation);
        }
    }
}

pub(crate) fn encode_identifier(hash: &mut CanonicalHasher, value: &IdentifierAssertion) {
    hash.text(value.namespace());
    hash.text(value.value());
    encode_trust(hash, value.trust());
}

pub(crate) fn encode_entity(hash: &mut CanonicalHasher, value: &EntityRecord) {
    encode_address(hash, value.address());
    hash.revision_id(value.revision());
    hash.u16(value.lifecycle() as u16);
    hash.text(value.label());
    hash.optional_text(value.kind());
    hash.optional_text(value.lineage());
    hash.usize(value.aliases().len());
    for alias in value.aliases() {
        hash.text(alias);
    }
    hash.usize(value.identifiers().len());
    for identifier in value.identifiers() {
        encode_identifier(hash, identifier);
    }
    hash.usize(value.properties().len());
    for (name, property) in value.properties() {
        hash.text(name);
        hash.text(property);
    }
    hash.usize(value.origins().len());
    for origin in value.origins() {
        encode_origin(hash, origin);
    }
}

pub(crate) fn encode_observation(hash: &mut CanonicalHasher, value: &ObservationRecord) {
    encode_address(hash, value.address());
    encode_address(hash, value.entity());
    hash.revision_id(value.revision());
    hash.u16(value.lifecycle() as u16);
    hash.text(value.content());
    hash.usize(value.origins().len());
    for origin in value.origins() {
        encode_origin(hash, origin);
    }
}

pub(crate) fn encode_relation(hash: &mut CanonicalHasher, value: &RelationRecord) {
    encode_address(hash, value.address());
    encode_address(hash, value.subject());
    hash.text(value.predicate());
    encode_address(hash, value.object());
    hash.revision_id(value.revision());
    hash.u16(value.lifecycle() as u16);
    hash.usize(value.origins().len());
    for origin in value.origins() {
        encode_origin(hash, origin);
    }
}

pub(crate) fn encode_space_version(hash: &mut CanonicalHasher, value: &SpaceVersion) {
    hash.usize(value.domains().len());
    for domain in value.domains() {
        hash.u16(domain.domain());
        hash.u64(domain.semantic());
        hash.u64(domain.indexed());
        hash.u64(domain.mirrored());
    }
    hash.hash(value.combined_hash().as_bytes());
}

fn valid_token(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(char::is_control)
        && value.trim() == value
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(|character| character == '\0')
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: &str = "018f6b7a-4d3c-7abc-8def-000000000001";
    const SNAPSHOT: &str = "018f6b7a-4d3c-7abc-8def-000000000002";
    const REVISION: &str = "018f6b7a-4d3c-7abc-8def-000000000003";

    fn version() -> SpaceVersion {
        SpaceVersion::new(
            vec![DomainGeneration::new(0, 1, 1, 1)],
            SemanticHash::from_bytes([7; 32]),
        )
        .unwrap()
    }

    fn entity(id: &str) -> EntityRecord {
        EntityRecord::new(
            ObjectAddress::new(SPACE.parse().unwrap(), LogicalId::new(id).unwrap()),
            REVISION.parse().unwrap(),
            id,
            Some("concept"),
        )
        .unwrap()
    }

    #[test]
    fn qualified_ids_are_framed_reserved_and_deterministic() {
        let space: SpaceId = SPACE.parse().unwrap();
        let first = QualifiedObjectId::derive(space, &LogicalId::new("ab").unwrap());
        let second = QualifiedObjectId::derive(space, &LogicalId::new("a").unwrap());
        assert!(first.as_str().starts_with(QUALIFIED_ID_PREFIX));
        assert_ne!(first, second);
        assert!(LogicalId::new(first.as_str()).is_err());
    }

    #[test]
    fn snapshot_sorts_input_and_hash_is_order_independent() {
        let forward = MemorySnapshot::new(
            SNAPSHOT.parse().unwrap(),
            SPACE.parse().unwrap(),
            version(),
            vec![entity("a"), entity("b")],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let reverse = MemorySnapshot::new(
            SNAPSHOT.parse().unwrap(),
            SPACE.parse().unwrap(),
            version(),
            vec![entity("b"), entity("a")],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(forward.header(), reverse.header());
        assert_eq!(forward.entities()[0].address().logical_id().as_str(), "a");
    }

    #[test]
    fn snapshot_rejects_duplicate_and_dangling_records() {
        let duplicate = MemorySnapshot::new(
            SNAPSHOT.parse().unwrap(),
            SPACE.parse().unwrap(),
            version(),
            vec![entity("a"), entity("a")],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            duplicate.unwrap_err().code(),
            MergeErrorCode::DuplicateInput
        );

        let observation = ObservationRecord::new(
            ObjectAddress::new(
                SPACE.parse().unwrap(),
                LogicalId::new("observation").unwrap(),
            ),
            ObjectAddress::new(SPACE.parse().unwrap(), LogicalId::new("missing").unwrap()),
            REVISION.parse().unwrap(),
            "content",
        )
        .unwrap();
        let dangling = MemorySnapshot::new(
            SNAPSHOT.parse().unwrap(),
            SPACE.parse().unwrap(),
            version(),
            vec![entity("a")],
            vec![observation],
            Vec::new(),
        );
        assert_eq!(
            dangling.unwrap_err().code(),
            MergeErrorCode::DanglingEndpoint
        );
    }

    #[test]
    fn every_semantic_field_changes_the_entity_hash() {
        let base = entity("a");
        let changed = entity("a")
            .with_properties([("field".to_string(), "value".to_string())])
            .unwrap();
        assert_ne!(base.semantic_hash(), changed.semantic_hash());
    }
}
