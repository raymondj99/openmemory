//! Fsynced old-or-new promotion intent and idempotent recovery.

use std::path::{Component, Path, PathBuf};

use openmemory_core::config::Config;
use openmemory_core::space::{MergeJobId, SpaceId};
use openmemory_graph::{MemoryError, MemoryResult};
use serde::{Deserialize, Serialize};

use crate::partition::{DomainStore, DOMAINS_DIR, DOMAIN_MANIFEST_FILE};
use crate::space::capture_space_snapshot;

const INTENT_VERSION: u32 = 1;
const SINGLE_STORE_ENTRIES: &[&str] = &[
    "memory.sqlite",
    "memory.sqlite-wal",
    "memory.sqlite-shm",
    "fulltext.sqlite",
    "fulltext.sqlite-wal",
    "fulltext.sqlite-shm",
    "bm25.json",
    "metadata.sqlite",
    "metadata.sqlite-wal",
    "metadata.sqlite-shm",
    "vectors.bin",
    "vectors.usearch",
    "vectors.usearch.meta.json",
    "embeddings",
];

/// New spaces swap one dedicated store directory. The legacy personal-global
/// root moves only enumerated `DomainStore` artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionMode {
    Directory,
    LegacyArtifacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionPhase {
    Prepared,
    TargetBackedUp,
    StagingPromoted,
    PromotedVerified,
    CatalogCommitted,
    Complete,
}

/// Durable bounded intent. Every path is profile-relative and validated before
/// it is joined to the canonical profile root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePromotionIntent {
    pub version: u32,
    pub job_id: MergeJobId,
    pub source_space_id: SpaceId,
    pub target_space_id: SpaceId,
    pub mode: PromotionMode,
    pub domains: usize,
    pub live_root: String,
    pub staging_root: String,
    pub backup_root: String,
    pub old_target_hash: String,
    pub new_target_hash: String,
    pub plan_hash: String,
    pub phase: PromotionPhase,
}

impl MergePromotionIntent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        job_id: MergeJobId,
        source_space_id: SpaceId,
        target_space_id: SpaceId,
        mode: PromotionMode,
        domains: usize,
        live_root: String,
        staging_root: String,
        backup_root: String,
        old_target_hash: String,
        new_target_hash: String,
        plan_hash: String,
    ) -> MemoryResult<Self> {
        let intent = Self {
            version: INTENT_VERSION,
            job_id,
            source_space_id,
            target_space_id,
            mode,
            domains,
            live_root,
            staging_root,
            backup_root,
            old_target_hash,
            new_target_hash,
            plan_hash,
            phase: PromotionPhase::Prepared,
        };
        validate_intent(&intent)?;
        Ok(intent)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionReport {
    pub intent_path: PathBuf,
    pub backup_root: PathBuf,
    pub promoted_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionRecovery {
    NoIntent,
    ReadyForCatalogCommit,
    RolledBack,
    Complete,
}

/// Verify fresh hashes and disk/filesystem preconditions, then perform the
/// fsynced rename sequence. The caller must have paused target admissions and
/// closed every target registry handle.
pub fn promote_staged_root(
    config: &Config,
    profile_root: &Path,
    mut intent: MergePromotionIntent,
) -> MemoryResult<PromotionReport> {
    validate_intent(&intent)?;
    if intent.phase != PromotionPhase::Prepared {
        return Err(MemoryError::InvalidInput(
            "new promotion must begin in prepared phase".to_string(),
        ));
    }
    let roots = resolve_roots(profile_root, &intent)?;
    if roots.intent.exists() || roots.backup.exists() {
        return Err(MemoryError::InvalidInput(
            "promotion intent or backup already exists; recover first".to_string(),
        ));
    }
    if !roots.live.exists() || !roots.staging.exists() {
        return Err(MemoryError::InvalidInput(
            "live target or verified staging root is missing".to_string(),
        ));
    }
    ensure_same_filesystem(&roots.live, &roots.staging)?;
    let staging_bytes = tree_size(&roots.staging)?;
    let available_root = nearest_existing_parent(&roots.backup).ok_or_else(|| {
        MemoryError::InvalidInput("backup has no existing filesystem parent".to_string())
    })?;
    let available = fs4::available_space(available_root)?;
    let required = staging_bytes.saturating_add(staging_bytes / 10);
    if available < required {
        return Err(MemoryError::InvalidInput(format!(
            "insufficient staging/backup disk: need {required} bytes, have {available}"
        )));
    }
    verify_hash(
        config,
        &roots.live,
        intent.domains,
        intent.target_space_id,
        &intent.old_target_hash,
    )?;
    verify_hash(
        config,
        &roots.staging,
        intent.domains,
        intent.target_space_id,
        &intent.new_target_hash,
    )?;

    write_intent(&roots.intent, &intent)?;
    backup_live(&roots, intent.mode, intent.domains)?;
    intent.phase = PromotionPhase::TargetBackedUp;
    write_intent(&roots.intent, &intent)?;
    promote_staging(&roots, intent.mode, intent.domains)?;
    intent.phase = PromotionPhase::StagingPromoted;
    write_intent(&roots.intent, &intent)?;
    verify_hash(
        config,
        &roots.live,
        intent.domains,
        intent.target_space_id,
        &intent.new_target_hash,
    )?;
    intent.phase = PromotionPhase::PromotedVerified;
    write_intent(&roots.intent, &intent)?;
    Ok(PromotionReport {
        intent_path: roots.intent,
        backup_root: roots.backup,
        promoted_hash: intent.new_target_hash,
    })
}

/// Mark the product-catalog/lineage/job transaction durable, then clear the
/// filesystem intent. Backups remain for retention cleanup.
pub fn complete_promotion(
    profile_root: &Path,
    target_space: SpaceId,
    job_id: MergeJobId,
) -> MemoryResult<()> {
    let path = intent_path(profile_root, target_space);
    let mut intent = load_intent(&path)?;
    if intent.job_id != job_id
        || intent.target_space_id != target_space
        || intent.phase < PromotionPhase::PromotedVerified
    {
        return Err(MemoryError::InvalidInput(
            "promotion completion does not match verified intent".to_string(),
        ));
    }
    intent.phase = PromotionPhase::CatalogCommitted;
    write_intent(&path, &intent)?;
    intent.phase = PromotionPhase::Complete;
    write_intent(&path, &intent)?;
    std::fs::remove_file(&path)?;
    fsync_dir(profile_root)?;
    Ok(())
}

/// Recover every unambiguous old-or-new filesystem state. Ambiguous hashes or
/// extra/missing artifacts fail closed and leave the intent in place.
pub fn recover_promotion(
    config: &Config,
    profile_root: &Path,
    target_space: SpaceId,
) -> MemoryResult<PromotionRecovery> {
    let path = intent_path(profile_root, target_space);
    if !path.exists() {
        return Ok(PromotionRecovery::NoIntent);
    }
    let mut intent = load_intent(&path)?;
    validate_intent(&intent)?;
    if intent.target_space_id != target_space {
        return Err(recovery_required("intent target does not match filename"));
    }
    let roots = resolve_roots(profile_root, &intent)?;
    let live_hash = existing_hash(config, &roots.live, intent.domains, intent.target_space_id)?;
    let backup_hash = existing_hash(
        config,
        &roots.backup,
        intent.domains,
        intent.target_space_id,
    )?;
    let staging_hash = existing_hash(
        config,
        &roots.staging,
        intent.domains,
        intent.target_space_id,
    )?;

    if live_hash.as_deref() == Some(intent.new_target_hash.as_str())
        && backup_hash.as_deref() == Some(intent.old_target_hash.as_str())
    {
        intent.phase = PromotionPhase::PromotedVerified;
        write_intent(&roots.intent, &intent)?;
        return Ok(PromotionRecovery::ReadyForCatalogCommit);
    }
    if live_hash.as_deref() == Some(intent.old_target_hash.as_str())
        && backup_hash.is_none()
        && staging_hash.as_deref() == Some(intent.new_target_hash.as_str())
    {
        backup_live(&roots, intent.mode, intent.domains)?;
        intent.phase = PromotionPhase::TargetBackedUp;
        write_intent(&roots.intent, &intent)?;
        promote_staging(&roots, intent.mode, intent.domains)?;
        intent.phase = PromotionPhase::StagingPromoted;
        write_intent(&roots.intent, &intent)?;
        verify_hash(
            config,
            &roots.live,
            intent.domains,
            intent.target_space_id,
            &intent.new_target_hash,
        )?;
        intent.phase = PromotionPhase::PromotedVerified;
        write_intent(&roots.intent, &intent)?;
        return Ok(PromotionRecovery::ReadyForCatalogCommit);
    }
    if live_hash.is_none()
        && backup_hash.as_deref() == Some(intent.old_target_hash.as_str())
        && staging_hash.as_deref() == Some(intent.new_target_hash.as_str())
    {
        promote_staging(&roots, intent.mode, intent.domains)?;
        verify_hash(
            config,
            &roots.live,
            intent.domains,
            intent.target_space_id,
            &intent.new_target_hash,
        )?;
        intent.phase = PromotionPhase::PromotedVerified;
        write_intent(&roots.intent, &intent)?;
        return Ok(PromotionRecovery::ReadyForCatalogCommit);
    }
    if live_hash.is_none()
        && backup_hash.as_deref() == Some(intent.old_target_hash.as_str())
        && staging_hash.is_none()
    {
        restore_backup(&roots, intent.mode, intent.domains)?;
        verify_hash(
            config,
            &roots.live,
            intent.domains,
            intent.target_space_id,
            &intent.old_target_hash,
        )?;
        std::fs::remove_file(&roots.intent)?;
        fsync_dir(profile_root)?;
        return Ok(PromotionRecovery::RolledBack);
    }
    if intent.phase == PromotionPhase::Complete || intent.phase == PromotionPhase::CatalogCommitted
    {
        if live_hash.as_deref() == Some(intent.new_target_hash.as_str()) {
            std::fs::remove_file(&roots.intent)?;
            fsync_dir(profile_root)?;
            return Ok(PromotionRecovery::Complete);
        }
    }
    Err(recovery_required(
        "filesystem hashes do not match an old-or-new recovery state",
    ))
}

struct PromotionRoots {
    profile: PathBuf,
    live: PathBuf,
    staging: PathBuf,
    backup: PathBuf,
    intent: PathBuf,
}

fn resolve_roots(
    profile_root: &Path,
    intent: &MergePromotionIntent,
) -> MemoryResult<PromotionRoots> {
    let profile = profile_root.canonicalize()?;
    let live = if intent.mode == PromotionMode::LegacyArtifacts && intent.live_root == "." {
        profile.clone()
    } else {
        resolve_relative(&profile, &intent.live_root)?
    };
    let staging = resolve_relative(&profile, &intent.staging_root)?;
    let backup = resolve_relative(&profile, &intent.backup_root)?;
    if live == staging || live == backup || staging == backup {
        return Err(MemoryError::InvalidInput(
            "promotion roots must be distinct".to_string(),
        ));
    }
    Ok(PromotionRoots {
        intent: intent_path(&profile, intent.target_space_id),
        profile,
        live,
        staging,
        backup,
    })
}

fn resolve_relative(profile: &Path, relative: &str) -> MemoryResult<PathBuf> {
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.len() > 1_024
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(MemoryError::InvalidInput(
            "promotion root key is not a safe relative path".to_string(),
        ));
    }
    let joined = profile.join(path);
    if let Some(parent) = nearest_existing_parent(&joined) {
        let canonical = parent.canonicalize()?;
        if !canonical.starts_with(profile) {
            return Err(MemoryError::InvalidInput(
                "promotion root escapes profile".to_string(),
            ));
        }
    }
    Ok(joined)
}

fn validate_intent(intent: &MergePromotionIntent) -> MemoryResult<()> {
    if intent.version != INTENT_VERSION
        || intent.source_space_id == intent.target_space_id
        || intent.domains == 0
        || intent.domains > 1_024
        || !valid_hash(&intent.old_target_hash)
        || !valid_hash(&intent.new_target_hash)
        || !valid_hash(&intent.plan_hash)
    {
        return Err(MemoryError::InvalidInput(
            "merge promotion intent is invalid".to_string(),
        ));
    }
    Ok(())
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn intent_path(profile_root: &Path, target: SpaceId) -> PathBuf {
    profile_root.join(format!(".merge-intent-{target}.json"))
}

fn write_intent(path: &Path, intent: &MergePromotionIntent) -> MemoryResult<()> {
    let encoded = serde_json::to_vec(intent)?;
    if encoded.len() > 16 * 1024 {
        return Err(MemoryError::InvalidInput(
            "promotion intent exceeds 16 KiB".to_string(),
        ));
    }
    let temp = path.with_extension("json.tmp");
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, path)?;
    fsync_dir(
        path.parent()
            .ok_or_else(|| MemoryError::InvalidInput("intent has no parent".to_string()))?,
    )
}

fn load_intent(path: &Path) -> MemoryResult<MergePromotionIntent> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > 16 * 1024 {
        return Err(recovery_required("promotion intent exceeds 16 KiB"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| recovery_required(&format!("promotion intent is corrupt: {error}")))
}

fn verify_hash(
    config: &Config,
    root: &Path,
    domains: usize,
    space_id: SpaceId,
    expected: &str,
) -> MemoryResult<()> {
    let actual = existing_hash(config, root, domains, space_id)?.ok_or_else(|| {
        MemoryError::InvalidInput(format!("store root {} is missing", root.display()))
    })?;
    if actual != expected {
        return Err(MemoryError::ChangeSetStale(format!(
            "store hash moved: expected {expected}, found {actual}"
        )));
    }
    Ok(())
}

fn existing_hash(
    config: &Config,
    root: &Path,
    domains: usize,
    space_id: SpaceId,
) -> MemoryResult<Option<String>> {
    if !store_exists(root, domains) {
        return Ok(None);
    }
    let store = DomainStore::open_scoped(config, root, domains, space_id)?;
    let snapshot = capture_space_snapshot(&store)?;
    Ok(Some(snapshot.canonical.snapshot_hash.to_string()))
}

fn store_exists(root: &Path, domains: usize) -> bool {
    if domains == 1 {
        root.join(openmemory_graph::MEMORY_DB_FILE).is_file()
    } else {
        root.join(DOMAINS_DIR).is_dir() && root.join(DOMAIN_MANIFEST_FILE).is_file()
    }
}

fn backup_live(roots: &PromotionRoots, mode: PromotionMode, domains: usize) -> MemoryResult<()> {
    std::fs::create_dir_all(
        roots
            .backup
            .parent()
            .ok_or_else(|| MemoryError::InvalidInput("backup has no parent".to_string()))?,
    )?;
    match mode {
        PromotionMode::Directory => std::fs::rename(&roots.live, &roots.backup)?,
        PromotionMode::LegacyArtifacts => {
            std::fs::create_dir(&roots.backup)?;
            move_artifacts(&roots.live, &roots.backup, domains)?;
        }
    }
    fsync_dir(&roots.profile)
}

fn promote_staging(
    roots: &PromotionRoots,
    mode: PromotionMode,
    domains: usize,
) -> MemoryResult<()> {
    match mode {
        PromotionMode::Directory => std::fs::rename(&roots.staging, &roots.live)?,
        PromotionMode::LegacyArtifacts => move_artifacts(&roots.staging, &roots.live, domains)?,
    }
    fsync_dir(&roots.profile)
}

fn restore_backup(roots: &PromotionRoots, mode: PromotionMode, domains: usize) -> MemoryResult<()> {
    match mode {
        PromotionMode::Directory => std::fs::rename(&roots.backup, &roots.live)?,
        PromotionMode::LegacyArtifacts => move_artifacts(&roots.backup, &roots.live, domains)?,
    }
    fsync_dir(&roots.profile)
}

fn move_artifacts(source: &Path, target: &Path, domains: usize) -> MemoryResult<()> {
    std::fs::create_dir_all(target)?;
    let entries: &[&str] = if domains > 1 {
        &[DOMAINS_DIR, DOMAIN_MANIFEST_FILE]
    } else {
        SINGLE_STORE_ENTRIES
    };
    for entry in entries {
        let from = source.join(entry);
        if from.exists() {
            let to = target.join(entry);
            if to.exists() {
                return Err(recovery_required(
                    "artifact destination already exists during promotion",
                ));
            }
            std::fs::rename(from, to)?;
        }
    }
    fsync_dir(source)?;
    fsync_dir(target)
}

#[cfg(unix)]
fn ensure_same_filesystem(left: &Path, right: &Path) -> MemoryResult<()> {
    use std::os::unix::fs::MetadataExt;
    let left = nearest_existing_parent(left)
        .ok_or_else(|| MemoryError::InvalidInput("live root has no existing parent".to_string()))?;
    let right = nearest_existing_parent(right).ok_or_else(|| {
        MemoryError::InvalidInput("staging root has no existing parent".to_string())
    })?;
    if left.metadata()?.dev() != right.metadata()?.dev() {
        return Err(MemoryError::InvalidInput(
            "material merge requires one filesystem".to_string(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_same_filesystem(_left: &Path, _right: &Path) -> MemoryResult<()> {
    Err(MemoryError::InvalidInput(
        "material merge is not advertised on this platform".to_string(),
    ))
}

fn nearest_existing_parent(path: &Path) -> Option<&Path> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate);
        }
        current = candidate.parent();
    }
    None
}

fn tree_size(root: &Path) -> MemoryResult<u64> {
    fn walk(path: &Path, total: &mut u64, entries: &mut usize) -> MemoryResult<()> {
        *entries = entries.saturating_add(1);
        if *entries > 2_000_000 {
            return Err(MemoryError::InvalidInput(
                "staging tree exceeds entry bound".to_string(),
            ));
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Err(MemoryError::InvalidInput(
                "symlinks are forbidden in merge staging".to_string(),
            ));
        }
        if metadata.is_file() {
            *total = total.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                walk(&entry?.path(), total, entries)?;
            }
        }
        Ok(())
    }
    let mut total = 0;
    let mut entries = 0;
    walk(root, &mut total, &mut entries)?;
    Ok(total)
}

fn fsync_dir(path: &Path) -> MemoryResult<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn recovery_required(reason: &str) -> MemoryError {
    MemoryError::InvalidInput(format!("merge recovery required: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::materialize_snapshot;
    use openmemory_core::space::RevisionId;
    use openmemory_merge::canonical::{CanonicalEntity, CanonicalSpaceSnapshot};
    use std::collections::BTreeSet;

    fn snapshot(space: SpaceId, labels: &[&str]) -> CanonicalSpaceSnapshot {
        CanonicalSpaceSnapshot::new(
            space,
            1,
            labels
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    CanonicalEntity::new(
                        space,
                        format!("entity-{index}"),
                        RevisionId::new(),
                        (*label).to_string(),
                        BTreeSet::new(),
                        "concept".to_string(),
                        BTreeSet::new(),
                        String::new(),
                    )
                    .unwrap()
                })
                .collect(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn prepared(
        profile: &Path,
    ) -> (
        Config,
        MergePromotionIntent,
        CanonicalSpaceSnapshot,
        CanonicalSpaceSnapshot,
    ) {
        let config = Config::default();
        let source = SpaceId::new();
        let target = SpaceId::new();
        let old = snapshot(target, &["old"]);
        let new = snapshot(target, &["old", "new"]);
        let live = profile
            .join("spaces")
            .join(target.to_string())
            .join("store");
        let stage = profile.join(".merge-staging").join("job");
        std::fs::create_dir_all(live.parent().unwrap()).unwrap();
        std::fs::create_dir_all(stage.parent().unwrap()).unwrap();
        materialize_snapshot(&config, &live, 1, &old).unwrap();
        materialize_snapshot(&config, &stage, 1, &new).unwrap();
        let intent = MergePromotionIntent::new(
            MergeJobId::new(),
            source,
            target,
            PromotionMode::Directory,
            1,
            format!("spaces/{target}/store"),
            ".merge-staging/job".to_string(),
            ".merge-backup/job".to_string(),
            old.snapshot_hash.to_string(),
            new.snapshot_hash.to_string(),
            format!("blake3:{}", "11".repeat(32)),
        )
        .unwrap();
        (config, intent, old, new)
    }

    #[test]
    fn directory_promotion_is_verified_and_recovery_is_idempotent() {
        let profile = tempfile::tempdir().unwrap();
        let (config, intent, _old, new) = prepared(profile.path());
        let job = intent.job_id;
        let target = intent.target_space_id;
        let report = promote_staged_root(&config, profile.path(), intent).unwrap();
        assert_eq!(report.promoted_hash, new.snapshot_hash.to_string());
        assert_eq!(
            recover_promotion(&config, profile.path(), target).unwrap(),
            PromotionRecovery::ReadyForCatalogCommit
        );
        complete_promotion(profile.path(), target, job).unwrap();
        assert_eq!(
            recover_promotion(&config, profile.path(), target).unwrap(),
            PromotionRecovery::NoIntent
        );
        assert!(report.backup_root.exists());
    }

    #[test]
    fn recovery_resumes_after_target_backup_boundary() {
        let profile = tempfile::tempdir().unwrap();
        let (config, mut intent, _old, _new) = prepared(profile.path());
        let roots = resolve_roots(profile.path(), &intent).unwrap();
        write_intent(&roots.intent, &intent).unwrap();
        backup_live(&roots, intent.mode, intent.domains).unwrap();
        intent.phase = PromotionPhase::TargetBackedUp;
        write_intent(&roots.intent, &intent).unwrap();
        assert_eq!(
            recover_promotion(&config, profile.path(), intent.target_space_id).unwrap(),
            PromotionRecovery::ReadyForCatalogCommit
        );
        assert_eq!(
            recover_promotion(&config, profile.path(), intent.target_space_id).unwrap(),
            PromotionRecovery::ReadyForCatalogCommit
        );
    }
}
