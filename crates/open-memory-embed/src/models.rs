//! Static registry of supported embedding models.
//!
//! Two models ship in v0.1:
//!
//! - `nomic-embed-text-v1.5` (default, 768-dim, mean-pooled)
//! - `snowflake-arctic-embed-l-v2.0` (alternate, 1024-dim, CLS-pooled)
//!
//! Each entry carries the metadata needed to download the model
//! (Hugging Face URL + SHA-256), tokenize inputs (prefixes, max
//! tokens), and run inference ([`PoolingStrategy`] + ONNX output
//! tensor name).
//!
//! # Integrity verification
//!
//! [`OnnxEmbedder::load_for_model`] hashes the on-disk `model.onnx`
//! and `tokenizer.json` files with SHA-256 and compares against the
//! `_sha256` fields here before handing any bytes to the ONNX runtime.
//! v0.2.0 ships with empty placeholders, which the verifier treats as
//! [`VerificationOutcome::Skipped`] (warns, loads anyway) — populating
//! real hashes is tracked for v0.3 and tightens the path to "always
//! verified" with no further code changes.
//!
//! [`OnnxEmbedder::load_for_model`]: crate::onnx::OnnxEmbedder::load_for_model
//! [`VerificationOutcome::Skipped`]: crate::integrity::VerificationOutcome::Skipped

use crate::onnx::{OnnxOptions, PoolingStrategy};

/// Metadata describing one supported embedding model.
#[derive(Debug, Clone, Copy)]
pub struct Model {
    /// Canonical short name (e.g. `"nomic-embed-text-v1.5"`).
    pub name: &'static str,
    /// Alternate names that resolve to the same model.
    pub aliases: &'static [&'static str],
    /// Hugging Face repo ID, used to construct download URLs.
    pub repo_id: &'static str,
    /// Output embedding dimensionality.
    pub dimensions: usize,
    /// Maximum sequence length the model accepts.
    pub max_tokens: usize,
    /// How to reduce per-token hidden states to a sentence embedding.
    pub pooling: PoolingStrategy,
    /// Name of the ONNX output tensor that holds the embedding.
    pub output_tensor: &'static str,
    /// Prefix to prepend to query texts before embedding.
    pub search_prefix: &'static str,
    /// Prefix to prepend to document texts before embedding.
    pub document_prefix: &'static str,
    /// HTTPS URL for the ONNX model file.
    pub onnx_url: &'static str,
    /// HTTPS URL for the tokenizer.json file.
    pub tokenizer_url: &'static str,
    /// SHA-256 checksum of the ONNX file in lowercase hex. Empty
    /// means not yet recorded — future download code treats empty
    /// as "skip verify".
    pub onnx_sha256: &'static str,
    /// SHA-256 checksum of the tokenizer.json file in lowercase hex.
    pub tokenizer_sha256: &'static str,
}

impl Model {
    /// Build an [`OnnxOptions`] block matching this model's pooling
    /// strategy, max-token cap, and output tensor name.
    #[must_use]
    pub fn onnx_options(&self) -> OnnxOptions {
        OnnxOptions {
            max_tokens: self.max_tokens,
            pooling: self.pooling,
            output_tensor: self.output_tensor,
        }
    }

    /// Prefix `text` with this model's [`search_prefix`].
    ///
    /// Some models (e.g. Nomic v1.5) embed queries differently from
    /// documents; pass the search-side string through this helper
    /// before calling [`Embedder::embed`].
    ///
    /// [`search_prefix`]: Model::search_prefix
    /// [`Embedder::embed`]: crate::Embedder::embed
    #[must_use]
    pub fn format_query(&self, text: &str) -> String {
        format!("{}{}", self.search_prefix, text)
    }

    /// Prefix `text` with this model's [`document_prefix`].
    ///
    /// [`document_prefix`]: Model::document_prefix
    #[must_use]
    pub fn format_document(&self, text: &str) -> String {
        format!("{}{}", self.document_prefix, text)
    }
}

// ---------------------------------------------------------------------
// Model definitions
// ---------------------------------------------------------------------

/// `nomic-embed-text-v1.5` — 768-dim, mean-pooled. Default model.
pub const NOMIC_EMBED_TEXT_V1_5: Model = Model {
    name: "nomic-embed-text-v1.5",
    aliases: &["nomic", "nomic-embed-text"],
    repo_id: "nomic-ai/nomic-embed-text-v1.5",
    dimensions: 768,
    max_tokens: 8192,
    pooling: PoolingStrategy::MeanPooling,
    output_tensor: "last_hidden_state",
    search_prefix: "search_query: ",
    document_prefix: "search_document: ",
    onnx_url: "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/main/onnx/model.onnx",
    tokenizer_url:
        "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/main/tokenizer.json",
    onnx_sha256: "",
    tokenizer_sha256: "",
};

/// `snowflake-arctic-embed-l-v2.0` — 1024-dim, CLS-pooled.
pub const SNOWFLAKE_ARCTIC_EMBED_L_V2: Model = Model {
    name: "snowflake-arctic-embed-l-v2.0",
    aliases: &["arctic", "arctic-embed-l-v2.0"],
    repo_id: "Snowflake/snowflake-arctic-embed-l-v2.0",
    dimensions: 1024,
    max_tokens: 8192,
    pooling: PoolingStrategy::ClsToken,
    output_tensor: "last_hidden_state",
    search_prefix: "query: ",
    document_prefix: "",
    onnx_url: "https://huggingface.co/Snowflake/snowflake-arctic-embed-l-v2.0/resolve/main/onnx/model.onnx",
    tokenizer_url:
        "https://huggingface.co/Snowflake/snowflake-arctic-embed-l-v2.0/resolve/main/tokenizer.json",
    onnx_sha256: "",
    tokenizer_sha256: "",
};

const ALL_MODELS: &[&Model] = &[&NOMIC_EMBED_TEXT_V1_5, &SNOWFLAKE_ARCTIC_EMBED_L_V2];

/// Static lookup for every supported embedding model.
///
/// Use [`ModelRegistry::default()`] to construct one — there is no
/// per-instance state, but exposing it as a struct keeps the door
/// open for a runtime registry (extra models loaded from a config
/// file) without breaking the API later.
#[derive(Debug, Clone, Copy)]
pub struct ModelRegistry {
    models: &'static [&'static Model],
}

impl ModelRegistry {
    /// Construct a registry containing the built-in model set.
    #[must_use]
    pub const fn new() -> Self {
        Self { models: ALL_MODELS }
    }

    /// Look up a model by name or alias.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&'static Model> {
        self.models
            .iter()
            .copied()
            .find(|m| m.name == name || m.aliases.contains(&name))
    }

    /// All registered models, in declaration order.
    #[must_use]
    pub fn all(&self) -> &[&'static Model] {
        self.models
    }

    /// The default model. First entry in the registry.
    #[must_use]
    pub fn default_model(&self) -> &'static Model {
        self.models[0]
    }
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_two_models() {
        let r = ModelRegistry::default();
        assert_eq!(r.all().len(), 2);
    }

    #[test]
    fn default_model_is_nomic() {
        let r = ModelRegistry::default();
        assert_eq!(r.default_model().name, "nomic-embed-text-v1.5");
    }

    #[test]
    fn lookup_by_canonical_name() {
        let r = ModelRegistry::default();
        let m = r.get("nomic-embed-text-v1.5").expect("should exist");
        assert_eq!(m.dimensions, 768);
        assert_eq!(m.pooling, PoolingStrategy::MeanPooling);

        let m = r
            .get("snowflake-arctic-embed-l-v2.0")
            .expect("should exist");
        assert_eq!(m.dimensions, 1024);
        assert_eq!(m.pooling, PoolingStrategy::ClsToken);
    }

    #[test]
    fn lookup_by_alias() {
        let r = ModelRegistry::default();
        assert_eq!(
            r.get("nomic").map(|m| m.name),
            Some("nomic-embed-text-v1.5")
        );
        assert_eq!(
            r.get("arctic").map(|m| m.name),
            Some("snowflake-arctic-embed-l-v2.0")
        );
        assert_eq!(
            r.get("arctic-embed-l-v2.0").map(|m| m.name),
            Some("snowflake-arctic-embed-l-v2.0")
        );
    }

    #[test]
    fn lookup_unknown_returns_none() {
        let r = ModelRegistry::default();
        assert!(r.get("does-not-exist").is_none());
    }

    #[test]
    fn nomic_search_prefix_is_documented() {
        assert_eq!(NOMIC_EMBED_TEXT_V1_5.search_prefix, "search_query: ");
        assert_eq!(NOMIC_EMBED_TEXT_V1_5.document_prefix, "search_document: ");
    }

    #[test]
    fn arctic_query_prefix_is_documented() {
        assert_eq!(SNOWFLAKE_ARCTIC_EMBED_L_V2.search_prefix, "query: ");
        assert_eq!(SNOWFLAKE_ARCTIC_EMBED_L_V2.document_prefix, "");
    }

    #[test]
    fn format_query_applies_prefix() {
        let formatted = NOMIC_EMBED_TEXT_V1_5.format_query("hello");
        assert_eq!(formatted, "search_query: hello");
    }

    #[test]
    fn format_document_applies_prefix() {
        let formatted = NOMIC_EMBED_TEXT_V1_5.format_document("a doc");
        assert_eq!(formatted, "search_document: a doc");
    }

    #[test]
    fn format_query_passes_through_when_prefix_is_empty() {
        let formatted = SNOWFLAKE_ARCTIC_EMBED_L_V2.format_document("a doc");
        assert_eq!(formatted, "a doc");
    }

    #[test]
    fn onnx_options_from_model_matches_fields() {
        let opts = NOMIC_EMBED_TEXT_V1_5.onnx_options();
        assert_eq!(opts.max_tokens, 8192);
        assert_eq!(opts.pooling, PoolingStrategy::MeanPooling);
        assert_eq!(opts.output_tensor, "last_hidden_state");

        let opts = SNOWFLAKE_ARCTIC_EMBED_L_V2.onnx_options();
        assert_eq!(opts.pooling, PoolingStrategy::ClsToken);
    }

    #[test]
    fn urls_point_at_huggingface() {
        for m in ModelRegistry::default().all() {
            assert!(
                m.onnx_url.starts_with("https://huggingface.co/"),
                "{} onnx_url not on huggingface: {}",
                m.name,
                m.onnx_url,
            );
            assert!(
                m.tokenizer_url.starts_with("https://huggingface.co/"),
                "{} tokenizer_url not on huggingface: {}",
                m.name,
                m.tokenizer_url,
            );
            assert!(
                m.onnx_url.contains(m.repo_id),
                "{} onnx_url does not embed repo_id",
                m.name,
            );
        }
    }

    #[test]
    fn checksums_are_empty_or_64_hex_chars() {
        for m in ModelRegistry::default().all() {
            for (label, hash) in [
                ("onnx_sha256", m.onnx_sha256),
                ("tokenizer_sha256", m.tokenizer_sha256),
            ] {
                assert!(
                    hash.is_empty()
                        || (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())),
                    "{} {label} is not empty or 64-hex: {hash:?}",
                    m.name,
                );
            }
        }
    }
}
