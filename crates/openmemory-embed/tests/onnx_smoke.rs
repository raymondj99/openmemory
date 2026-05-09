//! End-to-end smoke test for [`OnnxEmbedder`].
//!
//! This test is gated behind the `OPENMEMORY_TEST_MODEL_DIR` env var
//! because shipping a real ONNX text-embedding model under
//! `tests/fixtures/` blows past the 2 MB committed-fixture budget.
//!
//! To run it locally:
//!
//! ```text
//! OPENMEMORY_TEST_MODEL_DIR=$HOME/.openmemory/models/nomic-embed-text-v1.5 \
//! ORT_DYLIB_PATH=/path/to/libonnxruntime.dylib \
//! cargo test -p openmemory-embed --test onnx_smoke -- --nocapture
//! ```
//!
//! When the env var is unset (the default for CI), the test exits 0
//! after printing a one-line skip notice.

use openmemory_embed::{Embedder, OnnxEmbedder};

#[test]
fn smoke_real_onnx_model() {
    let Ok(dir) = std::env::var("OPENMEMORY_TEST_MODEL_DIR") else {
        eprintln!("skipping: OPENMEMORY_TEST_MODEL_DIR not set");
        return;
    };
    let dim: usize = std::env::var("OPENMEMORY_TEST_MODEL_DIM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(768);

    let embedder = OnnxEmbedder::load(std::path::Path::new(&dir), "smoke", dim)
        .expect("model directory should contain model.onnx + tokenizer.json");

    let v = embedder.embed(&["hello, world", "openmemory smoke"]);
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].len(), dim);
    assert_eq!(v[1].len(), dim);

    // Should be unit-norm after L2 normalisation.
    for row in &v {
        let norm: f32 = row.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "row not unit-norm: {norm}");
    }
}
