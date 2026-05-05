//! `open-memory` command-line entry point.

#![forbid(unsafe_code)]

mod cli;
mod commands;

fn main() -> std::process::ExitCode {
    init_tracing();

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

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .try_init();
}
