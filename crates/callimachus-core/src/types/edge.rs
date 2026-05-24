use crate::types::location::Location;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub corpus_id: String,
    pub from_entity_id: String,
    pub to_entity_id: String,
    /// Adapter-defined kind: "calls", "extends", "imports", "meets",
    /// "located_in", "mentions", "allied_with", etc.
    pub kind: String,
    pub location: Location,
    pub confidence: f32,
    /// The manifest `current_version` at which this edge was last written.
    /// `None` for rows that pre-date migration 012.
    #[serde(default)]
    pub derived_at_version: Option<String>,
}

impl Edge {
    pub fn new(
        id: String,
        corpus_id: String,
        from_entity_id: String,
        to_entity_id: String,
        kind: String,
        location: Location,
    ) -> Self {
        Self {
            id,
            corpus_id,
            from_entity_id,
            to_entity_id,
            kind,
            location,
            confidence: 0.5,
            derived_at_version: None,
        }
    }
}

impl Default for Edge {
    fn default() -> Self {
        Self::new(
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            Location::default(),
        )
    }
}
