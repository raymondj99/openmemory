//! `openmemory model`: manage embedding models.
//!
//! Subcommands: `download` fetches ONNX model files from Hugging Face
//! into `~/.openmemory/models/<name>/`; `list` shows every model in
//! the registry and whether its files are cached locally.

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_embed::{ModelManager, ModelRegistry, RuntimeManager};

use crate::cli::ModelCommand;

pub fn run(command: ModelCommand) -> Result<()> {
    match command {
        ModelCommand::Download(args) => download(args.model.as_deref()),
        ModelCommand::List => list(),
        ModelCommand::Use(args) => use_model(&args.model),
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

    let model_ready = manager.downloaded_model_dir(model).is_some();
    if model_ready {
        println!(
            "Model '{}' already downloaded at {}",
            model.name,
            manager
                .downloaded_model_dir(model)
                .expect("model present")
                .display()
        );
    }

    // Install the platform-matched ONNX Runtime alongside the model so
    // `openmemory recall --mode vector` works on a clean machine
    // without manual `LD_LIBRARY_PATH` / `ORT_DYLIB_PATH` plumbing.
    install_runtime()?;

    if model_ready {
        return Ok(());
    }

    println!("Downloading '{}'...", model.name);
    manager
        .download(model)
        .with_context(|| format!("downloading model '{}'", model.name))?;
    println!("Model '{}' ready.", model.name);
    Ok(())
}

fn install_runtime() -> Result<()> {
    let runtime = RuntimeManager::from_config().context("resolving runtime directory")?;
    if runtime.is_installed() {
        return Ok(());
    }
    println!(
        "Installing ONNX Runtime {}...",
        openmemory_embed::ONNX_RUNTIME_VERSION
    );
    runtime.install().context("installing ONNX Runtime")?;
    println!(
        "ONNX Runtime {} ready.",
        openmemory_embed::ONNX_RUNTIME_VERSION
    );
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

fn use_model(name: &str) -> Result<()> {
    let registry = ModelRegistry::default();
    let model = registry
        .get(name)
        .with_context(|| format!("unknown model {name:?}"))?;

    let config_path = Config::config_path().context("resolving config path")?;
    let mut config = Config::load().unwrap_or_default();
    config.default.model = Some(model.name.to_string());
    config.save(&config_path).context("saving config")?;

    let models_dir = Config::models_dir().ok();
    let downloaded = models_dir.is_some_and(|d| {
        openmemory_embed::ModelManager::new(d)
            .downloaded_model_dir(model)
            .is_some()
    });

    println!("Active model set to '{}'.", model.name);
    if !downloaded {
        println!(
            "Note: model not yet downloaded. Run `openmemory model download {}` first.",
            name
        );
    }
    println!("Takes effect on the next `openmemory mcp`, `remember`, or `recall` invocation.");
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
