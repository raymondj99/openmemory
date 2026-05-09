//! SHA-256 integrity verification for model files.
//!
//! Each [`Model`] in the registry carries an `onnx_sha256` /
//! `tokenizer_sha256` field. Before handing a file to the ONNX runtime,
//! callers should run [`verify_sha256`] against the registry-recorded
//! hash; a mismatch surfaces as [`EmbedError::ChecksumMismatch`] so the
//! load path stops before parsing untrusted bytes.
//!
//! # Hash coverage
//!
//! Both shipped models have recorded SHA-256 hashes. [`verify_sha256`]
//! treats an empty expected hash as [`VerificationOutcome::Skipped`]
//! (logs a warning and proceeds), so future models added to the
//! registry without hashes degrade gracefully.
//!
//! [`Model`]: crate::models::Model

use crate::error::{EmbedError, EmbedResult};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// 64 KiB read buffer. Big enough that read syscalls dominate hashing
/// work for typical ONNX files (≈300 MB), small enough to stay in L1.
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Outcome of a verification attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationOutcome {
    /// The file's SHA-256 matched the expected hash.
    Verified,
    /// The expected hash was empty — the registry has not yet recorded
    /// a known-good value, so verification was skipped. Callers should
    /// decide whether to log + proceed or refuse to load.
    Skipped,
}

/// Verify that `path` hashes to `expected_hex` (case-insensitive,
/// whitespace-tolerant; canonicalised to lowercase before comparison).
///
/// * Returns `Ok(VerificationOutcome::Verified)` when the hash matches.
/// * Returns `Ok(VerificationOutcome::Skipped)` when `expected_hex`
///   is empty after trimming, signalling that the registry entry has
///   no recorded hash yet.
/// * Returns [`Err(EmbedError::ChecksumMismatch)`] when the hash does
///   not match.
/// * Returns [`Err(EmbedError::Io)`] when the file can't be opened or
///   read.
///
/// The file is streamed through SHA-256 in 64 KiB blocks so memory
/// stays bounded regardless of model size.
pub fn verify_sha256(path: &Path, expected_hex: &str) -> EmbedResult<VerificationOutcome> {
    let expected = expected_hex.trim().to_ascii_lowercase();
    if expected.is_empty() {
        return Ok(VerificationOutcome::Skipped);
    }
    let actual = hash_file_sha256(path)?;
    if actual == expected {
        Ok(VerificationOutcome::Verified)
    } else {
        Err(EmbedError::ChecksumMismatch {
            path: path.display().to_string(),
            expected,
            actual,
        })
    }
}

/// Stream `path` through SHA-256 and return the lowercase-hex digest.
fn hash_file_sha256(path: &Path) -> EmbedResult<String> {
    let f = File::open(path)?;
    let mut reader = BufReader::with_capacity(HASH_BUFFER_BYTES, f);
    let mut hasher = Sha256::new();
    // Heap-allocate the read buffer — putting 64 KiB on the stack would
    // dominate the function's frame and trips `clippy::large_stack_arrays`.
    let mut buf = vec![0u8; HASH_BUFFER_BYTES];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(hex_lowercase(&digest))
}

fn hex_lowercase(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// SHA-256 of the empty string (well-known vector).
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    /// SHA-256 of `b"hello world"` (well-known vector).
    const HELLO_WORLD_SHA256: &str =
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

    fn write_file(dir: &Path, name: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(contents).unwrap();
        f.sync_all().unwrap();
        path
    }

    #[test]
    fn hash_file_sha256_matches_known_vector_for_hello_world() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "hello.txt", b"hello world");
        let actual = hash_file_sha256(&path).unwrap();
        assert_eq!(actual, HELLO_WORLD_SHA256);
    }

    #[test]
    fn hash_file_sha256_matches_known_vector_for_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "empty.txt", b"");
        let actual = hash_file_sha256(&path).unwrap();
        assert_eq!(actual, EMPTY_SHA256);
    }

    #[test]
    fn hash_file_sha256_streams_files_larger_than_one_buffer() {
        // 256 KiB of repeating bytes — multiple read passes through
        // the 64 KiB buffer.
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![0xABu8; HASH_BUFFER_BYTES * 4];
        let path = write_file(dir.path(), "big.bin", &payload);

        // Compute the same hash via Sha256::update directly so we
        // don't need an external `sha256sum`.
        let mut h = Sha256::new();
        h.update(&payload);
        let expected = hex_lowercase(&h.finalize());

        let actual = hash_file_sha256(&path).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn verify_sha256_accepts_correct_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "hello.txt", b"hello world");
        let outcome = verify_sha256(&path, HELLO_WORLD_SHA256).unwrap();
        assert_eq!(outcome, VerificationOutcome::Verified);
    }

    #[test]
    fn verify_sha256_rejects_wrong_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "hello.txt", b"hello world");
        let bogus = "0".repeat(64);
        let err = verify_sha256(&path, &bogus).unwrap_err();
        match err {
            EmbedError::ChecksumMismatch {
                path: p,
                expected,
                actual,
            } => {
                assert!(p.contains("hello.txt"));
                assert_eq!(expected, bogus);
                assert_eq!(actual, HELLO_WORLD_SHA256);
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_sha256_normalises_uppercase_and_whitespace_in_expected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "hello.txt", b"hello world");
        let mixed = format!("  {}  ", HELLO_WORLD_SHA256.to_ascii_uppercase());
        let outcome = verify_sha256(&path, &mixed).unwrap();
        assert_eq!(outcome, VerificationOutcome::Verified);
    }

    #[test]
    fn verify_sha256_treats_empty_expected_as_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "any.txt", b"contents do not matter");
        let outcome = verify_sha256(&path, "").unwrap();
        assert_eq!(outcome, VerificationOutcome::Skipped);
    }

    #[test]
    fn verify_sha256_treats_whitespace_only_expected_as_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "any.txt", b"contents");
        let outcome = verify_sha256(&path, "   \t\n").unwrap();
        assert_eq!(outcome, VerificationOutcome::Skipped);
    }

    #[test]
    fn verify_sha256_returns_io_error_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist");
        let err = verify_sha256(&path, HELLO_WORLD_SHA256).unwrap_err();
        assert!(matches!(err, EmbedError::Io(_)));
    }

    #[test]
    fn hex_lowercase_matches_format_x() {
        // Spot-check against the standard library's lowercase-hex
        // formatter so we know our hand-rolled encoder agrees.
        use std::fmt::Write as _;
        let bytes = [0x00u8, 0x01, 0x10, 0xff, 0xab, 0xcd];
        let ours = hex_lowercase(&bytes);
        let mut expected = String::with_capacity(bytes.len() * 2);
        for b in &bytes {
            write!(expected, "{b:02x}").unwrap();
        }
        assert_eq!(ours, expected);
    }
}
