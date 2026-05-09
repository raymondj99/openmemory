//! `openmemory model`: manage embedding models.
//!
//! Subcommands: `download` fetches ONNX model files from Hugging Face
//! into `~/.openmemory/models/<name>/`; `list` shows every model in
//! the registry and whether its files are cached locally.

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_embed::{ModelManager, ModelRegistry};

use crate::cli::ModelCommand;

pub fn run(command: ModelCommand) -> Result<()> {
    match command {
        ModelCommand::Download(args) => download(args.model.as_deref()),
        ModelCommand::List => list(),
    }
}

fn download(name: Option<&str>) -> Result<()> {
    let models_dir = Config::models_dir().context("resolving models directory")?;
    let registry = ModelRegistry::default();

    let model = if let Some(name) = name {
        registry
            .get(name)
            .with_context(|| format!("unknown model {name:?}"))?
    } else {
        registry.default_model()
    };

    let manager = ModelManager::new(models_dir);

    if let Some(dir) = manager.downloaded_model_dir(model) {
        println!(
            "Model '{}' already downloaded at {}",
            model.name,
            dir.display()
        );
        return Ok(());
    }

    println!("Downloading '{}'...", model.name);
    manager
        .download(model)
        .with_context(|| format!("downloading model '{}'", model.name))?;
    println!("Model '{}' ready.", model.name);
    Ok(())
}

fn list() -> Result<()> {
    let models_dir = Config::models_dir().context("resolving models directory")?;
    let registry = ModelRegistry::default();
    let manager = ModelManager::new(models_dir);

    let default = registry.default_model();
    println!("Available embedding models:\n");
    for model in registry.all() {
        let is_default = model.name == default.name;
        let downloaded = manager.downloaded_model_dir(model).is_some();

        let status = if downloaded {
            "downloaded"
        } else {
            "not downloaded"
        };
        let tag = if is_default { " (default)" } else { "" };

        println!("  {}{tag}", model.name);
        println!("    dimensions : {}", model.dimensions);
        println!("    pooling    : {:?}", model.pooling);
        println!("    status     : {status}");
        if !model.aliases.is_empty() {
            println!("    aliases    : {}", model.aliases.join(", "));
        }
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_runs_without_error() {
        let dir = tempfile::tempdir().unwrap();
        crate::cli::with_home(dir.path(), || {
            list().unwrap();
        });
    }
}
