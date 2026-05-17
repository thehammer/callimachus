use serde::{Deserialize, Serialize};

/// A named block/section within a complex function, with a human-readable description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityBlock {
    pub id: String,
    pub entity_id: String,
    pub corpus_id: String,
    pub label: String,
    pub description: String,
    pub position: i64,
}
