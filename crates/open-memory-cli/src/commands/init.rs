//! `open-memory init` — bootstrap the data directory.
//!
//! Creates `~/.open-memory/`, writes a default `config.toml`, and lays down
//! the per-profile data directory under `data/<profile>/`. Idempotent
//! unless the user passes `--force`, in which case the existing config is
//! overwritten.

use anyhow::{Context, Result};
use open_memory_core::config::Config;

use crate::cli::InitArgs;

pub fn run(profile: &str, args: InitArgs) -> Result<()> {
    let home = Config::home_dir().context("resolving open-memory home")?;
    let config_path = home.join("config.toml");
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;

    std::fs::create_dir_all(&home).context("creating home directory")?;
    std::fs::create_dir_all(&data_dir).context("creating profile data directory")?;

    let config_existed = config_path.exists();
    if config_existed && !args.force {
        println!(
            "open-memory already initialised at {} (config exists; pass --force to overwrite)",
            home.display()
        );
        println!("data directory: {}", data_dir.display());
        return Ok(());
    }

    let config = Config::default();
    config
        .save(&config_path)
        .context("writing default config.toml")?;

    println!("Initialised open-memory at {}", home.display());
    println!("  config:  {}", config_path.display());
    println!("  profile: {profile}");
    println!("  data:    {}", data_dir.display());
    if config_existed {
        println!("  (existing config was overwritten via --force)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::with_home;

    #[test]
    fn init_creates_home_config_and_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            run("default", InitArgs { force: false }).unwrap();
            assert!(dir.path().join("config.toml").exists());
            assert!(dir.path().join("data").join("default").is_dir());
        });
    }

    #[test]
    fn init_is_idempotent_without_force() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            run("default", InitArgs { force: false }).unwrap();
            // Second run should not error and should not modify the file.
            let before = std::fs::metadata(dir.path().join("config.toml"))
                .unwrap()
                .modified()
                .unwrap();
            run("default", InitArgs { force: false }).unwrap();
            let after = std::fs::metadata(dir.path().join("config.toml"))
                .unwrap()
                .modified()
                .unwrap();
            assert_eq!(before, after);
        });
    }

    #[test]
    fn init_overwrites_with_force() {
        let dir = tempfile::tempdir().unwrap();
        with_home(dir.path(), || {
            run("default", InitArgs { force: false }).unwrap();
            std::fs::write(dir.path().join("config.toml"), "garbage").unwrap();
            run("default", InitArgs { force: true }).unwrap();
            // After --force, the config should be valid TOML again.
            let cfg = Config::load_from(dir.path().join("config.toml")).unwrap();
            assert_eq!(cfg.search.max_results, 10);
        });
    }
}
