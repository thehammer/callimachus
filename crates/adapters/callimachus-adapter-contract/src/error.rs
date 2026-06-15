//! Errors raised by the adapter contract.
//!
//! The contract crate is deliberately storage-agnostic, so it does not depend
//! on `callimachus-core`'s `CalError` (which carries `rusqlite` variants). The
//! only fallible operation in the type closure is location-URI parsing, so this
//! module provides a single thin error type for it. `callimachus-core` provides
//! a `From<LocationParseError> for CalError` so core call sites can still bubble
//! the error through their `Result` alias if they choose.

use std::fmt;

/// Failure parsing a `calli://` location URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationParseError(pub String);

impl fmt::Display for LocationParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid location URI: {}", self.0)
    }
}

impl std::error::Error for LocationParseError {}

/// Convenience result alias for contract-crate fallible operations.
pub type Result<T> = std::result::Result<T, LocationParseError>;
