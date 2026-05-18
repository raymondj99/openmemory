//! `openmemory` command-line entry point.

#![forbid(unsafe_code)]

mod cli;
mod commands;

fn main() -> std::process::ExitCode {
    init_tracing();
    init_ort_dylib_path();

    let args = std::env::args_os();
    match cli::run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            for cause in e.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
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
/// `~/.openmemory/runtime/onnxruntime-<version>/lib/`; this function
/// wires that path into the env before any `ort` code runs, but never
/// overrides a user-set `ORT_DYLIB_PATH` or `LD_LIBRARY_PATH`.
#[cfg(feature = "embeddings")]
fn init_ort_dylib_path() {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }
    if let Ok(rm) = openmemory_embed::RuntimeManager::from_config() {
        rm.set_ort_dylib_path_if_present();
    }
}

#[cfg(not(feature = "embeddings"))]
fn init_ort_dylib_path() {}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .try_init();
}
