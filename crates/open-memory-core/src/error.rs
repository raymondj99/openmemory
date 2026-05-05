use thiserror::Error;

pub type OmResult<T> = Result<T, OmError>;

#[derive(Error, Debug)]
pub enum OmError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("schema version {current} is newer than supported (max {max}); upgrade open-memory")]
    SchemaTooNew { current: u32, max: u32 },

    #[error("migration failed at version {version}: {reason}")]
    Migration { version: u32, reason: String },
}

impl From<rusqlite::Error> for OmError {
    fn from(e: rusqlite::Error) -> Self {
        OmError::Sqlite(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_io_error() {
        let err: OmError =
            std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
        assert!(err.to_string().contains("I/O error"));
    }

    #[test]
    fn display_sqlite_error() {
        let err = OmError::Sqlite("busy".into());
        assert_eq!(err.to_string(), "SQLite error: busy");
    }

    #[test]
    fn display_config_error() {
        let err = OmError::Config("missing key".into());
        assert_eq!(err.to_string(), "config error: missing key");
    }

    #[test]
    fn display_invalid_input() {
        let err = OmError::InvalidInput("empty name".into());
        assert_eq!(err.to_string(), "invalid input: empty name");
    }

    #[test]
    fn display_schema_too_new() {
        let err = OmError::SchemaTooNew { current: 5, max: 3 };
        assert!(err.to_string().contains("version 5"));
        assert!(err.to_string().contains("max 3"));
    }

    #[test]
    fn display_migration_error() {
        let err = OmError::Migration {
            version: 2,
            reason: "column exists".into(),
        };
        assert!(err.to_string().contains("version 2"));
        assert!(err.to_string().contains("column exists"));
    }
}
