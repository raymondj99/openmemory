//! Model file download from Hugging Face.
//!
//! [`ModelManager`] owns the `~/.openmemory/models/` directory and
//! handles downloading `model.onnx` + `tokenizer.json` for any model
//! in the registry. Downloads retry transient network failures with
//! exponential backoff via [`openmemory_core::retry`].

use crate::error::{EmbedError, EmbedResult};
use crate::models::Model;
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::info;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_MODEL_BYTES: u64 = 500 * 1024 * 1024;
const MAX_DATA_BYTES: u64 = 3 * 1024 * 1024 * 1024;
const DOWNLOAD_BUF_BYTES: usize = 64 * 1024;

/// Manages the on-disk model directory at `~/.openmemory/models/`.
pub struct ModelManager {
    models_dir: PathBuf,
}

impl ModelManager {
    pub fn new(models_dir: PathBuf) -> Self {
        Self { models_dir }
    }

    /// Construct from the default config path (`~/.openmemory/models/`).
    pub fn from_config() -> EmbedResult<Self> {
        let dir = openmemory_core::config::Config::models_dir()
            .map_err(|e| EmbedError::Io(std::io::Error::other(e.to_string())))?;
        Ok(Self::new(dir))
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    fn model_dir(&self, model_name: &str) -> PathBuf {
        self.models_dir.join(model_name)
    }

    fn has_model_files(dir: &Path, model: &Model) -> bool {
        has_required_file(&dir.join("model.onnx"))
            && has_required_file(&dir.join("tokenizer.json"))
            && (!model.has_external_data() || has_required_file(&dir.join("model.onnx_data")))
    }

    /// Return the on-disk directory for `model` if all required files
    /// exist. Checks the canonical name first, then aliases.
    pub fn downloaded_model_dir(&self, model: &Model) -> Option<PathBuf> {
        let dir = self.model_dir(model.name);
        if Self::has_model_files(&dir, model) {
            return Some(dir);
        }
        for alias in model.aliases {
            let dir = self.model_dir(alias);
            if Self::has_model_files(&dir, model) {
                return Some(dir);
            }
        }
        None
    }

    /// Download model files from Hugging Face.
    /// Skips files that already exist on disk. Retries transient
    /// network failures with exponential backoff. After writing each
    /// file, verifies its SHA-256 against the registry hash (when the
    /// hash is non-empty).
    pub fn download(&self, model: &Model) -> EmbedResult<()> {
        let dir = self.model_dir(model.name);
        std::fs::create_dir_all(&dir)?;

        let mut files: Vec<(&str, &str, &str, u64)> = vec![
            (
                "model.onnx",
                model.onnx_url,
                model.onnx_sha256,
                MAX_MODEL_BYTES,
            ),
            (
                "tokenizer.json",
                model.tokenizer_url,
                model.tokenizer_sha256,
                MAX_MODEL_BYTES,
            ),
        ];
        if model.has_external_data() {
            files.push((
                "model.onnx_data",
                model.onnx_data_url,
                model.onnx_data_sha256,
                MAX_DATA_BYTES,
            ));
        }

        let retry_config = openmemory_core::retry::RetryConfig::network();
        let is_retryable = |e: &DownloadAttemptError| e.retryable;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(READ_TIMEOUT)
            .timeout_write(WRITE_TIMEOUT)
            .build();

        for (filename, url, expected_sha256, max_bytes) in files {
            let dest = dir.join(filename);
            if has_required_file(&dest) {
                info!("{filename} already exists, skipping");
                continue;
            }
            if dest.exists() {
                info!("{filename} exists but is incomplete, replacing");
                std::fs::remove_file(&dest)?;
            }
            clean_part_file(&dest);

            info!("Downloading {filename} from {url}");

            let written = openmemory_core::retry::with_retry(&retry_config, is_retryable, || {
                fetch_to_file(&agent, filename, url, &dest, max_bytes)
            })
            .map_err(DownloadAttemptError::into_error)?;

            if !expected_sha256.is_empty() {
                crate::integrity::verify_sha256(&dest, expected_sha256)?;
            }

            info!("Saved {filename} ({written} bytes)");
        }

        Ok(())
    }
}

#[derive(Debug)]
struct DownloadAttemptError {
    error: EmbedError,
    retryable: bool,
}

impl DownloadAttemptError {
    fn retryable(message: String) -> Self {
        Self {
            error: EmbedError::Download(message),
            retryable: true,
        }
    }

    fn permanent(message: String) -> Self {
        Self {
            error: EmbedError::Download(message),
            retryable: false,
        }
    }

    fn into_error(self) -> EmbedError {
        self.error
    }
}

impl fmt::Display for DownloadAttemptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

fn has_required_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

fn part_path(dest: &Path) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    parent.join(format!(".{name}.part"))
}

fn clean_part_file(dest: &Path) {
    let tmp = part_path(dest);
    if tmp.exists() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn fetch_to_file(
    agent: &ureq::Agent,
    filename: &str,
    url: &str,
    dest: &Path,
    max_bytes: u64,
) -> Result<u64, DownloadAttemptError> {
    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let message = format!(
                "{filename} download failed with HTTP {status} from {}",
                response.get_url()
            );
            if is_retryable_status(status) {
                return Err(DownloadAttemptError::retryable(message));
            }
            return Err(DownloadAttemptError::permanent(message));
        }
        Err(ureq::Error::Transport(e)) => {
            return Err(DownloadAttemptError::retryable(format!(
                "{filename} transport failed: {e}"
            )));
        }
    };

    if let Some(len_str) = response.header("Content-Length") {
        if let Ok(len) = len_str.parse::<u64>() {
            if len > max_bytes {
                return Err(DownloadAttemptError::permanent(format!(
                    "{filename} Content-Length {len} exceeds {max_bytes} byte limit"
                )));
            }
        }
    }

    let tmp = part_path(dest);
    let mut file = std::fs::File::create(&tmp)
        .map_err(|e| DownloadAttemptError::permanent(format!("{filename} create failed: {e}")))?;

    let mut reader = response.into_reader();
    let mut buf = vec![0u8; DOWNLOAD_BUF_BYTES];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            classify_io_error(filename, &e)
        })?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_bytes {
            let _ = std::fs::remove_file(&tmp);
            return Err(DownloadAttemptError::permanent(format!(
                "{filename} exceeds {max_bytes} byte limit"
            )));
        }
        file.write_all(&buf[..n]).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            DownloadAttemptError::permanent(format!("{filename} write failed: {e}"))
        })?;
    }

    if total == 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err(DownloadAttemptError::retryable(format!(
            "{filename} response was empty"
        )));
    }

    file.sync_all().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        DownloadAttemptError::permanent(format!("{filename} sync failed: {e}"))
    })?;
    drop(file);

    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        DownloadAttemptError::permanent(format!("{filename} rename failed: {e}"))
    })?;

    Ok(total)
}

fn classify_io_error(filename: &str, e: &std::io::Error) -> DownloadAttemptError {
    let retryable = matches!(
        e.kind(),
        ErrorKind::Interrupted
            | ErrorKind::TimedOut
            | ErrorKind::WouldBlock
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
    );
    let message = format!("{filename} read failed: {e}");
    if retryable {
        DownloadAttemptError::retryable(message)
    } else {
        DownloadAttemptError::permanent(message)
    }
}

fn is_retryable_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloaded_model_dir_returns_none_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(tmp.path().to_path_buf());
        assert!(mgr
            .downloaded_model_dir(&crate::NOMIC_EMBED_TEXT_V1_5)
            .is_none());
    }

    #[test]
    fn downloaded_model_dir_finds_canonical_name() {
        let tmp = tempfile::tempdir().unwrap();
        let model = &crate::NOMIC_EMBED_TEXT_V1_5;
        let dir = tmp.path().join(model.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.onnx"), b"fake").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();

        let mgr = ModelManager::new(tmp.path().to_path_buf());
        assert_eq!(mgr.downloaded_model_dir(model).unwrap(), dir);
    }

    #[test]
    fn downloaded_model_dir_finds_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let model = &crate::NOMIC_EMBED_TEXT_V1_5;
        let dir = tmp.path().join("nomic");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.onnx"), b"fake").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();

        let mgr = ModelManager::new(tmp.path().to_path_buf());
        assert_eq!(mgr.downloaded_model_dir(model).unwrap(), dir);
    }

    #[test]
    fn downloaded_model_dir_requires_both_files() {
        let tmp = tempfile::tempdir().unwrap();
        let model = &crate::NOMIC_EMBED_TEXT_V1_5;
        let dir = tmp.path().join(model.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.onnx"), b"fake").unwrap();

        let mgr = ModelManager::new(tmp.path().to_path_buf());
        assert!(mgr.downloaded_model_dir(model).is_none());
    }

    #[test]
    fn downloaded_model_dir_rejects_empty_files() {
        let tmp = tempfile::tempdir().unwrap();
        let model = &crate::NOMIC_EMBED_TEXT_V1_5;
        let dir = tmp.path().join(model.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.onnx"), b"").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();

        let mgr = ModelManager::new(tmp.path().to_path_buf());
        assert!(mgr.downloaded_model_dir(model).is_none());
    }

    #[test]
    fn clean_part_file_removes_stale_temp() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("model.onnx");
        let part = tmp.path().join(".model.onnx.part");
        std::fs::write(&part, b"stale").unwrap();
        assert!(part.exists());
        clean_part_file(&dest);
        assert!(!part.exists());
    }

    #[test]
    fn clean_part_file_noop_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("model.onnx");
        clean_part_file(&dest);
    }
}
