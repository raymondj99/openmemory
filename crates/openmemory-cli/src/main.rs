//! `openmemory` command-line entry point.
//!
//! Thin shell over the library crate (`src/lib.rs`). All real work
//! lives in `openmemory_cli::cli` / `commands` / `ui`; keeping this
//! file tiny means integration tests can hit the same code paths
//! without spawning the binary.

#![forbid(unsafe_code)]

use openmemory_cli::{cli, ui};

fn main() -> std::process::ExitCode {
    init_tracing();
    init_ort_dylib_path();

    let args = std::env::args_os();
    match cli::run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            let mut stream = ui::stderr_stream();
            ui::error::render(&mut stream, &e);
            std::process::ExitCode::FAILURE
        }
    }
}

/// Point `ort` at the per-user ONNX Runtime install if one is present.
///
/// `ort` is built with `load-dynamic`, so it `dlopen`s
/// `libonnxruntime` at the path given by `ORT_DYLIB_PATH` (or falls
/// back to `LD_LIBRARY_PATH` discovery). Users who installed via
/// `openmemory model download` get the runtime under
/// `<home>/runtime/onnxruntime-<version>/lib/`; this function wires
/// that path into the env before any `ort` code runs, but never
/// overrides a user-set `ORT_DYLIB_PATH` or `LD_LIBRARY_PATH`.
///
/// Home resolution mirrors what the rest of the CLI does *after*
/// clap parsing, except we run before clap: scan argv for a
/// `--home <path>` pair, fall back to `OPENMEMORY_HOME`, and finally
/// to `~/.openmemory`. Without this, `openmemory --home X mcp`
/// children spawned by a parent that customised `--home` would miss
/// the runtime install and panic on the first vector-mode call.
#[cfg(feature = "embeddings")]
fn init_ort_dylib_path() {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }
    // If `--home <path>` was passed on the command line, promote it
    // into the env so the embed crate's `Config::home_dir()` picks
    // it up. We don't touch `OPENMEMORY_HOME` if the user already
    // set it explicitly — that's the documented override.
    if std::env::var_os("OPENMEMORY_HOME").is_none() {
        if let Some(home) = parse_home_arg() {
            // SAFETY: set_var is safe in single-threaded contexts;
            // we run from `main` before any worker threads spawn.
            std::env::set_var("OPENMEMORY_HOME", home);
        }
    }
    if let Ok(rm) = openmemory_embed::RuntimeManager::from_config() {
        rm.set_ort_dylib_path_if_present();
    }
}

#[cfg(not(feature = "embeddings"))]
fn init_ort_dylib_path() {}

/// Best-effort `--home <path>` extraction from `std::env::args`.
/// Recognised forms: `--home X`, `--home=X`. Returns `None` if the
/// flag is absent or malformed; clap will surface a clearer error
/// later in that case.
#[cfg(feature = "embeddings")]
fn parse_home_arg() -> Option<std::path::PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let s = arg.to_string_lossy();
        if let Some(rest) = s.strip_prefix("--home=") {
            return Some(std::path::PathBuf::from(rest.to_string()));
        }
        if s == "--home" {
            return args.next().map(std::path::PathBuf::from);
        }
    }
    None
}

/// Initialise the global tracing subscriber.
///
/// For most subcommands this is a stderr fmt subscriber (the existing
/// behavior). When the user is launching the TUI — either explicitly
/// with `openmemory tui` or implicitly with a bare `openmemory` —
/// stderr will be hidden under the alt-screen and any log line would
/// corrupt the rendered frame. We detect that case via a cheap argv
/// scan and route logs to `<home>/tui/log.jsonl` instead so they're
/// still captured but don't break the UI. If the file can't be opened
/// (read-only home, permission error) we install a sink writer rather
/// than fall back to stderr — corrupting the frame is the worse user
/// outcome.
fn init_tracing() {
    if is_tui_invocation() {
        match open_tui_log_file() {
            Some(file) => {
                let writer = std::sync::Mutex::new(file);
                let _ = tracing_subscriber::fmt()
                    .with_writer(writer)
                    .with_ansi(false)
                    .try_init();
            }
            None => {
                let _ = tracing_subscriber::fmt()
                    .with_writer(std::io::sink)
                    .try_init();
            }
        }
    } else {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .try_init();
    }
}

/// Does the user-supplied argv look like a TUI launch? Returns `true`
/// for `openmemory` (no subcommand) and `openmemory tui`. Conservative
/// on purpose: we'd rather miss a true positive and keep logs on
/// stderr than misclassify a scriptable subcommand as a TUI launch.
fn is_tui_invocation() -> bool {
    let mut args = std::env::args_os().skip(1);
    let Some(first) = args.next() else {
        // Bare `openmemory`.
        return true;
    };
    let first = first.to_string_lossy();
    // Skip past leading global flags before the subcommand.
    let mut remaining = if first.starts_with("--") {
        // Could be `--home X tui` or similar; peek past one or two.
        let mut peeked = vec![first.to_string()];
        for _ in 0..4 {
            if let Some(arg) = args.next() {
                peeked.push(arg.to_string_lossy().to_string());
            }
        }
        peeked
    } else {
        vec![first.to_string()]
    };
    remaining.retain(|a| !a.starts_with("--") && !a.is_empty());
    // First non-flag token is the subcommand.
    let subcommand = remaining.into_iter().find(|a| !a.is_empty());
    match subcommand {
        Some(s) if s == "tui" => true,
        Some(_) => false,
        None => true,
    }
}

fn open_tui_log_file() -> Option<std::fs::File> {
    let home = openmemory_core::config::Config::home_dir().ok()?;
    let dir = home.join("tui");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("log.jsonl");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()
}
