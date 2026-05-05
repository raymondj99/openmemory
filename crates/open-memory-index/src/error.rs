use std::path::PathBuf;

use open_memory_core::error::OmError;
use thiserror::Error;

pub type IndexResult<T> = Result<T, IndexError>;

#[derive(Error, Debug)]
pub enum IndexError {
    #[error("SQLite error: {0}")]
    Sqlite(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("corrupt index at {path}: {detail}")]
    Corrupt { path: PathBuf, detail: String },

    #[error("lock poisoned: {0}")]
    Lock(String),

    #[error("dimension mismatch: index has {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("schema error: {0}")]
    Schema(String),
}

impl From<rusqlite::Error> for IndexError {
    fn from(e: rusqlite::Error) -> Self {
        IndexError::Sqlite(e.to_string())
    }
}

impl From<OmError> for IndexError {
    fn from(e: OmError) -> Self {
        match e {
            OmError::Io(io) => IndexError::Io(io),
            OmError::Sqlite(s) => IndexError::Sqlite(s),
            OmError::SchemaTooNew { current, max } => IndexError::Schema(format!(
                "schema version {current} is newer than supported (max {max})"
            )),
            OmError::Migration { version, reason } => {
                IndexError::Schema(format!("migration {version} failed: {reason}"))
            }
            OmError::Config(s) => IndexError::InvalidInput(s),
            OmError::InvalidInput(s) => IndexError::InvalidInput(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_sqlite_error() {
        let err: IndexError = rusqlite::Error::InvalidQuery.into();
        assert!(err.to_string().contains("SQLite error"));
    }

    #[test]
    fn display_io_error() {
        let err: IndexError = std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
        assert!(err.to_string().contains("I/O error"));
    }

    #[test]
    fn display_corrupt() {
        let err = IndexError::Corrupt {
            path: PathBuf::from("/data/idx"),
            detail: "bad magic".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("corrupt index"));
        assert!(msg.contains("/data/idx"));
        assert!(msg.contains("bad magic"));
    }

    #[test]
    fn display_lock() {
        let err = IndexError::Lock("poisoned".into());
        assert_eq!(err.to_string(), "lock poisoned: poisoned");
    }

    #[test]
    fn display_dimension_mismatch() {
        let err = IndexError::DimensionMismatch {
            expected: 768,
            actual: 1024,
        };
        let msg = err.to_string();
        assert!(msg.contains("768"));
        assert!(msg.contains("1024"));
    }

    #[test]
    fn display_invalid_input() {
        let err = IndexError::InvalidInput("empty uri".into());
        assert_eq!(err.to_string(), "invalid input: empty uri");
    }

    #[test]
    fn display_schema() {
        let err = IndexError::Schema("bad column".into());
        assert_eq!(err.to_string(), "schema error: bad column");
    }

    #[test]
    fn from_om_error_io() {
        let om = OmError::Io(std::io::Error::new(std::io::ErrorKind::Other, "x"));
        let idx: IndexError = om.into();
        assert!(matches!(idx, IndexError::Io(_)));
    }

    #[test]
    fn from_om_error_schema_too_new() {
        let om = OmError::SchemaTooNew { current: 9, max: 1 };
        let idx: IndexError = om.into();
        match idx {
            IndexError::Schema(msg) => {
                assert!(msg.contains("9"));
                assert!(msg.contains("1"));
            }
            other => panic!("expected Schema, got {other:?}"),
        }
    }

    #[test]
    fn from_om_error_migration() {
        let om = OmError::Migration {
            version: 3,
            reason: "boom".into(),
        };
        let idx: IndexError = om.into();
        match idx {
            IndexError::Schema(msg) => {
                assert!(msg.contains("3"));
                assert!(msg.contains("boom"));
            }
            other => panic!("expected Schema, got {other:?}"),
        }
    }

    #[test]
    fn from_om_error_config_to_invalid_input() {
        let om = OmError::Config("nope".into());
        let idx: IndexError = om.into();
        assert!(matches!(idx, IndexError::InvalidInput(_)));
    }

    #[test]
    fn from_om_error_invalid_input() {
        let om = OmError::InvalidInput("bad".into());
        let idx: IndexError = om.into();
        assert!(matches!(idx, IndexError::InvalidInput(_)));
    }

    #[test]
    fn from_om_error_sqlite() {
        let om = OmError::Sqlite("locked".into());
        let idx: IndexError = om.into();
        assert!(matches!(idx, IndexError::Sqlite(_)));
    }
}
