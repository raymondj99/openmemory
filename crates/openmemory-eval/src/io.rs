//! Shared on-disk loaders used by the per-benchmark adapters.
//!
//! Each adapter ships the same three-stream JSONL layout (`corpus.jsonl`,
//! `queries.jsonl`, `judgments.jsonl`). The parser lives here so the
//! adapters stay just thin per-benchmark wrappers; error context is
//! consistent across them and validation lives in one place.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Parse a JSONL file into a `Vec<T>`. Empty lines are skipped. Errors
/// are annotated with the file path and 1-based line number so adapter
/// callers don't have to wrap them again.
pub fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("reading line {} of {}", i + 1, path.display()))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: T = serde_json::from_str(trimmed)
            .with_context(|| format!("parsing line {} of {}", i + 1, path.display()))?;
        out.push(parsed);
    }
    Ok(out)
}
