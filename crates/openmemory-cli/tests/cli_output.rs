//! End-to-end tests for the human-facing `openmemory` CLI output. Each
//! test spawns the real binary against a tempdir home, pins
//! `--color=never` so glyphs fall back to ASCII and ANSI escapes are
//! stripped, and asserts on stable layout invariants.
//!
//! These tests cover the visual layout end-to-end so a regression in
//! wiring between commands and `ui::*` primitives shows up here instead
//! of escaping to release. Per-primitive unit tests still live in
//! `crates/openmemory-cli/src/ui/`.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Locate the `openmemory` binary cargo built for this test.
/// Same heuristic as `mcp_e2e.rs::binary_path`.
fn binary_path() -> PathBuf {
    if let Ok(raw) = std::env::var("CARGO_BIN_EXE_openmemory") {
        return PathBuf::from(raw);
    }
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let exe_name = if cfg!(windows) {
        "openmemory.exe"
    } else {
        "openmemory"
    };
    path.push(exe_name);
    assert!(
        path.exists(),
        "could not locate openmemory binary at {}",
        path.display()
    );
    path
}

/// Run `openmemory` with `--color=never` against the given home and
/// return its captured output. Inherits the parent env minus
/// `NO_COLOR`/`CLICOLOR_FORCE` so the test harness's env doesn't sway
/// the result.
fn run(home: &PathBuf, args: &[&str]) -> Output {
    let mut cmd = Command::new(binary_path());
    cmd.env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("OPENMEMORY_HOME", home)
        .arg("--color=never")
        .args(args);
    cmd.output().expect("spawn openmemory")
}

fn stdout(o: &Output) -> String {
    assert!(
        o.status.success(),
        "expected success; status={:?} stderr={}",
        o.status,
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout.clone()).expect("utf-8 stdout")
}

fn stderr(o: &Output) -> String {
    String::from_utf8(o.stderr.clone()).expect("utf-8 stderr")
}

#[test]
fn init_renders_welcome_box() {
    let home = tempfile::tempdir().expect("tempdir");
    let out = run(&home.path().to_path_buf(), &["init"]);
    let body = stdout(&out);
    // Box corners + edges in ASCII mode.
    assert!(body.contains("+- openmemory"), "missing top edge: {body}");
    assert!(body.contains("initialised"), "missing subtitle: {body}");
    // Each key is present with its absolute path.
    assert!(body.contains("home"));
    assert!(body.contains("config"));
    assert!(body.contains("data"));
    assert!(body.contains("profile"));
    assert!(body.contains("default"));
    // Bottom edge.
    assert!(
        body.lines().last().unwrap_or("").starts_with('+'),
        "missing bottom edge: {body}"
    );
}

#[test]
fn init_rerun_is_one_line() {
    let home = tempfile::tempdir().expect("tempdir");
    run(&home.path().to_path_buf(), &["init"]);

    let out = run(&home.path().to_path_buf(), &["init"]);
    let body = stdout(&out);
    // Idempotent path: no banner, single check line.
    assert!(!body.contains("+- openmemory"), "should not box: {body}");
    assert!(
        body.contains("already initialised"),
        "missing notice: {body}"
    );
    let non_blank = body.lines().filter(|l| !l.trim().is_empty()).count();
    assert!(
        non_blank <= 2,
        "should be terse, got {non_blank} lines: {body}"
    );
}

#[test]
fn status_renders_header_and_kv_table() {
    let home = tempfile::tempdir().expect("tempdir");
    run(&home.path().to_path_buf(), &["init"]);

    let out = run(&home.path().to_path_buf(), &["status"]);
    let body = stdout(&out);
    // Section header line only (no closing edge for the header).
    let header_line = body.lines().find(|l| l.contains("status")).unwrap_or("");
    assert!(
        header_line.starts_with('+'),
        "header should start with +: {header_line:?}"
    );
    assert!(header_line.contains("profile: default"));

    // Key/value rows are present and right-aligned (each numeric value
    // starts at the same column).
    let numeric_rows: Vec<&str> = body
        .lines()
        .filter(|l| l.contains("entities") || l.contains("observations") || l.contains("relations"))
        .collect();
    assert!(numeric_rows.len() >= 3, "expected status rows: {body}");
    let value_cols: Vec<usize> = numeric_rows
        .iter()
        .map(|l| l.find(|c: char| c.is_ascii_digit()).expect("digit column"))
        .collect();
    assert!(
        value_cols.windows(2).all(|w| w[0] == w[1]),
        "values not right-aligned: {numeric_rows:?}"
    );
}

#[test]
fn recall_with_no_results_prints_dim_notice() {
    let home = tempfile::tempdir().expect("tempdir");
    run(&home.path().to_path_buf(), &["init"]);

    let out = run(&home.path().to_path_buf(), &["recall", "anything"]);
    let body = stdout(&out);
    assert!(body.contains("no results"), "missing notice: {body}");
    // No card scaffolding.
    assert!(!body.contains("·"));
}

#[test]
fn list_entities_with_empty_store_prints_dim_notice() {
    let home = tempfile::tempdir().expect("tempdir");
    run(&home.path().to_path_buf(), &["init"]);

    let out = run(&home.path().to_path_buf(), &["list-entities"]);
    let body = stdout(&out);
    assert!(body.contains("no entities"), "missing notice: {body}");
}

#[test]
fn remember_then_list_entities_aligns_columns() {
    let home = tempfile::tempdir().expect("tempdir");
    run(&home.path().to_path_buf(), &["init"]);
    let remember = run(
        &home.path().to_path_buf(),
        &[
            "remember",
            "Raymond",
            "--entity-type",
            "person",
            "--observation",
            "prefers Rust",
        ],
    );
    let remember_body = stdout(&remember);
    assert!(remember_body.contains("remembered 1 observation"));
    assert!(remember_body.contains("Raymond"));
    assert!(remember_body.contains("(person)"));

    let listed = run(&home.path().to_path_buf(), &["list-entities"]);
    let body = stdout(&listed);
    let lines: Vec<&str> = body.lines().collect();
    // Header row, rule, one data row.
    assert!(lines
        .iter()
        .any(|l| l.contains("name") && l.contains("type")));
    assert!(lines
        .iter()
        .any(|l| l.contains("Raymond") && l.contains("person")));
}

#[test]
fn forget_without_yes_prints_danger_error() {
    let home = tempfile::tempdir().expect("tempdir");
    run(&home.path().to_path_buf(), &["init"]);

    let out = Command::new(binary_path())
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("OPENMEMORY_HOME", home.path())
        .arg("--color=never")
        .args(["forget-entity", "SomeEntity"])
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "expected failure");
    let err = stderr(&out);
    // ASCII fail glyph + recognisable message.
    assert!(err.contains("x refusing"), "missing danger line: {err}");
    assert!(err.contains("--yes"));
}

#[test]
fn json_flag_emits_plain_json_not_ui() {
    let home = tempfile::tempdir().expect("tempdir");
    run(&home.path().to_path_buf(), &["init"]);

    let out = run(&home.path().to_path_buf(), &["list-entities", "--json"]);
    let body = stdout(&out);
    // Pure JSON: parses, no banner/box scaffolding leaks in.
    let trimmed = body.trim();
    let _: serde_json::Value = serde_json::from_str(trimmed).expect("json output");
    assert!(!body.contains('+'));
    assert!(!body.contains("no entities"));
}

#[test]
fn no_color_setup_uses_ascii_next_step_arrows() {
    let home = tempfile::tempdir().expect("tempdir");
    let out = Command::new(binary_path())
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("OPENMEMORY_HOME", home.path())
        .env("HOME", home.path())
        .env("CODEX_HOME", home.path().join("missing-codex-home"))
        .env("OPENMEMORY_SETUP_SKIP_VERIFY", "1")
        .env("PATH", "")
        .arg("--color=never")
        .args(["setup", "--client", "codex"])
        .output()
        .expect("spawn");

    let body = stdout(&out);
    assert!(body.contains(">  remember"), "missing ASCII arrow: {body}");
    assert!(
        !body.contains('›'),
        "unicode arrow leaked in no-color output: {body}"
    );
}
