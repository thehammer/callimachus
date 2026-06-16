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

    #[error("{0}")]
    Anyhow(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, CalError>;

/// Bubble a contract-crate location-parse failure through `CalError`.
///
/// `Location::parse` now lives in `callimachus-adapter-contract` and returns
/// its own storage-free `LocationParseError`. Core call sites that propagate
/// through the `CalError`-based `Result` keep working via this conversion,
/// landing in the existing `InvalidLocation` variant.
impl From<callimachus_adapter_contract::LocationParseError> for CalError {
    fn from(e: callimachus_adapter_contract::LocationParseError) -> Self {
        CalError::InvalidLocation(e.0)
    }
}
