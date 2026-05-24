use serde::{Deserialize, Serialize};

/// A corpus-level architectural invariant or recurring pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub id: String,
    pub corpus_id: String,
    pub title: String,
    pub statement: String,
    pub confidence: f32,
    /// The LLM model that generated this artifact.
    pub model: String,
    /// Coarse quality tier: "opus" > "sonnet" > "haiku" > "unknown".
    pub model_tier: String,
    pub generated_at: String,
    /// The manifest `current_version` at which this theme was last written.
    /// `None` for rows that pre-date migration 012.
    #[serde(default)]
    pub derived_at_version: Option<String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            id: String::new(),
            corpus_id: String::new(),
            title: String::new(),
            statement: String::new(),
            confidence: 0.0,
            model: String::new(),
            model_tier: String::new(),
            generated_at: String::new(),
            derived_at_version: None,
        }
    }
}
