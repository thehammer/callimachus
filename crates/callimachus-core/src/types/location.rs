use crate::error::{CalError, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// URI machinery for the `calli://` scheme.
pub struct LocationUri;

impl LocationUri {
    const SCHEME: &'static str = "calli://";

    /// Format a corpus_id and path into a `calli://` URI.
    pub fn format(corpus_id: &str, path: &str) -> String {
        format!("{}{}/{}", Self::SCHEME, corpus_id, path)
    }

    /// Parse a `calli://` URI into a [`Location`].
    pub fn parse(uri: &str) -> Result<Location> {
        let rest = uri
            .strip_prefix(Self::SCHEME)
            .ok_or_else(|| CalError::InvalidLocation(format!("missing calli:// scheme: {uri}")))?;

        let (corpus_id, path) = rest
            .split_once('/')
            .ok_or_else(|| CalError::InvalidLocation(format!("missing path segment: {uri}")))?;

        if corpus_id.is_empty() {
            return Err(CalError::InvalidLocation(format!("empty corpus_id: {uri}")));
        }

        Ok(Location {
            corpus_id: corpus_id.to_string(),
            path: path.to_string(),
        })
    }
}

/// A canonical pointer into a corpus — stable, citeable, reproducible.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Location {
    pub corpus_id: String,
    pub path: String,
}

impl Location {
    pub fn new(corpus_id: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            corpus_id: corpus_id.into(),
            path: path.into(),
        }
    }

    /// Parse a `calli://` URI into a Location. Delegates to [`LocationUri::parse`].
    pub fn parse(uri: &str) -> Result<Self> {
        LocationUri::parse(uri)
    }

    /// Return the `calli://` URI for this location.
    pub fn uri(&self) -> String {
        LocationUri::format(&self.corpus_id, &self.path)
    }
}

impl Default for Location {
    fn default() -> Self {
        Self::new("", "")
    }
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.uri())
    }
}

// ── Custom serde ─────────────────────────────────────────────────────────────
//
// Wire format preserves the old `{corpus_id, path, uri}` shape so that MCP/HTTP
// payloads remain unchanged.  Deserialization accepts an object with or without a
// `uri` field; if present, its value is ignored and Location is constructed from
// corpus_id + path alone.

impl Serialize for Location {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Location", 3)?;
        s.serialize_field("corpus_id", &self.corpus_id)?;
        s.serialize_field("path", &self.path)?;
        s.serialize_field("uri", &self.uri())?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for Location {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct LocationWire {
            corpus_id: String,
            path: String,
            // Accept but ignore the legacy `uri` field.
            #[serde(default)]
            uri: Option<serde_json::Value>,
        }

        let wire = LocationWire::deserialize(deserializer)?;
        let _ = wire.uri; // explicitly consumed / ignored
        Ok(Location {
            corpus_id: wire.corpus_id,
            path: wire.path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let loc = Location::new("xenos", "ch/3/sc/7");
        assert_eq!(loc.uri(), "calli://xenos/ch/3/sc/7");

        let parsed = Location::parse(&loc.uri()).unwrap();
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

    #[test]
    fn location_uri_parse_and_format() {
        let loc = LocationUri::parse("calli://corpus1/chapter/1").unwrap();
        assert_eq!(loc.corpus_id, "corpus1");
        assert_eq!(loc.path, "chapter/1");
        assert_eq!(
            LocationUri::format("corpus1", "chapter/1"),
            "calli://corpus1/chapter/1"
        );
    }

    #[test]
    fn location_uri_parse_rejects_bad_scheme() {
        assert!(LocationUri::parse("https://corpus1/ch/1").is_err());
    }

    #[test]
    fn location_uri_parse_rejects_missing_path() {
        assert!(LocationUri::parse("calli://corpus1").is_err());
    }

    #[test]
    fn serde_emits_uri_field() {
        let loc = Location::new("xenos", "ch/1");
        let json = serde_json::to_string(&loc).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["uri"], "calli://xenos/ch/1");
        assert_eq!(v["corpus_id"], "xenos");
        assert_eq!(v["path"], "ch/1");
    }

    #[test]
    fn serde_round_trip_with_uri() {
        // Old-format JSON (uri field present) deserializes correctly.
        let json = r#"{"corpus_id":"xenos","path":"ch/1","uri":"calli://xenos/ch/1"}"#;
        let loc: Location = serde_json::from_str(json).unwrap();
        assert_eq!(loc.corpus_id, "xenos");
        assert_eq!(loc.path, "ch/1");
        assert_eq!(loc.uri(), "calli://xenos/ch/1");
    }

    #[test]
    fn serde_round_trip_without_uri() {
        // New-format JSON (no uri field) also deserializes correctly.
        let json = r#"{"corpus_id":"xenos","path":"ch/1"}"#;
        let loc: Location = serde_json::from_str(json).unwrap();
        assert_eq!(loc.corpus_id, "xenos");
        assert_eq!(loc.path, "ch/1");
    }
}
