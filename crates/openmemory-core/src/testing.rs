pub use crate::clock::FixedClock;

pub trait Embedder: Send + Sync + 'static {
    /// Embed text without applying any model-specific task prefix.
    fn embed(&self, texts: &[&str]) -> Vec<Vec<f32>>;

    /// Embed search queries. Real model implementations can apply the
    /// registry's query prefix; simple test doubles fall back to raw text.
    fn embed_query(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        self.embed(texts)
    }

    /// Embed indexed documents. Real model implementations can apply the
    /// registry's document prefix; simple test doubles fall back to raw text.
    fn embed_documents(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        self.embed(texts)
    }

    fn dimensions(&self) -> usize;
}

pub struct FakeEmbedder {
    dims: usize,
}

impl FakeEmbedder {
    #[must_use]
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }
}

impl Embedder for FakeEmbedder {
    fn embed(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        texts
            .iter()
            .map(|text| {
                let mut v = vec![0.0f32; self.dims];
                for (i, byte) in text.bytes().enumerate() {
                    v[i % self.dims] += f32::from(byte) / 255.0;
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    v.iter_mut().for_each(|x| *x /= norm);
                }
                v
            })
            .collect()
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn fake_embedder_produces_correct_dimensions() {
        let e = FakeEmbedder::new(128);
        let vecs = e.embed(&["hello", "world"]);
        assert_eq!(vecs.len(), 2);
        assert_eq!(vecs[0].len(), 128);
        assert_eq!(vecs[1].len(), 128);
    }

    #[test]
    fn fake_embedder_output_is_normalized() {
        let e = FakeEmbedder::new(64);
        let vecs = e.embed(&["some text here"]);
        let norm: f32 = vecs[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn fake_embedder_different_texts_give_different_vectors() {
        let e = FakeEmbedder::new(64);
        let vecs = e.embed(&["alpha", "beta"]);
        assert_ne!(vecs[0], vecs[1]);
    }

    #[test]
    fn fake_embedder_same_text_gives_same_vector() {
        let e = FakeEmbedder::new(64);
        let v1 = e.embed(&["hello"]);
        let v2 = e.embed(&["hello"]);
        assert_eq!(v1[0], v2[0]);
    }

    #[test]
    fn fake_embedder_query_and_document_defaults_match_raw() {
        let e = FakeEmbedder::new(32);
        assert_eq!(e.embed_query(&["hello"]), e.embed(&["hello"]));
        assert_eq!(e.embed_documents(&["hello"]), e.embed(&["hello"]));
    }

    #[test]
    fn fake_embedder_is_send_sync() {
        let e: Arc<dyn Embedder> = Arc::new(FakeEmbedder::new(32));
        let handle = std::thread::spawn(move || e.embed(&["threaded"]));
        let vecs = handle.join().unwrap();
        assert_eq!(vecs[0].len(), 32);
    }

    #[test]
    fn fixed_clock_reexported() {
        let clock = FixedClock::new(100);
        assert_eq!(crate::clock::Clock::now_secs(&clock), 100);
    }
}
