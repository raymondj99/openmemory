use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use openmemory_admin::{
    AdminBackupCreateReport, AdminBackupManifest, AdminBackupPreflightResponse, AdminBackupRequest,
    AdminDiagnostic, AdminError, AdminErrorCode, AdminJobState, AdminRestorePreflightRequest,
    AdminRestorePreflightResponse, AdminRestoreReport, AdminRestoreRequest, ADMIN_API_VERSION,
};
use openmemory_engine::partition::DomainStore;
use openmemory_graph::new_id;

use crate::state::JobRegistry;
use crate::{load_config, profile_data_dir, store_admin_error, unix_now_secs, DaemonConfig};

pub(crate) fn backup_preflight(
    config: &DaemonConfig,
    request: &AdminBackupRequest,
) -> AdminBackupPreflightResponse {
    let source_dir = profile_data_dir(config.home(), config.active_profile());
    let destination_dir = request
        .destination_dir
        .as_ref()
        .map_or_else(|| config.home().join("backups"), PathBuf::from);
    let mut diagnostics = Vec::new();

    if !source_dir.exists() {
        diagnostics.push(AdminDiagnostic {
            component: "backup".into(),
            code: AdminErrorCode::ProfileNotInitialized,
            message: "active profile is not initialized on disk".into(),
            hint: Some("Run `openmemory init` for this profile.".into()),
            details: serde_json::json!({ "source_dir": source_dir.display().to_string() }),
        });
    } else {
        match path_is_same_or_descendant(&destination_dir, &source_dir) {
            Ok(true) => diagnostics.push(AdminDiagnostic {
                component: "backup".into(),
                code: AdminErrorCode::BackupPreflightFailed,
                message: "backup destination cannot be inside the profile data directory".into(),
                hint: Some(
                    "Choose a destination outside the active profile data directory.".into(),
                ),
                details: serde_json::json!({
                    "source_dir": source_dir.display().to_string(),
                    "destination_dir": destination_dir.display().to_string(),
                }),
            }),
            Ok(false) => {}
            Err(error) => diagnostics.push(AdminDiagnostic {
                component: "backup".into(),
                code: AdminErrorCode::BackupPreflightFailed,
                message: "backup destination containment could not be checked".into(),
                hint: Some("Choose a destination with readable parent directories.".into()),
                details: serde_json::json!({
                    "source_dir": source_dir.display().to_string(),
                    "destination_dir": destination_dir.display().to_string(),
                    "error": error.to_string(),
                }),
            }),
        }
    }

    if let Some(diagnostic) = backup_destination_diagnostic(&destination_dir) {
        diagnostics.push(diagnostic);
    }

    let (estimated_files, estimated_bytes) = match directory_size(&source_dir) {
        Ok(size) => size,
        Err(error) => {
            diagnostics.push(AdminDiagnostic {
                component: "backup".into(),
                code: AdminErrorCode::BackupPreflightFailed,
                message: "profile directory could not be scanned".into(),
                hint: None,
                details: serde_json::json!({ "error": error.to_string() }),
            });
            (0, 0)
        }
    };

    AdminBackupPreflightResponse {
        profile: config.active_profile().to_string(),
        source_dir: source_dir.display().to_string(),
        destination_dir: destination_dir.display().to_string(),
        estimated_files,
        estimated_bytes,
        ready: diagnostics.is_empty(),
        diagnostics,
    }
}

pub(crate) fn restore_preflight(
    request: &AdminRestorePreflightRequest,
) -> AdminRestorePreflightResponse {
    let backup_dir = PathBuf::from(&request.backup_dir);
    let manifest_path = backup_dir.join("openmemory-backup.json");
    let mut diagnostics = Vec::new();
    if !backup_dir.is_dir() {
        diagnostics.push(AdminDiagnostic {
            component: "restore".into(),
            code: AdminErrorCode::RestorePreflightFailed,
            message: "backup directory does not exist".into(),
            hint: Some("Choose a directory produced by `POST /admin/backup/create`.".into()),
            details: serde_json::json!({ "backup_dir": backup_dir.display().to_string() }),
        });
    }
    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => match serde_json::from_str::<AdminBackupManifest>(&text) {
            Ok(manifest) => {
                if manifest.api_version != ADMIN_API_VERSION {
                    diagnostics.push(AdminDiagnostic {
                        component: "restore".into(),
                        code: AdminErrorCode::RestorePreflightFailed,
                        message: "backup manifest API version is not supported".into(),
                        hint: Some(format!("Expected backup API version {ADMIN_API_VERSION}.")),
                        details: serde_json::json!({ "api_version": manifest.api_version }),
                    });
                }
                match directory_size(&backup_dir) {
                    Ok((files, bytes)) => {
                        if files < manifest.files_copied.saturating_add(1)
                            || bytes < manifest.bytes_copied
                        {
                            diagnostics.push(AdminDiagnostic {
                                component: "restore".into(),
                                code: AdminErrorCode::RestorePreflightFailed,
                                message: "backup contents do not match the manifest".into(),
                                hint: Some(
                                    "Create a new backup or choose an intact backup directory."
                                        .into(),
                                ),
                                details: serde_json::json!({
                                    "manifest_files": manifest.files_copied,
                                    "actual_files": files,
                                    "manifest_bytes": manifest.bytes_copied,
                                    "actual_bytes": bytes,
                                }),
                            });
                        }
                    }
                    Err(error) => diagnostics.push(AdminDiagnostic {
                        component: "restore".into(),
                        code: AdminErrorCode::RestorePreflightFailed,
                        message: "backup directory could not be scanned".into(),
                        hint: None,
                        details: serde_json::json!({ "error": error.to_string() }),
                    }),
                }
                Some(manifest)
            }
            Err(error) => {
                diagnostics.push(AdminDiagnostic {
                    component: "restore".into(),
                    code: AdminErrorCode::RestorePreflightFailed,
                    message: "backup manifest is invalid".into(),
                    hint: None,
                    details: serde_json::json!({ "error": error.to_string() }),
                });
                None
            }
        },
        Err(error) => {
            diagnostics.push(AdminDiagnostic {
                component: "restore".into(),
                code: AdminErrorCode::RestorePreflightFailed,
                message: "backup manifest could not be read".into(),
                hint: None,
                details: serde_json::json!({
                    "manifest_path": manifest_path.display().to_string(),
                    "error": error.to_string(),
                }),
            });
            None
        }
    };

    AdminRestorePreflightResponse {
        backup_dir: backup_dir.display().to_string(),
        ready: diagnostics.is_empty(),
        manifest,
        diagnostics,
    }
}

pub(crate) fn spawn_backup_create_job(
    config: DaemonConfig,
    request: AdminBackupRequest,
    job_id: String,
    jobs: Arc<JobRegistry>,
) {
    tokio::spawn(async move {
        let _ = jobs.update(&job_id, |job| {
            job.state = AdminJobState::Running;
            job.started_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
            job.message = Some("backup running".into());
        });
        let result =
            tokio::task::spawn_blocking(move || backup_create_blocking(&config, &request)).await;
        match result {
            Ok(Ok(report)) => {
                let result = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Succeeded;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("backup completed".into());
                    job.result = result;
                    job.error = None;
                });
            }
            Ok(Err(error)) => {
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Failed;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("backup failed".into());
                    job.error = Some(error);
                });
            }
            Err(error) => {
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Failed;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("backup worker failed".into());
                    job.error = Some(
                        AdminError::new(
                            AdminErrorCode::Internal,
                            "backup worker failed",
                            Option::<String>::None,
                            true,
                        )
                        .with_details(serde_json::json!({ "error": error.to_string() })),
                    );
                });
            }
        }
    });
}

pub(crate) fn spawn_restore_job(
    config: DaemonConfig,
    request: AdminRestoreRequest,
    job_id: String,
    jobs: Arc<JobRegistry>,
) {
    tokio::spawn(async move {
        let _ = jobs.update(&job_id, |job| {
            job.state = AdminJobState::Running;
            job.started_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
            job.message = Some("restore running".into());
        });
        let result = tokio::task::spawn_blocking(move || restore_blocking(&config, &request)).await;
        match result {
            Ok(Ok(report)) => {
                let result = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Succeeded;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("restore completed".into());
                    job.result = result;
                    job.error = None;
                });
            }
            Ok(Err(error)) => {
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Failed;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("restore failed".into());
                    job.error = Some(error);
                });
            }
            Err(error) => {
                let _ = jobs.update(&job_id, |job| {
                    job.state = AdminJobState::Failed;
                    job.finished_at_unix_secs = Some(unix_now_secs().unwrap_or(0));
                    job.message = Some("restore worker failed".into());
                    job.error = Some(
                        AdminError::new(
                            AdminErrorCode::Internal,
                            "restore worker failed",
                            Option::<String>::None,
                            true,
                        )
                        .with_details(serde_json::json!({ "error": error.to_string() })),
                    );
                });
            }
        }
    });
}

fn backup_create_blocking(
    config: &DaemonConfig,
    request: &AdminBackupRequest,
) -> Result<AdminBackupCreateReport, AdminError> {
    let preflight = backup_preflight(config, request);
    if !preflight.ready {
        return Err(AdminError::new(
            AdminErrorCode::BackupPreflightFailed,
            "backup preflight failed",
            Some("Resolve diagnostics before creating a backup."),
            false,
        )
        .with_details(serde_json::json!({ "preflight": preflight })));
    }
    let source_dir = PathBuf::from(&preflight.source_dir);
    let destination_dir = PathBuf::from(&preflight.destination_dir);
    std::fs::create_dir_all(&destination_dir).map_err(|error| {
        AdminError::new(
            AdminErrorCode::BackupPreflightFailed,
            "backup destination could not be created",
            Option::<String>::None,
            false,
        )
        .with_details(serde_json::json!({ "error": error.to_string() }))
    })?;
    fsync_parent_dir(&destination_dir).map_err(admin_backup_io)?;
    checkpoint_profile_for_backup(config, &source_dir)?;

    let created_at = unix_now_secs().unwrap_or(0);
    let unique = new_id();
    let backup_name = format!("{}-{created_at}-{}", config.active_profile(), &unique[..8]);
    let final_dir = destination_dir.join(&backup_name);
    let staging_dir = destination_dir.join(format!(".{backup_name}.tmp"));
    ensure_destination_outside_source(&final_dir, &source_dir).map_err(admin_backup_io)?;
    ensure_destination_outside_source(&staging_dir, &source_dir).map_err(admin_backup_io)?;
    if staging_dir.exists() {
        std::fs::remove_dir_all(&staging_dir).map_err(admin_backup_io)?;
    }
    let result = (|| {
        copy_dir_recursive(&source_dir, &staging_dir)?;
        fsync_dir(&staging_dir)?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(admin_backup_io(error));
    }
    let (files_copied, bytes_copied) = directory_size(&staging_dir).map_err(admin_backup_io)?;
    let manifest = AdminBackupManifest {
        api_version: ADMIN_API_VERSION.to_string(),
        profile: config.active_profile().to_string(),
        created_at_unix_secs: created_at,
        source_dir: source_dir.display().to_string(),
        files_copied,
        bytes_copied,
    };
    let manifest_path = staging_dir.join("openmemory-backup.json");
    let manifest_text = serde_json::to_string_pretty(&manifest)
        .map(|text| format!("{text}\n"))
        .map_err(|error| {
            AdminError::new(
                AdminErrorCode::Internal,
                "backup manifest could not be encoded",
                Option::<String>::None,
                true,
            )
            .with_details(serde_json::json!({ "error": error.to_string() }))
        })?;
    write_backup_manifest(&manifest_path, manifest_text.as_bytes()).map_err(admin_backup_io)?;
    if final_dir.exists() {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(AdminError::new(
            AdminErrorCode::Conflict,
            "backup destination already exists",
            Option::<String>::None,
            false,
        )
        .with_details(serde_json::json!({ "backup_dir": final_dir.display().to_string() })));
    }
    if let Err(error) = std::fs::rename(&staging_dir, &final_dir) {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(admin_backup_io(error));
    }
    fsync_dir(&destination_dir).map_err(admin_backup_io)?;
    Ok(AdminBackupCreateReport {
        backup_dir: final_dir.display().to_string(),
        manifest_path: final_dir
            .join("openmemory-backup.json")
            .display()
            .to_string(),
        manifest,
    })
}

fn restore_blocking(
    config: &DaemonConfig,
    request: &AdminRestoreRequest,
) -> Result<AdminRestoreReport, AdminError> {
    let preflight = restore_preflight(&AdminRestorePreflightRequest {
        backup_dir: request.backup_dir.clone(),
    });
    if !preflight.ready {
        return Err(AdminError::new(
            AdminErrorCode::RestorePreflightFailed,
            "restore preflight failed",
            Some("Resolve diagnostics before restoring a backup."),
            false,
        )
        .with_details(serde_json::json!({ "preflight": preflight })));
    }
    let manifest = preflight.manifest.ok_or_else(|| {
        AdminError::new(
            AdminErrorCode::RestorePreflightFailed,
            "restore manifest was not available after preflight",
            Option::<String>::None,
            false,
        )
    })?;
    let target_profile = request
        .target_profile
        .clone()
        .unwrap_or_else(|| manifest.profile.clone());
    validate_restore_target_profile(&target_profile)?;
    let target_dir = profile_data_dir(config.home(), &target_profile);
    if target_dir.exists() && !request.replace_existing {
        return Err(AdminError::new(
            AdminErrorCode::Conflict,
            "restore target profile already exists",
            Some("Set replace_existing to true after taking a fresh backup."),
            false,
        )
        .with_details(serde_json::json!({
            "target_profile": target_profile,
            "target_dir": target_dir.display().to_string(),
        })));
    }

    let loaded_config = load_config(config.home()).map_err(|error| {
        AdminError::new(
            AdminErrorCode::ConfigInvalid,
            "OpenMemory config could not be loaded",
            Some("Fix config.toml and retry."),
            false,
        )
        .with_details(serde_json::json!({ "error": error }))
    })?;
    let backup_dir = PathBuf::from(&request.backup_dir);
    let data_root = config.home().join("data");
    std::fs::create_dir_all(&data_root).map_err(admin_restore_io)?;
    let unique = new_id();
    let staging_dir = data_root.join(format!(".restore-{target_profile}-{}.tmp", &unique[..8]));
    if staging_dir.exists() {
        std::fs::remove_dir_all(&staging_dir).map_err(admin_restore_io)?;
    }
    let result = (|| {
        copy_dir_recursive(&backup_dir, &staging_dir)?;
        fsync_dir(&staging_dir)?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(admin_restore_io(error));
    }

    DomainStore::open_existing(&loaded_config, &staging_dir)
        .map_err(|error| store_admin_error("restored memory store is unreadable", error))?;

    let mut previous_dir = None;
    let mut replaced_existing = false;
    let old_dir = if target_dir.exists() {
        let backup_existing = data_root.join(format!(
            ".restore-previous-{target_profile}-{}",
            &unique[..8]
        ));
        std::fs::rename(&target_dir, &backup_existing).map_err(admin_restore_io)?;
        fsync_dir(&data_root).map_err(admin_restore_io)?;
        previous_dir = Some(backup_existing.display().to_string());
        replaced_existing = true;
        Some(backup_existing)
    } else {
        None
    };

    if let Err(error) = std::fs::rename(&staging_dir, &target_dir) {
        if let Some(old_dir) = old_dir.as_ref() {
            let _ = std::fs::rename(old_dir, &target_dir);
        }
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(admin_restore_io(error));
    }
    fsync_dir(&data_root).map_err(admin_restore_io)?;

    Ok(AdminRestoreReport {
        backup_dir: backup_dir.display().to_string(),
        restored_profile: target_profile,
        restored_dir: target_dir.display().to_string(),
        replaced_existing,
        previous_dir,
    })
}

fn admin_restore_io(error: std::io::Error) -> AdminError {
    AdminError::new(
        AdminErrorCode::RestorePreflightFailed,
        "restore filesystem operation failed",
        Option::<String>::None,
        false,
    )
    .with_details(serde_json::json!({ "error": error.to_string() }))
}

fn admin_backup_io(error: std::io::Error) -> AdminError {
    AdminError::new(
        AdminErrorCode::BackupPreflightFailed,
        "backup filesystem operation failed",
        Option::<String>::None,
        false,
    )
    .with_details(serde_json::json!({ "error": error.to_string() }))
}

fn backup_destination_diagnostic(destination_dir: &Path) -> Option<AdminDiagnostic> {
    if destination_dir.exists() && !destination_dir.is_dir() {
        return Some(AdminDiagnostic {
            component: "backup".into(),
            code: AdminErrorCode::BackupPreflightFailed,
            message: "backup destination is not a directory".into(),
            hint: Some("Choose a writable directory for backups.".into()),
            details: serde_json::json!({ "destination_dir": destination_dir.display().to_string() }),
        });
    }

    let probe_dir = if destination_dir.exists() {
        destination_dir.to_path_buf()
    } else {
        existing_ancestor(destination_dir).unwrap_or_else(|| PathBuf::from("."))
    };
    if !probe_dir.is_dir() {
        return Some(AdminDiagnostic {
            component: "backup".into(),
            code: AdminErrorCode::BackupPreflightFailed,
            message: "backup destination parent is not a directory".into(),
            hint: Some("Choose a writable directory for backups.".into()),
            details: serde_json::json!({
                "destination_dir": destination_dir.display().to_string(),
                "probe_dir": probe_dir.display().to_string(),
            }),
        });
    }

    let probe_path = probe_dir.join(format!(".openmemory-backup-preflight-{}", new_id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe_path)
    {
        Ok(file) => {
            let sync_result = file.sync_all();
            drop(file);
            let _ = std::fs::remove_file(&probe_path);
            sync_result.err().map(|error| AdminDiagnostic {
                component: "backup".into(),
                code: AdminErrorCode::BackupPreflightFailed,
                message: "backup destination write check failed".into(),
                hint: Some("Choose a writable directory for backups.".into()),
                details: serde_json::json!({ "error": error.to_string() }),
            })
        }
        Err(error) => Some(AdminDiagnostic {
            component: "backup".into(),
            code: AdminErrorCode::BackupPreflightFailed,
            message: "backup destination is not writable".into(),
            hint: Some("Choose a writable directory for backups.".into()),
            details: serde_json::json!({
                "probe_dir": probe_dir.display().to_string(),
                "error": error.to_string(),
            }),
        }),
    }
}

fn checkpoint_profile_for_backup(
    config: &DaemonConfig,
    source_dir: &Path,
) -> Result<(), AdminError> {
    let loaded_config = load_config(config.home()).map_err(|error| {
        AdminError::new(
            AdminErrorCode::ConfigInvalid,
            "OpenMemory config could not be loaded",
            Some("Fix config.toml and retry."),
            false,
        )
        .with_details(serde_json::json!({ "error": error }))
    })?;
    let store = DomainStore::open_existing(&loaded_config, source_dir)
        .map_err(|error| store_admin_error("memory store is unreadable", error))?;
    for domain in store.stores() {
        domain
            .wal_checkpoint()
            .map_err(|error| store_admin_error("memory store checkpoint failed", error))?;
    }
    Ok(())
}

fn directory_size(path: &Path) -> std::io::Result<(u64, u64)> {
    if !path.exists() {
        return Ok((0, 0));
    }
    let mut files = 0;
    let mut bytes = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let (sub_files, sub_bytes) = directory_size(&entry.path())?;
            files += sub_files;
            bytes += sub_bytes;
        } else if file_type.is_file() {
            let meta = entry.metadata()?;
            files += 1;
            bytes += meta.len();
        }
    }
    Ok((files, bytes))
}

pub(crate) fn copy_dir_recursive(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&source_path, &destination_path)?;
            std::fs::File::open(&destination_path)?.sync_all()?;
        }
    }
    fsync_dir(destination)?;
    Ok(())
}

fn write_backup_manifest(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(content)?;
    file.sync_all()?;
    fsync_parent_dir(path)?;
    Ok(())
}

fn fsync_parent_dir(path: &Path) -> std::io::Result<()> {
    fsync_dir(path.parent().unwrap_or_else(|| Path::new(".")))
}

fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    let handle = std::fs::File::open(dir)?;
    handle.sync_all()
}

fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.exists())
        .map(Path::to_path_buf)
}

pub(crate) fn validate_restore_target_profile(profile: &str) -> Result<(), AdminError> {
    let path = Path::new(profile);
    let invalid = profile.trim().is_empty()
        || profile.contains('/')
        || profile.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)));
    if invalid {
        return Err(AdminError::new(
            AdminErrorCode::InvalidRequest,
            "restore target profile is invalid",
            Some("Use a single profile name without path separators or parent segments."),
            false,
        )
        .with_details(serde_json::json!({ "target_profile": profile })));
    }
    Ok(())
}

fn ensure_destination_outside_source(destination: &Path, source: &Path) -> std::io::Result<()> {
    if path_is_same_or_descendant(destination, source)? {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "backup destination {} resolves inside source {}",
                destination.display(),
                source.display()
            ),
        ))
    } else {
        Ok(())
    }
}

fn path_is_same_or_descendant(path: &Path, parent: &Path) -> std::io::Result<bool> {
    Ok(resolve_with_existing_ancestor(path)?.starts_with(resolve_with_existing_ancestor(parent)?))
}

fn resolve_with_existing_ancestor(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = absolute_path(path)?;
    if let Ok(canonical) = absolute.canonicalize() {
        return Ok(canonical);
    }

    let ancestor = existing_ancestor(&absolute).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no existing ancestor for {}", path.display()),
        )
    })?;
    let canonical_ancestor = ancestor.canonicalize()?;
    let suffix = absolute
        .strip_prefix(&ancestor)
        .unwrap_or_else(|_| Path::new(""));
    Ok(canonical_ancestor.join(suffix))
}

fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
