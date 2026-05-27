//! `openmemory init` — bootstrap the data directory.
//!
//! Creates `~/.openmemory/`, writes a default `config.toml`, and lays down
//! the per-profile data directory under `data/<profile>/`. Idempotent
//! unless the user passes `--force`, in which case the existing config is
//! overwritten.

use std::io::Write;

use anyhow::{Context, Result};
use openmemory_core::config::Config;

use crate::cli::InitArgs;
use crate::ui::banner::{Banner, Line};
use crate::ui::glyph::Glyph;
use crate::ui::steps::Steps;
use crate::ui::{paint, stdout_stream, style};

pub fn run(profile: &str, args: InitArgs) -> Result<()> {
    let home = Config::home_dir().context("resolving openmemory home")?;
    let config_path = home.join("config.toml");
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;

    std::fs::create_dir_all(&home).context("creating home directory")?;
    std::fs::create_dir_all(&data_dir).context("creating profile data directory")?;

    let mut stream = stdout_stream();
    let config_existed = config_path.exists();
    if config_existed && !args.force {
        // Idempotent path: one calm step line, no banner.
        let mut steps = Steps::new(&mut stream).opener(false);
        steps
            .step(format!("already initialised at {}", home.display()))
            .finish_ok(format!("profile: {profile}"));
        return Ok(());
    }

    let config = Config::default();
    config
        .save(&config_path)
        .context("writing default config.toml")?;

    Banner::new("openmemory")
        .subtitle("initialised")
        .line(Line::Pair {
            label: "home   ".into(),
            value: home.display().to_string(),
        })
        .line(Line::Pair {
            label: "config ".into(),
            value: config_path.display().to_string(),
        })
        .line(Line::Pair {
            label: "data   ".into(),
            value: data_dir.display().to_string(),
        })
        .line(Line::Pair {
            label: "profile".into(),
            value: profile.to_string(),
        })
        .render(&mut stream);

    if config_existed {
        let arrow = paint(style::WARN, Glyph::Arrow.as_str());
        let note = paint(style::MUTED, "existing config was overwritten via --force.");
        let _ = writeln!(&mut stream, "  {arrow} {note}");
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
