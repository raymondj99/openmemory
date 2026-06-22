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

// The `model` subcommand only exists with the `embeddings` feature; in
// `--no-default-features` builds it is compiled out entirely.
#[cfg(feature = "embeddings")]
#[test]
fn model_list_marks_active_model_and_persists_across_use() {
    let home = tempfile::tempdir().expect("tempdir");
    let home_path = home.path().to_path_buf();
    run(&home_path, &["init"]);

    // Baseline listing: exactly one row should be tagged `active`, and
    // that row's status line must mention the registry default
    // (nomic-embed-text-v1.5 today, but we don't pin the name here).
    let baseline = stdout(&run(&home_path, &["model", "list"]));
    let active_lines: Vec<&str> = baseline.lines().filter(|l| l.contains("active")).collect();
    assert_eq!(
        active_lines.len(),
        1,
        "exactly one row should be active, got {active_lines:?} in:\n{baseline}"
    );
    let baseline_active = active_lines[0].to_string();
    assert!(
        baseline_active.contains("nomic-embed-text-v1.5") || baseline_active.contains("nomic"),
        "default-active row should reference nomic-* model: {baseline_active:?}"
    );

    // Flip the active model to arctic via its alias; this exercises
    // both the alias resolver in `model use` and the listing's
    // alias-aware lookup in `resolve_active`.
    let use_out = run(&home_path, &["model", "use", "arctic"]);
    let use_body = stdout(&use_out);
    assert!(
        use_body.contains("snowflake-arctic-embed-l-v2.0"),
        "model use should resolve the alias and confirm canonical name: {use_body}"
    );

    // After the flip the active row must be the arctic model, and the
    // previously-active nomic row must lose its `active` tag.
    let after = stdout(&run(&home_path, &["model", "list"]));
    let after_active: Vec<&str> = after.lines().filter(|l| l.contains("active")).collect();
    assert_eq!(
        after_active.len(),
        1,
        "exactly one row should be active after use, got {after_active:?} in:\n{after}"
    );
    assert!(
        after_active[0].contains("snowflake-arctic-embed-l-v2.0"),
        "active row should now be arctic, got {:?}",
        after_active[0]
    );
    assert!(
        !after.lines().any(|l| {
            l.contains("nomic-embed-text-v1.5") && l.contains("active") && !l.contains("snowflake")
        }),
        "nomic row must no longer be tagged active: {after}"
    );
}

#[cfg(feature = "embeddings")]
#[test]
fn model_list_warns_when_configured_model_is_unknown() {
    let home = tempfile::tempdir().expect("tempdir");
    let home_path = home.path().to_path_buf();
    run(&home_path, &["init"]);

    // Write a config that pins a model the registry has never heard
    // of. Mirrors a real failure mode: a binary upgrade dropped the
    // model, or the user hand-edited config.toml.
    let config_path = home_path.join("config.toml");
    std::fs::write(
        &config_path,
        "[default]\nmodel = \"ghost-model-v9\"\n[memory]\n[index]\n[search]\n[watch]\n",
    )
    .expect("write config");

    let body = stdout(&run(&home_path, &["model", "list"]));
    assert!(
        body.contains("ghost-model-v9") && body.contains("no longer registered"),
        "should surface a notice about the unknown configured model: {body}"
    );
    // Fallback: the registry default must still appear as active so
    // the user can see what's actually in use.
    let active_lines: Vec<&str> = body.lines().filter(|l| l.contains("active")).collect();
    assert_eq!(
        active_lines.len(),
        1,
        "fallback should produce exactly one active row: {body}"
    );
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
