// Three of the four `PlatformArtifact` constants are referenced only
// by tests on any given host (`cfg(target_os, target_arch)` selects
// exactly one for `current_artifact`). We keep all four compiled so
// the test suite can verify each one's URL/SHA/path bindings without
// per-platform CI matrices, and silence the resulting dead-code
// warnings at module level.
#![allow(dead_code)]

//! Platform-matched ONNX Runtime install for `ort = "load-dynamic"`.
//!
//! `ort` 2.0 is compiled with the `load-dynamic` feature so the
//! `openmemory` binary expects `libonnxruntime` to be discoverable at
//! runtime via `ORT_DYLIB_PATH` or `LD_LIBRARY_PATH`. We don't bundle
//! the shared library in the release tarball (it varies by platform
//! and would double the tarball size for users who never touch
//! embeddings). Instead, [`RuntimeManager`] downloads the
//! platform-matched Microsoft ONNX Runtime release into
//! `~/.openmemory/runtime/onnxruntime-<version>/lib/` the first time
//! the user asks for embeddings via `openmemory model download`.
//!
//! The CLI's `main` then calls [`RuntimeManager::set_ort_dylib_path_if_present`]
//! at startup so the env var is set before `ort` initializes. Users
//! who already have `libonnxruntime` on `LD_LIBRARY_PATH` or who set
//! `ORT_DYLIB_PATH` themselves take precedence; the runtime manager
//! never overwrites a user-supplied value.

use std::fs;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};

use tracing::info;

use crate::error::{EmbedError, EmbedResult};

/// ONNX Runtime release pinned to the ort crate version we compile
/// against. ort 2.0.0-rc.9 ships compatible bindings for ONNX Runtime
/// 1.20.x. Bump this in lockstep with the workspace `ort` dependency.
pub const ONNX_RUNTIME_VERSION: &str = "1.20.0";

/// Maximum tarball size accepted from the upstream release. ONNX
/// Runtime 1.20 tarballs are 25-30 MB across our supported targets;
/// 256 MB is the hard ceiling against a malformed Content-Length.
const MAX_TARBALL_BYTES: u64 = 256 * 1024 * 1024;

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const DOWNLOAD_BUF_BYTES: usize = 64 * 1024;

/// Per-target release artifact. We hardcode SHA-256 hashes so the
/// integrity check matches the [`super::download`] model-download
/// policy: any change to the upstream artifact requires a code change
/// here, not a silent re-download.
///
/// All four artifact constants below are referenced by the tests; only
/// the one matching the current `cfg(target_os, target_arch)` is used
/// at runtime, so non-host variants would otherwise be flagged as
/// dead code by `clippy`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct PlatformArtifact {
    /// Tarball filename, e.g. `onnxruntime-linux-x64-1.20.0.tgz`.
    archive: &'static str,
    /// SHA-256 hex of the tarball as published in the Microsoft
    /// ONNX Runtime v1.20.0 release.
    sha256: &'static str,
    /// Top-level directory inside the tarball, e.g.
    /// `onnxruntime-linux-x64-1.20.0`.
    archive_root: &'static str,
    /// Library filename inside `lib/`. On linux this is the versioned
    /// SO name (`libonnxruntime.so.1.20.0`); on macOS the versioned
    /// dylib (`libonnxruntime.1.20.0.dylib`).
    dylib_name: &'static str,
    /// Symlink that ort actually `dlopen`s (`libonnxruntime.so` or
    /// `libonnxruntime.dylib`). Created during install if absent.
    dylib_symlink: &'static str,
}

const LINUX_X86_64: PlatformArtifact = PlatformArtifact {
    archive: "onnxruntime-linux-x64-1.20.0.tgz",
    sha256: "aa70d48b22e264b82e83f63245b51ddc9a47ae4a3a66903efaff1ba68b7b5930",
    archive_root: "onnxruntime-linux-x64-1.20.0",
    dylib_name: "libonnxruntime.so.1.20.0",
    dylib_symlink: "libonnxruntime.so",
};

const LINUX_AARCH64: PlatformArtifact = PlatformArtifact {
    archive: "onnxruntime-linux-aarch64-1.20.0.tgz",
    sha256: "b4d7c6e2c45f8edabe5d28e9bc59ec8d5a4a4af36660cda16e94b2ad85f2a52a",
    archive_root: "onnxruntime-linux-aarch64-1.20.0",
    dylib_name: "libonnxruntime.so.1.20.0",
    dylib_symlink: "libonnxruntime.so",
};

const MACOS_AARCH64: PlatformArtifact = PlatformArtifact {
    archive: "onnxruntime-osx-arm64-1.20.0.tgz",
    sha256: "2bcfaafa9ff0a3a94f78e3af2f135ffde5bb2d79b08e83a50dbc450b0d20ddae",
    archive_root: "onnxruntime-osx-arm64-1.20.0",
    dylib_name: "libonnxruntime.1.20.0.dylib",
    dylib_symlink: "libonnxruntime.dylib",
};

const MACOS_X86_64: PlatformArtifact = PlatformArtifact {
    archive: "onnxruntime-osx-x86_64-1.20.0.tgz",
    sha256: "d28e603b47b74050f2c30a7069bf3fb371cfba7205d7771f22cabc7b02953757",
    archive_root: "onnxruntime-osx-x86_64-1.20.0",
    dylib_name: "libonnxruntime.1.20.0.dylib",
    dylib_symlink: "libonnxruntime.dylib",
};

fn current_artifact() -> Option<PlatformArtifact> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some(LINUX_X86_64);
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Some(LINUX_AARCH64);
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some(MACOS_AARCH64);
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Some(MACOS_X86_64);
    }
    #[allow(unreachable_code)]
    None
}

/// Manages the on-disk ONNX Runtime install at
/// `<runtime_root>/onnxruntime-<version>/lib/`.
pub struct RuntimeManager {
    runtime_root: PathBuf,
}

impl RuntimeManager {
    pub fn new(runtime_root: PathBuf) -> Self {
        Self { runtime_root }
    }

    /// Resolve the runtime root from the standard openmemory home
    /// (`~/.openmemory/runtime/` or `$OPENMEMORY_HOME/runtime/`).
    pub fn from_config() -> EmbedResult<Self> {
        let root = openmemory_core::config::Config::runtime_dir()
            .map_err(|e| EmbedError::Config(e.to_string()))?;
        Ok(Self::new(root))
    }

    fn install_dir(&self, art: &PlatformArtifact) -> PathBuf {
        self.runtime_root.join(art.archive_root)
    }

    /// Absolute path to the versioned dylib (e.g.
    /// `…/lib/libonnxruntime.so.1.20.0`). Returned unconditionally;
    /// use [`Self::is_installed`] to check whether the file actually
    /// exists.
    pub fn dylib_path(&self) -> Option<PathBuf> {
        current_artifact().map(|art| self.install_dir(&art).join("lib").join(art.dylib_name))
    }

    /// Absolute path to the unversioned dylib symlink (e.g.
    /// `…/lib/libonnxruntime.so`). This is the path ort actually
    /// `dlopen`s when `ORT_DYLIB_PATH` is unset.
    pub fn dylib_symlink_path(&self) -> Option<PathBuf> {
        current_artifact().map(|art| self.install_dir(&art).join("lib").join(art.dylib_symlink))
    }

    /// True iff the versioned dylib is already extracted on disk.
    pub fn is_installed(&self) -> bool {
        self.dylib_path().is_some_and(|p| p.exists())
    }

    /// Download + verify + extract the platform-matched ONNX Runtime
    /// release into the runtime root. Idempotent: returns the dylib
    /// path immediately when already installed.
    pub fn install(&self) -> EmbedResult<PathBuf> {
        let art = current_artifact().ok_or_else(|| {
            EmbedError::Config(format!(
                "no pinned ONNX Runtime artifact for {} {}; set ORT_DYLIB_PATH manually",
                std::env::consts::OS,
                std::env::consts::ARCH,
            ))
        })?;
        let install_dir = self.install_dir(&art);
        let lib_dir = install_dir.join("lib");
        let dylib = lib_dir.join(art.dylib_name);
        if dylib.exists() {
            info!(
                "ONNX Runtime {} already installed at {}",
                ONNX_RUNTIME_VERSION,
                dylib.display()
            );
            return Ok(dylib);
        }

        fs::create_dir_all(&self.runtime_root)?;

        // Download the tarball to a sibling staging directory so a
        // crash mid-extract never leaves a partial install_dir.
        let staging = self
            .runtime_root
            .join(format!(".{}.staging", art.archive_root));
        if staging.exists() {
            fs::remove_dir_all(&staging).ok();
        }
        fs::create_dir_all(&staging)?;
        let tarball = staging.join(art.archive);
        let url = format!(
            "https://github.com/microsoft/onnxruntime/releases/download/v{}/{}",
            ONNX_RUNTIME_VERSION, art.archive,
        );
        info!(
            "Downloading ONNX Runtime {} from {}",
            ONNX_RUNTIME_VERSION, url
        );
        download_tarball(&url, &tarball, MAX_TARBALL_BYTES)?;
        verify_sha256(&tarball, art.sha256)?;

        // Extract just the lib/ subtree into the install dir.
        extract_lib_subtree(&tarball, &install_dir, art.archive_root)?;

        // Ensure the unversioned symlink exists so consumers that look
        // for `libonnxruntime.so` (no version) find it.
        ensure_dylib_symlink(&lib_dir, art.dylib_name, art.dylib_symlink)?;

        // Drop the tarball + staging directory now that extraction is
        // complete; users only need the lib/ subtree.
        fs::remove_dir_all(&staging).ok();

        if !dylib.exists() {
            return Err(EmbedError::Config(format!(
                "extraction completed but {} is missing",
                dylib.display()
            )));
        }
        info!("ONNX Runtime installed at {}", dylib.display());
        Ok(dylib)
    }

    /// Set `ORT_DYLIB_PATH` for the current process if (1) the user
    /// hasn't already set it, (2) the user hasn't set `LD_LIBRARY_PATH`
    /// to a directory containing libonnxruntime, and (3) the runtime
    /// manager has a freshly installed dylib. Idempotent and safe to
    /// call from `main` before any `ort` code runs.
    pub fn set_ort_dylib_path_if_present(&self) {
        if std::env::var_os("ORT_DYLIB_PATH").is_some() {
            return;
        }
        let Some(dylib) = self.dylib_path() else {
            return;
        };
        if !dylib.exists() {
            return;
        }
        // SAFETY: `set_var` is safe in single-threaded contexts; we
        // call this from `main` before any worker threads are spawned.
        std::env::set_var("ORT_DYLIB_PATH", &dylib);
    }
}

fn download_tarball(url: &str, dest: &Path, max_bytes: u64) -> EmbedResult<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(READ_TIMEOUT)
        .timeout_write(WRITE_TIMEOUT)
        .build();

    let response = agent
        .get(url)
        .call()
        .map_err(|e| EmbedError::Download(format!("HTTP GET {url}: {e}")))?;

    if let Some(content_length) = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
    {
        if content_length > max_bytes {
            return Err(EmbedError::Download(format!(
                "{} Content-Length {content_length} exceeds {max_bytes} byte limit",
                dest.display()
            )));
        }
    }

    let mut reader = response.into_reader().take(max_bytes + 1);
    let mut file = fs::File::create(dest)
        .map_err(|e| EmbedError::Download(format!("creating {}: {e}", dest.display())))?;
    let mut buf = vec![0u8; DOWNLOAD_BUF_BYTES];
    let mut written: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| EmbedError::Download(format!("reading {url}: {e}")))?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > max_bytes {
            return Err(EmbedError::Download(format!(
                "{} exceeded {max_bytes} byte limit",
                dest.display()
            )));
        }
        io::Write::write_all(&mut file, &buf[..n])
            .map_err(|e| EmbedError::Download(format!("writing {}: {e}", dest.display())))?;
    }
    Ok(())
}

fn verify_sha256(path: &Path, expected_hex: &str) -> EmbedResult<()> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)
        .map_err(|e| EmbedError::Download(format!("opening {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; DOWNLOAD_BUF_BYTES];
    loop {
        let n = io::Read::read(&mut file, &mut buf)
            .map_err(|e| EmbedError::Download(format!("hashing {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got = format!("{:x}", hasher.finalize());
    if got != expected_hex {
        return Err(EmbedError::Download(format!(
            "{} sha256 mismatch: got {got}, expected {expected_hex}",
            path.display()
        )));
    }
    Ok(())
}

fn extract_lib_subtree(tarball: &Path, install_dir: &Path, archive_root: &str) -> EmbedResult<()> {
    let file = fs::File::open(tarball)
        .map_err(|e| EmbedError::Download(format!("opening {}: {e}", tarball.display())))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let lib_prefix = format!("{archive_root}/lib/");
    fs::create_dir_all(install_dir.join("lib")).map_err(|e| {
        EmbedError::Download(format!("creating {}/lib: {e}", install_dir.display()))
    })?;

    for entry in archive
        .entries()
        .map_err(|e| EmbedError::Download(format!("reading {}: {e}", tarball.display())))?
    {
        let mut entry = entry.map_err(|e| EmbedError::Download(format!("tar entry: {e}")))?;
        let raw_path = entry
            .path()
            .map_err(|e| EmbedError::Download(format!("tar entry path: {e}")))?
            .into_owned();
        // macOS ONNX Runtime tarballs prefix every entry with `./`; the
        // Linux ones do not. Strip the leading `./` so the same
        // `lib_prefix` match works for both.
        let path_str = raw_path.to_string_lossy();
        let normalized = path_str.strip_prefix("./").unwrap_or(&path_str);
        let Some(rel) = normalized.strip_prefix(&lib_prefix) else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        // Reject any path that tries to escape the install dir.
        if rel.contains("..") {
            return Err(EmbedError::Download(format!(
                "tar entry {rel:?} contains traversal sequence",
            )));
        }
        let dest = install_dir.join("lib").join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| EmbedError::Download(format!("creating {}: {e}", parent.display())))?;
        }
        entry
            .unpack(&dest)
            .map_err(|e| EmbedError::Download(format!("extracting {rel}: {e}")))?;
    }
    Ok(())
}

fn ensure_dylib_symlink(lib_dir: &Path, target: &str, link_name: &str) -> EmbedResult<()> {
    let link = lib_dir.join(link_name);
    // Use symlink_metadata so a broken symlink left over from a prior
    // failed extraction is still considered "present" and re-running the
    // installer can recover instead of failing with EEXIST.
    if link.symlink_metadata().is_ok() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, &link).map_err(|e| {
            EmbedError::Download(format!(
                "creating symlink {} -> {target}: {e}",
                link.display()
            ))
        })?;
    }
    #[cfg(not(unix))]
    {
        // No-op fallback; ort will resolve the versioned name directly.
        let _ = target;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_version_is_well_formed() {
        // The pinned version must match the artifact basenames; this
        // catches typos when bumping ONNX Runtime.
        assert!(ONNX_RUNTIME_VERSION.split('.').count() == 3);
        for art in [LINUX_X86_64, LINUX_AARCH64, MACOS_AARCH64, MACOS_X86_64] {
            assert!(
                art.archive.contains(ONNX_RUNTIME_VERSION),
                "artifact {} does not contain pinned version {}",
                art.archive,
                ONNX_RUNTIME_VERSION
            );
            assert!(
                art.archive_root.contains(ONNX_RUNTIME_VERSION),
                "archive_root {} does not contain pinned version {}",
                art.archive_root,
                ONNX_RUNTIME_VERSION
            );
            assert_eq!(
                art.sha256.len(),
                64,
                "sha256 for {} must be 64 hex chars",
                art.archive
            );
        }
    }

    #[test]
    fn is_installed_returns_false_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = RuntimeManager::new(tmp.path().to_path_buf());
        assert!(!mgr.is_installed());
    }

    #[test]
    fn dylib_path_uses_versioned_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = RuntimeManager::new(tmp.path().to_path_buf());
        if let Some(path) = mgr.dylib_path() {
            let s = path.to_string_lossy();
            assert!(s.contains(ONNX_RUNTIME_VERSION), "{s} should be versioned");
        }
    }

    #[test]
    fn set_ort_dylib_path_respects_user_override() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = RuntimeManager::new(tmp.path().to_path_buf());
        // The test process must not have a real ORT_DYLIB_PATH leak in.
        std::env::set_var("ORT_DYLIB_PATH", "/already/set");
        mgr.set_ort_dylib_path_if_present();
        assert_eq!(
            std::env::var("ORT_DYLIB_PATH").as_deref(),
            Ok("/already/set"),
            "must not overwrite user-set ORT_DYLIB_PATH"
        );
        std::env::remove_var("ORT_DYLIB_PATH");
    }

    /// Build a synthetic ONNX Runtime tarball that mimics the layout
    /// upstream ships for the requested `entry_prefix` (`""` for Linux,
    /// `"./"` for macOS). Inside the lib/ subtree we ship one real
    /// "dylib" plus an unversioned symlink pointing at it, mirroring the
    /// real release artifact.
    fn build_fake_tarball(dir: &Path, archive_root: &str, entry_prefix: &str) -> PathBuf {
        let path = dir.join("fake.tgz");
        let file = fs::File::create(&path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let dylib_name = "libfake.so.1.20.0";
        let symlink_name = "libfake.so";
        let dylib_bytes = b"FAKE-ONNX-RUNTIME-BINARY";

        // Real file entry.
        let mut header = tar::Header::new_gnu();
        header.set_size(dylib_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let dylib_path = format!("{entry_prefix}{archive_root}/lib/{dylib_name}");
        builder
            .append_data(&mut header, &dylib_path, &dylib_bytes[..])
            .unwrap();

        // Symlink entry, target relative within lib/.
        let mut link_header = tar::Header::new_gnu();
        link_header.set_size(0);
        link_header.set_mode(0o777);
        link_header.set_entry_type(tar::EntryType::Symlink);
        link_header.set_cksum();
        let symlink_path = format!("{entry_prefix}{archive_root}/lib/{symlink_name}");
        builder
            .append_link(&mut link_header, &symlink_path, dylib_name)
            .unwrap();

        // Out-of-scope entry that the extractor must skip.
        let mut other = tar::Header::new_gnu();
        let payload: &[u8] = b"include header";
        other.set_size(payload.len() as u64);
        other.set_mode(0o644);
        other.set_entry_type(tar::EntryType::Regular);
        other.set_cksum();
        let include_path = format!("{entry_prefix}{archive_root}/include/foo.h");
        builder
            .append_data(&mut other, &include_path, payload)
            .unwrap();

        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn extracts_macos_style_tarball_with_dot_slash_prefix() {
        // Regression: macOS ONNX Runtime tarballs prefix every entry
        // with `./`. The original prefix matcher dropped every entry,
        // leaving the install dir empty and the post-check error.
        let tmp = tempfile::tempdir().unwrap();
        let archive_root = "onnxruntime-osx-arm64-1.20.0";
        let tarball = build_fake_tarball(tmp.path(), archive_root, "./");
        let install_dir = tmp.path().join("install");

        extract_lib_subtree(&tarball, &install_dir, archive_root).unwrap();

        let dylib = install_dir.join("lib").join("libfake.so.1.20.0");
        assert!(dylib.exists(), "macOS-style dylib must extract: {dylib:?}");
        assert_eq!(
            fs::read(&dylib).unwrap(),
            b"FAKE-ONNX-RUNTIME-BINARY",
            "extracted bytes must match the tarball entry"
        );

        let symlink = install_dir.join("lib").join("libfake.so");
        let meta = symlink
            .symlink_metadata()
            .expect("symlink entry must extract on macOS-style tarballs");
        assert!(
            meta.file_type().is_symlink(),
            "{symlink:?} must be a symlink"
        );

        // The include/ subtree should not have leaked in.
        assert!(
            !install_dir.join("include").exists(),
            "non-lib entries must be skipped"
        );
    }

    #[test]
    fn extracts_linux_style_tarball_without_dot_slash_prefix() {
        // Make sure the same matcher still works for Linux tarballs,
        // which do not have the leading `./` prefix. This is the
        // shipping-since-v0.3.2 happy path.
        let tmp = tempfile::tempdir().unwrap();
        let archive_root = "onnxruntime-linux-x64-1.20.0";
        let tarball = build_fake_tarball(tmp.path(), archive_root, "");
        let install_dir = tmp.path().join("install");

        extract_lib_subtree(&tarball, &install_dir, archive_root).unwrap();

        assert!(install_dir.join("lib").join("libfake.so.1.20.0").exists());
        assert!(install_dir
            .join("lib")
            .join("libfake.so")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn ensure_dylib_symlink_recovers_from_broken_link() {
        // Regression: a prior failed run can leave a broken symlink. We
        // must treat that as "already there" so retrying the install
        // succeeds instead of dying with EEXIST.
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().to_path_buf();
        #[cfg(unix)]
        std::os::unix::fs::symlink("libfake.so.1.20.0", lib.join("libfake.so")).unwrap();
        // The target deliberately does not exist. `Path::exists()` would
        // follow the link and report false; we must use `symlink_metadata`.
        assert!(!lib.join("libfake.so.1.20.0").exists());

        ensure_dylib_symlink(&lib, "libfake.so.1.20.0", "libfake.so").unwrap();
    }
}
