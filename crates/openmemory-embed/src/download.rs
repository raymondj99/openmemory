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

    fn has_model_files(dir: &Path) -> bool {
        has_required_file(&dir.join("model.onnx")) && has_required_file(&dir.join("tokenizer.json"))
    }

    /// Return the on-disk directory for `model` if both required files
    /// exist. Checks the canonical name first, then aliases.
    pub fn downloaded_model_dir(&self, model: &Model) -> Option<PathBuf> {
        let dir = self.model_dir(model.name);
        if Self::has_model_files(&dir) {
            return Some(dir);
        }
        for alias in model.aliases {
            let dir = self.model_dir(alias);
            if Self::has_model_files(&dir) {
                return Some(dir);
            }
        }
        None
    }

    /// Download model files from Hugging Face.
    /// Skips files that already exist on disk. Retries transient
    /// network failures with exponential backoff.
    pub fn download(&self, model: &Model) -> EmbedResult<()> {
        let dir = self.model_dir(model.name);
        std::fs::create_dir_all(&dir)?;

        let files = [
            ("model.onnx", model.onnx_url),
            ("tokenizer.json", model.tokenizer_url),
        ];

        let retry_config = openmemory_core::retry::RetryConfig::network();
        let is_retryable = |e: &DownloadAttemptError| e.retryable;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(READ_TIMEOUT)
            .timeout_write(WRITE_TIMEOUT)
            .build();

        for (filename, url) in files {
            let dest = dir.join(filename);
            if has_required_file(&dest) {
                info!("{filename} already exists, skipping");
                continue;
            }
            if dest.exists() {
                info!("{filename} exists but is incomplete, replacing");
                std::fs::remove_file(&dest)?;
            }

            info!("Downloading {filename} from {url}");

            let bytes = openmemory_core::retry::with_retry(&retry_config, is_retryable, || {
                fetch_bytes(&agent, filename, url)
            })
            .map_err(DownloadAttemptError::into_error)?;

            write_atomic(&dest, &bytes)?;
            info!("Saved {filename} ({} bytes)", bytes.len());
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

fn fetch_bytes(
    agent: &ureq::Agent,
    filename: &str,
    url: &str,
) -> Result<Vec<u8>, DownloadAttemptError> {
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

    let mut buf = Vec::new();
    response.into_reader().read_to_end(&mut buf).map_err(|e| {
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
    })?;

    if buf.is_empty() {
        return Err(DownloadAttemptError::retryable(format!(
            "{filename} response was empty"
        )));
    }

    Ok(buf)
}

fn is_retryable_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

fn write_atomic(dest: &Path, bytes: &[u8]) -> EmbedResult<()> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = dest
        .file_name()
        .ok_or_else(|| EmbedError::Download(format!("path has no file name: {dest:?}")))?;
    let tmp = parent.join(format!(".{}.part", file_name.to_string_lossy()));
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(EmbedError::Io(e));
    }
    Ok(())
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
    fn write_atomic_replaces_part_file_with_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("model.onnx");
        write_atomic(&dest, b"complete").unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"complete");
        assert!(!tmp.path().join(".model.onnx.part").exists());
    }
}
