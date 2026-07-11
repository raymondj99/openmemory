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

static ORT_INIT: OnceLock<()> = OnceLock::new();

/// Attempt to find the ONNX Runtime shared library in common
/// locations and set `ORT_DYLIB_PATH` if not already set.
///
/// Runs exactly once per process via `OnceLock`. The `set_var` call
/// happens before any threads are spawned (the MCP server's tokio
/// runtime starts after bootstrap returns).
#[allow(unsafe_code)]
fn init_ort_env() {
    ORT_INIT.get_or_init(|| {
        if std::env::var("ORT_DYLIB_PATH").is_ok() {
            return;
        }

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

        for path in candidates {
            if Path::new(path).exists() {
                // SAFETY: called exactly once via OnceLock, before the
                // tokio runtime (and its thread pool) is created.
                unsafe { std::env::set_var("ORT_DYLIB_PATH", path) };
                info!("Auto-detected ONNX Runtime at {path}");
                return;
            }
        }
    });
}

/// Load the default text embedder from a locally cached model.
///
/// Returns `None` when the ONNX Runtime is unavailable, the model
/// has not been downloaded yet, or the model cannot be loaded. The
/// caller should continue in keyword-only mode.
///
/// To download the model first, use [`ensure_model`].
pub fn load_embedder(models_dir: &Path) -> Option<crate::CachedEmbedder> {
    init_ort_env();

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
    init_ort_env();
    if std::env::var("ORT_DYLIB_PATH").is_err() {
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
