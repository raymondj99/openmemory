//! Embedder bootstrap shared by `openmemory-cli` and `openmemory-mcp`.
//!
//! [`load_embedder`] resolves the default ONNX model from a local
//! cache directory and returns an [`OnnxEmbedder`] ready for use.
//! It never touches the network; returns `None` on any miss so
//! callers fall back to keyword-only search at the boundary.
//!
//! [`ensure_model`] is the download counterpart: it fetches from
//! Hugging Face when the model is absent and the ONNX Runtime is
//! available. Intended for `openmemory model download`, not server
//! startup.

use crate::download::ModelManager;
use crate::models::{Model, ModelRegistry};
use crate::OnnxEmbedder;
use std::path::Path;
use std::sync::OnceLock;
use tracing::{info, warn};

/// Resolve which model to use.
///
/// Priority: `OPENMEMORY_MODEL` env var > `default.model` in
/// config.toml > registry default (nomic-embed-text-v1.5).
fn resolve_model(registry: &ModelRegistry) -> &'static Model {
    if let Ok(name) = std::env::var("OPENMEMORY_MODEL") {
        if let Some(model) = registry.get(&name) {
            info!("Using model from OPENMEMORY_MODEL: {}", model.name);
            return model;
        }
        warn!(
            "OPENMEMORY_MODEL={name:?} not found in registry, \
             falling back to config/default"
        );
    }
    if let Ok(config) = openmemory_core::config::Config::load() {
        if let Some(name) = &config.default.model {
            if let Some(model) = registry.get(name) {
                info!("Using model from config: {}", model.name);
                return model;
            }
            warn!(
                "config default.model={name:?} not found in registry, \
                 falling back to default"
            );
        }
    }
    registry.default_model()
}

static ORT_PATH_INIT: OnceLock<bool> = OnceLock::new();

/// Configure ONNX Runtime from an explicit or discovered library path.
///
/// `ort::init_from` records the dynamic-loader path without mutating the
/// process environment. Actual runtime loading remains lazy so a missing
/// runtime cannot break keyword-only startup before a model is present.
fn init_ort_loader_path() -> bool {
    *ORT_PATH_INIT.get_or_init(|| {
        let configured = std::env::var_os("ORT_DYLIB_PATH").map(std::path::PathBuf::from);
        let discovered = configured.or_else(|| {
            let candidates: &[&str] = if cfg!(target_os = "macos") {
                &[
                    "/opt/homebrew/opt/onnxruntime/lib/libonnxruntime.dylib",
                    "/usr/local/opt/onnxruntime/lib/libonnxruntime.dylib",
                    "/usr/local/lib/libonnxruntime.dylib",
                ]
            } else {
                &[
                    "/usr/lib/libonnxruntime.so",
                    "/usr/local/lib/libonnxruntime.so",
                    "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
                    "/usr/lib/aarch64-linux-gnu/libonnxruntime.so",
                ]
            };
            candidates
                .iter()
                .map(std::path::PathBuf::from)
                .find(|path| path.exists())
        });

        if let Some(path) = discovered {
            info!("Configuring ONNX Runtime from {}", path.display());
            // Creating this builder safely records ort's process-global loader
            // path. Do not commit it here: session construction owns the
            // actual load, as it did before this bootstrap refactor.
            let _ = ort::init_from(path.to_string_lossy());
            true
        } else {
            false
        }
    })
}

/// Load the default text embedder from a locally cached model.
///
/// Returns `None` when the ONNX Runtime is unavailable, the model
/// has not been downloaded yet, or the model cannot be loaded. The
/// caller should continue in keyword-only mode.
///
/// To download the model first, use [`ensure_model`].
pub fn load_embedder(models_dir: &Path) -> Option<crate::CachedEmbedder> {
    init_ort_loader_path();

    let manager = ModelManager::new(models_dir.to_path_buf());
    let registry = ModelRegistry::default();
    let model = resolve_model(&registry);

    let model_dir = if let Some(dir) = manager.downloaded_model_dir(model) {
        dir
    } else {
        info!(
            "Embedding model not downloaded. Running in keyword-only mode. \
             Run `openmemory model download` for semantic search."
        );
        return None;
    };

    match OnnxEmbedder::load_for_model(&model_dir, model) {
        Ok(embedder) => {
            info!("Loaded embedding model: {}", model.name);
            let cache_path = models_dir.join("embeddings").join("cache.sqlite");
            let cache = crate::EmbeddingCache::open(&cache_path).or_else(|error| {
                warn!(
                    "Failed to open embedding cache at {}: {error}; using an in-memory cache",
                    cache_path.display()
                );
                crate::EmbeddingCache::in_memory()
            });
            match cache {
                Ok(cache) => Some(crate::CachedEmbedder::new(embedder, cache)),
                Err(error) => {
                    warn!("Failed to initialize embedding cache: {error}");
                    None
                }
            }
        }
        Err(e) => {
            warn!(
                "Failed to load embedding model: {e}. \
                 Running in keyword-only mode."
            );
            None
        }
    }
}

/// Download the default embedding model if not already present.
///
/// Skips the download when the ONNX Runtime is not available on
/// this system (no point caching a model that can't be loaded).
///
/// Returns `true` when the model is available (was already cached or
/// was freshly downloaded). Returns `false` when ORT is missing or
/// the download fails.
pub fn ensure_model(models_dir: &Path) -> bool {
    if !init_ort_loader_path() {
        info!("ONNX Runtime not found. Skipping model download.");
        return false;
    }

    let manager = ModelManager::new(models_dir.to_path_buf());
    let registry = ModelRegistry::default();
    let model = resolve_model(&registry);

    if manager.downloaded_model_dir(model).is_some() {
        return true;
    }

    info!(
        "Embedding model '{}' not found locally, downloading...",
        model.name
    );
    if let Err(e) = manager.download(model) {
        warn!("Failed to download embedding model: {e}");
        return false;
    }
    true
}

/// Convenience wrapper: resolve `~/.openmemory/models/` from the
/// config and call [`load_embedder`].
pub fn load_default_embedder() -> Option<crate::CachedEmbedder> {
    let models_dir = openmemory_core::config::Config::models_dir().ok()?;
    load_embedder(&models_dir)
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_model_cache_falls_back_without_loading_ort() {
        let models = tempfile::tempdir().expect("create empty model cache");
        assert!(super::load_embedder(models.path()).is_none());
    }
}
