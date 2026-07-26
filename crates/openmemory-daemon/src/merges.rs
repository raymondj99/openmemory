//! Directional merge job planning and short-lived confirmation authority.

use std::path::Path;

use openmemory_core::space::{MergeJobId, PrincipalId, SpaceId, SpaceRole};
use openmemory_merge::canonical::CanonicalSpaceSnapshot;
use openmemory_merge::identity::IdentityResolutionReceipt;
use openmemory_merge::planner::{plan_merge, CandidateSet, MergePlan, MergePolicy, PlanRequest};
use rand::RngCore;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use thiserror::Error;

use crate::product_store::{ProductStore, ProductStoreError};

#[derive(Debug, Error)]
pub enum MergeServiceError {
    #[error("merge request is invalid: {0}")]
    Invalid(String),
    #[error("merge job is stale or missing")]
    Stale,
    #[error("merge action is unauthorized")]
    Unauthorized,
    #[error("merge persistence failed: {0}")]
    Storage(String),
}

impl From<ProductStoreError> for MergeServiceError {
    fn from(error: ProductStoreError) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<rusqlite::Error> for MergeServiceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct MergeService {
    catalog: ProductStore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeJobRecord {
    pub id: MergeJobId,
    pub profile: String,
    pub source_space_id: SpaceId,
    pub target_space_id: SpaceId,
    pub requested_by: String,
    pub state: String,
    pub source_snapshot_hash: Option<Vec<u8>>,
    pub target_snapshot_hash: Option<Vec<u8>>,
    pub plan_hash: Option<Vec<u8>>,
    pub predicted_result_hash: Option<Vec<u8>>,
    pub report_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl MergeService {
    pub fn open(home: &Path) -> Result<Self, MergeServiceError> {
        Ok(Self {
            catalog: ProductStore::open(home)?,
        })
    }

    pub fn create_job(
        &self,
        profile: &str,
        source: SpaceId,
        target: SpaceId,
        requested_by: &PrincipalId,
        idempotency_key: &str,
        role: SpaceRole,
        now: i64,
    ) -> Result<MergeJobId, MergeServiceError> {
        if source == target
            || profile.is_empty()
            || profile.len() > 64
            || idempotency_key.is_empty()
            || idempotency_key.len() > 128
        {
            return Err(MergeServiceError::Invalid(
                "source/target/profile are invalid".to_string(),
            ));
        }
        if !role.allows(SpaceRole::Maintainer) {
            return Err(MergeServiceError::Unauthorized);
        }
        let conn = self.catalog.connect()?;
        if let Some(existing) = conn
            .query_row(
                "SELECT id, source_space_id, target_space_id FROM merge_jobs
                 WHERE profile=?1 AND requested_by=?2 AND idempotency_key=?3",
                params![profile, requested_by.to_string(), idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        {
            if existing.1 != source.to_string() || existing.2 != target.to_string() {
                return Err(MergeServiceError::Invalid(
                    "merge idempotency key was reused with different spaces".to_string(),
                ));
            }
            return existing.0.parse().map_err(|error| {
                MergeServiceError::Storage(format!("invalid stored merge job ID: {error}"))
            });
        }
        let id = MergeJobId::new();
        conn.execute(
            "INSERT INTO merge_jobs(
                id, profile, source_space_id, target_space_id, requested_by,
                idempotency_key, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'discovering', ?7, ?7)",
            params![
                id.to_string(),
                profile,
                source.to_string(),
                target.to_string(),
                requested_by.to_string(),
                idempotency_key,
                now
            ],
        )?;
        Ok(id)
    }

    /// Build a complete deterministic plan from immutable snapshots and
    /// current authoritative receipts, then persist only hashes/report data.
    pub fn plan(
        &self,
        id: MergeJobId,
        source: CanonicalSpaceSnapshot,
        target: CanonicalSpaceSnapshot,
        candidates: CandidateSet,
        resolutions: Vec<IdentityResolutionReceipt>,
        lineage_base: Option<CanonicalSpaceSnapshot>,
        policy: MergePolicy,
        now: i64,
    ) -> Result<MergePlan, MergeServiceError> {
        let row = self
            .catalog
            .connect()?
            .query_row(
                "SELECT source_space_id, target_space_id, state FROM merge_jobs WHERE id=?1",
                [id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(MergeServiceError::Stale)?;
        if row.0 != source.space_id.to_string()
            || row.1 != target.space_id.to_string()
            || !matches!(
                row.2.as_str(),
                "discovering" | "awaiting_review" | "planned"
            )
        {
            return Err(MergeServiceError::Stale);
        }
        let plan = plan_merge(&PlanRequest {
            source,
            target,
            lineage_base,
            candidates,
            resolutions,
            policy,
        })
        .map_err(|error| MergeServiceError::Invalid(error.to_string()))?;
        let report = serde_json::json!({
            "accounting": plan.accounting,
            "conflict_count": plan.conflicts.len(),
            "resolution_count": plan.resolution_receipts.len(),
        });
        let changed = self.catalog.connect()?.execute(
            "UPDATE merge_jobs SET state='planned', source_snapshot_hash=?1,
                target_snapshot_hash=?2, target_generation_json=?3, plan_hash=?4,
                predicted_result_hash=?5, report_json=?6, updated_at=?7
             WHERE id=?8 AND state IN ('discovering','awaiting_review','planned')",
            params![
                plan.source_snapshot_hash.as_bytes().as_slice(),
                plan.target_snapshot_hash.as_bytes().as_slice(),
                serde_json::json!({
                    "semantic_generation": plan.expected_target_version.semantic_generation
                })
                .to_string(),
                plan.plan_hash.as_bytes().as_slice(),
                plan.predicted_result_hash.as_bytes().as_slice(),
                report.to_string(),
                now,
                id.to_string()
            ],
        )?;
        if changed != 1 {
            return Err(MergeServiceError::Stale);
        }
        Ok(plan)
    }

    /// Mint a plaintext confirmation once and store only its hash.
    pub fn mint_confirmation(
        &self,
        id: MergeJobId,
        plan: &MergePlan,
        now: i64,
        ttl_secs: i64,
    ) -> Result<String, MergeServiceError> {
        if ttl_secs <= 0 || ttl_secs > 900 {
            return Err(MergeServiceError::Invalid(
                "confirmation TTL must be in 1..=900 seconds".to_string(),
            ));
        }
        let mut token = [0_u8; 32];
        rand::thread_rng().fill_bytes(&mut token);
        let hash = confirmation_hash(&token, id, plan);
        let changed = self.catalog.connect()?.execute(
            "UPDATE merge_jobs SET confirmation_hash=?1, confirmation_expires_at=?2,
                updated_at=?3
             WHERE id=?4 AND state='planned' AND plan_hash=?5
               AND target_snapshot_hash=?6",
            params![
                hash.as_bytes().as_slice(),
                now.saturating_add(ttl_secs),
                now,
                id.to_string(),
                plan.plan_hash.as_bytes().as_slice(),
                plan.target_snapshot_hash.as_bytes().as_slice()
            ],
        )?;
        if changed != 1 {
            return Err(MergeServiceError::Stale);
        }
        Ok(hex_encode(&token))
    }

    /// Consume a confirmation only if plan and target version are unchanged.
    pub fn confirm(
        &self,
        id: MergeJobId,
        token: &str,
        plan: &MergePlan,
        current_target: &CanonicalSpaceSnapshot,
        actor_role: SpaceRole,
        now: i64,
    ) -> Result<(), MergeServiceError> {
        if !actor_role.allows(SpaceRole::Maintainer) {
            return Err(MergeServiceError::Unauthorized);
        }
        current_target
            .validate()
            .map_err(|error| MergeServiceError::Invalid(error.to_string()))?;
        if current_target.snapshot_hash != plan.target_snapshot_hash
            || current_target.semantic_generation
                != plan.expected_target_version.semantic_generation
        {
            return Err(MergeServiceError::Stale);
        }
        let token = hex_decode_32(token)?;
        let expected = confirmation_hash(&token, id, plan);
        let mut conn = self.catalog.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = tx
            .query_row(
                "SELECT confirmation_hash FROM merge_jobs
                 WHERE id=?1 AND state='planned' AND confirmation_expires_at>?2
                   AND plan_hash=?3 AND target_snapshot_hash=?4",
                params![
                    id.to_string(),
                    now,
                    plan.plan_hash.as_bytes().as_slice(),
                    plan.target_snapshot_hash.as_bytes().as_slice()
                ],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .ok_or(MergeServiceError::Stale)?;
        if !constant_time_eq(&stored, expected.as_bytes()) {
            return Err(MergeServiceError::Unauthorized);
        }
        tx.execute(
            "UPDATE merge_jobs SET state='confirmed', confirmation_hash=NULL,
                confirmation_expires_at=NULL, updated_at=?1 WHERE id=?2",
            params![now, id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn transition(
        &self,
        id: MergeJobId,
        expected: &str,
        next: &str,
        now: i64,
    ) -> Result<(), MergeServiceError> {
        if !valid_transition(expected, next) {
            return Err(MergeServiceError::Invalid(
                "invalid merge job state transition".to_string(),
            ));
        }
        let changed = self.catalog.connect()?.execute(
            "UPDATE merge_jobs SET state=?1, updated_at=?2 WHERE id=?3 AND state=?4",
            params![next, now, id.to_string(), expected],
        )?;
        if changed != 1 {
            return Err(MergeServiceError::Stale);
        }
        Ok(())
    }

    pub fn mark_awaiting_review(
        &self,
        id: MergeJobId,
        source_hash: &[u8; 32],
        target_hash: &[u8; 32],
        unresolved: usize,
        now: i64,
    ) -> Result<(), MergeServiceError> {
        let changed = self.catalog.connect()?.execute(
            "UPDATE merge_jobs SET state='awaiting_review',
                source_snapshot_hash=?1, target_snapshot_hash=?2,
                report_json=?3, updated_at=?4
             WHERE id=?5 AND state IN ('discovering','awaiting_review')",
            params![
                source_hash.as_slice(),
                target_hash.as_slice(),
                serde_json::json!({"unresolved_candidates": unresolved}).to_string(),
                now,
                id.to_string()
            ],
        )?;
        if changed != 1 {
            return Err(MergeServiceError::Stale);
        }
        Ok(())
    }

    pub fn get_job(&self, id: MergeJobId) -> Result<MergeJobRecord, MergeServiceError> {
        self.catalog
            .connect()?
            .query_row(
                "SELECT id, profile, source_space_id, target_space_id, requested_by,
                        state, source_snapshot_hash, target_snapshot_hash, plan_hash,
                        predicted_result_hash, report_json, created_at, updated_at
                 FROM merge_jobs WHERE id=?1",
                [id.to_string()],
                |row| {
                    let parse_id = |index| -> rusqlite::Result<String> { row.get(index) };
                    Ok((
                        parse_id(0)?,
                        row.get::<_, String>(1)?,
                        parse_id(2)?,
                        parse_id(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<Vec<u8>>>(6)?,
                        row.get::<_, Option<Vec<u8>>>(7)?,
                        row.get::<_, Option<Vec<u8>>>(8)?,
                        row.get::<_, Option<Vec<u8>>>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, i64>(12)?,
                    ))
                },
            )
            .optional()?
            .ok_or(MergeServiceError::Stale)
            .and_then(|row| {
                Ok(MergeJobRecord {
                    id: row.0.parse().map_err(|error| {
                        MergeServiceError::Storage(format!("invalid merge job ID: {error}"))
                    })?,
                    profile: row.1,
                    source_space_id: row.2.parse().map_err(|error| {
                        MergeServiceError::Storage(format!("invalid source space ID: {error}"))
                    })?,
                    target_space_id: row.3.parse().map_err(|error| {
                        MergeServiceError::Storage(format!("invalid target space ID: {error}"))
                    })?,
                    requested_by: row.4,
                    state: row.5,
                    source_snapshot_hash: row.6,
                    target_snapshot_hash: row.7,
                    plan_hash: row.8,
                    predicted_result_hash: row.9,
                    report_json: row.10,
                    created_at: row.11,
                    updated_at: row.12,
                })
            })
    }

    pub fn cancel(&self, id: MergeJobId, now: i64) -> Result<(), MergeServiceError> {
        let changed = self.catalog.connect()?.execute(
            "UPDATE merge_jobs SET state='cancelled', confirmation_hash=NULL,
                confirmation_expires_at=NULL, updated_at=?1
             WHERE id=?2 AND state IN
                ('discovering','awaiting_review','planned','confirmed','staging','verified')",
            params![now, id.to_string()],
        )?;
        if changed != 1 {
            return Err(MergeServiceError::Stale);
        }
        Ok(())
    }

    /// Atomically make the control plane agree with a verified promoted root.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_promotion(
        &self,
        id: MergeJobId,
        source_hash: &[u8; 32],
        prior_target_hash: &[u8; 32],
        result_target_hash: &[u8; 32],
        manifest_relpath: &str,
        now: i64,
    ) -> Result<(), MergeServiceError> {
        if manifest_relpath.is_empty()
            || manifest_relpath.len() > 1_024
            || std::path::Path::new(manifest_relpath).is_absolute()
            || std::path::Path::new(manifest_relpath)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(MergeServiceError::Invalid(
                "lineage manifest path is unsafe".to_string(),
            ));
        }
        let mut conn = self.catalog.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job = tx
            .query_row(
                "SELECT source_space_id, target_space_id, state
                 FROM merge_jobs WHERE id=?1",
                [id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(MergeServiceError::Stale)?;
        if !matches!(job.2.as_str(), "promoting" | "verified") {
            return Err(MergeServiceError::Stale);
        }
        let lineage_id = format!("lineage:{id}");
        tx.execute(
            "INSERT INTO space_lineages(
                id, source_space_id, target_space_id, merge_job_id,
                source_snapshot_hash, prior_target_hash, result_target_hash,
                manifest_relpath, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO NOTHING",
            params![
                lineage_id,
                job.0,
                job.1,
                id.to_string(),
                source_hash.as_slice(),
                prior_target_hash.as_slice(),
                result_target_hash.as_slice(),
                manifest_relpath,
                now
            ],
        )?;
        tx.execute(
            "UPDATE memory_spaces SET catalog_generation=catalog_generation+1,
                updated_at=?1 WHERE id=?2",
            params![now, job.1],
        )?;
        let changed = tx.execute(
            "UPDATE merge_jobs SET state='succeeded', updated_at=?1
             WHERE id=?2 AND state IN ('promoting','verified')",
            params![now, id.to_string()],
        )?;
        if changed != 1 {
            return Err(MergeServiceError::Stale);
        }
        tx.commit()?;
        Ok(())
    }
}

fn confirmation_hash(token: &[u8; 32], id: MergeJobId, plan: &MergePlan) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"openmemory/merge-confirmation/v1\0");
    hasher.update(token);
    hasher.update(id.to_string().as_bytes());
    hasher.update(plan.plan_hash.as_bytes());
    hasher.update(plan.target_snapshot_hash.as_bytes());
    hasher.update(
        &plan
            .expected_target_version
            .semantic_generation
            .to_le_bytes(),
    );
    hasher.finalize()
}

fn valid_transition(expected: &str, next: &str) -> bool {
    matches!(
        (expected, next),
        ("discovering", "awaiting_review")
            | ("awaiting_review", "planned")
            | ("confirmed", "staging")
            | ("staging", "verified")
            | ("verified", "promoting")
            | ("promoting", "succeeded")
            | (_, "cancelled")
            | (_, "conflicted")
            | (_, "failed")
            | (_, "recovery_required")
    )
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

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode_32(value: &str) -> Result<[u8; 32], MergeServiceError> {
    if value.len() != 64 {
        return Err(MergeServiceError::Unauthorized);
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).map_err(|_| MergeServiceError::Unauthorized)?;
        bytes[index] = u8::from_str_radix(pair, 16).map_err(|_| MergeServiceError::Unauthorized)?;
    }
    Ok(bytes)
}
