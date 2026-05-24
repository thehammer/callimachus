use serde::{Deserialize, Serialize};

/// A named block/section within a complex function, with a human-readable description.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityBlock {
    pub id: String,
    pub entity_id: String,
    pub corpus_id: String,
    pub label: String,
    pub description: String,
    pub position: i64,
    /// The manifest `current_version` at which this block was last written.
    /// `None` for rows that pre-date migration 012.
    #[serde(default)]
    pub derived_at_version: Option<String>,
}

