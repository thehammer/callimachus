use crate::types::{Entity, Location, Scope};
use serde::{Deserialize, Serialize};

// ── search ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Keyword,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchInput {
    pub corpus_id: String,
    pub query: String,
    #[serde(default)]
    pub mode: SearchMode,
    pub scope: Option<Scope>,
    pub limit: Option<u32>,
    /// Blend weight for hybrid search: `α * semantic + (1-α) * keyword`.
    /// Range [0.0, 1.0]. Default 0.5. Only used when `mode = Hybrid`.
    pub semantic_weight: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub location: Location,
    pub snippet: String,
    pub relevance: f32,
    pub kind: String,
    /// 0-based start line of the chunk in its source file.
    /// `None` for non-code corpora or pre-migration pinakes.
    #[serde(default)]
    pub start_line: Option<u32>,
    /// 0-based end line (inclusive) of the chunk in its source file.
    /// `None` for non-code corpora or pre-migration pinakes.
    #[serde(default)]
    pub end_line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOutput {
    pub results: Vec<SearchResult>,
    pub total: usize,
}

// ── read ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadDepth {
    Summary,
    Scenes,
    #[default]
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadInput {
    pub corpus_id: Option<String>,
    pub location: String,
    #[serde(default)]
    pub depth: ReadDepth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadOutput {
    pub location: Location,
    pub kind: String,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub entities_present: Vec<Entity>,
    pub child_locations: Vec<Location>,
    /// 0-based start line of the chunk in its source file.
    /// `None` for non-code corpora or pre-migration pinakes.
    #[serde(default)]
    pub start_line: Option<u32>,
    /// 0-based end line (inclusive) of the chunk in its source file.
    /// `None` for non-code corpora or pre-migration pinakes.
    #[serde(default)]
    pub end_line: Option<u32>,
}

// ── related ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedInput {
    pub corpus_id: String,
    pub location: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedItem {
    pub location: Location,
    pub relationship: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedOutput {
    pub items: Vec<RelatedItem>,
}
