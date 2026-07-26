//! Persistent identity candidate decisions with a strict non-authoritative
//! agent boundary.

use std::path::Path;
use std::sync::{mpsc, Arc};

use openmemory_core::space::{ActorKind, ChangeSetId, PrincipalId, SpaceRole};
use openmemory_merge::identity::{
    analyze_identity, AgentIdentityProposal, DecisionSource, DeterministicIdentity,
    IdentityCandidate, IdentityDecision, IdentityPacket, IdentityResolutionReceipt,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::product_store::{ProductStore, ProductStoreError};

#[derive(Debug, Error)]
pub enum IdentityServiceError {
    #[error("identity request is invalid: {0}")]
    Invalid(String),
    #[error("identity candidate is stale or missing")]
    Stale,
    #[error("identity action is unauthorized")]
    Unauthorized,
    #[error("identity persistence failed: {0}")]
    Storage(String),
}

impl From<ProductStoreError> for IdentityServiceError {
    fn from(error: ProductStoreError) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<rusqlite::Error> for IdentityServiceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentModelProvenance {
    pub provider: String,
    pub model: String,
    pub model_version: String,
    pub prompt_version: String,
    pub temperature_milli: u16,
    pub tool_policy: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityCandidateRecord {
    pub merge_job_id: String,
    pub candidate: IdentityCandidate,
    pub packet: IdentityPacket,
    pub deterministic: DeterministicIdentity,
    pub proposal_state: String,
    pub current_decision: Option<IdentityDecision>,
    pub current_event_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IdentityService {
    catalog: ProductStore,
}

impl IdentityService {
    pub fn open(home: &Path) -> Result<Self, IdentityServiceError> {
        Ok(Self {
            catalog: ProductStore::open(home)?,
        })
    }

    /// Persist one immutable packet and its deterministic analysis.
    pub fn record_candidate(
        &self,
        merge_job_id: &str,
        candidate_id: &str,
        packet: &IdentityPacket,
        now: i64,
    ) -> Result<IdentityCandidate, IdentityServiceError> {
        self.record_candidate_with_policy(merge_job_id, candidate_id, packet, true, now)
    }

    /// As [`Self::record_candidate`], with a team-policy gate for automatic
    /// trusted-same receipts. Trusted-different evidence is always recorded.
    pub fn record_candidate_with_policy(
        &self,
        merge_job_id: &str,
        candidate_id: &str,
        packet: &IdentityPacket,
        allow_deterministic_same: bool,
        now: i64,
    ) -> Result<IdentityCandidate, IdentityServiceError> {
        packet
            .validate()
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        let deterministic = analyze_identity(packet)
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        let candidate = IdentityCandidate::from_packet(candidate_id.to_string(), packet)
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        let packet_json = serde_json::to_string(packet)
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        if packet_json.len() > 256 * 1024 {
            return Err(IdentityServiceError::Invalid(
                "identity packet exceeds 256 KiB".to_string(),
            ));
        }
        let mut conn = self.catalog.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT INTO identity_candidates(
                id, merge_job_id, left_space_id, left_entity_id, left_revision_id,
                right_space_id, right_entity_id, right_revision_id, policy_generation,
                packet_hash, packet_json, deterministic_state, proposal_state,
                created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     'pending', ?13, ?13)
             ON CONFLICT(merge_job_id, left_space_id, left_entity_id,
                         right_space_id, right_entity_id) DO NOTHING",
            params![
                candidate.candidate_id,
                merge_job_id,
                candidate.left.address.space_id.to_string(),
                candidate.left.address.logical_id,
                candidate.left.revision_id.to_string(),
                candidate.right.address.space_id.to_string(),
                candidate.right.address.logical_id,
                candidate.right.revision_id.to_string(),
                candidate.policy_generation,
                candidate.packet_hash.as_bytes().as_slice(),
                packet_json,
                deterministic_name(deterministic),
                now
            ],
        )?;
        if inserted == 1 {
            let authoritative = match deterministic {
                DeterministicIdentity::Same if allow_deterministic_same => {
                    Some((IdentityDecision::Same, deterministic_source(packet)))
                }
                DeterministicIdentity::Different => Some((
                    IdentityDecision::Different,
                    DecisionSource::VerifiedIdentifier,
                )),
                DeterministicIdentity::Same
                | DeterministicIdentity::Undetermined
                | DeterministicIdentity::ConflictingProofs => None,
            };
            if let Some((decision, source)) = authoritative {
                let event_id = format!("identity-event:{}", ChangeSetId::new());
                tx.execute(
                    "INSERT INTO identity_decision_events(
                        id, candidate_id, decision, decision_source, principal_id,
                        actor_kind, packet_hash, rationale_json, created_at)
                     VALUES (?1, ?2, ?3, ?4, NULL, 'system', ?5, ?6, ?7)",
                    params![
                        event_id,
                        candidate_id,
                        decision_name(decision),
                        decision_source_name(source),
                        packet.binding_hash.as_bytes().as_slice(),
                        serde_json::json!({"deterministic": deterministic_name(deterministic)})
                            .to_string(),
                        now
                    ],
                )?;
                tx.execute(
                    "UPDATE identity_candidates SET current_decision=?1,
                        current_event_id=?2, proposal_state='decided', updated_at=?3
                     WHERE id=?4",
                    params![decision_name(decision), event_id, now, candidate_id],
                )?;
            }
        }
        tx.commit()?;
        Ok(candidate)
    }

    /// Store an optional agent suggestion as an event only. It never updates
    /// the authoritative decision columns and cannot produce a receipt.
    pub fn record_agent_proposal(
        &self,
        candidate_id: &str,
        proposal: &AgentIdentityProposal,
        provenance: &AgentModelProvenance,
        now: i64,
    ) -> Result<String, IdentityServiceError> {
        let (packet, deterministic) = self.load_packet(candidate_id)?;
        proposal
            .validate(&packet, deterministic)
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        if proposal.model != provenance.model
            || proposal.prompt_version != provenance.prompt_version
            || provenance.provider.is_empty()
            || provenance.model_version.is_empty()
            || provenance.tool_policy.is_empty()
            || provenance.temperature_milli > 2_000
        {
            return Err(IdentityServiceError::Invalid(
                "agent model provenance does not bind the proposal".to_string(),
            ));
        }
        let event_id = format!("identity-event:{}", ChangeSetId::new());
        let mut conn = self.catalog.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO identity_decision_events(
                id, candidate_id, decision, decision_source, principal_id, actor_kind,
                packet_hash, rationale_json, model_provenance_json, created_at)
             VALUES (?1, ?2, ?3, 'agent_proposal', NULL, 'agent', ?4, ?5, ?6, ?7)",
            params![
                event_id,
                candidate_id,
                decision_name(proposal.decision),
                proposal.packet_hash.as_bytes().as_slice(),
                serde_json::to_string(proposal)
                    .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?,
                serde_json::to_string(provenance)
                    .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?,
                now
            ],
        )?;
        tx.execute(
            "UPDATE identity_candidates SET proposal_state='agent_proposed',
                updated_at=?1 WHERE id=?2 AND current_event_id IS NULL",
            params![now, candidate_id],
        )?;
        tx.commit()?;
        Ok(event_id)
    }

    /// Append a human decision, supersede the prior event, and return the
    /// exact revision/packet/policy-bound planner receipt.
    pub fn decide_human(
        &self,
        candidate_id: &str,
        reviewer: &PrincipalId,
        actor_kind: ActorKind,
        role: SpaceRole,
        authority_generation: u64,
        decision: IdentityDecision,
        rationale: &str,
        now: i64,
    ) -> Result<IdentityResolutionReceipt, IdentityServiceError> {
        if actor_kind != ActorKind::Human
            || !role.allows(SpaceRole::Reviewer)
            || authority_generation == 0
        {
            return Err(IdentityServiceError::Unauthorized);
        }
        if rationale.is_empty() || rationale.len() > 2_048 {
            return Err(IdentityServiceError::Invalid(
                "human rationale must contain 1..=2048 bytes".to_string(),
            ));
        }
        let (packet, deterministic) = self.load_packet(candidate_id)?;
        let contradicts_proof = matches!(
            (deterministic, decision),
            (DeterministicIdentity::Same, IdentityDecision::Different)
                | (DeterministicIdentity::Different, IdentityDecision::Same)
                | (
                    DeterministicIdentity::ConflictingProofs,
                    IdentityDecision::Same | IdentityDecision::Different
                )
        );
        if contradicts_proof {
            return Err(IdentityServiceError::Invalid(
                "trusted identity proof cannot be overridden".to_string(),
            ));
        }
        let candidate = IdentityCandidate::from_packet(candidate_id.to_string(), &packet)
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        let event_id = format!("identity-event:{}", ChangeSetId::new());
        let mut conn = self.catalog.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<String> = tx
            .query_row(
                "SELECT current_event_id FROM identity_candidates WHERE id=?1",
                [candidate_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        tx.execute(
            "INSERT INTO identity_decision_events(
                id, candidate_id, supersedes_event_id, decision, decision_source,
                principal_id, actor_kind, packet_hash, rationale_json, created_at)
             VALUES (?1, ?2, ?3, ?4, 'human', ?5, 'human', ?6, ?7, ?8)",
            params![
                event_id,
                candidate_id,
                current,
                decision_name(decision),
                reviewer.to_string(),
                packet.binding_hash.as_bytes().as_slice(),
                serde_json::json!({
                    "rationale": rationale,
                    "authority_generation": authority_generation
                })
                .to_string(),
                now
            ],
        )?;
        let changed = tx.execute(
            "UPDATE identity_candidates SET current_decision=?1, current_event_id=?2,
                proposal_state='decided', updated_at=?3
             WHERE id=?4 AND packet_hash=?5",
            params![
                decision_name(decision),
                event_id,
                now,
                candidate_id,
                packet.binding_hash.as_bytes().as_slice()
            ],
        )?;
        if changed != 1 {
            return Err(IdentityServiceError::Stale);
        }
        tx.commit()?;
        IdentityResolutionReceipt::new(
            &candidate,
            event_id,
            decision,
            DecisionSource::Human,
            packet.policy_generation,
        )
        .map_err(|error| IdentityServiceError::Invalid(error.to_string()))
    }

    /// Load and validate the current authoritative receipt. Agent events never
    /// satisfy this query.
    pub fn current_receipt(
        &self,
        candidate_id: &str,
    ) -> Result<Option<IdentityResolutionReceipt>, IdentityServiceError> {
        let (packet, _) = self.load_packet(candidate_id)?;
        let candidate = IdentityCandidate::from_packet(candidate_id.to_string(), &packet)
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        let row = self
            .catalog
            .connect()?
            .query_row(
                "SELECT candidate.current_event_id, candidate.current_decision,
                        event.decision_source
                 FROM identity_candidates AS candidate
                 JOIN identity_decision_events AS event
                   ON event.id=candidate.current_event_id
                 WHERE candidate.id=?1 AND candidate.current_event_id IS NOT NULL
                   AND event.decision_source<>'agent_proposal'",
                [candidate_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(event, decision, source)| {
            IdentityResolutionReceipt::new(
                &candidate,
                event,
                parse_decision(&decision)?,
                parse_decision_source(&source)?,
                packet.policy_generation,
            )
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))
        })
        .transpose()
    }

    pub fn get_candidate(
        &self,
        candidate_id: &str,
    ) -> Result<IdentityCandidateRecord, IdentityServiceError> {
        let row = self
            .catalog
            .connect()?
            .query_row(
                "SELECT merge_job_id, packet_json, deterministic_state,
                        proposal_state, current_decision, current_event_id
                 FROM identity_candidates WHERE id=?1",
                [candidate_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(IdentityServiceError::Stale)?;
        candidate_record(candidate_id, row)
    }

    pub fn list_candidates(
        &self,
        merge_job_id: &str,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IdentityCandidateRecord>, IdentityServiceError> {
        if merge_job_id.is_empty() || limit == 0 || limit > 256 {
            return Err(IdentityServiceError::Invalid(
                "candidate query is outside bounds".to_string(),
            ));
        }
        if state.is_some_and(|value| !matches!(value, "pending" | "decided" | "agent_proposed")) {
            return Err(IdentityServiceError::Invalid(
                "candidate proposal state is invalid".to_string(),
            ));
        }
        let conn = self.catalog.connect()?;
        let mut statement = conn.prepare(
            "SELECT id, merge_job_id, packet_json, deterministic_state,
                    proposal_state, current_decision, current_event_id
             FROM identity_candidates
             WHERE merge_job_id=?1 AND (?2 IS NULL OR proposal_state=?2)
             ORDER BY id LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![
                    merge_job_id,
                    state,
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| {
                let id = row.0;
                candidate_record(&id, (row.1, row.2, row.3, row.4, row.5, row.6))
            })
            .collect()
    }

    fn load_packet(
        &self,
        candidate_id: &str,
    ) -> Result<(IdentityPacket, DeterministicIdentity), IdentityServiceError> {
        let row = self
            .catalog
            .connect()?
            .query_row(
                "SELECT packet_json, deterministic_state FROM identity_candidates WHERE id=?1",
                [candidate_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or(IdentityServiceError::Stale)?;
        let packet: IdentityPacket = serde_json::from_str(&row.0)
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        packet
            .validate()
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        Ok((packet, parse_deterministic(&row.1)?))
    }
}

fn candidate_record(
    candidate_id: &str,
    row: (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    ),
) -> Result<IdentityCandidateRecord, IdentityServiceError> {
    let packet: IdentityPacket = serde_json::from_str(&row.1)
        .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
    packet
        .validate()
        .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
    Ok(IdentityCandidateRecord {
        merge_job_id: row.0,
        candidate: IdentityCandidate::from_packet(candidate_id.to_string(), &packet)
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?,
        packet,
        deterministic: parse_deterministic(&row.2)?,
        proposal_state: row.3,
        current_decision: row.4.as_deref().map(parse_decision).transpose()?,
        current_event_id: row.5,
    })
}

/// Optional review adapter. Implementations receive one already-bounded,
/// immutable packet and cannot access graph/catalog mutation APIs.
pub trait IdentityAgent: Send + Sync + 'static {
    fn propose(
        &self,
        packet: &IdentityPacket,
    ) -> Result<(AgentIdentityProposal, AgentModelProvenance), String>;
}

struct IdentityAgentTask {
    candidate_id: String,
    packet: IdentityPacket,
    created_at: i64,
    completion: mpsc::Sender<Result<String, String>>,
}

/// Explicit bounded asynchronous queue. No queue or provider is constructed
/// by default, so recall/write/promotion paths cannot make outbound calls.
#[derive(Clone)]
pub struct IdentityAgentQueue {
    sender: mpsc::SyncSender<IdentityAgentTask>,
}

impl IdentityAgentQueue {
    pub fn spawn(
        service: IdentityService,
        agent: Arc<dyn IdentityAgent>,
        capacity: usize,
    ) -> Result<Self, IdentityServiceError> {
        if capacity == 0 || capacity > 1_024 {
            return Err(IdentityServiceError::Invalid(
                "identity agent queue capacity must be in 1..=1024".to_string(),
            ));
        }
        let (sender, receiver) = mpsc::sync_channel::<IdentityAgentTask>(capacity);
        std::thread::Builder::new()
            .name("openmemory-identity-agent".to_string())
            .spawn(move || {
                while let Ok(task) = receiver.recv() {
                    let outcome = agent
                        .propose(&task.packet)
                        .and_then(|(proposal, provenance)| {
                            service
                                .record_agent_proposal(
                                    &task.candidate_id,
                                    &proposal,
                                    &provenance,
                                    task.created_at,
                                )
                                .map_err(|error| error.to_string())
                        });
                    let _ = task.completion.send(outcome);
                }
            })
            .map_err(|error| IdentityServiceError::Storage(error.to_string()))?;
        Ok(Self { sender })
    }

    pub fn enqueue(
        &self,
        candidate_id: String,
        packet: IdentityPacket,
        created_at: i64,
    ) -> Result<mpsc::Receiver<Result<String, String>>, IdentityServiceError> {
        packet
            .validate()
            .map_err(|error| IdentityServiceError::Invalid(error.to_string()))?;
        if candidate_id.is_empty() || candidate_id.len() > 128 {
            return Err(IdentityServiceError::Invalid(
                "identity candidate ID is invalid".to_string(),
            ));
        }
        let (completion, receiver) = mpsc::channel();
        self.sender
            .try_send(IdentityAgentTask {
                candidate_id,
                packet,
                created_at,
                completion,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    IdentityServiceError::Invalid("identity agent queue is full".to_string())
                }
                mpsc::TrySendError::Disconnected(_) => {
                    IdentityServiceError::Storage("identity agent queue stopped".to_string())
                }
            })?;
        Ok(receiver)
    }
}

fn deterministic_name(value: DeterministicIdentity) -> &'static str {
    match value {
        DeterministicIdentity::Same => "same",
        DeterministicIdentity::Different => "different",
        DeterministicIdentity::Undetermined => "undetermined",
        DeterministicIdentity::ConflictingProofs => "conflicting_proofs",
    }
}

fn deterministic_source(packet: &IdentityPacket) -> DecisionSource {
    if packet.evidence.iter().any(|evidence| {
        matches!(
            evidence,
            openmemory_merge::identity::IdentityEvidence::Lineage { .. }
        )
    }) {
        DecisionSource::Lineage
    } else {
        DecisionSource::VerifiedIdentifier
    }
}

fn decision_source_name(source: DecisionSource) -> &'static str {
    match source {
        DecisionSource::Lineage => "lineage",
        DecisionSource::VerifiedIdentifier => "verified_identifier",
        DecisionSource::Human => "human",
    }
}

fn parse_decision_source(value: &str) -> Result<DecisionSource, IdentityServiceError> {
    match value {
        "lineage" => Ok(DecisionSource::Lineage),
        "verified_identifier" => Ok(DecisionSource::VerifiedIdentifier),
        "human" => Ok(DecisionSource::Human),
        _ => Err(IdentityServiceError::Invalid(
            "invalid authoritative decision source".to_string(),
        )),
    }
}

fn parse_deterministic(value: &str) -> Result<DeterministicIdentity, IdentityServiceError> {
    match value {
        "same" => Ok(DeterministicIdentity::Same),
        "different" => Ok(DeterministicIdentity::Different),
        "undetermined" => Ok(DeterministicIdentity::Undetermined),
        "conflicting_proofs" => Ok(DeterministicIdentity::ConflictingProofs),
        _ => Err(IdentityServiceError::Invalid(
            "invalid deterministic identity state".to_string(),
        )),
    }
}

fn decision_name(value: IdentityDecision) -> &'static str {
    match value {
        IdentityDecision::Same => "same",
        IdentityDecision::Different => "different",
        IdentityDecision::Undetermined => "undetermined",
    }
}

fn parse_decision(value: &str) -> Result<IdentityDecision, IdentityServiceError> {
    match value {
        "same" => Ok(IdentityDecision::Same),
        "different" => Ok(IdentityDecision::Different),
        "undetermined" => Ok(IdentityDecision::Undetermined),
        _ => Err(IdentityServiceError::Invalid(
            "invalid identity decision".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmemory_core::space::{RevisionId, SpaceId};
    use openmemory_merge::canonical::CanonicalEntity;
    use openmemory_merge::identity::{
        ContextSignal, EntityAddress, EntityRevisionRef, IdentityEvidence,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn packet() -> IdentityPacket {
        let left_space = SpaceId::new();
        let right_space = SpaceId::new();
        let left = CanonicalEntity::new(
            left_space,
            "left".to_string(),
            RevisionId::new(),
            "cerpheus".to_string(),
            BTreeSet::new(),
            "concept".to_string(),
            BTreeSet::new(),
            String::new(),
        )
        .unwrap();
        let right = CanonicalEntity::new(
            right_space,
            "right".to_string(),
            RevisionId::new(),
            "cerpheus".to_string(),
            BTreeSet::new(),
            "concept".to_string(),
            BTreeSet::new(),
            String::new(),
        )
        .unwrap();
        IdentityPacket::new(
            EntityRevisionRef {
                address: EntityAddress::new(left_space, left.logical_id).unwrap(),
                revision_id: left.revision_id,
                semantic_hash: left.semantic_hash,
                controlled_kind: left.controlled_kind,
            },
            EntityRevisionRef {
                address: EntityAddress::new(right_space, right.logical_id).unwrap(),
                revision_id: right.revision_id,
                semantic_hash: right.semantic_hash,
                controlled_kind: right.controlled_kind,
            },
            1,
            1,
            BTreeMap::new(),
            vec![IdentityEvidence::Context {
                evidence_id: "label".to_string(),
                signal: ContextSignal::Label,
                detail: "same label".to_string(),
            }],
        )
        .unwrap()
    }

    #[test]
    fn agent_proposal_never_becomes_authoritative() {
        let home = tempfile::tempdir().unwrap();
        let service = IdentityService::open(home.path()).unwrap();
        let packet = packet();
        service
            .record_candidate("job", "candidate", &packet, 1)
            .unwrap();
        let proposal = AgentIdentityProposal {
            packet_hash: packet.binding_hash,
            decision: IdentityDecision::Same,
            cited_evidence_ids: BTreeSet::from(["label".to_string()]),
            rationale: "possibly the same".to_string(),
            model: "review-model".to_string(),
            prompt_version: "v1".to_string(),
        };
        service
            .record_agent_proposal(
                "candidate",
                &proposal,
                &AgentModelProvenance {
                    provider: "local".to_string(),
                    model: "review-model".to_string(),
                    model_version: "1".to_string(),
                    prompt_version: "v1".to_string(),
                    temperature_milli: 0,
                    tool_policy: "none".to_string(),
                },
                2,
            )
            .unwrap();
        assert!(service.current_receipt("candidate").unwrap().is_none());
        let receipt = service
            .decide_human(
                "candidate",
                &"local:reviewer".parse().unwrap(),
                ActorKind::Human,
                SpaceRole::Reviewer,
                1,
                IdentityDecision::Same,
                "confirmed from reviewed context",
                3,
            )
            .unwrap();
        assert_eq!(receipt.decision, IdentityDecision::Same);
        assert_eq!(
            service.current_receipt("candidate").unwrap().unwrap(),
            receipt
        );
    }
}
