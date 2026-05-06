//! `openmemory completions <shell>` — emit shell-completion scripts via
//! `clap_complete`.

#[cfg(feature = "completions")]
use anyhow::Result;
#[cfg(feature = "completions")]
use clap::CommandFactory;
#[cfg(feature = "completions")]
use clap_complete::{generate, Shell};

#[cfg(feature = "completions")]
pub fn run(shell: Shell) -> Result<()> {
    let mut cmd = crate::cli::Cli::command();
    let mut stdout = std::io::stdout();
    generate(shell, &mut cmd, "openmemory", &mut stdout);
    Ok(())
}

#[cfg(not(feature = "completions"))]
#[allow(dead_code)]
pub fn run() -> anyhow::Result<()> {
    anyhow::bail!(
        "this build does not include the completions feature; rebuild openmemory with \
         `--features completions`"
    )
}

#[cfg(all(test, feature = "completions"))]
mod tests {
    use super::*;

    #[test]
    fn completions_runs_for_bash() {
        run(Shell::Bash).unwrap();
    }

    #[test]
    fn completions_runs_for_zsh() {
        run(Shell::Zsh).unwrap();
    }

    #[test]
    fn completions_runs_for_fish() {
        run(Shell::Fish).unwrap();
    }
}
