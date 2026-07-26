//! Proof-gated entity identity resolution with an untrusted agent boundary.
//!
//! Labels and semantic similarity discover candidates. Only shared lineage,
//! configured authoritative identifiers, or an audited human decision can
//! establish identity. Agent output is a revision-bound proposal and never a
//! canonical graph mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{PocError, PocResult};

const DB_FILE: &str = "identity.sqlite";
const MAX_LABEL_BYTES: usize = 512;
const MAX_ALIASES: usize = 64;
const MAX_IDENTIFIER_ASSERTIONS: usize = 32;
const MAX_RELATION_ASSERTIONS: usize = 64;
const MAX_SOURCE_REFS: usize = 64;
const MAX_NEIGHBORS: usize = 1_024;
const MAX_DESCRIPTION_BYTES: usize = 64 * 1_024;
const MAX_PROPOSAL_EVIDENCE: usize = 128;
const MAX_RELATION_SUGGESTIONS: usize = 32;
const MAX_RATIONALE_BYTES: usize = 4 * 1_024;

/// An entity address is qualified by its semantic space.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityId {
    pub space_id: String,
    pub logical_id: String,
}

impl EntityId {
    #[must_use]
    pub fn new(space_id: impl Into<String>, logical_id: impl Into<String>) -> Self {
        Self {
            space_id: space_id.into(),
            logical_id: logical_id.into(),
        }
    }

    fn storage_key(&self) -> String {
        format!("{}\u{1f}{}", self.space_id, self.logical_id)
    }
}

/// The revision-bound semantic context made available to identity resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRecord {
    pub id: EntityId,
    pub revision_id: String,
    pub lineage_id: Option<String>,
    pub label: String,
    /// Alternate surface forms used for candidate discovery, never proof.
    pub aliases: BTreeSet<String>,
    /// A controlled, coarse kind such as `service`, `dataset`, or `person`.
    pub kind: Option<String>,
    /// Namespace-qualified identifier assertions. Only source-verified values
    /// in a configured unique namespace can become identity proof.
    pub identifier_assertions: BTreeMap<String, IdentifierAssertion>,
    pub description: String,
    /// Stable logical IDs of neighboring concepts, used only as context.
    pub neighbors: BTreeSet<String>,
    /// Versioned source identifiers or URLs. Shared values are context only.
    pub source_refs: BTreeSet<String>,
    /// Directional semantic assertions extracted from a cited source.
    pub relation_assertions: Vec<RelationAssertion>,
}

impl EntityRecord {
    #[must_use]
    pub fn new(
        space_id: impl Into<String>,
        logical_id: impl Into<String>,
        revision_id: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            id: EntityId::new(space_id, logical_id),
            revision_id: revision_id.into(),
            lineage_id: None,
            label: label.into(),
            aliases: BTreeSet::new(),
            kind: None,
            identifier_assertions: BTreeMap::new(),
            description: String::new(),
            neighbors: BTreeSet::new(),
            source_refs: BTreeSet::new(),
            relation_assertions: Vec::new(),
        }
    }
}

/// Trust attached by the ingestion layer to an external assertion.
///
/// `SourceVerified` means a named verifier checked the assertion against a
/// source snapshot already present in the entity's `source_refs`. Agent or
/// user text must enter as `Claimed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssertionTrust {
    Claimed,
    SourceVerified {
        source_ref: String,
        verifier: String,
    },
}

/// A namespace-qualified external identifier with explicit ingestion trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentifierAssertion {
    value: String,
    trust: AssertionTrust,
}

impl IdentifierAssertion {
    #[must_use]
    pub fn claimed(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            trust: AssertionTrust::Claimed,
        }
    }

    #[must_use]
    pub fn source_verified(
        value: impl Into<String>,
        source_ref: impl Into<String>,
        verifier: impl Into<String>,
    ) -> Self {
        Self {
            value: value.into(),
            trust: AssertionTrust::SourceVerified {
                source_ref: source_ref.into(),
                verifier: verifier.into(),
            },
        }
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    fn is_source_verified_for(&self, record: &EntityRecord) -> bool {
        match &self.trust {
            AssertionTrust::Claimed => false,
            AssertionTrust::SourceVerified {
                source_ref,
                verifier,
            } => !verifier.trim().is_empty() && record.source_refs.contains(source_ref),
        }
    }
}

/// A directional relation claim on the containing entity.
///
/// The target selector is deliberately conservative: a normalized name or
/// alias and controlled kind must both match. The result is review evidence,
/// never an automatically applied graph edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationAssertion {
    relation_type: String,
    target_label: String,
    target_kind: String,
    trust: AssertionTrust,
}

impl RelationAssertion {
    /// Build a source-verified relation assertion.
    ///
    /// # Errors
    ///
    /// Rejects malformed relation types and empty target/provenance fields.
    pub fn source_verified(
        relation_type: impl Into<String>,
        target_label: impl Into<String>,
        target_kind: impl Into<String>,
        source_ref: impl Into<String>,
        verifier: impl Into<String>,
    ) -> PocResult<Self> {
        Self::new(
            relation_type,
            target_label,
            target_kind,
            AssertionTrust::SourceVerified {
                source_ref: source_ref.into(),
                verifier: verifier.into(),
            },
        )
    }

    /// Build an unverified relation claim from user or agent content.
    ///
    /// # Errors
    ///
    /// Rejects malformed relation types or empty target fields.
    pub fn claimed(
        relation_type: impl Into<String>,
        target_label: impl Into<String>,
        target_kind: impl Into<String>,
    ) -> PocResult<Self> {
        Self::new(
            relation_type,
            target_label,
            target_kind,
            AssertionTrust::Claimed,
        )
    }

    fn new(
        relation_type: impl Into<String>,
        target_label: impl Into<String>,
        target_kind: impl Into<String>,
        trust: AssertionTrust,
    ) -> PocResult<Self> {
        let relation_type = relation_type.into();
        let target_label = target_label.into();
        let target_kind = target_kind.into();
        if !valid_relation_type(&relation_type)
            || target_label.trim().is_empty()
            || target_kind.trim().is_empty()
        {
            return Err(PocError::Invalid(
                "malformed relation assertion".to_string(),
            ));
        }
        if let AssertionTrust::SourceVerified {
            source_ref,
            verifier,
        } = &trust
        {
            if source_ref.trim().is_empty() || verifier.trim().is_empty() {
                return Err(PocError::Invalid(
                    "verified relation assertion requires provenance".to_string(),
                ));
            }
        }
        Ok(Self {
            relation_type,
            target_label,
            target_kind,
            trust,
        })
    }

    fn is_source_verified_for(&self, record: &EntityRecord) -> bool {
        match &self.trust {
            AssertionTrust::Claimed => false,
            AssertionTrust::SourceVerified {
                source_ref,
                verifier,
            } => !verifier.trim().is_empty() && record.source_refs.contains(source_ref),
        }
    }
}

/// Relationship between two controlled ontology kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindCompatibility {
    Compatible,
    Incompatible,
    Unknown,
}

/// Versioned evidence policy supplied by the product/ontology layer.
///
/// Unknown identifier namespaces and kind pairs are context, not proof or
/// contradiction. `legacy_kind_mismatch_is_contradiction` exists only to keep
/// the first POC behavior measurable against the revised design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityRules {
    policy_version: String,
    authoritative_namespaces: BTreeSet<String>,
    kind_relations: BTreeMap<(String, String), KindCompatibility>,
    legacy_kind_mismatch_is_contradiction: bool,
}

impl IdentityRules {
    #[must_use]
    pub fn new() -> Self {
        Self {
            policy_version: "identity-rules-v2".to_string(),
            authoritative_namespaces: BTreeSet::new(),
            kind_relations: BTreeMap::new(),
            legacy_kind_mismatch_is_contradiction: false,
        }
    }

    #[must_use]
    pub fn with_policy_version(mut self, policy_version: impl Into<String>) -> Self {
        self.policy_version = policy_version.into();
        self
    }

    #[must_use]
    pub fn with_authoritative_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.authoritative_namespaces.insert(namespace.into());
        self
    }

    #[must_use]
    pub fn with_kind_relation(
        mut self,
        left: impl Into<String>,
        right: impl Into<String>,
        relation: KindCompatibility,
    ) -> Self {
        self.kind_relations
            .insert(canonical_kind_pair(left.into(), right.into()), relation);
        self
    }

    fn legacy() -> Self {
        Self::new()
            .with_policy_version("identity-rules-legacy-v1")
            .with_authoritative_namespace("repository")
            .with_authoritative_namespace("wikidata")
            .with_kind_relation("service", "dataset", KindCompatibility::Incompatible)
            .with_legacy_kind_mismatch()
    }

    fn with_legacy_kind_mismatch(mut self) -> Self {
        self.legacy_kind_mismatch_is_contradiction = true;
        self
    }

    fn is_authoritative(&self, namespace: &str) -> bool {
        self.authoritative_namespaces.contains(namespace)
    }

    fn kind_relation(&self, left: &str, right: &str) -> KindCompatibility {
        if left == right {
            return KindCompatibility::Compatible;
        }
        self.kind_relations
            .get(&canonical_kind_pair(left.to_string(), right.to_string()))
            .copied()
            .unwrap_or(if self.legacy_kind_mismatch_is_contradiction {
                KindCompatibility::Incompatible
            } else {
                KindCompatibility::Unknown
            })
    }
}

impl Default for IdentityRules {
    fn default() -> Self {
        Self::new()
    }
}

fn canonical_kind_pair(left: String, right: String) -> (String, String) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

/// Symmetric key for a pair of entities. Ordering cannot change its identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PairKey {
    pub first: EntityId,
    pub second: EntityId,
}

impl PairKey {
    /// Build a canonical cross-space pair.
    ///
    /// # Errors
    ///
    /// Rejects self-pairs and pairs within one semantic space.
    pub fn new(left: &EntityId, right: &EntityId) -> PocResult<Self> {
        if left == right {
            return Err(PocError::Invalid("identity self-pair".to_string()));
        }
        if left.space_id == right.space_id {
            return Err(PocError::Invalid(
                "identity candidates must cross spaces".to_string(),
            ));
        }
        let (first, second) = if left < right {
            (left.clone(), right.clone())
        } else {
            (right.clone(), left.clone())
        };
        Ok(Self { first, second })
    }

    fn storage_key(&self) -> String {
        format!(
            "{}\u{1e}{}",
            self.first.storage_key(),
            self.second.storage_key()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    Proof,
    Contradiction,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: u32,
    pub strength: EvidenceStrength,
    pub kind: String,
    pub summary: String,
    /// Present only for a source-verified directional relation assertion.
    pub relation: Option<RelationEvidence>,
}

/// Directional, source-bound relation evidence available to a reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationEvidence {
    pub relation_type: String,
    pub subject: EntityId,
    pub object: EntityId,
    pub source_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeterministicResolution {
    ProvenSame { basis: String },
    ProvenDifferent { basis: String },
    ConflictingProofs { basis: String },
    NeedsAgent,
    NotCandidate,
}

impl DeterministicResolution {
    #[must_use]
    pub fn decision(&self) -> Option<IdentityDecision> {
        match self {
            Self::ProvenSame { .. } => Some(IdentityDecision::Same),
            Self::ProvenDifferent { .. } => Some(IdentityDecision::Different),
            Self::ConflictingProofs { .. } | Self::NeedsAgent | Self::NotCandidate => None,
        }
    }
}

/// Immutable input packet for an agent or reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityPacket {
    pub pair: PairKey,
    pub policy_version: String,
    pub first: EntityRecord,
    pub second: EntityRecord,
    pub evidence: Vec<Evidence>,
    pub deterministic: DeterministicResolution,
}

impl IdentityPacket {
    #[must_use]
    pub fn has_contradiction(&self) -> bool {
        self.evidence
            .iter()
            .any(|item| item.strength == EvidenceStrength::Contradiction)
    }

    fn revisions(&self) -> (&str, &str) {
        (&self.first.revision_id, &self.second.revision_id)
    }

    /// Hash the complete immutable packet, including policy and evidence.
    ///
    /// # Errors
    ///
    /// Returns deterministic packet serialization failures.
    pub fn binding_hash(&self) -> PocResult<String> {
        let encoded = serde_json::to_vec(self)?;
        Ok(blake3::hash(&encoded).to_hex().to_string())
    }
}

/// Analyze a pair without model inference.
///
/// # Errors
///
/// Rejects invalid pairs or two records that do not match canonical pair
/// ordering.
pub fn analyze_pair(left: &EntityRecord, right: &EntityRecord) -> PocResult<IdentityPacket> {
    analyze_pair_with_rules(left, right, &IdentityRules::legacy())
}

/// Analyze a pair using an explicit, versioned evidence policy.
///
/// # Errors
///
/// Rejects invalid cross-space pairs.
pub fn analyze_pair_with_rules(
    left: &EntityRecord,
    right: &EntityRecord,
    rules: &IdentityRules,
) -> PocResult<IdentityPacket> {
    if !valid_token(&rules.policy_version, 128)
        || rules.authoritative_namespaces.len() > MAX_IDENTIFIER_ASSERTIONS
        || rules.kind_relations.len() > 256
        || rules
            .authoritative_namespaces
            .iter()
            .any(|namespace| !valid_token(namespace, 64))
        || rules
            .kind_relations
            .keys()
            .any(|(left, right)| !valid_token(left, 64) || !valid_token(right, 64))
    {
        return Err(PocError::Invalid(
            "identity policy version is malformed".to_string(),
        ));
    }
    validate_entity_record(left)?;
    validate_entity_record(right)?;
    let pair = PairKey::new(&left.id, &right.id)?;
    let (first, second) = if left.id == pair.first {
        (left.clone(), right.clone())
    } else {
        (right.clone(), left.clone())
    };
    let mut evidence = Vec::new();
    collect_lineage_evidence(&first, &second, &mut evidence);
    let identifiers = collect_identifier_evidence(&first, &second, rules, &mut evidence);
    let labels_match = collect_context_evidence(&first, &second, rules, &mut evidence);
    collect_relation_evidence(&first, &second, &mut evidence);
    let deterministic = classify_resolution(&evidence, identifiers, labels_match);

    Ok(IdentityPacket {
        pair,
        policy_version: rules.policy_version.clone(),
        first,
        second,
        evidence,
        deterministic,
    })
}

fn validate_entity_record(record: &EntityRecord) -> PocResult<()> {
    if record.id.space_id.trim().is_empty()
        || record.id.space_id.len() > 512
        || record.id.logical_id.trim().is_empty()
        || record.id.logical_id.len() > 512
        || record.revision_id.trim().is_empty()
        || record.revision_id.len() > 512
        || record.label.trim().is_empty()
        || record.label.len() > MAX_LABEL_BYTES
        || record.aliases.len() > MAX_ALIASES
        || record.identifier_assertions.len() > MAX_IDENTIFIER_ASSERTIONS
        || record.relation_assertions.len() > MAX_RELATION_ASSERTIONS
        || record.source_refs.len() > MAX_SOURCE_REFS
        || record.neighbors.len() > MAX_NEIGHBORS
        || record.description.len() > MAX_DESCRIPTION_BYTES
    {
        return Err(PocError::Invalid(
            "entity record exceeds identity packet bounds".to_string(),
        ));
    }
    if record
        .aliases
        .iter()
        .any(|alias| alias.trim().is_empty() || alias.len() > MAX_LABEL_BYTES)
        || record.source_refs.iter().any(|source| {
            source.trim().is_empty() || source.len() > 2_048 || source.contains(char::is_control)
        })
        || record
            .neighbors
            .iter()
            .any(|neighbor| neighbor.trim().is_empty() || neighbor.len() > 512)
        || record
            .identifier_assertions
            .iter()
            .any(|(namespace, assertion)| {
                !valid_token(namespace, 64)
                    || assertion.value.trim().is_empty()
                    || assertion.value.len() > 1_024
                    || match &assertion.trust {
                        AssertionTrust::Claimed => false,
                        AssertionTrust::SourceVerified {
                            source_ref,
                            verifier,
                        } => {
                            source_ref.trim().is_empty()
                                || source_ref.len() > 2_048
                                || !valid_token(verifier, 128)
                        }
                    }
            })
        || record.relation_assertions.iter().any(|assertion| {
            assertion.target_label.len() > MAX_LABEL_BYTES
                || !valid_token(&assertion.target_kind, 64)
                || match &assertion.trust {
                    AssertionTrust::Claimed => false,
                    AssertionTrust::SourceVerified {
                        source_ref,
                        verifier,
                    } => {
                        source_ref.trim().is_empty()
                            || source_ref.len() > 2_048
                            || !valid_token(verifier, 128)
                    }
                }
        })
    {
        return Err(PocError::Invalid(
            "entity record contains malformed identity evidence".to_string(),
        ));
    }
    if let Some(kind) = &record.kind {
        if !valid_token(kind, 64) {
            return Err(PocError::Invalid("entity kind is malformed".to_string()));
        }
    }
    Ok(())
}

fn valid_token(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn collect_lineage_evidence(
    first: &EntityRecord,
    second: &EntityRecord,
    evidence: &mut Vec<Evidence>,
) {
    let (Some(first_lineage), Some(second_lineage)) = (&first.lineage_id, &second.lineage_id)
    else {
        return;
    };
    if first_lineage == second_lineage {
        push_evidence(
            evidence,
            EvidenceStrength::Proof,
            "shared_lineage",
            format!("shared lineage {first_lineage}"),
        );
    } else {
        push_evidence(
            evidence,
            EvidenceStrength::Context,
            "different_lineage",
            "different recorded lineages".to_string(),
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct IdentifierComparison {
    shared: bool,
    conflicting: bool,
}

fn collect_identifier_evidence(
    first: &EntityRecord,
    second: &EntityRecord,
    rules: &IdentityRules,
    evidence: &mut Vec<Evidence>,
) -> IdentifierComparison {
    let mut comparison = IdentifierComparison {
        shared: false,
        conflicting: false,
    };
    for (namespace, first_assertion) in &first.identifier_assertions {
        if let Some(second_assertion) = second.identifier_assertions.get(namespace) {
            let first_value = first_assertion.value();
            let second_value = second_assertion.value();
            if !rules.is_authoritative(namespace) {
                if first_value == second_value {
                    push_evidence(
                        evidence,
                        EvidenceStrength::Context,
                        "shared_non_authoritative_id",
                        format!("{namespace}={first_value}"),
                    );
                }
                continue;
            }
            let both_verified = first_assertion.is_source_verified_for(first)
                && second_assertion.is_source_verified_for(second);
            if first_value == second_value && both_verified {
                comparison.shared = true;
                push_evidence(
                    evidence,
                    EvidenceStrength::Proof,
                    "shared_authoritative_id",
                    format!("{namespace}={first_value}"),
                );
            } else if first_value != second_value && both_verified {
                comparison.conflicting = true;
                push_evidence(
                    evidence,
                    EvidenceStrength::Contradiction,
                    "conflicting_authoritative_id",
                    format!("{namespace}: {first_value} != {second_value}"),
                );
            } else {
                push_evidence(
                    evidence,
                    EvidenceStrength::Context,
                    if first_value == second_value {
                        "shared_unverified_identifier"
                    } else {
                        "conflicting_unverified_identifier"
                    },
                    format!("{namespace}: {first_value} ? {second_value}"),
                );
            }
        }
    }
    comparison
}

fn collect_context_evidence(
    first: &EntityRecord,
    second: &EntityRecord,
    rules: &IdentityRules,
    evidence: &mut Vec<Evidence>,
) -> bool {
    let shared_names = normalized_names(first)
        .intersection(&normalized_names(second))
        .take(4)
        .cloned()
        .collect::<Vec<_>>();
    let names_match = !shared_names.is_empty();
    if names_match {
        push_evidence(
            evidence,
            EvidenceStrength::Context,
            "shared_name_or_alias",
            shared_names.join(", "),
        );
    }

    if let (Some(first_kind), Some(second_kind)) = (&first.kind, &second.kind) {
        match rules.kind_relation(first_kind, second_kind) {
            KindCompatibility::Compatible => push_evidence(
                evidence,
                EvidenceStrength::Context,
                "compatible_kind",
                format!("{first_kind} ~ {second_kind}"),
            ),
            KindCompatibility::Incompatible => push_evidence(
                evidence,
                EvidenceStrength::Contradiction,
                "incompatible_kind",
                format!("{first_kind} != {second_kind}"),
            ),
            KindCompatibility::Unknown => push_evidence(
                evidence,
                EvidenceStrength::Context,
                "unresolved_kind_relation",
                format!("{first_kind} ? {second_kind}"),
            ),
        }
    }

    let shared_sources = first
        .source_refs
        .intersection(&second.source_refs)
        .take(4)
        .cloned()
        .collect::<Vec<_>>();
    if !shared_sources.is_empty() {
        push_evidence(
            evidence,
            EvidenceStrength::Context,
            "shared_source_reference",
            shared_sources.join(", "),
        );
    }

    let shared_neighbors = first
        .neighbors
        .intersection(&second.neighbors)
        .take(4)
        .cloned()
        .collect::<Vec<_>>();
    if !shared_neighbors.is_empty() {
        push_evidence(
            evidence,
            EvidenceStrength::Context,
            "shared_neighbors",
            shared_neighbors.join(", "),
        );
    }

    let description_overlap = token_overlap_basis_points(&first.description, &second.description);
    if description_overlap > 0 {
        push_evidence(
            evidence,
            EvidenceStrength::Context,
            "description_token_overlap",
            format!("{description_overlap} basis points"),
        );
    }
    names_match
}

fn collect_relation_evidence(
    first: &EntityRecord,
    second: &EntityRecord,
    evidence: &mut Vec<Evidence>,
) {
    collect_directional_relation_evidence(first, second, evidence);
    collect_directional_relation_evidence(second, first, evidence);
}

fn collect_directional_relation_evidence(
    subject: &EntityRecord,
    object: &EntityRecord,
    evidence: &mut Vec<Evidence>,
) {
    for assertion in &subject.relation_assertions {
        if !relation_target_matches(assertion, object) {
            continue;
        }
        match &assertion.trust {
            AssertionTrust::Claimed => push_evidence(
                evidence,
                EvidenceStrength::Context,
                "unverified_relation_claim",
                format!(
                    "{} claims {} -> {}",
                    subject.label, assertion.relation_type, object.label
                ),
            ),
            AssertionTrust::SourceVerified {
                source_ref,
                verifier,
            } if !verifier.trim().is_empty() && subject.source_refs.contains(source_ref) => {
                push_relation_evidence(
                    evidence,
                    RelationEvidence {
                        relation_type: assertion.relation_type.clone(),
                        subject: subject.id.clone(),
                        object: object.id.clone(),
                        source_ref: source_ref.clone(),
                    },
                );
            }
            AssertionTrust::SourceVerified { .. } => push_evidence(
                evidence,
                EvidenceStrength::Context,
                "unverified_relation_provenance",
                format!(
                    "{} claim lacks a bound source snapshot",
                    assertion.relation_type
                ),
            ),
        }
    }
}

fn relation_target_matches(assertion: &RelationAssertion, object: &EntityRecord) -> bool {
    object.kind.as_deref() == Some(assertion.target_kind.as_str())
        && normalized_names(object).contains(&normalize_label(&assertion.target_label))
}

fn normalized_names(record: &EntityRecord) -> BTreeSet<String> {
    std::iter::once(&record.label)
        .chain(&record.aliases)
        .map(|name| normalize_label(name))
        .collect()
}

fn classify_resolution(
    evidence: &[Evidence],
    identifiers: IdentifierComparison,
    labels_match: bool,
) -> DeterministicResolution {
    let shared_lineage = evidence.iter().any(|item| item.kind == "shared_lineage");
    let has_contradiction = evidence
        .iter()
        .any(|item| item.strength == EvidenceStrength::Contradiction);
    let has_proof = shared_lineage || identifiers.shared;
    if has_proof && has_contradiction {
        DeterministicResolution::ConflictingProofs {
            basis: "proof_and_contradiction".to_string(),
        }
    } else if shared_lineage {
        DeterministicResolution::ProvenSame {
            basis: "shared_lineage".to_string(),
        }
    } else if identifiers.conflicting {
        DeterministicResolution::ProvenDifferent {
            basis: "conflicting_authoritative_id".to_string(),
        }
    } else if identifiers.shared {
        DeterministicResolution::ProvenSame {
            basis: "shared_authoritative_id".to_string(),
        }
    } else if labels_match {
        DeterministicResolution::NeedsAgent
    } else {
        DeterministicResolution::NotCandidate
    }
}

fn push_evidence(
    evidence: &mut Vec<Evidence>,
    strength: EvidenceStrength,
    kind: &str,
    summary: String,
) {
    evidence.push(Evidence {
        id: u32::try_from(evidence.len()).unwrap_or(u32::MAX),
        strength,
        kind: kind.to_string(),
        summary,
        relation: None,
    });
}

fn push_relation_evidence(evidence: &mut Vec<Evidence>, relation: RelationEvidence) {
    let source_ref = relation.source_ref.clone();
    evidence.push(Evidence {
        id: u32::try_from(evidence.len()).unwrap_or(u32::MAX),
        strength: EvidenceStrength::Context,
        kind: format!("verified_relation_{}", relation.relation_type),
        summary: source_ref,
        relation: Some(relation),
    });
}

#[must_use]
pub fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn token_overlap_basis_points(left: &str, right: &str) -> u16 {
    let left = tokens(left);
    let right = tokens(right);
    let union = left.union(&right).count();
    if union == 0 {
        return 0;
    }
    let intersection = left.intersection(&right).count();
    u16::try_from(intersection.saturating_mul(10_000) / union).unwrap_or(10_000)
}

fn tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() > 2)
        .map(str::to_lowercase)
        .collect()
}

/// Indexed cross-space candidate discovery. There is no all-pairs scan.
#[derive(Debug, Default)]
pub struct CandidateIndex {
    labels: BTreeMap<String, BTreeSet<EntityId>>,
    lineages: BTreeMap<String, BTreeSet<EntityId>>,
    identifiers: BTreeMap<(String, String), BTreeSet<EntityId>>,
    verified_identifiers: BTreeMap<(String, String), BTreeSet<EntityId>>,
    kinds: BTreeMap<EntityId, String>,
}

pub const MAX_CANDIDATES_PER_ENTITY: usize = 256;

/// A bounded, deterministically ordered candidate page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateBatch {
    pub candidates: BTreeSet<EntityId>,
    /// More exact candidates existed than the requested bound. A merge job
    /// must paginate or request reviewer disambiguation; it must not assume
    /// that omitted candidates are different identities.
    pub truncated: bool,
}

impl CandidateIndex {
    /// Add one validated entity record to the exact candidate indexes.
    ///
    /// # Errors
    ///
    /// Rejects records that exceed packet/index bounds.
    pub fn insert(&mut self, record: &EntityRecord) -> PocResult<()> {
        validate_entity_record(record)?;
        for name in normalized_names(record) {
            self.labels
                .entry(name)
                .or_default()
                .insert(record.id.clone());
        }
        if let Some(lineage) = &record.lineage_id {
            self.lineages
                .entry(lineage.clone())
                .or_default()
                .insert(record.id.clone());
        }
        for (namespace, assertion) in &record.identifier_assertions {
            let key = (namespace.clone(), assertion.value().to_string());
            self.identifiers
                .entry(key.clone())
                .or_default()
                .insert(record.id.clone());
            if assertion.is_source_verified_for(record) {
                self.verified_identifiers
                    .entry(key)
                    .or_default()
                    .insert(record.id.clone());
            }
        }
        if let Some(kind) = &record.kind {
            self.kinds.insert(record.id.clone(), kind.clone());
        }
        Ok(())
    }

    /// Return unique candidates in `other_space` for one entity.
    ///
    /// Verified-identifier and lineage matches are added before source-bound
    /// relation targets, then names and untrusted IDs. A large homonym or
    /// untrusted-ID bucket therefore cannot starve stronger signals.
    ///
    /// # Errors
    ///
    /// Rejects zero or excessively large bounds.
    pub fn candidates_for(
        &self,
        record: &EntityRecord,
        other_space: &str,
        limit: usize,
    ) -> PocResult<CandidateBatch> {
        if limit == 0 || limit > MAX_CANDIDATES_PER_ENTITY {
            return Err(PocError::Invalid(format!(
                "candidate limit must be in 1..={MAX_CANDIDATES_PER_ENTITY}"
            )));
        }
        let mut candidates = BTreeSet::new();
        let mut truncated = false;
        for (namespace, assertion) in &record.identifier_assertions {
            if assertion.is_source_verified_for(record) {
                if let Some(matches) = self
                    .verified_identifiers
                    .get(&(namespace.clone(), assertion.value().to_string()))
                {
                    add_bounded_matches(
                        matches,
                        other_space,
                        limit,
                        &mut candidates,
                        &mut truncated,
                    );
                }
            }
        }
        if let Some(lineage) = &record.lineage_id {
            if let Some(matches) = self.lineages.get(lineage) {
                add_bounded_matches(matches, other_space, limit, &mut candidates, &mut truncated);
            }
        }
        for assertion in &record.relation_assertions {
            if !assertion.is_source_verified_for(record) {
                continue;
            }
            if let Some(matches) = self.labels.get(&normalize_label(&assertion.target_label)) {
                add_bounded_kind_matches(
                    matches,
                    other_space,
                    &assertion.target_kind,
                    &self.kinds,
                    limit,
                    &mut candidates,
                    &mut truncated,
                );
            }
        }
        for name in normalized_names(record) {
            if let Some(matches) = self.labels.get(&name) {
                add_bounded_matches(matches, other_space, limit, &mut candidates, &mut truncated);
            }
        }
        for (namespace, assertion) in &record.identifier_assertions {
            if let Some(matches) = self
                .identifiers
                .get(&(namespace.clone(), assertion.value().to_string()))
            {
                add_bounded_matches(matches, other_space, limit, &mut candidates, &mut truncated);
            }
        }
        Ok(CandidateBatch {
            candidates,
            truncated,
        })
    }
}

fn add_bounded_kind_matches(
    matches: &BTreeSet<EntityId>,
    other_space: &str,
    required_kind: &str,
    kinds: &BTreeMap<EntityId, String>,
    limit: usize,
    candidates: &mut BTreeSet<EntityId>,
    truncated: &mut bool,
) {
    for id in matches.iter().filter(|id| {
        id.space_id == other_space && kinds.get(*id).is_some_and(|kind| kind == required_kind)
    }) {
        if candidates.contains(id) {
            continue;
        }
        if candidates.len() == limit {
            *truncated = true;
            break;
        }
        candidates.insert(id.clone());
    }
}

fn add_bounded_matches(
    matches: &BTreeSet<EntityId>,
    other_space: &str,
    limit: usize,
    candidates: &mut BTreeSet<EntityId>,
    truncated: &mut bool,
) {
    for id in matches.iter().filter(|id| id.space_id == other_space) {
        if candidates.contains(id) {
            continue;
        }
        if candidates.len() == limit {
            *truncated = true;
            break;
        }
        candidates.insert(id.clone());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityDecision {
    Same,
    Different,
    Undetermined,
}

/// Non-canonical semantic edge suggested alongside a `different` decision.
/// It requires its own reviewed graph changeset after identity review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationSuggestion {
    pub relation_type: String,
    pub subject: EntityId,
    pub object: EntityId,
    pub evidence_ids: Vec<u32>,
}

/// Untrusted, structured agent output. Evidence IDs cite the immutable packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProposal {
    pub pair: PairKey,
    pub packet_hash: String,
    pub first_revision: String,
    pub second_revision: String,
    pub recommendation: IdentityDecision,
    pub evidence_ids: Vec<u32>,
    #[serde(default)]
    pub relation_suggestions: Vec<RelationSuggestion>,
    pub rationale: String,
    pub model_id: String,
    pub prompt_version: String,
}

/// Validate the agent boundary without treating model confidence as proof.
///
/// # Errors
///
/// Rejects stale, misbound, uncited, or contradiction-ignoring proposals.
pub fn validate_agent_proposal(packet: &IdentityPacket, proposal: &AgentProposal) -> PocResult<()> {
    if proposal.packet_hash != packet.binding_hash()? {
        return Err(PocError::Conflict(
            "agent proposal is stale for the evidence packet or policy".to_string(),
        ));
    }
    if matches!(
        packet.deterministic,
        DeterministicResolution::ConflictingProofs { .. }
    ) {
        return Err(PocError::Conflict(
            "conflicting deterministic proofs require direct human review".to_string(),
        ));
    }
    if let Some(deterministic) = packet.deterministic.decision() {
        if proposal.recommendation != IdentityDecision::Undetermined
            && proposal.recommendation != deterministic
        {
            return Err(PocError::Conflict(
                "agent cannot override a deterministic identity decision".to_string(),
            ));
        }
    }
    if proposal.pair != packet.pair {
        return Err(PocError::Invalid(
            "agent proposal is bound to a different pair".to_string(),
        ));
    }
    let (first_revision, second_revision) = packet.revisions();
    if proposal.first_revision != first_revision || proposal.second_revision != second_revision {
        return Err(PocError::Conflict(
            "agent proposal is stale for the entity revisions".to_string(),
        ));
    }
    if proposal.evidence_ids.is_empty() || proposal.evidence_ids.len() > MAX_PROPOSAL_EVIDENCE {
        return Err(PocError::Invalid(
            "agent proposal evidence count is out of bounds".to_string(),
        ));
    }
    if proposal.evidence_ids.iter().collect::<BTreeSet<_>>().len() != proposal.evidence_ids.len() {
        return Err(PocError::Invalid(
            "agent proposal contains duplicate evidence IDs".to_string(),
        ));
    }
    let available = packet
        .evidence
        .iter()
        .map(|item| item.id)
        .collect::<BTreeSet<_>>();
    if proposal
        .evidence_ids
        .iter()
        .any(|evidence_id| !available.contains(evidence_id))
    {
        return Err(PocError::Invalid(
            "agent proposal cites unknown evidence".to_string(),
        ));
    }
    validate_relation_suggestions(packet, proposal, &available)?;
    if proposal.recommendation == IdentityDecision::Same && packet.has_contradiction() {
        return Err(PocError::Conflict(
            "agent cannot override deterministic contradictions".to_string(),
        ));
    }
    if proposal.rationale.trim().is_empty()
        || proposal.rationale.len() > MAX_RATIONALE_BYTES
        || proposal.model_id.trim().is_empty()
        || proposal.model_id.len() > 256
        || proposal.prompt_version.trim().is_empty()
        || proposal.prompt_version.len() > 256
    {
        return Err(PocError::Invalid(
            "agent provenance and rationale are required".to_string(),
        ));
    }
    Ok(())
}

fn validate_relation_suggestions(
    packet: &IdentityPacket,
    proposal: &AgentProposal,
    available_evidence: &BTreeSet<u32>,
) -> PocResult<()> {
    if proposal.relation_suggestions.len() > MAX_RELATION_SUGGESTIONS {
        return Err(PocError::Invalid(
            "too many relation suggestions".to_string(),
        ));
    }
    if !proposal.relation_suggestions.is_empty()
        && proposal.recommendation != IdentityDecision::Different
    {
        return Err(PocError::Invalid(
            "relations may only accompany a different-identity recommendation".to_string(),
        ));
    }
    let mut unique_suggestions = BTreeSet::new();
    for suggestion in &proposal.relation_suggestions {
        if !unique_suggestions.insert((
            suggestion.relation_type.as_str(),
            &suggestion.subject,
            &suggestion.object,
        )) {
            return Err(PocError::Invalid(
                "duplicate relation suggestion".to_string(),
            ));
        }
        let endpoints_match = (suggestion.subject == packet.pair.first
            && suggestion.object == packet.pair.second)
            || (suggestion.subject == packet.pair.second && suggestion.object == packet.pair.first);
        if !endpoints_match || suggestion.subject == suggestion.object {
            return Err(PocError::Invalid(
                "relation suggestion endpoints must be the analyzed pair".to_string(),
            ));
        }
        if !valid_relation_type(&suggestion.relation_type) {
            return Err(PocError::Invalid(
                "relation suggestion type is invalid".to_string(),
            ));
        }
        if suggestion.evidence_ids.is_empty()
            || suggestion.evidence_ids.len() > MAX_PROPOSAL_EVIDENCE
            || suggestion
                .evidence_ids
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != suggestion.evidence_ids.len()
            || suggestion.evidence_ids.iter().any(|evidence_id| {
                !available_evidence.contains(evidence_id)
                    || !proposal.evidence_ids.contains(evidence_id)
            })
        {
            return Err(PocError::Invalid(
                "relation suggestion must cite packet evidence".to_string(),
            ));
        }
        let has_directional_evidence = suggestion.evidence_ids.iter().any(|evidence_id| {
            packet.evidence.iter().any(|evidence| {
                evidence.id == *evidence_id
                    && evidence.relation.as_ref().is_some_and(|relation| {
                        relation.relation_type == suggestion.relation_type
                            && relation.subject == suggestion.subject
                            && relation.object == suggestion.object
                    })
            })
        });
        if !has_directional_evidence {
            return Err(PocError::Invalid(
                "relation suggestion lacks source-verified directional evidence".to_string(),
            ));
        }
    }
    Ok(())
}

fn valid_relation_type(relation_type: &str) -> bool {
    !relation_type.is_empty()
        && relation_type.len() <= 64
        && relation_type
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyRoute {
    AlreadyDecided(IdentityDecision),
    Revalidate {
        known: IdentityDecision,
        observed: IdentityDecision,
    },
    AutoApply(IdentityDecision),
    AskAgent,
    HumanReview(IdentityDecision),
    KeepSeparate,
}

/// Default policy controls. Agent recommendations are never auto-applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityPolicy {
    pub auto_apply_lineage: bool,
    pub auto_apply_authoritative_id: bool,
}

/// Durable decision context used to suppress an identical review packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownDecision {
    pub decision: IdentityDecision,
    pub packet_hash: String,
}

impl From<&DecisionRecord> for KnownDecision {
    fn from(record: &DecisionRecord) -> Self {
        Self {
            decision: record.decision,
            packet_hash: record.packet_hash.clone(),
        }
    }
}

impl IdentityPolicy {
    #[must_use]
    pub fn personal_default() -> Self {
        Self {
            auto_apply_lineage: true,
            auto_apply_authoritative_id: true,
        }
    }

    #[must_use]
    pub fn team_default() -> Self {
        Self {
            auto_apply_lineage: true,
            auto_apply_authoritative_id: false,
        }
    }
}

#[must_use]
pub fn route_packet(
    packet: &IdentityPacket,
    policy: IdentityPolicy,
    known: Option<&KnownDecision>,
) -> PolicyRoute {
    if let Some(known) = known {
        let decision = known.decision;
        let packet_hash_matches = packet
            .binding_hash()
            .is_ok_and(|packet_hash| packet_hash == known.packet_hash);
        if !packet_hash_matches {
            return PolicyRoute::Revalidate {
                known: decision,
                observed: packet
                    .deterministic
                    .decision()
                    .unwrap_or(IdentityDecision::Undetermined),
            };
        }
        if matches!(
            packet.deterministic,
            DeterministicResolution::ConflictingProofs { .. }
        ) {
            return PolicyRoute::Revalidate {
                known: decision,
                observed: IdentityDecision::Undetermined,
            };
        }
        if let Some(observed) = packet.deterministic.decision() {
            if observed != decision {
                return PolicyRoute::Revalidate {
                    known: decision,
                    observed,
                };
            }
        }
        return PolicyRoute::AlreadyDecided(decision);
    }
    match &packet.deterministic {
        DeterministicResolution::ProvenSame { basis } if basis == "shared_lineage" => {
            if policy.auto_apply_lineage {
                PolicyRoute::AutoApply(IdentityDecision::Same)
            } else {
                PolicyRoute::HumanReview(IdentityDecision::Same)
            }
        }
        DeterministicResolution::ProvenSame { .. } => {
            if policy.auto_apply_authoritative_id {
                PolicyRoute::AutoApply(IdentityDecision::Same)
            } else {
                PolicyRoute::HumanReview(IdentityDecision::Same)
            }
        }
        DeterministicResolution::ProvenDifferent { .. } => {
            PolicyRoute::AutoApply(IdentityDecision::Different)
        }
        DeterministicResolution::ConflictingProofs { .. } => {
            PolicyRoute::HumanReview(IdentityDecision::Undetermined)
        }
        DeterministicResolution::NeedsAgent => PolicyRoute::AskAgent,
        DeterministicResolution::NotCandidate => PolicyRoute::KeepSeparate,
    }
}

/// A validated agent recommendation always enters review under default policy.
///
/// # Errors
///
/// Returns proposal validation failures.
pub fn route_agent_proposal(
    packet: &IdentityPacket,
    proposal: &AgentProposal,
) -> PocResult<PolicyRoute> {
    validate_agent_proposal(packet, proposal)?;
    Ok(match proposal.recommendation {
        IdentityDecision::Undetermined => PolicyRoute::KeepSeparate,
        decision => PolicyRoute::HumanReview(decision),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalState {
    Proposed,
    Applied,
    Rejected,
    Deferred,
}

impl ProposalState {
    fn parse(value: &str) -> PocResult<Self> {
        match value {
            "proposed" => Ok(Self::Proposed),
            "applied" => Ok(Self::Applied),
            "rejected" => Ok(Self::Rejected),
            "deferred" => Ok(Self::Deferred),
            other => Err(PocError::Invalid(format!(
                "unknown identity proposal state {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalReceipt {
    pub id: String,
    pub state: ProposalState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    pub id: String,
    pub pair: PairKey,
    pub decision: IdentityDecision,
    pub reviewer: String,
    pub reason: String,
    pub supersedes: Option<String>,
    pub packet_hash: String,
}

/// Subprocess-only crash injection points for identity review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityReviewCrashPoint {
    AfterDecisionInsert,
    AfterProposalStateUpdate,
    AfterCommit,
}

/// Durable proposal and decision ledger. It intentionally owns no graph rows.
#[derive(Debug, Clone)]
pub struct IdentityLedger {
    db_path: PathBuf,
}

impl IdentityLedger {
    /// Open or create the identity ledger.
    ///
    /// # Errors
    ///
    /// Returns filesystem, `SQLite`, or schema initialization failures.
    pub fn open(root: &Path) -> PocResult<Self> {
        std::fs::create_dir_all(root)?;
        let ledger = Self {
            db_path: root.join(DB_FILE),
        };
        let conn = ledger.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS identity_proposals (
                 id TEXT PRIMARY KEY,
                 idempotency_key TEXT NOT NULL UNIQUE,
                 request_hash TEXT NOT NULL,
                 pair_key TEXT NOT NULL,
                 pair_json TEXT NOT NULL,
                 packet_hash TEXT NOT NULL,
                 first_revision TEXT NOT NULL,
                 second_revision TEXT NOT NULL,
                 proposal_json TEXT NOT NULL,
                 state TEXT NOT NULL CHECK(state IN ('proposed','applied','rejected','deferred')),
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_identity_proposals_pair
                 ON identity_proposals(pair_key, state);
             CREATE TABLE IF NOT EXISTS identity_decisions (
                 id TEXT PRIMARY KEY,
                 pair_key TEXT NOT NULL,
                 pair_json TEXT NOT NULL,
                 packet_hash TEXT NOT NULL,
                 decision TEXT NOT NULL CHECK(decision IN ('same','different','undetermined')),
                 reviewer TEXT NOT NULL,
                 reason TEXT NOT NULL,
                 proposal_id TEXT REFERENCES identity_proposals(id),
                 supersedes TEXT REFERENCES identity_decisions(id),
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS identity_heads (
                 pair_key TEXT PRIMARY KEY,
                 decision_id TEXT NOT NULL REFERENCES identity_decisions(id)
             );",
        )?;
        Ok(ledger)
    }

    fn connect(&self) -> PocResult<Connection> {
        let conn = Connection::open(&self.db_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;",
        )?;
        Ok(conn)
    }

    /// Persist a validated agent proposal idempotently.
    ///
    /// # Errors
    ///
    /// Returns validation, serialization, storage, or idempotency conflicts.
    pub fn submit(
        &self,
        idempotency_key: &str,
        packet: &IdentityPacket,
        proposal: &AgentProposal,
    ) -> PocResult<ProposalReceipt> {
        validate_agent_proposal(packet, proposal)?;
        let proposal_json = serde_json::to_string(proposal)?;
        let pair_json = serde_json::to_string(&packet.pair)?;
        let request_hash = blake3::hash(proposal_json.as_bytes()).to_hex().to_string();
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT id, request_hash, state FROM identity_proposals
                 WHERE idempotency_key = ?1",
                [idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((id, existing_hash, state)) = existing {
            if existing_hash != request_hash {
                return Err(PocError::Conflict(
                    "identity idempotency key reused with different proposal".to_string(),
                ));
            }
            return Ok(ProposalReceipt {
                id,
                state: ProposalState::parse(&state)?,
            });
        }
        let id = Uuid::now_v7().to_string();
        let (first_revision, second_revision) = packet.revisions();
        tx.execute(
            "INSERT INTO identity_proposals(
                 id, idempotency_key, request_hash, pair_key, pair_json,
                 packet_hash, first_revision, second_revision, proposal_json, state, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'proposed', ?10)",
            params![
                id,
                idempotency_key,
                request_hash,
                packet.pair.storage_key(),
                pair_json,
                packet.binding_hash()?,
                first_revision,
                second_revision,
                proposal_json,
                now_seconds()?
            ],
        )?;
        tx.commit()?;
        Ok(ProposalReceipt {
            id,
            state: ProposalState::Proposed,
        })
    }

    /// Apply one review decision if the proposal and entity revisions are live.
    ///
    /// # Errors
    ///
    /// Returns optimistic concurrency, state, authorization-input, or storage
    /// failures. Reviewer authorization is intentionally a daemon concern.
    pub fn review(
        &self,
        proposal_id: &str,
        packet: &IdentityPacket,
        reviewer: &str,
        decision: IdentityDecision,
        reason: &str,
    ) -> PocResult<DecisionRecord> {
        self.review_inner(proposal_id, packet, reviewer, decision, reason, None)
    }

    /// Review with a deliberate subprocess abort at a durable boundary.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::review`] before the selected abort.
    pub fn review_with_crash_point(
        &self,
        proposal_id: &str,
        packet: &IdentityPacket,
        reviewer: &str,
        decision: IdentityDecision,
        reason: &str,
        crash_point: IdentityReviewCrashPoint,
    ) -> PocResult<DecisionRecord> {
        self.review_inner(
            proposal_id,
            packet,
            reviewer,
            decision,
            reason,
            Some(crash_point),
        )
    }

    fn review_inner(
        &self,
        proposal_id: &str,
        packet: &IdentityPacket,
        reviewer: &str,
        decision: IdentityDecision,
        reason: &str,
        crash_point: Option<IdentityReviewCrashPoint>,
    ) -> PocResult<DecisionRecord> {
        if reviewer.trim().is_empty() || reason.trim().is_empty() {
            return Err(PocError::Invalid(
                "reviewer and reason are required".to_string(),
            ));
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = tx
            .query_row(
                "SELECT pair_key, pair_json, packet_hash, first_revision, second_revision, state
                 FROM identity_proposals WHERE id = ?1",
                [proposal_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| PocError::NotFound(format!("identity proposal {proposal_id}")))?;
        if ProposalState::parse(&stored.5)? != ProposalState::Proposed {
            return Err(PocError::Conflict(
                "identity proposal was already reviewed".to_string(),
            ));
        }
        let (first_revision, second_revision) = packet.revisions();
        if stored.0 != packet.pair.storage_key()
            || stored.2 != packet.binding_hash()?
            || stored.3 != first_revision
            || stored.4 != second_revision
        {
            return Err(PocError::Conflict(
                "identity proposal is stale or bound to another pair".to_string(),
            ));
        }
        let pair: PairKey = serde_json::from_str(&stored.1)?;
        let previous = current_head_id(&tx, &stored.0)?;
        let record = DecisionRecord {
            id: Uuid::now_v7().to_string(),
            pair,
            decision,
            reviewer: reviewer.to_string(),
            reason: reason.to_string(),
            supersedes: previous,
            packet_hash: stored.2,
        };
        insert_decision(&tx, &record, Some(proposal_id))?;
        abort_if(crash_point, IdentityReviewCrashPoint::AfterDecisionInsert);
        tx.execute(
            "UPDATE identity_proposals SET state = 'applied' WHERE id = ?1",
            [proposal_id],
        )?;
        abort_if(
            crash_point,
            IdentityReviewCrashPoint::AfterProposalStateUpdate,
        );
        tx.commit()?;
        abort_if(crash_point, IdentityReviewCrashPoint::AfterCommit);
        Ok(record)
    }

    /// Cache an agent abstention for exactly the analyzed revisions.
    ///
    /// This changes no identity decision and owns no graph rows. A later entity
    /// revision is a different lookup and may be analyzed again.
    ///
    /// # Errors
    ///
    /// Returns optimistic state, binding, or storage failures.
    pub fn defer(
        &self,
        proposal_id: &str,
        packet: &IdentityPacket,
        actor: &str,
        reason: &str,
    ) -> PocResult<()> {
        if actor.trim().is_empty() || reason.trim().is_empty() {
            return Err(PocError::Invalid(
                "defer actor and reason are required".to_string(),
            ));
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = tx
            .query_row(
                "SELECT pair_key, packet_hash, first_revision, second_revision, state
                 FROM identity_proposals WHERE id = ?1",
                [proposal_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| PocError::NotFound(format!("identity proposal {proposal_id}")))?;
        if ProposalState::parse(&stored.4)? != ProposalState::Proposed {
            return Err(PocError::Conflict(
                "identity proposal was already handled".to_string(),
            ));
        }
        let (first_revision, second_revision) = packet.revisions();
        if stored.0 != packet.pair.storage_key()
            || stored.1 != packet.binding_hash()?
            || stored.2 != first_revision
            || stored.3 != second_revision
        {
            return Err(PocError::Conflict(
                "identity proposal is stale or bound to another pair".to_string(),
            ));
        }
        tx.execute(
            "UPDATE identity_proposals SET state = 'deferred' WHERE id = ?1",
            [proposal_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Find an existing agent result for one exact evidence packet.
    ///
    /// # Errors
    ///
    /// Returns storage or state decoding failures.
    pub fn proposal_for_revisions(
        &self,
        packet: &IdentityPacket,
    ) -> PocResult<Option<ProposalReceipt>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT id, state FROM identity_proposals
             WHERE pair_key = ?1 AND packet_hash = ?2
             ORDER BY created_at DESC, id DESC LIMIT 1",
            params![packet.pair.storage_key(), packet.binding_hash()?],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(id, state)| {
            Ok(ProposalReceipt {
                id,
                state: ProposalState::parse(&state)?,
            })
        })
        .transpose()
    }

    /// Reverse or refine the current decision without erasing history.
    ///
    /// # Errors
    ///
    /// Returns a conflict if the expected head moved.
    pub fn revise(
        &self,
        pair: &PairKey,
        expected_head: &str,
        reviewer: &str,
        decision: IdentityDecision,
        reason: &str,
    ) -> PocResult<DecisionRecord> {
        if reviewer.trim().is_empty() || reason.trim().is_empty() {
            return Err(PocError::Invalid(
                "reviewer and reason are required".to_string(),
            ));
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pair_key = pair.storage_key();
        let current = current_head_id(&tx, &pair_key)?
            .ok_or_else(|| PocError::NotFound("identity decision head".to_string()))?;
        if current != expected_head {
            return Err(PocError::Conflict(
                "identity decision head moved".to_string(),
            ));
        }
        let packet_hash = tx.query_row(
            "SELECT packet_hash FROM identity_decisions WHERE id = ?1",
            [&current],
            |row| row.get::<_, String>(0),
        )?;
        let record = DecisionRecord {
            id: Uuid::now_v7().to_string(),
            pair: pair.clone(),
            decision,
            reviewer: reviewer.to_string(),
            reason: reason.to_string(),
            supersedes: Some(current),
            packet_hash,
        };
        insert_decision(&tx, &record, None)?;
        tx.commit()?;
        Ok(record)
    }

    /// Read the latest durable decision for candidate suppression.
    ///
    /// # Errors
    ///
    /// Returns storage, schema, or decoding failures.
    pub fn current_decision(&self, pair: &PairKey) -> PocResult<Option<DecisionRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT d.id, d.pair_json, d.decision, d.reviewer, d.reason, d.supersedes,
                    d.packet_hash
             FROM identity_heads h
             JOIN identity_decisions d ON d.id = h.decision_id
             WHERE h.pair_key = ?1",
            [pair.storage_key()],
            |row| {
                let pair_json = row.get::<_, String>(1)?;
                let decision = row.get::<_, String>(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    pair_json,
                    decision,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?
        .map(|stored| {
            Ok(DecisionRecord {
                id: stored.0,
                pair: serde_json::from_str(&stored.1)?,
                decision: parse_decision(&stored.2)?,
                reviewer: stored.3,
                reason: stored.4,
                supersedes: stored.5,
                packet_hash: stored.6,
            })
        })
        .transpose()
    }

    /// Count immutable decision events for audit assertions.
    ///
    /// # Errors
    ///
    /// Returns storage failures.
    pub fn decision_event_count(&self, pair: &PairKey) -> PocResult<u64> {
        let conn = self.connect()?;
        let count = conn.query_row(
            "SELECT count(*) FROM identity_decisions WHERE pair_key = ?1",
            [pair.storage_key()],
            |row| row.get::<_, u64>(0),
        )?;
        Ok(count)
    }
}

fn current_head_id(tx: &rusqlite::Transaction<'_>, pair_key: &str) -> PocResult<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT decision_id FROM identity_heads WHERE pair_key = ?1",
            [pair_key],
            |row| row.get(0),
        )
        .optional()?)
}

fn insert_decision(
    tx: &rusqlite::Transaction<'_>,
    record: &DecisionRecord,
    proposal_id: Option<&str>,
) -> PocResult<()> {
    let pair_key = record.pair.storage_key();
    tx.execute(
        "INSERT INTO identity_decisions(
             id, pair_key, pair_json, packet_hash, decision, reviewer, reason,
             proposal_id, supersedes, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            record.id,
            pair_key,
            serde_json::to_string(&record.pair)?,
            record.packet_hash,
            decision_name(record.decision),
            record.reviewer,
            record.reason,
            proposal_id,
            record.supersedes,
            now_seconds()?
        ],
    )?;
    tx.execute(
        "INSERT INTO identity_heads(pair_key, decision_id) VALUES (?1, ?2)
         ON CONFLICT(pair_key) DO UPDATE SET decision_id = excluded.decision_id",
        params![pair_key, record.id],
    )?;
    Ok(())
}

fn decision_name(decision: IdentityDecision) -> &'static str {
    match decision {
        IdentityDecision::Same => "same",
        IdentityDecision::Different => "different",
        IdentityDecision::Undetermined => "undetermined",
    }
}

fn parse_decision(value: &str) -> PocResult<IdentityDecision> {
    match value {
        "same" => Ok(IdentityDecision::Same),
        "different" => Ok(IdentityDecision::Different),
        "undetermined" => Ok(IdentityDecision::Undetermined),
        other => Err(PocError::Invalid(format!(
            "unknown identity decision {other:?}"
        ))),
    }
}

fn now_seconds() -> PocResult<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| PocError::Invalid(format!("system clock before epoch: {error}")))?;
    i64::try_from(duration.as_secs())
        .map_err(|_| PocError::Invalid("timestamp does not fit i64".to_string()))
}

fn abort_if(actual: Option<IdentityReviewCrashPoint>, expected: IdentityReviewCrashPoint) {
    if actual == Some(expected) {
        std::process::abort();
    }
}
