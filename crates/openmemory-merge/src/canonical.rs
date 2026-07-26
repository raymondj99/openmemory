//! Bounded canonical semantic snapshot records.

use std::collections::{BTreeMap, BTreeSet};

use openmemory_core::space::{RevisionId, SpaceId};
use serde::{Deserialize, Serialize};

use crate::error::{MergeError, MergeResult};
use crate::hash::{CanonicalHasher, SemanticHash, SnapshotHash};

pub const CANONICAL_FORMAT_VERSION: u32 = 1;
pub const MAX_ENTITIES: usize = 100_000;
pub const MAX_OBSERVATIONS: usize = 1_000_000;
pub const MAX_RELATIONS: usize = 1_000_000;
pub const MAX_LOGICAL_ID_BYTES: usize = 256;
pub const MAX_LABEL_BYTES: usize = 4 * 1024;
pub const MAX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SET_ITEMS: usize = 1_024;

fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

/// Logical IDs are wire identifiers, never filesystem components.
#[must_use]
pub fn valid_logical_id(value: &str) -> bool {
    bounded(value, MAX_LOGICAL_ID_BYTES)
        && value != "."
        && value != ".."
        && !value.contains(['/', '\\'])
}

fn validate_string_set(
    values: &BTreeSet<String>,
    field: &'static str,
    max_item_bytes: usize,
) -> MergeResult<()> {
    if values.len() > MAX_SET_ITEMS || values.iter().any(|value| !bounded(value, max_item_bytes)) {
        return Err(MergeError::InvalidField { field });
    }
    Ok(())
}

/// Trust attached to an external identifier assertion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "trust")]
pub enum AssertionTrust {
    Claimed,
    SourceVerified {
        source_snapshot_id: String,
        verifier_version: String,
        resolver_generation: u64,
    },
}

/// Namespace-qualified entity identifier retained in canonical history.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentifierAssertion {
    pub namespace: String,
    pub raw_value: String,
    pub canonical_value: Option<String>,
    pub trust: AssertionTrust,
}

impl IdentifierAssertion {
    pub fn validate(&self) -> MergeResult<()> {
        if !bounded(&self.namespace, 128)
            || !bounded(&self.raw_value, 1_024)
            || self
                .canonical_value
                .as_deref()
                .is_some_and(|value| !bounded(value, 1_024))
        {
            return Err(MergeError::InvalidField {
                field: "identifier",
            });
        }
        if let AssertionTrust::SourceVerified {
            source_snapshot_id,
            verifier_version,
            resolver_generation,
        } = &self.trust
        {
            if !bounded(source_snapshot_id, 512)
                || !bounded(verifier_version, 128)
                || *resolver_generation == 0
                || self.canonical_value.is_none()
            {
                return Err(MergeError::InvalidField {
                    field: "verified_identifier",
                });
            }
        }
        Ok(())
    }
}

/// Stable origin key for one immutable contribution.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginKey {
    pub space_id: SpaceId,
    pub logical_id: String,
    pub revision_id: RevisionId,
}

impl OriginKey {
    pub fn validate(&self) -> MergeResult<()> {
        if !valid_logical_id(&self.logical_id) {
            return Err(MergeError::InvalidField {
                field: "origin_logical_id",
            });
        }
        Ok(())
    }
}

/// Full immutable source projection retained when identities coalesce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityContribution {
    pub origin: OriginKey,
    pub label: String,
    pub aliases: BTreeSet<String>,
    pub controlled_kind: String,
    pub identifiers: BTreeSet<IdentifierAssertion>,
    pub description: String,
    pub confidence_bits: u32,
    pub source: String,
    pub semantic_hash: SemanticHash,
}

impl EntityContribution {
    fn validate(&self) -> MergeResult<()> {
        self.origin.validate()?;
        validate_entity_fields(
            &self.label,
            &self.aliases,
            &self.controlled_kind,
            &self.identifiers,
            &self.description,
            self.confidence_bits,
            &self.source,
        )?;
        let expected = hash_entity_fields(
            &self.label,
            &self.aliases,
            &self.controlled_kind,
            &self.identifiers,
            &self.description,
            self.confidence_bits,
            &self.source,
        );
        if expected != self.semantic_hash {
            return Err(MergeError::SemanticHashMismatch);
        }
        Ok(())
    }
}

/// Current entity projection plus all immutable origin contributions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalEntity {
    pub logical_id: String,
    pub revision_id: RevisionId,
    pub label: String,
    pub aliases: BTreeSet<String>,
    pub controlled_kind: String,
    pub identifiers: BTreeSet<IdentifierAssertion>,
    pub description: String,
    pub confidence_bits: u32,
    pub source: String,
    pub semantic_hash: SemanticHash,
    pub contributions: BTreeMap<OriginKey, EntityContribution>,
}

impl CanonicalEntity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        space_id: SpaceId,
        logical_id: String,
        revision_id: RevisionId,
        label: String,
        aliases: BTreeSet<String>,
        controlled_kind: String,
        identifiers: BTreeSet<IdentifierAssertion>,
        description: String,
    ) -> MergeResult<Self> {
        Self::new_complete(
            space_id,
            logical_id,
            revision_id,
            label,
            aliases,
            controlled_kind,
            identifiers,
            description,
            1.0_f32.to_bits(),
            String::new(),
        )
    }

    /// Build a complete current graph projection.
    #[allow(clippy::too_many_arguments)]
    pub fn new_complete(
        space_id: SpaceId,
        logical_id: String,
        revision_id: RevisionId,
        label: String,
        aliases: BTreeSet<String>,
        controlled_kind: String,
        identifiers: BTreeSet<IdentifierAssertion>,
        description: String,
        confidence_bits: u32,
        source: String,
    ) -> MergeResult<Self> {
        if !valid_logical_id(&logical_id) {
            return Err(MergeError::InvalidField {
                field: "entity_logical_id",
            });
        }
        validate_entity_fields(
            &label,
            &aliases,
            &controlled_kind,
            &identifiers,
            &description,
            confidence_bits,
            &source,
        )?;
        let semantic_hash = hash_entity_fields(
            &label,
            &aliases,
            &controlled_kind,
            &identifiers,
            &description,
            confidence_bits,
            &source,
        );
        let origin = OriginKey {
            space_id,
            logical_id: logical_id.clone(),
            revision_id,
        };
        let contribution = EntityContribution {
            origin: origin.clone(),
            label: label.clone(),
            aliases: aliases.clone(),
            controlled_kind: controlled_kind.clone(),
            identifiers: identifiers.clone(),
            description: description.clone(),
            confidence_bits,
            source: source.clone(),
            semantic_hash,
        };
        Ok(Self {
            logical_id,
            revision_id,
            label,
            aliases,
            controlled_kind,
            identifiers,
            description,
            confidence_bits,
            source,
            semantic_hash,
            contributions: BTreeMap::from([(origin, contribution)]),
        })
    }

    pub fn validate(&self) -> MergeResult<()> {
        if !valid_logical_id(&self.logical_id) {
            return Err(MergeError::InvalidField {
                field: "entity_logical_id",
            });
        }
        validate_entity_fields(
            &self.label,
            &self.aliases,
            &self.controlled_kind,
            &self.identifiers,
            &self.description,
            self.confidence_bits,
            &self.source,
        )?;
        if hash_entity_fields(
            &self.label,
            &self.aliases,
            &self.controlled_kind,
            &self.identifiers,
            &self.description,
            self.confidence_bits,
            &self.source,
        ) != self.semantic_hash
        {
            return Err(MergeError::SemanticHashMismatch);
        }
        if self.contributions.is_empty() || self.contributions.len() > MAX_SET_ITEMS {
            return Err(MergeError::InvalidField {
                field: "entity_contributions",
            });
        }
        for (key, contribution) in &self.contributions {
            contribution.validate()?;
            if key != &contribution.origin {
                return Err(MergeError::InvalidField {
                    field: "entity_contribution_key",
                });
            }
        }
        Ok(())
    }

    /// Add the current projection as a contribution owned by the result
    /// space. Materialized copies retain their source origins and also gain
    /// this exact local revision-bound origin.
    pub fn add_local_contribution(&mut self, space_id: SpaceId) -> MergeResult<()> {
        let origin = OriginKey {
            space_id,
            logical_id: self.logical_id.clone(),
            revision_id: self.revision_id,
        };
        let contribution = EntityContribution {
            origin: origin.clone(),
            label: self.label.clone(),
            aliases: self.aliases.clone(),
            controlled_kind: self.controlled_kind.clone(),
            identifiers: self.identifiers.clone(),
            description: self.description.clone(),
            confidence_bits: self.confidence_bits,
            source: self.source.clone(),
            semantic_hash: self.semantic_hash,
        };
        contribution.validate()?;
        self.contributions.entry(origin).or_insert(contribution);
        Ok(())
    }
}

fn validate_entity_fields(
    label: &str,
    aliases: &BTreeSet<String>,
    controlled_kind: &str,
    identifiers: &BTreeSet<IdentifierAssertion>,
    description: &str,
    confidence_bits: u32,
    source: &str,
) -> MergeResult<()> {
    let confidence = f32::from_bits(confidence_bits);
    if !bounded(label, MAX_LABEL_BYTES)
        || !bounded(controlled_kind, 128)
        || description.len() > MAX_CONTENT_BYTES
        || description.chars().any(char::is_control)
        || !confidence.is_finite()
        || !(0.0..=1.0).contains(&confidence)
        || source.len() > 1_024
        || source.chars().any(char::is_control)
        || identifiers.len() > MAX_SET_ITEMS
    {
        return Err(MergeError::InvalidField { field: "entity" });
    }
    validate_string_set(aliases, "aliases", MAX_LABEL_BYTES)?;
    for identifier in identifiers {
        identifier.validate()?;
    }
    Ok(())
}

fn hash_assertion(hasher: &mut CanonicalHasher, assertion: &IdentifierAssertion) {
    hasher.string(&assertion.namespace);
    hasher.string(&assertion.raw_value);
    hasher.bool(assertion.canonical_value.is_some());
    if let Some(value) = &assertion.canonical_value {
        hasher.string(value);
    }
    match &assertion.trust {
        AssertionTrust::Claimed => hasher.tag("claimed"),
        AssertionTrust::SourceVerified {
            source_snapshot_id,
            verifier_version,
            resolver_generation,
        } => {
            hasher.tag("source_verified");
            hasher.string(source_snapshot_id);
            hasher.string(verifier_version);
            hasher.u64(*resolver_generation);
        }
    }
}

fn hash_entity_fields(
    label: &str,
    aliases: &BTreeSet<String>,
    controlled_kind: &str,
    identifiers: &BTreeSet<IdentifierAssertion>,
    description: &str,
    confidence_bits: u32,
    source: &str,
) -> SemanticHash {
    let mut hasher = CanonicalHasher::new(b"openmemory/entity/v1");
    hasher.string(label);
    hasher.u64(aliases.len() as u64);
    for alias in aliases {
        hasher.string(alias);
    }
    hasher.string(controlled_kind);
    hasher.u64(identifiers.len() as u64);
    for identifier in identifiers {
        hash_assertion(&mut hasher, identifier);
    }
    hasher.string(description);
    hasher.u32(confidence_bits);
    hasher.string(source);
    SemanticHash::from_bytes(hasher.finish())
}

/// Semantic lifecycle of a canonical object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Active,
    Retired,
}

/// Current observation/claim projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalObservation {
    pub logical_id: String,
    pub entity_id: String,
    pub revision_id: RevisionId,
    pub content: String,
    pub observed_at: i64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub confidence_bits: u32,
    pub concepts: BTreeSet<String>,
    pub source_files: BTreeSet<String>,
    pub source: String,
    pub memory_tier: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub importance_bits: Option<u32>,
    pub source_kind: Option<String>,
    pub lifecycle: Lifecycle,
    pub semantic_hash: SemanticHash,
    pub origins: BTreeMap<OriginKey, SemanticHash>,
}

impl CanonicalObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        space_id: SpaceId,
        logical_id: String,
        entity_id: String,
        revision_id: RevisionId,
        content: String,
        concepts: BTreeSet<String>,
        source: String,
        lifecycle: Lifecycle,
    ) -> MergeResult<Self> {
        Self::new_complete(
            space_id,
            logical_id,
            entity_id,
            revision_id,
            content,
            0,
            None,
            None,
            1.0_f32.to_bits(),
            concepts,
            BTreeSet::new(),
            source,
            "episodic".to_string(),
            None,
            None,
            None,
            None,
            lifecycle,
        )
    }

    /// Build a complete observation projection, including all v2 semantic
    /// fields and excluding only operational access counters.
    #[allow(clippy::too_many_arguments)]
    pub fn new_complete(
        space_id: SpaceId,
        logical_id: String,
        entity_id: String,
        revision_id: RevisionId,
        content: String,
        observed_at: i64,
        valid_from: Option<i64>,
        valid_until: Option<i64>,
        confidence_bits: u32,
        concepts: BTreeSet<String>,
        source_files: BTreeSet<String>,
        source: String,
        memory_tier: String,
        title: Option<String>,
        summary: Option<String>,
        importance_bits: Option<u32>,
        source_kind: Option<String>,
        lifecycle: Lifecycle,
    ) -> MergeResult<Self> {
        validate_observation_fields(
            &logical_id,
            &entity_id,
            &content,
            observed_at,
            valid_from,
            valid_until,
            confidence_bits,
            &concepts,
            &source_files,
            &source,
            &memory_tier,
            title.as_deref(),
            summary.as_deref(),
            importance_bits,
            source_kind.as_deref(),
        )?;
        let semantic_hash = hash_observation_fields(
            &entity_id,
            &content,
            observed_at,
            valid_from,
            valid_until,
            confidence_bits,
            &concepts,
            &source_files,
            &source,
            &memory_tier,
            title.as_deref(),
            summary.as_deref(),
            importance_bits,
            source_kind.as_deref(),
            lifecycle,
        );
        let origin = OriginKey {
            space_id,
            logical_id: logical_id.clone(),
            revision_id,
        };
        Ok(Self {
            logical_id,
            entity_id,
            revision_id,
            content,
            observed_at,
            valid_from,
            valid_until,
            confidence_bits,
            concepts,
            source_files,
            source,
            memory_tier,
            title,
            summary,
            importance_bits,
            source_kind,
            lifecycle,
            semantic_hash,
            origins: BTreeMap::from([(origin, semantic_hash)]),
        })
    }

    pub fn validate(&self) -> MergeResult<()> {
        validate_observation_fields(
            &self.logical_id,
            &self.entity_id,
            &self.content,
            self.observed_at,
            self.valid_from,
            self.valid_until,
            self.confidence_bits,
            &self.concepts,
            &self.source_files,
            &self.source,
            &self.memory_tier,
            self.title.as_deref(),
            self.summary.as_deref(),
            self.importance_bits,
            self.source_kind.as_deref(),
        )?;
        if hash_observation_fields(
            &self.entity_id,
            &self.content,
            self.observed_at,
            self.valid_from,
            self.valid_until,
            self.confidence_bits,
            &self.concepts,
            &self.source_files,
            &self.source,
            &self.memory_tier,
            self.title.as_deref(),
            self.summary.as_deref(),
            self.importance_bits,
            self.source_kind.as_deref(),
            self.lifecycle,
        ) != self.semantic_hash
            || self.origins.is_empty()
            || self.origins.len() > MAX_SET_ITEMS
        {
            return Err(MergeError::SemanticHashMismatch);
        }
        for origin in self.origins.keys() {
            origin.validate()?;
        }
        Ok(())
    }
}

fn validate_observation_fields(
    logical_id: &str,
    entity_id: &str,
    content: &str,
    observed_at: i64,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    confidence_bits: u32,
    concepts: &BTreeSet<String>,
    source_files: &BTreeSet<String>,
    source: &str,
    memory_tier: &str,
    title: Option<&str>,
    summary: Option<&str>,
    importance_bits: Option<u32>,
    source_kind: Option<&str>,
) -> MergeResult<()> {
    let confidence = f32::from_bits(confidence_bits);
    let importance = importance_bits.map(f32::from_bits);
    if !valid_logical_id(logical_id)
        || !valid_logical_id(entity_id)
        || !bounded(content, MAX_CONTENT_BYTES)
        || valid_from
            .zip(valid_until)
            .is_some_and(|(start, end)| start > end)
        || !confidence.is_finite()
        || !(0.0..=1.0).contains(&confidence)
        || source.len() > 1_024
        || source.chars().any(char::is_control)
        || !bounded(memory_tier, 128)
        || title.is_some_and(|value| invalid_optional(value, MAX_LABEL_BYTES))
        || summary.is_some_and(|value| invalid_optional(value, 64 * 1024))
        || source_kind.is_some_and(|value| invalid_optional(value, 256))
        || importance.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return Err(MergeError::InvalidField {
            field: "observation",
        });
    }
    let _ = observed_at;
    validate_string_set(concepts, "concepts", MAX_LABEL_BYTES)?;
    validate_string_set(source_files, "source_files", 16 * 1024)
}

fn invalid_optional(value: &str, maximum: usize) -> bool {
    value.len() > maximum || value.chars().any(char::is_control)
}

fn hash_observation_fields(
    entity_id: &str,
    content: &str,
    observed_at: i64,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    confidence_bits: u32,
    concepts: &BTreeSet<String>,
    source_files: &BTreeSet<String>,
    source: &str,
    memory_tier: &str,
    title: Option<&str>,
    summary: Option<&str>,
    importance_bits: Option<u32>,
    source_kind: Option<&str>,
    lifecycle: Lifecycle,
) -> SemanticHash {
    let mut hasher = CanonicalHasher::new(b"openmemory/observation/v1");
    hasher.string(entity_id);
    hasher.string(content);
    hasher.u64(observed_at as u64);
    hash_optional_i64(&mut hasher, valid_from);
    hash_optional_i64(&mut hasher, valid_until);
    hasher.u32(confidence_bits);
    hasher.u64(concepts.len() as u64);
    for concept in concepts {
        hasher.string(concept);
    }
    hasher.u64(source_files.len() as u64);
    for source_file in source_files {
        hasher.string(source_file);
    }
    hasher.string(source);
    hasher.string(memory_tier);
    hash_optional_string(&mut hasher, title);
    hash_optional_string(&mut hasher, summary);
    hasher.bool(importance_bits.is_some());
    if let Some(bits) = importance_bits {
        hasher.u32(bits);
    }
    hash_optional_string(&mut hasher, source_kind);
    hasher.tag(match lifecycle {
        Lifecycle::Active => "active",
        Lifecycle::Retired => "retired",
    });
    SemanticHash::from_bytes(hasher.finish())
}

fn hash_optional_i64(hasher: &mut CanonicalHasher, value: Option<i64>) {
    hasher.bool(value.is_some());
    if let Some(value) = value {
        hasher.u64(value as u64);
    }
}

fn hash_optional_string(hasher: &mut CanonicalHasher, value: Option<&str>) {
    hasher.bool(value.is_some());
    if let Some(value) = value {
        hasher.string(value);
    }
}

/// Current canonical relation assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRelation {
    pub logical_id: String,
    pub revision_id: RevisionId,
    pub from_entity: String,
    pub to_entity: String,
    pub relation_type: String,
    pub weight: f64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub source: String,
    pub evidence: BTreeSet<String>,
    pub lifecycle: Lifecycle,
    pub semantic_hash: SemanticHash,
    pub origins: BTreeMap<OriginKey, SemanticHash>,
}

impl CanonicalRelation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        space_id: SpaceId,
        logical_id: String,
        revision_id: RevisionId,
        from_entity: String,
        to_entity: String,
        relation_type: String,
        weight: f64,
        source: String,
        evidence: BTreeSet<String>,
        lifecycle: Lifecycle,
    ) -> MergeResult<Self> {
        Self::new_complete(
            space_id,
            logical_id,
            revision_id,
            from_entity,
            to_entity,
            relation_type,
            weight,
            None,
            None,
            source,
            evidence,
            lifecycle,
        )
    }

    /// Build a complete canonical relation assertion.
    #[allow(clippy::too_many_arguments)]
    pub fn new_complete(
        space_id: SpaceId,
        logical_id: String,
        revision_id: RevisionId,
        from_entity: String,
        to_entity: String,
        relation_type: String,
        weight: f64,
        valid_from: Option<i64>,
        valid_until: Option<i64>,
        source: String,
        evidence: BTreeSet<String>,
        lifecycle: Lifecycle,
    ) -> MergeResult<Self> {
        validate_relation_fields(
            &logical_id,
            &from_entity,
            &to_entity,
            &relation_type,
            weight,
            valid_from,
            valid_until,
            &source,
            &evidence,
        )?;
        let semantic_hash = hash_relation_fields(
            &from_entity,
            &to_entity,
            &relation_type,
            weight,
            valid_from,
            valid_until,
            &source,
            &evidence,
            lifecycle,
        );
        let origin = OriginKey {
            space_id,
            logical_id: logical_id.clone(),
            revision_id,
        };
        Ok(Self {
            logical_id,
            revision_id,
            from_entity,
            to_entity,
            relation_type,
            weight,
            valid_from,
            valid_until,
            source,
            evidence,
            lifecycle,
            semantic_hash,
            origins: BTreeMap::from([(origin, semantic_hash)]),
        })
    }

    pub fn validate(&self) -> MergeResult<()> {
        validate_relation_fields(
            &self.logical_id,
            &self.from_entity,
            &self.to_entity,
            &self.relation_type,
            self.weight,
            self.valid_from,
            self.valid_until,
            &self.source,
            &self.evidence,
        )?;
        if hash_relation_fields(
            &self.from_entity,
            &self.to_entity,
            &self.relation_type,
            self.weight,
            self.valid_from,
            self.valid_until,
            &self.source,
            &self.evidence,
            self.lifecycle,
        ) != self.semantic_hash
            || self.origins.is_empty()
            || self.origins.len() > MAX_SET_ITEMS
        {
            return Err(MergeError::SemanticHashMismatch);
        }
        for origin in self.origins.keys() {
            origin.validate()?;
        }
        Ok(())
    }
}

fn validate_relation_fields(
    logical_id: &str,
    from_entity: &str,
    to_entity: &str,
    relation_type: &str,
    weight: f64,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    source: &str,
    evidence: &BTreeSet<String>,
) -> MergeResult<()> {
    if !valid_logical_id(logical_id)
        || !valid_logical_id(from_entity)
        || !valid_logical_id(to_entity)
        || !bounded(relation_type, 128)
        || !weight.is_finite()
        || valid_from
            .zip(valid_until)
            .is_some_and(|(start, end)| start > end)
        || source.len() > 1_024
        || source.chars().any(char::is_control)
    {
        return Err(MergeError::InvalidField { field: "relation" });
    }
    validate_string_set(evidence, "relation_evidence", 2_048)
}

fn hash_relation_fields(
    from_entity: &str,
    to_entity: &str,
    relation_type: &str,
    weight: f64,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    source: &str,
    evidence: &BTreeSet<String>,
    lifecycle: Lifecycle,
) -> SemanticHash {
    let mut hasher = CanonicalHasher::new(b"openmemory/relation/v1");
    hasher.string(from_entity);
    hasher.string(to_entity);
    hasher.string(relation_type);
    hasher.u64(weight.to_bits());
    hash_optional_i64(&mut hasher, valid_from);
    hash_optional_i64(&mut hasher, valid_until);
    hasher.string(source);
    hasher.u64(evidence.len() as u64);
    for item in evidence {
        hasher.string(item);
    }
    hasher.tag(match lifecycle {
        Lifecycle::Active => "active",
        Lifecycle::Retired => "retired",
    });
    SemanticHash::from_bytes(hasher.finish())
}

/// Immutable, sorted semantic snapshot for one complete memory space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSpaceSnapshot {
    pub format_version: u32,
    pub space_id: SpaceId,
    pub semantic_generation: u64,
    pub entities: Vec<CanonicalEntity>,
    pub observations: Vec<CanonicalObservation>,
    pub relations: Vec<CanonicalRelation>,
    pub snapshot_hash: SnapshotHash,
}

impl CanonicalSpaceSnapshot {
    pub fn new(
        space_id: SpaceId,
        semantic_generation: u64,
        mut entities: Vec<CanonicalEntity>,
        mut observations: Vec<CanonicalObservation>,
        mut relations: Vec<CanonicalRelation>,
    ) -> MergeResult<Self> {
        entities.sort_by(|left, right| left.logical_id.cmp(&right.logical_id));
        observations.sort_by(|left, right| left.logical_id.cmp(&right.logical_id));
        relations.sort_by(|left, right| left.logical_id.cmp(&right.logical_id));
        let mut snapshot = Self {
            format_version: CANONICAL_FORMAT_VERSION,
            space_id,
            semantic_generation,
            entities,
            observations,
            relations,
            snapshot_hash: SnapshotHash::from_bytes([0; 32]),
        };
        snapshot.validate_records()?;
        snapshot.snapshot_hash = snapshot.recompute_hash();
        Ok(snapshot)
    }

    /// Revalidate bounds, ordering, hashes, and endpoints.
    pub fn validate(&self) -> MergeResult<()> {
        if self.format_version != CANONICAL_FORMAT_VERSION {
            return Err(MergeError::UnsupportedVersion(self.format_version));
        }
        self.validate_records()?;
        if self.recompute_hash() != self.snapshot_hash {
            return Err(MergeError::SnapshotHashMismatch);
        }
        Ok(())
    }

    fn validate_records(&self) -> MergeResult<()> {
        if self.entities.len() > MAX_ENTITIES
            || self.observations.len() > MAX_OBSERVATIONS
            || self.relations.len() > MAX_RELATIONS
        {
            return Err(MergeError::InvalidField {
                field: "snapshot_size",
            });
        }
        ensure_unique_sorted(
            self.entities
                .iter()
                .map(|entity| entity.logical_id.as_str()),
            "entity",
        )?;
        ensure_unique_sorted(
            self.observations
                .iter()
                .map(|observation| observation.logical_id.as_str()),
            "observation",
        )?;
        ensure_unique_sorted(
            self.relations
                .iter()
                .map(|relation| relation.logical_id.as_str()),
            "relation",
        )?;
        let entity_ids = self
            .entities
            .iter()
            .map(|entity| entity.logical_id.as_str())
            .collect::<BTreeSet<_>>();
        for entity in &self.entities {
            entity.validate()?;
        }
        for observation in &self.observations {
            observation.validate()?;
            if !entity_ids.contains(observation.entity_id.as_str()) {
                return Err(MergeError::DanglingRelationEndpoint);
            }
        }
        for relation in &self.relations {
            relation.validate()?;
            if !entity_ids.contains(relation.from_entity.as_str())
                || !entity_ids.contains(relation.to_entity.as_str())
            {
                return Err(MergeError::DanglingRelationEndpoint);
            }
        }
        Ok(())
    }

    /// Recompute semantic content hash, excluding operational generation.
    #[must_use]
    pub fn recompute_hash(&self) -> SnapshotHash {
        let mut hasher = CanonicalHasher::new(b"openmemory/space-snapshot/v1");
        hasher.u32(self.format_version);
        hasher.string(&self.space_id.to_string());
        hasher.u64(self.entities.len() as u64);
        for entity in &self.entities {
            hasher.string(&entity.logical_id);
            hasher.string(&entity.revision_id.to_string());
            hasher.bytes(entity.semantic_hash.as_bytes());
            hasher.u64(entity.contributions.len() as u64);
            for (origin, contribution) in &entity.contributions {
                hash_origin(&mut hasher, origin);
                hasher.bytes(contribution.semantic_hash.as_bytes());
            }
        }
        hasher.u64(self.observations.len() as u64);
        for observation in &self.observations {
            hasher.string(&observation.logical_id);
            hasher.string(&observation.revision_id.to_string());
            hasher.bytes(observation.semantic_hash.as_bytes());
            hash_origins(&mut hasher, &observation.origins);
        }
        hasher.u64(self.relations.len() as u64);
        for relation in &self.relations {
            hasher.string(&relation.logical_id);
            hasher.string(&relation.revision_id.to_string());
            hasher.bytes(relation.semantic_hash.as_bytes());
            hash_origins(&mut hasher, &relation.origins);
        }
        SnapshotHash::from_bytes(hasher.finish())
    }

    #[must_use]
    pub fn entity(&self, logical_id: &str) -> Option<&CanonicalEntity> {
        self.entities
            .binary_search_by_key(&logical_id, |entity| entity.logical_id.as_str())
            .ok()
            .map(|index| &self.entities[index])
    }
}

fn hash_origin(hasher: &mut CanonicalHasher, origin: &OriginKey) {
    hasher.string(&origin.space_id.to_string());
    hasher.string(&origin.logical_id);
    hasher.string(&origin.revision_id.to_string());
}

fn hash_origins(hasher: &mut CanonicalHasher, origins: &BTreeMap<OriginKey, SemanticHash>) {
    hasher.u64(origins.len() as u64);
    for (origin, hash) in origins {
        hash_origin(hasher, origin);
        hasher.bytes(hash.as_bytes());
    }
}

fn ensure_unique_sorted<'a>(
    values: impl IntoIterator<Item = &'a str>,
    kind: &'static str,
) -> MergeResult<()> {
    let mut previous = None;
    for value in values {
        if previous.is_some_and(|prior| prior >= value) {
            return Err(MergeError::Duplicate { kind });
        }
        previous = Some(value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(space: SpaceId, id: &str) -> CanonicalEntity {
        CanonicalEntity::new(
            space,
            id.to_string(),
            RevisionId::new(),
            id.to_string(),
            BTreeSet::new(),
            "concept".to_string(),
            BTreeSet::new(),
            String::new(),
        )
        .unwrap()
    }

    #[test]
    fn snapshot_order_is_canonical_and_hash_is_order_independent() {
        let space = SpaceId::new();
        let a = entity(space, "a");
        let b = entity(space, "b");
        let forward =
            CanonicalSpaceSnapshot::new(space, 1, vec![a.clone(), b.clone()], vec![], vec![])
                .unwrap();
        let reverse = CanonicalSpaceSnapshot::new(space, 99, vec![b, a], vec![], vec![]).unwrap();
        assert_eq!(forward.snapshot_hash, reverse.snapshot_hash);
        assert_eq!(forward.entities[0].logical_id, "a");
    }

    #[test]
    fn rejects_dangling_endpoints_nonfinite_weight_and_tampering() {
        let space = SpaceId::new();
        assert!(CanonicalRelation::new(
            space,
            "r".to_string(),
            RevisionId::new(),
            "a".to_string(),
            "b".to_string(),
            "uses".to_string(),
            f64::NAN,
            "test".to_string(),
            BTreeSet::new(),
            Lifecycle::Active,
        )
        .is_err());
        let relation = CanonicalRelation::new(
            space,
            "r".to_string(),
            RevisionId::new(),
            "a".to_string(),
            "missing".to_string(),
            "uses".to_string(),
            1.0,
            "test".to_string(),
            BTreeSet::new(),
            Lifecycle::Active,
        )
        .unwrap();
        assert_eq!(
            CanonicalSpaceSnapshot::new(space, 1, vec![entity(space, "a")], vec![], vec![relation])
                .unwrap_err(),
            MergeError::DanglingRelationEndpoint
        );
        let mut snapshot =
            CanonicalSpaceSnapshot::new(space, 1, vec![entity(space, "a")], vec![], vec![])
                .unwrap();
        snapshot.entities[0].label = "tampered".to_string();
        assert_eq!(snapshot.validate(), Err(MergeError::SemanticHashMismatch));
    }
}
