//! `openmemory` command-line entry point.

#![forbid(unsafe_code)]

mod cli;
mod commands;
mod ui;

fn main() -> std::process::ExitCode {
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
