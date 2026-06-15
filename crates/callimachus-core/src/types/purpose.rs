use crate::types::provenance::Provenance;
use serde::{Deserialize, Serialize};

/// Why an entity exists — the "purpose" layer separate from behavioral summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityPurpose {
    pub entity_id: String,
    pub corpus_id: String,
    pub purpose: String,
    /// The LLM model that generated this artifact.
    pub model: String,
    /// Coarse quality tier: "opus" > "sonnet" > "haiku" > "unknown".
    pub model_tier: String,
    pub generated_at: String,
    /// The honest-provenance tag for this purpose. `None` for rows that
    /// pre-date migration 013.
    #[serde(default)]
    pub provenance: Option<Provenance>,
}
