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
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub first_location: Option<Location>,
    pub last_location: Option<Location>,
    pub appearance_count: u32,
    /// Extraction confidence 0.0–1.0.
    pub confidence: f32,
}

impl Entity {
    pub fn new(id: String, corpus_id: String, canonical_name: String, kind: String) -> Self {
        Self {
            id,
            corpus_id,
            canonical_name,
            kind,
            aliases: vec![],
            description: None,
            first_location: None,
            last_location: None,
            appearance_count: 0,
            confidence: 0.5,
        }
    }
}
