use serde::{Deserialize, Serialize};

/// A corpus-level architectural invariant or recurring pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub id: String,
    pub corpus_id: String,
    pub title: String,
    pub statement: String,
    pub confidence: f32,
    pub model: Option<String>,
    pub generated_at: String,
}
