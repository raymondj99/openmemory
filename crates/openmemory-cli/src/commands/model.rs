//! `openmemory model`: manage embedding models.
//!
//! Subcommands: `download` fetches ONNX model files from Hugging Face
//! into `~/.openmemory/models/<name>/`; `list` shows every model in
//! the registry and whether its files are cached locally.

use std::io::Write;

use anyhow::{Context, Result};
use openmemory_core::config::Config;
use openmemory_embed::{ModelManager, ModelRegistry, RuntimeManager};

use crate::cli::ModelCommand;
use crate::ui::glyph::Glyph;
use crate::ui::steps::Steps;
use crate::ui::{card, paint, stdout_stream, style};

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
    let mut stream = stdout_stream();
    let mut steps = Steps::new(&mut stream);

    let model_ready = manager.downloaded_model_dir(model).is_some();
    if model_ready {
        let path = manager
            .downloaded_model_dir(model)
            .expect("model present")
            .display()
            .to_string();
        steps
            .step(format!("model {}", model.name))
            .finish_ok(format!("already at {path}"));
    }

    install_runtime(&mut steps)?;

    if model_ready {
        return Ok(());
    }

    let step = steps.step(format!("downloading model {}", model.name));
    match manager.download(model) {
        Ok(()) => step.finish_ok("ready"),
        Err(e) => {
            step.finish_fail(&format!("{e:#}"));
            return Err(e).with_context(|| format!("downloading model '{}'", model.name));
        }
    }
    Ok(())
}

fn install_runtime<W: Write>(steps: &mut Steps<'_, W>) -> Result<()> {
    let runtime = RuntimeManager::from_config().context("resolving runtime directory")?;
    if runtime.is_installed() {
        return Ok(());
    }
    let label = format!(
        "installing ONNX Runtime {}",
        openmemory_embed::ONNX_RUNTIME_VERSION
    );
    let step = steps.step(label);
    match runtime.install() {
        Ok(_) => {
            step.finish_ok("ready");
            Ok(())
        }
        Err(e) => {
            step.finish_fail(&format!("{e:#}"));
            Err(e).context("installing ONNX Runtime")
        }
    }
}

fn list() -> Result<()> {
    let models_dir = Config::models_dir().context("resolving models directory")?;
    let registry = ModelRegistry::default();
    let manager = ModelManager::new(models_dir);

    let default = registry.default_model();
    let mut stream = stdout_stream();
    let heading = paint(style::ACCENT_DIM, "available embedding models");
    let _ = writeln!(&mut stream, "  {heading}");
    card::separator(&mut stream);

    for (i, model) in registry.all().iter().copied().enumerate() {
        if i > 0 {
            card::separator(&mut stream);
        }
        let is_default = model.name == default.name;
        let downloaded = manager.downloaded_model_dir(model).is_some();
        let status = if downloaded {
            "downloaded"
        } else {
            "not downloaded"
        };
        let sep = Glyph::Separator.as_str();
        let suffix = if is_default {
            format!("default {sep} {status}")
        } else {
            status.to_string()
        };
        let header = model.name.to_string();
        let body = if model.aliases.is_empty() {
            format!("{} dim {sep} pooling {:?}", model.dimensions, model.pooling)
        } else {
            format!(
                "{} dim {sep} pooling {:?} {sep} aliases {}",
                model.dimensions,
                model.pooling,
                model.aliases.join(", ")
            )
        };
        card::render(&mut stream, &header, &suffix, &body);
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

    let mut stream = stdout_stream();
    let glyph = paint(style::SUCCESS, Glyph::Ok.as_str());
    let name_painted = paint(style::ACCENT_DIM, model.name);
    let _ = writeln!(&mut stream, "  {glyph} active model set to {name_painted}");
    let arrow = paint(style::ACCENT_DIM, Glyph::Arrow.as_str());
    if !downloaded {
        let hint = paint(
            style::MUTED,
            &format!("not yet downloaded; run `openmemory model download {name}` first."),
        );
        let _ = writeln!(&mut stream, "  {arrow} {hint}");
    }
    let next = paint(
        style::MUTED,
        "takes effect on the next mcp / remember / recall invocation.",
    );
    let _ = writeln!(&mut stream, "  {arrow} {next}");
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
