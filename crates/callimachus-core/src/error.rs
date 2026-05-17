use thiserror::Error;

#[derive(Debug, Error)]
pub enum CalError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("corpus not found: {0}")]
    CorpusNotFound(String),

    #[error("invalid location URI: {0}")]
    InvalidLocation(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CalError>;
