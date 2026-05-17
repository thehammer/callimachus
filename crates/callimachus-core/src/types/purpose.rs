use serde::{Deserialize, Serialize};

/// Why an entity exists — the "purpose" layer separate from behavioral summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityPurpose {
    pub entity_id: String,
    pub corpus_id: String,
    pub purpose: String,
    pub model: Option<String>,
    pub generated_at: String,
}
