use crate::error::{CalError, Result};
use serde::{Deserialize, Serialize};

const SCHEME: &str = "calli://";

/// A canonical pointer into a corpus — stable, citeable, reproducible.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Location {
    pub corpus_id: String,
    pub path: String,
    pub uri: String,
}

impl Location {
    pub fn new(corpus_id: impl Into<String>, path: impl Into<String>) -> Self {
        let corpus_id = corpus_id.into();
        let path = path.into();
        let uri = format!("{}{}/{}", SCHEME, corpus_id, path);
        Self {
            corpus_id,
            path,
            uri,
        }
    }

    pub fn parse(uri: &str) -> Result<Self> {
        let rest = uri
            .strip_prefix(SCHEME)
            .ok_or_else(|| CalError::InvalidLocation(format!("missing calli:// scheme: {uri}")))?;

        let (corpus_id, path) = rest
            .split_once('/')
            .ok_or_else(|| CalError::InvalidLocation(format!("missing path segment: {uri}")))?;

        if corpus_id.is_empty() {
            return Err(CalError::InvalidLocation(format!("empty corpus_id: {uri}")));
        }

        Ok(Self {
            corpus_id: corpus_id.to_string(),
            path: path.to_string(),
            uri: uri.to_string(),
        })
    }
}

impl Default for Location {
    fn default() -> Self {
        Self::new("", "")
    }
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let loc = Location::new("xenos", "ch/3/sc/7");
        assert_eq!(loc.uri, "calli://xenos/ch/3/sc/7");

        let parsed = Location::parse(&loc.uri).unwrap();
        assert_eq!(parsed, loc);
    }

    #[test]
    fn parse_rejects_bad_scheme() {
        assert!(Location::parse("https://xenos/ch/3").is_err());
    }

    #[test]
    fn parse_rejects_missing_path() {
        assert!(Location::parse("calli://xenos").is_err());
    }
}
