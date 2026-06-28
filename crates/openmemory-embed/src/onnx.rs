//! ONNX Runtime-backed text embedder. CPU only in v0.1.
//!
//! Loads a model directory containing `model.onnx` and
//! `tokenizer.json`, then runs inference: tokenize → forward pass →
//! pool tokens → L2-normalize. The shared `Session` is wrapped in an
//! `Arc` because `ort` Sessions are `Send + Sync` since rc.6.

use crate::error::{EmbedError, EmbedResult};
use crate::integrity::{verify_sha256, VerificationOutcome};
use crate::models::Model;
use crate::traits::Embedder;
use ort::execution_providers::CPUExecutionProvider;
use ort::session::Session;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, error, warn};

const DEFAULT_MAX_TOKENS: usize = 8192;
const DEFAULT_OUTPUT_TENSOR: &str = "last_hidden_state";

/// How to reduce per-token hidden states into a single sentence
/// embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolingStrategy {
    /// Average per-token embeddings, weighted by the attention mask.
    MeanPooling,
    /// Use the hidden state of the `[CLS]` token (index 0).
    ClsToken,
}

/// Optional load-time tuning knobs.
#[derive(Debug, Clone, Copy)]
pub struct OnnxOptions {
    pub max_tokens: usize,
    pub pooling: PoolingStrategy,
    pub output_tensor: &'static str,
    pub search_prefix: &'static str,
    pub document_prefix: &'static str,
}

impl Default for OnnxOptions {
    fn default() -> Self {
        Self {
            max_tokens: DEFAULT_MAX_TOKENS,
            pooling: PoolingStrategy::MeanPooling,
            output_tensor: DEFAULT_OUTPUT_TENSOR,
            search_prefix: "",
            document_prefix: "",
        }
    }
}

/// CPU-only ONNX Runtime embedder.
pub struct OnnxEmbedder {
    session: Arc<Session>,
    tokenizer: Arc<tokenizers::Tokenizer>,
    dimensions: usize,
    model_name: String,
    max_tokens: usize,
    pooling: PoolingStrategy,
    output_tensor: &'static str,
    search_prefix: &'static str,
    document_prefix: &'static str,
    has_token_type_ids: bool,
}

impl OnnxEmbedder {
    /// Load a model with default options (mean-pooled, 8192 max tokens,
    /// `last_hidden_state` output).
    pub fn load(model_dir: &Path, model_name: &str, dimensions: usize) -> EmbedResult<Self> {
        Self::load_with_options(model_dir, model_name, dimensions, OnnxOptions::default())
    }

    /// Load a model from the registry, verifying the on-disk
    /// `model.onnx` and `tokenizer.json` files against the
    /// SHA-256 hashes recorded on the [`Model`] entry before handing
    /// any bytes to the ONNX runtime.
    ///
    /// * A non-empty hash that matches → `Verified` (debug log).
    /// * A non-empty hash that does *not* match → returns
    ///   [`EmbedError::ChecksumMismatch`]; the caller never sees a
    ///   tampered file.
    /// * An empty hash (future models added to the registry without
    ///   recorded hashes) → `Skipped` with a warning.
    pub fn load_for_model(model_dir: &Path, model: &Model) -> EmbedResult<Self> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        // Existence first — `verify_sha256` would otherwise return an
        // I/O error, but `ModelNotFound` is the more useful surface.
        if !model_path.exists() {
            return Err(EmbedError::ModelNotFound(model_path.display().to_string()));
        }
        if !tokenizer_path.exists() {
            return Err(EmbedError::ModelNotFound(
                tokenizer_path.display().to_string(),
            ));
        }

        log_verification(
            "model.onnx",
            model.name,
            verify_sha256(&model_path, model.onnx_sha256)?,
        );
        log_verification(
            "tokenizer.json",
            model.name,
            verify_sha256(&tokenizer_path, model.tokenizer_sha256)?,
        );
        if model.has_external_data() {
            let data_path = model_dir.join("model.onnx_data");
            if !data_path.exists() {
                return Err(EmbedError::ModelNotFound(data_path.display().to_string()));
            }
            log_verification(
                "model.onnx_data",
                model.name,
                verify_sha256(&data_path, model.onnx_data_sha256)?,
            );
        }

        Self::load_with_options(
            model_dir,
            model.name,
            model.dimensions,
            model.onnx_options(),
        )
    }

    /// Load a model with caller-supplied options.
    pub fn load_with_options(
        model_dir: &Path,
        model_name: &str,
        dimensions: usize,
        opts: OnnxOptions,
    ) -> EmbedResult<Self> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() {
            return Err(EmbedError::ModelNotFound(model_path.display().to_string()));
        }
        if !tokenizer_path.exists() {
            return Err(EmbedError::ModelNotFound(
                tokenizer_path.display().to_string(),
            ));
        }

        let num_cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);

        let session = Session::builder()
            .map_err(|e| {
                EmbedError::Onnx(format!(
                    "ONNX Runtime not available. Set ORT_DYLIB_PATH to the runtime library. \
                     Details: {e}"
                ))
            })?
            .with_intra_threads(num_cores)
            .map_err(|e| EmbedError::Onnx(format!("intra-thread config: {e}")))?
            .with_inter_threads(2)
            .map_err(|e| EmbedError::Onnx(format!("inter-thread config: {e}")))?
            .with_execution_providers([CPUExecutionProvider::default().build()])
            .map_err(|e| EmbedError::Onnx(format!("execution provider config: {e}")))?
            .commit_from_file(&model_path)
            .map_err(|e| {
                EmbedError::Onnx(format!(
                    "failed to load ONNX model from {}: {}",
                    model_path.display(),
                    e
                ))
            })?;

        let has_token_type_ids = session.inputs.iter().any(|i| i.name == "token_type_ids");

        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            EmbedError::Tokenization(format!(
                "failed to load tokenizer from {}: {}",
                tokenizer_path.display(),
                e
            ))
        })?;

        Ok(Self {
            session: Arc::new(session),
            tokenizer: Arc::new(tokenizer),
            dimensions,
            model_name: model_name.to_string(),
            max_tokens: opts.max_tokens,
            pooling: opts.pooling,
            output_tensor: opts.output_tensor,
            search_prefix: opts.search_prefix,
            document_prefix: opts.document_prefix,
            has_token_type_ids,
        })
    }

    /// Name supplied at load time. Useful for logging and metrics.
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Fallible counterpart to [`Embedder::embed`]. Use this when you
    /// need to handle inference failures explicitly; the `Embedder`
    /// impl logs and returns an empty `Vec` on error.
    pub fn try_embed_batch(&self, texts: &[&str]) -> EmbedResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        debug!(batch_size = texts.len(), model = %self.model_name, "embedding batch");

        let (input_ids_flat, attention_mask_flat, token_type_ids_flat, batch_size, seq_len) =
            self.tokenize_batch(texts)?;

        let input_ids_array =
            ndarray::Array2::from_shape_vec((batch_size, seq_len), input_ids_flat)
                .map_err(|e| EmbedError::Onnx(format!("input_ids shape error: {e}")))?;
        let attention_mask_array =
            ndarray::Array2::from_shape_vec((batch_size, seq_len), attention_mask_flat.clone())
                .map_err(|e| EmbedError::Onnx(format!("attention_mask shape error: {e}")))?;

        let inputs = if self.has_token_type_ids {
            let token_type_ids_array =
                ndarray::Array2::from_shape_vec((batch_size, seq_len), token_type_ids_flat)
                    .map_err(|e| EmbedError::Onnx(format!("token_type_ids shape error: {e}")))?;
            ort::inputs! {
                "input_ids" => input_ids_array,
                "attention_mask" => attention_mask_array,
                "token_type_ids" => token_type_ids_array,
            }
        } else {
            ort::inputs! {
                "input_ids" => input_ids_array,
                "attention_mask" => attention_mask_array,
            }
        }
        .map_err(|e| EmbedError::Onnx(format!("input binding error: {e}")))?;

        let outputs = self
            .session
            .run(inputs)
            .map_err(|e| EmbedError::Onnx(format!("inference failed: {e}")))?;

        let output_array = outputs
            .get(self.output_tensor)
            .ok_or_else(|| {
                EmbedError::Onnx(format!(
                    "output tensor `{}` not found in model `{}`. Declared outputs: {:?}",
                    self.output_tensor,
                    self.model_name,
                    outputs.keys().collect::<Vec<_>>()
                ))
            })?
            .try_extract_tensor::<f32>()
            .map_err(|e| EmbedError::Onnx(format!("tensor extract error: {e}")))?;

        let shape = output_array.shape();
        let token_embeddings: Vec<f32> = output_array.iter().copied().collect();

        let results = match shape {
            [out_batch, hidden_size] if *out_batch == batch_size => {
                Self::normalize_2d(&token_embeddings, batch_size, *hidden_size)
            }
            [out_batch, out_seq_len, hidden_size] if *out_batch == batch_size => {
                match self.pooling {
                    PoolingStrategy::MeanPooling => Self::mean_pooling(
                        &token_embeddings,
                        &attention_mask_flat,
                        batch_size,
                        *out_seq_len,
                        *hidden_size,
                    ),
                    PoolingStrategy::ClsToken => {
                        Self::cls_pooling(&token_embeddings, batch_size, *out_seq_len, *hidden_size)
                    }
                }
            }
            _ => {
                return Err(EmbedError::Onnx(format!(
                    "unexpected output shape {shape:?} for batch size {batch_size}"
                )));
            }
        };

        if let Some(first) = results.first() {
            if first.len() != self.dimensions {
                return Err(EmbedError::DimensionMismatch {
                    expected: self.dimensions,
                    actual: first.len(),
                });
            }
        }

        Ok(results)
    }

    fn try_embed_batch_with_prefix(
        &self,
        texts: &[&str],
        prefix: &str,
    ) -> EmbedResult<Vec<Vec<f32>>> {
        if prefix.is_empty() {
            return self.try_embed_batch(texts);
        }
        let prefixed: Vec<String> = texts.iter().map(|text| format!("{prefix}{text}")).collect();
        let refs: Vec<&str> = prefixed.iter().map(String::as_str).collect();
        self.try_embed_batch(&refs)
    }

    #[allow(clippy::type_complexity)]
    fn tokenize_batch(
        &self,
        texts: &[&str],
    ) -> EmbedResult<(Vec<i64>, Vec<i64>, Vec<i64>, usize, usize)> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| EmbedError::Tokenization(e.to_string()))?;

        let batch_size = encodings.len();
        let seq_len = encodings
            .iter()
            .map(|e| e.get_ids().len().min(self.max_tokens))
            .max()
            .unwrap_or(0);

        let total = batch_size * seq_len;
        let mut input_ids = vec![0i64; total];
        let mut attention_mask = vec![0i64; total];
        let token_type_ids = vec![0i64; total];

        for (b, encoding) in encodings.iter().enumerate() {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            let len = ids.len().min(self.max_tokens);
            let off = b * seq_len;
            for i in 0..len {
                input_ids[off + i] = i64::from(ids[i]);
                attention_mask[off + i] = i64::from(mask[i]);
            }
        }

        Ok((
            input_ids,
            attention_mask,
            token_type_ids,
            batch_size,
            seq_len,
        ))
    }

    fn mean_pooling(
        token_embeddings: &[f32],
        attention_mask: &[i64],
        batch_size: usize,
        seq_len: usize,
        hidden_size: usize,
    ) -> Vec<Vec<f32>> {
        let mut results = Vec::with_capacity(batch_size);
        for b in 0..batch_size {
            let mut pooled = vec![0.0f32; hidden_size];
            let mut count = 0.0f32;
            for s in 0..seq_len {
                let mask_val = attention_mask[b * seq_len + s] as f32;
                if mask_val > 0.0 {
                    let off = (b * seq_len + s) * hidden_size;
                    for d in 0..hidden_size {
                        pooled[d] += token_embeddings[off + d] * mask_val;
                    }
                    count += mask_val;
                }
            }
            if count > 0.0 {
                for v in &mut pooled {
                    *v /= count;
                }
            }
            l2_normalize_in_place(&mut pooled);
            results.push(pooled);
        }
        results
    }

    fn cls_pooling(
        token_embeddings: &[f32],
        batch_size: usize,
        seq_len: usize,
        hidden_size: usize,
    ) -> Vec<Vec<f32>> {
        let mut results = Vec::with_capacity(batch_size);
        for b in 0..batch_size {
            let off = b * seq_len * hidden_size;
            let mut pooled = token_embeddings[off..off + hidden_size].to_vec();
            l2_normalize_in_place(&mut pooled);
            results.push(pooled);
        }
        results
    }

    fn normalize_2d(embeddings: &[f32], batch_size: usize, hidden_size: usize) -> Vec<Vec<f32>> {
        let mut results = Vec::with_capacity(batch_size);
        for b in 0..batch_size {
            let off = b * hidden_size;
            let mut row = embeddings[off..off + hidden_size].to_vec();
            l2_normalize_in_place(&mut row);
            results.push(row);
        }
        results
    }
}

fn l2_normalize_in_place(values: &mut [f32]) {
    let norm: f32 = values.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in values {
            *v /= norm;
        }
    }
}

fn log_verification(label: &str, model_name: &str, outcome: VerificationOutcome) {
    match outcome {
        VerificationOutcome::Verified => debug!(
            target: "openmemory_embed::integrity",
            model = model_name,
            file = label,
            "checksum verified",
        ),
        VerificationOutcome::Skipped => warn!(
            target: "openmemory_embed::integrity",
            model = model_name,
            file = label,
            "registry has no recorded SHA-256 for this file; loading without integrity check",
        ),
    }
}

fn embed_or_log(result: EmbedResult<Vec<Vec<f32>>>) -> Vec<Vec<f32>> {
    match result {
        Ok(v) => v,
        Err(e) => {
            error!("OnnxEmbedder::embed failed: {e}");
            Vec::new()
        }
    }
}

impl Embedder for OnnxEmbedder {
    fn embed(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        embed_or_log(self.try_embed_batch(texts))
    }

    fn embed_query(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        embed_or_log(self.try_embed_batch_with_prefix(texts, self.search_prefix))
    }

    fn embed_documents(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        embed_or_log(self.try_embed_batch_with_prefix(texts, self.document_prefix))
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooling_strategy_eq() {
        assert_eq!(PoolingStrategy::MeanPooling, PoolingStrategy::MeanPooling);
        assert_ne!(PoolingStrategy::MeanPooling, PoolingStrategy::ClsToken);
    }

    #[test]
    fn options_default_is_mean_pooled_with_8192_tokens() {
        let opts = OnnxOptions::default();
        assert_eq!(opts.max_tokens, 8192);
        assert_eq!(opts.pooling, PoolingStrategy::MeanPooling);
        assert_eq!(opts.output_tensor, "last_hidden_state");
        assert_eq!(opts.search_prefix, "");
        assert_eq!(opts.document_prefix, "");
    }

    #[test]
    fn load_missing_model_dir_errors_with_model_not_found() {
        let tmp = std::env::temp_dir().join("openmemory-embed-no-such-dir-12345");
        let result = OnnxEmbedder::load(&tmp, "missing", 768);
        let Err(err) = result else {
            panic!("expected ModelNotFound error");
        };
        assert!(matches!(err, EmbedError::ModelNotFound(_)));
    }

    #[test]
    fn load_missing_tokenizer_errors_with_model_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("model.onnx"), b"fake").unwrap();
        let result = OnnxEmbedder::load(tmp.path(), "no-tokenizer", 768);
        let Err(err) = result else {
            panic!("expected ModelNotFound error");
        };
        let msg = err.to_string();
        assert!(matches!(err, EmbedError::ModelNotFound(_)));
        assert!(msg.contains("tokenizer.json"));
    }

    // -----------------------------------------------------------------
    // load_for_model integrity gate — runs before any ORT init, so
    // these tests do not need a real ONNX model file.
    // -----------------------------------------------------------------

    fn fake_model_dir(onnx: &[u8], tokenizer: &[u8]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("model.onnx"), onnx).unwrap();
        std::fs::write(tmp.path().join("tokenizer.json"), tokenizer).unwrap();
        tmp
    }

    fn fake_model(onnx_sha256: &'static str, tokenizer_sha256: &'static str) -> Model {
        Model {
            name: "test-fixture",
            aliases: &[],
            repo_id: "test/test-fixture",
            dimensions: 768,
            max_tokens: 8192,
            pooling: PoolingStrategy::MeanPooling,
            output_tensor: "last_hidden_state",
            search_prefix: "",
            document_prefix: "",
            onnx_url: "https://example.invalid/model.onnx",
            tokenizer_url: "https://example.invalid/tokenizer.json",
            onnx_sha256,
            tokenizer_sha256,
            onnx_data_url: "",
            onnx_data_sha256: "",
        }
    }

    fn fake_external_data_model(
        onnx_sha256: &'static str,
        tokenizer_sha256: &'static str,
        onnx_data_sha256: &'static str,
    ) -> Model {
        Model {
            onnx_data_url: "https://example.invalid/model.onnx_data",
            onnx_data_sha256,
            ..fake_model(onnx_sha256, tokenizer_sha256)
        }
    }

    const ALL_ZEROS_64: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    const ALL_F_64: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    #[test]
    fn load_for_model_rejects_mismatched_onnx_sha256() {
        let dir = fake_model_dir(b"not actually onnx", b"{\"tok\": true}");
        let model = fake_model(ALL_ZEROS_64, "");
        let Err(err) = OnnxEmbedder::load_for_model(dir.path(), &model) else {
            panic!("expected ChecksumMismatch error");
        };
        match err {
            EmbedError::ChecksumMismatch {
                path,
                expected,
                actual,
            } => {
                assert!(path.contains("model.onnx"), "path was {path}");
                assert_eq!(expected, ALL_ZEROS_64);
                assert_ne!(actual, expected);
                assert_eq!(actual.len(), 64);
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }

    #[test]
    fn load_for_model_rejects_mismatched_tokenizer_sha256() {
        let dir = fake_model_dir(b"not actually onnx", b"{\"tok\": true}");
        // ONNX hash is empty so the model.onnx check is skipped; the
        // mismatch must therefore come from the tokenizer.
        let model = fake_model("", ALL_F_64);
        let Err(err) = OnnxEmbedder::load_for_model(dir.path(), &model) else {
            panic!("expected ChecksumMismatch error");
        };
        match err {
            EmbedError::ChecksumMismatch { path, .. } => {
                assert!(path.contains("tokenizer.json"), "path was {path}");
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }

    #[test]
    fn load_for_model_returns_model_not_found_when_model_onnx_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let model = fake_model("", "");
        let Err(err) = OnnxEmbedder::load_for_model(tmp.path(), &model) else {
            panic!("expected ModelNotFound error");
        };
        match err {
            EmbedError::ModelNotFound(p) => assert!(p.contains("model.onnx"), "path was {p}"),
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[test]
    fn load_for_model_returns_model_not_found_when_tokenizer_absent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("model.onnx"), b"fake").unwrap();
        let model = fake_model("", "");
        let Err(err) = OnnxEmbedder::load_for_model(tmp.path(), &model) else {
            panic!("expected ModelNotFound error");
        };
        match err {
            EmbedError::ModelNotFound(p) => assert!(p.contains("tokenizer.json"), "path was {p}"),
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[test]
    fn load_for_model_returns_model_not_found_when_external_data_absent() {
        let dir = fake_model_dir(b"not actually onnx", b"{\"tok\": true}");
        let model = fake_external_data_model("", "", "");
        let Err(err) = OnnxEmbedder::load_for_model(dir.path(), &model) else {
            panic!("expected ModelNotFound error");
        };
        match err {
            EmbedError::ModelNotFound(p) => {
                assert!(p.contains("model.onnx_data"), "path was {p}");
            }
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[test]
    fn load_for_model_rejects_mismatched_external_data_sha256() {
        let dir = fake_model_dir(b"not actually onnx", b"{\"tok\": true}");
        std::fs::write(dir.path().join("model.onnx_data"), b"bad weights").unwrap();
        let model = fake_external_data_model("", "", ALL_ZEROS_64);
        let Err(err) = OnnxEmbedder::load_for_model(dir.path(), &model) else {
            panic!("expected ChecksumMismatch error");
        };
        match err {
            EmbedError::ChecksumMismatch { path, .. } => {
                assert!(path.contains("model.onnx_data"), "path was {path}");
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Pooling math — pure functions, no ONNX session needed.
    // -----------------------------------------------------------------

    #[test]
    fn mean_pooling_single_item_uniform_mask() {
        // batch=1 seq=2 hidden=3, all tokens unmasked
        let tokens = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mask = vec![1, 1];

        let out = OnnxEmbedder::mean_pooling(&tokens, &mask, 1, 2, 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), 3);

        let raw = [2.5f32, 3.5, 4.5];
        let norm: f32 = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
        for (i, &v) in out[0].iter().enumerate() {
            let expected = raw[i] / norm;
            assert!(
                (v - expected).abs() < 1e-5,
                "dim {i}: got {v}, want {expected}"
            );
        }
    }

    #[test]
    fn mean_pooling_respects_attention_mask() {
        // batch=2 seq=3 hidden=2; first batch masks out token 2.
        #[rustfmt::skip]
        let tokens = vec![
            1.0, 0.0,
            0.0, 1.0,
            9.0, 9.0,
            2.0, 2.0,
            4.0, 4.0,
            6.0, 6.0,
        ];
        let mask = vec![1, 1, 0, 1, 1, 1];

        let out = OnnxEmbedder::mean_pooling(&tokens, &mask, 2, 3, 2);
        assert_eq!(out.len(), 2);

        let expected_0 = 1.0f32 / 2.0f32.sqrt();
        assert!((out[0][0] - expected_0).abs() < 1e-5);
        assert!((out[0][1] - expected_0).abs() < 1e-5);

        let norm_1 = (4.0f32 * 4.0 + 4.0 * 4.0).sqrt();
        let expected_1 = 4.0 / norm_1;
        assert!((out[1][0] - expected_1).abs() < 1e-5);
        assert!((out[1][1] - expected_1).abs() < 1e-5);
    }

    #[test]
    fn mean_pooling_zero_mask_produces_zero_vector() {
        let tokens = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mask = vec![0, 0];
        let out = OnnxEmbedder::mean_pooling(&tokens, &mask, 1, 2, 3);
        assert_eq!(out.len(), 1);
        for &v in &out[0] {
            assert!((v - 0.0).abs() < 1e-10);
        }
    }

    #[test]
    fn cls_pooling_uses_first_token_per_batch() {
        #[rustfmt::skip]
        let tokens = vec![
            3.0, 4.0,
            9.0, 9.0,
            0.0, 5.0,
            7.0, 7.0,
        ];
        let out = OnnxEmbedder::cls_pooling(&tokens, 2, 2, 2);
        assert_eq!(out.len(), 2);
        assert!((out[0][0] - 0.6).abs() < 1e-6);
        assert!((out[0][1] - 0.8).abs() < 1e-6);
        assert!((out[1][0] - 0.0).abs() < 1e-6);
        assert!((out[1][1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_2d_normalizes_each_row() {
        let embeddings = vec![3.0, 4.0, 0.0, 0.0, 0.0, 2.0];
        let out = OnnxEmbedder::normalize_2d(&embeddings, 2, 3);
        assert_eq!(out.len(), 2);
        assert!((out[0][0] - 0.6).abs() < 1e-6);
        assert!((out[0][1] - 0.8).abs() < 1e-6);
        assert!((out[0][2] - 0.0).abs() < 1e-6);
        assert!((out[1][0] - 0.0).abs() < 1e-6);
        assert!((out[1][1] - 0.0).abs() < 1e-6);
        assert!((out[1][2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_is_no_op() {
        let mut v = vec![0.0, 0.0, 0.0];
        l2_normalize_in_place(&mut v);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }
}
