use thiserror::Error;

pub type EmbedResult<T> = Result<T, EmbedError>;

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("model not found: {0}")]
    ModelNotFound(String),

    #[error("ONNX runtime error: {0}")]
    Onnx(String),

    #[error("tokenization failed: {0}")]
    Tokenization(String),

    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    #[error(
        "checksum mismatch for {path}: expected sha256:{expected}, got sha256:{actual}. \
         The on-disk model file does not match the registry-recorded hash; refusing to load."
    )]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("cache error: {0}")]
    Cache(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_model_not_found() {
        let err = EmbedError::ModelNotFound("nomic".into());
        assert_eq!(err.to_string(), "model not found: nomic");
    }

    #[test]
    fn display_onnx() {
        let err = EmbedError::Onnx("session init failed".into());
        assert_eq!(err.to_string(), "ONNX runtime error: session init failed");
    }

    #[test]
    fn display_tokenization() {
        let err = EmbedError::Tokenization("invalid token".into());
        assert_eq!(err.to_string(), "tokenization failed: invalid token");
    }

    #[test]
    fn display_dimension_mismatch() {
        let err = EmbedError::DimensionMismatch {
            expected: 768,
            actual: 512,
        };
        assert_eq!(err.to_string(), "dimension mismatch: expected 768, got 512");
    }

    #[test]
    fn display_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let err = EmbedError::Io(io_err);
        assert!(err.to_string().contains("I/O error"));
        assert!(err.to_string().contains("file gone"));
    }

    #[test]
    fn display_cache_error() {
        let err = EmbedError::Cache("schema corrupt".into());
        assert_eq!(err.to_string(), "cache error: schema corrupt");
    }

    #[test]
    fn display_checksum_mismatch() {
        let err = EmbedError::ChecksumMismatch {
            path: "/models/nomic/model.onnx".into(),
            expected: "abc123".into(),
            actual: "def456".into(),
        };
        let s = err.to_string();
        assert!(s.contains("/models/nomic/model.onnx"));
        assert!(s.contains("expected sha256:abc123"));
        assert!(s.contains("got sha256:def456"));
        assert!(s.contains("refusing to load"));
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: EmbedError = io_err.into();
        assert!(matches!(err, EmbedError::Io(_)));
    }
}
