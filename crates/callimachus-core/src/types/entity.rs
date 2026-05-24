use crate::types::location::Location;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub corpus_id: String,
    pub canonical_name: String,
    /// Adapter-defined kind: "character", "place", "organization", "object",
    /// "function", "class", "concept", etc.
    pub kind: String,
    /// Cross-corpus abstract taxonomy kind (e.g. "person", "process", "component").
    /// Resolved from kind_taxonomy at insert time; empty string if no mapping.
    #[serde(default)]
    pub abstract_kind: String,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub first_location: Option<Location>,
    pub last_location: Option<Location>,
    pub appearance_count: u32,
    /// Extraction confidence 0.0–1.0.
    pub confidence: f32,
    /// The manifest `current_version` (git SHA or tree hash) at which this
    /// entity was last written.  `None` for rows that pre-date migration 012.
    #[serde(default)]
    pub derived_at_version: Option<String>,
}

impl Entity {
    pub fn new(id: String, corpus_id: String, canonical_name: String, kind: String) -> Self {
        Self {
            id,
            corpus_id,
            canonical_name,
            kind,
            abstract_kind: String::new(),
            aliases: vec![],
            description: None,
            first_location: None,
            last_location: None,
            appearance_count: 0,
            confidence: 0.5,
            derived_at_version: None,
        }
    }
}

impl Default for Entity {
    fn default() -> Self {
        Self::new(String::new(), String::new(), String::new(), String::new())
    }
}
