//! Public test doubles for downstream crates.
//!
//! Available behind the `testing` cargo feature. Add
//! `openmemory-embed = { workspace = true, features = ["testing"] }`
//! to `dev-dependencies` to use them.

use crate::traits::Embedder;

/// Deterministic [`Embedder`] that derives a fixed-dimension vector
/// from a stable BLAKE3 hash of the input text.
///
/// Two runs of the same process — or two different processes — produce
/// byte-identical embeddings for the same input, which is enough to
/// exercise hybrid-search code paths without loading an ONNX model.
pub struct StubEmbedder {
    dimensions: usize,
}

impl StubEmbedder {
    /// Construct a stub embedder with the default 128-dimension output.
    #[must_use]
    pub const fn new() -> Self {
        Self { dimensions: 128 }
    }

    /// Construct a stub embedder with a custom dimensionality.
    #[must_use]
    pub const fn with_dimensions(dimensions: usize) -> Self {
        Self { dimensions }
    }

    fn embed_one(&self, text: &str) -> Vec<f32> {
        let digest = blake3::hash(text.as_bytes());
        let bytes = digest.as_bytes();
        let mut v: Vec<f32> = (0..self.dimensions)
            .map(|i| (f32::from(bytes[i % bytes.len()]) - 127.5) / 127.5)
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        for x in &mut v {
            *x /= norm;
        }
        v
    }
}

impl Default for StubEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl Embedder for StubEmbedder {
    fn embed(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| self.embed_one(t)).collect()
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_is_deterministic() {
        let e = StubEmbedder::new();
        let a = e.embed(&["hello world"]);
        let b = e.embed(&["hello world"]);
        assert_eq!(a, b);
    }

    #[test]
    fn stub_outputs_unit_vectors() {
        let e = StubEmbedder::new();
        let v = e.embed(&["anything"]);
        let norm: f32 = v[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
    }

    #[test]
    fn stub_distinguishes_inputs() {
        let e = StubEmbedder::new();
        let v = e.embed(&["hello", "goodbye"]);
        assert_ne!(v[0], v[1]);
    }

    #[test]
    fn stub_respects_dimension_override() {
        let e = StubEmbedder::with_dimensions(64);
        assert_eq!(e.dimensions(), 64);
        let v = e.embed(&["x"]);
        assert_eq!(v[0].len(), 64);
    }

    #[test]
    fn stub_empty_input_produces_empty_output() {
        let e = StubEmbedder::new();
        let v = e.embed(&[]);
        assert!(v.is_empty());
    }

    #[test]
    fn stub_default_uses_128_dimensions() {
        let e = StubEmbedder::default();
        assert_eq!(e.dimensions(), 128);
    }
}
