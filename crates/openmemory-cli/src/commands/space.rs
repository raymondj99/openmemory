//! `openmemory space` — manage memory spaces.
//!
//! A memory space is an isolated store beside the personal-global
//! default: its entities, observations, relations, and indexed text
//! never leak into other spaces. Spaces live under
//! `<data_dir>/spaces/<name>/` and are addressed from MCP via the
//! `space` / `read_spaces` tool parameters.

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_engine::space::SpaceManager;

use crate::cli::SpaceCommand;

pub fn run(profile: &str, command: SpaceCommand) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let data_dir = Config::data_dir(profile).context("resolving data directory")?;
    let manager = SpaceManager::new(config.clone(), &data_dir, config.engine.domains);

    match command {
        SpaceCommand::Create(args) => {
            let info = manager
                .create(&args.name)
                .with_context(|| format!("creating space '{}'", args.name))?;
            println!("created space '{}' ({})", info.name, info.space_id);
            Ok(())
        }
        SpaceCommand::List => {
            let spaces = manager.list().context("listing spaces")?;
            println!("default (personal-global)");
            for space in spaces {
                println!("{} ({})", space.name, space.space_id);
            }
            Ok(())
        }
    }
}
