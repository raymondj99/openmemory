//! Deterministic three-way merge and crash-safe materialization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{AuditStore, ChangeSetDraft, DraftOp, MemorySnapshot, SubmitMode};
use crate::{PocError, PocResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticMemory {
    pub logical_id: String,
    pub content: String,
    pub deleted: bool,
}

impl From<&MemorySnapshot> for SemanticMemory {
    fn from(value: &MemorySnapshot) -> Self {
        Self {
            logical_id: value.logical_id.clone(),
            content: value.content.clone(),
            deleted: value.deleted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSnapshot {
    pub hash: String,
    pub memories: BTreeMap<String, MemorySnapshot>,
}

impl GraphSnapshot {
    /// Read a canonical semantic snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when storage access or hashing fails.
    pub fn read(store: &AuditStore) -> PocResult<Self> {
        let memories = store
            .snapshot()?
            .into_iter()
            .map(|memory| (memory.logical_id.clone(), memory))
            .collect();
        let hash = semantic_hash(&memories)?;
        Ok(Self { hash, memories })
    }

    /// Construct and hash a canonical snapshot from memory heads.
    ///
    /// # Errors
    ///
    /// Returns an error when canonical serialization fails.
    pub fn from_memories(memories: impl IntoIterator<Item = MemorySnapshot>) -> PocResult<Self> {
        let memories = memories
            .into_iter()
            .map(|memory| (memory.logical_id.clone(), memory))
            .collect::<BTreeMap<_, _>>();
        let hash = semantic_hash(&memories)?;
        Ok(Self { hash, memories })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeAction {
    Put {
        logical_id: String,
        content: String,
        expected_version: Option<u64>,
    },
    Delete {
        logical_id: String,
        expected_version: u64,
    },
}

impl MergeAction {
    fn logical_id(&self) -> &str {
        match self {
            Self::Put { logical_id, .. } | Self::Delete { logical_id, .. } => logical_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeConflict {
    pub logical_id: String,
    pub base: Option<SemanticMemory>,
    pub source: Option<SemanticMemory>,
    pub target: Option<SemanticMemory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergePlan {
    pub id: String,
    pub base_hash: String,
    pub source_hash: String,
    pub target_hash: String,
    pub result_hash: String,
    pub actions: Vec<MergeAction>,
    pub conflicts: Vec<MergeConflict>,
}

impl MergePlan {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Plan a directional `source -> target` merge against their common base.
///
/// # Errors
///
/// Returns an error when the predicted result cannot be serialized and
/// hashed.
pub fn plan_merge(
    base: &GraphSnapshot,
    source: &GraphSnapshot,
    target: &GraphSnapshot,
) -> PocResult<MergePlan> {
    let ids = base
        .memories
        .keys()
        .chain(source.memories.keys())
        .chain(target.memories.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut actions = Vec::new();
    let mut conflicts = Vec::new();

    for logical_id in ids {
        let base_value = base.memories.get(&logical_id);
        let source_value = source.memories.get(&logical_id);
        let target_value = target.memories.get(&logical_id);
        let base_semantic = base_value.map(SemanticMemory::from);
        let source_semantic = source_value.map(SemanticMemory::from);
        let target_semantic = target_value.map(SemanticMemory::from);

        if source_semantic == base_semantic || source_semantic == target_semantic {
            continue;
        }
        if target_semantic == base_semantic {
            if let Some(action) = action_for(source_value, target_value) {
                actions.push(action);
            }
            continue;
        }
        conflicts.push(MergeConflict {
            logical_id,
            base: base_semantic,
            source: source_semantic,
            target: target_semantic,
        });
    }

    actions.sort_by(|left, right| left.logical_id().cmp(right.logical_id()));
    conflicts.sort_by(|left, right| left.logical_id.cmp(&right.logical_id));
    let result_hash = predicted_result_hash(target, &actions)?;
    Ok(MergePlan {
        id: Uuid::now_v7().to_string(),
        base_hash: base.hash.clone(),
        source_hash: source.hash.clone(),
        target_hash: target.hash.clone(),
        result_hash,
        actions,
        conflicts,
    })
}

fn action_for(
    source: Option<&MemorySnapshot>,
    target: Option<&MemorySnapshot>,
) -> Option<MergeAction> {
    match source {
        Some(source) if !source.deleted => Some(MergeAction::Put {
            logical_id: source.logical_id.clone(),
            content: source.content.clone(),
            expected_version: target.map(|value| value.row_version),
        }),
        Some(source) => target
            .filter(|value| !value.deleted)
            .map(|target| MergeAction::Delete {
                logical_id: source.logical_id.clone(),
                expected_version: target.row_version,
            }),
        None => target
            .filter(|value| !value.deleted)
            .map(|target| MergeAction::Delete {
                logical_id: target.logical_id.clone(),
                expected_version: target.row_version,
            }),
    }
}

fn predicted_result_hash(target: &GraphSnapshot, actions: &[MergeAction]) -> PocResult<String> {
    let mut semantic = target
        .memories
        .iter()
        .map(|(id, value)| (id.clone(), SemanticMemory::from(value)))
        .collect::<BTreeMap<_, _>>();
    for action in actions {
        match action {
            MergeAction::Put {
                logical_id,
                content,
                ..
            } => {
                semantic.insert(
                    logical_id.clone(),
                    SemanticMemory {
                        logical_id: logical_id.clone(),
                        content: content.clone(),
                        deleted: false,
                    },
                );
            }
            MergeAction::Delete { logical_id, .. } => {
                if let Some(value) = semantic.get_mut(logical_id) {
                    value.deleted = true;
                }
            }
        }
    }
    hash_semantic_map(&semantic)
}

/// Apply a clean plan as one audited target transaction.
///
/// # Errors
///
/// Returns an error for unresolved conflicts, a moved target, a failed
/// transaction, or a post-apply verification mismatch.
pub fn apply_plan(store: &AuditStore, plan: &MergePlan, actor: &str) -> PocResult<()> {
    if !plan.is_clean() {
        return Err(PocError::Conflict(format!(
            "merge plan has {} unresolved conflicts",
            plan.conflicts.len()
        )));
    }
    let before = GraphSnapshot::read(store)?;
    if before.hash != plan.target_hash {
        return Err(PocError::Conflict(format!(
            "merge target moved: planned {}, found {}",
            plan.target_hash, before.hash
        )));
    }
    if plan.actions.is_empty() {
        return Ok(());
    }
    let ops = plan
        .actions
        .iter()
        .map(|action| match action {
            MergeAction::Put {
                logical_id,
                content,
                expected_version,
            } => DraftOp::MergePut {
                logical_id: logical_id.clone(),
                expected_version: *expected_version,
                content: content.clone(),
            },
            MergeAction::Delete {
                logical_id,
                expected_version,
            } => DraftOp::Delete {
                logical_id: logical_id.clone(),
                expected_version: *expected_version,
            },
        })
        .collect();
    let draft = ChangeSetDraft::new(
        format!("merge:{}", plan.id),
        actor,
        format!("material merge {}", plan.id),
        ops,
    );
    store.submit(&draft, SubmitMode::Apply)?;
    let after = GraphSnapshot::read(store)?;
    if after.hash != plan.result_hash {
        return Err(PocError::Invalid(format!(
            "post-merge verification failed: expected {}, found {}",
            plan.result_hash, after.hash
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapCrashPoint {
    AfterIntent,
    AfterBackupRename,
    AfterPromotion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeReport {
    pub target_hash: String,
    pub backup_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryStatus {
    NoIntent,
    Recovered(MaterializeReport),
}

#[derive(Debug, Serialize, Deserialize)]
struct SwapIntent {
    plan_id: String,
    target_root: PathBuf,
    staging_root: PathBuf,
    backup_root: PathBuf,
    expected_before_hash: String,
    expected_after_hash: String,
}

/// Build and validate a complete target copy, then promote it by directory
/// rename. The old target is retained as a recovery artifact.
///
/// # Errors
///
/// Returns an error for a conflicted or stale plan, existing recovery
/// artifacts, failed storage operations, or verification mismatch.
pub fn materialize_plan(
    target: &AuditStore,
    plan: &MergePlan,
    actor: &str,
    crash_point: Option<SwapCrashPoint>,
) -> PocResult<MaterializeReport> {
    if !plan.is_clean() {
        return Err(PocError::Conflict(
            "cannot materialize a conflicted merge plan".to_string(),
        ));
    }
    let current = GraphSnapshot::read(target)?;
    if current.hash != plan.target_hash {
        return Err(PocError::Conflict("merge target moved".to_string()));
    }
    let target_root = target.root().to_path_buf();
    let parent = target_root.parent().ok_or_else(|| {
        PocError::Invalid("merge target must have a parent directory".to_string())
    })?;
    let name = target_root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PocError::Invalid("merge target name is not UTF-8".to_string()))?;
    let staging_root = parent.join(format!(".{name}.merge-staging-{}", plan.id));
    let backup_root = parent.join(format!(".{name}.merge-backup-{}", plan.id));
    let intent_path = intent_path(&target_root)?;
    if intent_path.exists() || staging_root.exists() || backup_root.exists() {
        return Err(PocError::Conflict(
            "merge staging, backup, or intent already exists".to_string(),
        ));
    }

    let staged = target.snapshot_to(&staging_root)?;
    apply_plan(&staged, plan, actor)?;
    let staged_hash = GraphSnapshot::read(&staged)?.hash;
    if staged_hash != plan.result_hash {
        return Err(PocError::Invalid(
            "staged graph did not match planned result".to_string(),
        ));
    }
    drop(staged);

    let intent = SwapIntent {
        plan_id: plan.id.clone(),
        target_root: target_root.clone(),
        staging_root: staging_root.clone(),
        backup_root: backup_root.clone(),
        expected_before_hash: plan.target_hash.clone(),
        expected_after_hash: plan.result_hash.clone(),
    };
    write_intent(&intent_path, &intent)?;
    sync_directory(parent)?;
    maybe_abort(crash_point, SwapCrashPoint::AfterIntent);

    std::fs::rename(&target_root, &backup_root)?;
    sync_directory(parent)?;
    maybe_abort(crash_point, SwapCrashPoint::AfterBackupRename);

    std::fs::rename(&staging_root, &target_root)?;
    sync_directory(parent)?;
    maybe_abort(crash_point, SwapCrashPoint::AfterPromotion);

    finish_recovery(&intent_path, &intent)
}

/// Roll an interrupted material merge forward to its verified new graph.
///
/// # Errors
///
/// Returns an error when the durable state is ambiguous, corrupt, or cannot
/// be promoted and verified.
pub fn recover_materialized_merge(target_root: &Path) -> PocResult<RecoveryStatus> {
    let path = intent_path(target_root)?;
    if !path.exists() {
        return Ok(RecoveryStatus::NoIntent);
    }
    let intent: SwapIntent = serde_json::from_slice(&std::fs::read(&path)?)?;
    finish_recovery(&path, &intent).map(RecoveryStatus::Recovered)
}

fn finish_recovery(path: &Path, intent: &SwapIntent) -> PocResult<MaterializeReport> {
    let target_exists = intent.target_root.exists();
    let staging_exists = intent.staging_root.exists();
    let backup_exists = intent.backup_root.exists();
    let parent = intent.target_root.parent().ok_or_else(|| {
        PocError::Invalid("merge target must have a parent directory".to_string())
    })?;

    match (target_exists, staging_exists, backup_exists) {
        // Intent is durable but promotion has not started. Roll forward only
        // after validating both old and staged graphs.
        (true, true, false) => {
            verify_hash(&intent.target_root, &intent.expected_before_hash)?;
            verify_hash(&intent.staging_root, &intent.expected_after_hash)?;
            std::fs::rename(&intent.target_root, &intent.backup_root)?;
            sync_directory(parent)?;
            std::fs::rename(&intent.staging_root, &intent.target_root)?;
            sync_directory(parent)?;
        }
        // Old target is safely backed up; staged target is ready to promote.
        (false, true, true) => {
            verify_hash(&intent.backup_root, &intent.expected_before_hash)?;
            verify_hash(&intent.staging_root, &intent.expected_after_hash)?;
            std::fs::rename(&intent.staging_root, &intent.target_root)?;
            sync_directory(parent)?;
        }
        // Promotion completed before the process died.
        (true, false, true) => {}
        state => {
            return Err(PocError::Invalid(format!(
                "unrecoverable merge filesystem state target/staging/backup={state:?}"
            )));
        }
    }

    verify_hash(&intent.target_root, &intent.expected_after_hash)?;
    verify_hash(&intent.backup_root, &intent.expected_before_hash)?;
    std::fs::remove_file(path)?;
    sync_directory(parent)?;
    Ok(MaterializeReport {
        target_hash: intent.expected_after_hash.clone(),
        backup_root: intent.backup_root.clone(),
    })
}

fn verify_hash(root: &Path, expected: &str) -> PocResult<()> {
    let actual = GraphSnapshot::read(&AuditStore::open(root)?)?.hash;
    if actual != expected {
        return Err(PocError::Invalid(format!(
            "graph verification failed at {}: expected {expected}, found {actual}",
            root.display()
        )));
    }
    Ok(())
}

fn intent_path(target_root: &Path) -> PocResult<PathBuf> {
    let parent = target_root.parent().ok_or_else(|| {
        PocError::Invalid("merge target must have a parent directory".to_string())
    })?;
    let name = target_root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PocError::Invalid("merge target name is not UTF-8".to_string()))?;
    Ok(parent.join(format!(".{name}.merge-intent.json")))
}

fn write_intent(path: &Path, intent: &SwapIntent) -> PocResult<()> {
    let temporary = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(intent)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&json)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn sync_directory(path: &Path) -> PocResult<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn semantic_hash(memories: &BTreeMap<String, MemorySnapshot>) -> PocResult<String> {
    let semantic = memories
        .iter()
        .map(|(id, memory)| (id.clone(), SemanticMemory::from(memory)))
        .collect::<BTreeMap<_, _>>();
    hash_semantic_map(&semantic)
}

fn hash_semantic_map(memories: &BTreeMap<String, SemanticMemory>) -> PocResult<String> {
    let json = serde_json::to_vec(memories)?;
    Ok(blake3::hash(&json).to_hex().to_string())
}

fn maybe_abort(configured: Option<SwapCrashPoint>, reached: SwapCrashPoint) {
    if configured == Some(reached) {
        std::process::abort();
    }
}
