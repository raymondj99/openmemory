//! Daemon runtime discovery-file helpers.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use openmemory_admin::{DaemonRuntimeInfo, ADMIN_API_VERSION};

use crate::{validate_loopback, DaemonConfig, DaemonError, DAEMON_RUNTIME_FILE, RUN_DIR};

/// Path to the daemon runtime discovery file for an OpenMemory home.
#[must_use]
pub fn runtime_info_path(home: &Path) -> PathBuf {
    home.join(RUN_DIR).join(DAEMON_RUNTIME_FILE)
}

/// Build runtime metadata for a daemon bound to `bound_addr`.
pub fn runtime_info(
    config: &DaemonConfig,
    bound_addr: SocketAddr,
) -> Result<DaemonRuntimeInfo, DaemonError> {
    validate_loopback(bound_addr)?;
    Ok(DaemonRuntimeInfo {
        api_version: ADMIN_API_VERSION.to_string(),
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        bind_addr: bound_addr.to_string(),
        admin_url: format!("http://{bound_addr}"),
        home: config.home.display().to_string(),
        active_profile: config.active_profile.clone(),
        started_at_unix_secs: unix_now_secs()?,
    })
}

/// Write daemon runtime metadata under `<home>/run/daemon.json`.
pub fn write_runtime_info(home: &Path, info: &DaemonRuntimeInfo) -> Result<(), DaemonError> {
    let path = runtime_info_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_vec_pretty(info)?;
    write_atomic(&path, &content)
}

/// Read daemon runtime metadata if the discovery file exists.
pub fn read_runtime_info(home: &Path) -> Result<Option<DaemonRuntimeInfo>, DaemonError> {
    let path = runtime_info_path(home);
    match std::fs::read(&path) {
        Ok(content) => Ok(Some(serde_json::from_slice(&content)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(DaemonError::RuntimeIo(e)),
    }
}

/// Remove daemon runtime metadata. Missing files are treated as already
/// removed.
pub fn remove_runtime_info(home: &Path) -> Result<(), DaemonError> {
    match std::fs::remove_file(runtime_info_path(home)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::RuntimeIo(e)),
    }
}

pub(crate) fn write_atomic(path: &Path, content: &[u8]) -> Result<(), DaemonError> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(content)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(DaemonError::RuntimeIo(e))
        }
    }
}

pub(crate) fn unix_now_secs() -> Result<u64, DaemonError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| DaemonError::ClockBeforeUnixEpoch)?
        .as_secs())
}
