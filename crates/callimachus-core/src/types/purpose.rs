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
    /// The manifest `current_version` at which this purpose was last written.
    /// `None` for rows that pre-date migration 012.
    #[serde(default)]
    pub derived_at_version: Option<String>,
}

