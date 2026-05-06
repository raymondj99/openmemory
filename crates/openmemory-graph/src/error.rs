//! Error type for the knowledge-graph crate.

use thiserror::Error;

use openmemory_core::error::OmError;
use openmemory_index::IndexError;

pub type MemoryResult<T> = Result<T, MemoryError>;

#[derive(Error, Debug)]
pub enum MemoryError {
    #[error("SQLite error: {0}")]
    Sqlite(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("schema error: {0}")]
    Schema(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("entity not found: {0}")]
    EntityNotFound(String),

    #[error("observation not found: {0}")]
    ObservationNotFound(String),

    #[error("lock poisoned: {0}")]
    Lock(String),

    #[error("search index: {0}")]
    Index(#[from] IndexError),

    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
}

impl From<rusqlite::Error> for MemoryError {
    fn from(e: rusqlite::Error) -> Self {
        MemoryError::Sqlite(e.to_string())
    }
}

impl From<OmError> for MemoryError {
    fn from(e: OmError) -> Self {
        match e {
            OmError::Io(io) => MemoryError::Io(io),
            OmError::Sqlite(s) => MemoryError::Sqlite(s),
            OmError::SchemaTooNew { current, max } => MemoryError::Schema(format!(
                "schema version {current} is newer than supported (max {max})"
            )),
            OmError::Migration { version, reason } => {
                MemoryError::Schema(format!("migration {version} failed: {reason}"))
            }
            OmError::Config(s) | OmError::InvalidInput(s) => MemoryError::InvalidInput(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_invalid_input() {
        let err = MemoryError::InvalidInput("missing name".into());
        assert_eq!(err.to_string(), "invalid input: missing name");
    }

    #[test]
    fn display_entity_not_found() {
        let err = MemoryError::EntityNotFound("Raymond".into());
        assert_eq!(err.to_string(), "entity not found: Raymond");
    }

    #[test]
    fn from_rusqlite_error() {
        let err: MemoryError = rusqlite::Error::InvalidQuery.into();
        assert!(err.to_string().contains("SQLite error"));
    }

    #[test]
    fn from_om_error_schema_too_new() {
        let om = OmError::SchemaTooNew { current: 5, max: 1 };
        let me: MemoryError = om.into();
        match me {
            MemoryError::Schema(msg) => {
                assert!(msg.contains('5'));
                assert!(msg.contains('1'));
            }
            other => panic!("expected Schema, got {other:?}"),
        }
    }

    #[test]
    fn from_index_error() {
        let ie = IndexError::InvalidInput("bad".into());
        let me: MemoryError = ie.into();
        assert!(matches!(me, MemoryError::Index(_)));
    }
}
