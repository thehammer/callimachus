use crate::types::{Edge, Theme};
use serde::{Deserialize, Serialize};

// ── corpus_list ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CorpusListInput {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusListEntry {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub last_indexed: Option<String>,
    pub chunk_count: u64,
    pub entity_count: u64,
}

// ── corpus_overview ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusOverviewInput {
    pub corpus_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusOverviewOutput {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub chunk_count: u64,
    pub entity_count: u64,
    pub last_indexed: Option<String>,
    pub top_entities: Vec<crate::types::Entity>,
    pub summary: Option<String>,
    /// The `platform` fact's data, when the corpus has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<serde_json::Value>,
}

// ── list_facts ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFactsInput {
    pub corpus_id: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFactsOutput {
    pub facts: Vec<ListedFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListedFact {
    #[serde(flatten)]
    pub fact: crate::corrections::FactRecord,
    pub anchors: Vec<String>,
}

// ── corpus_themes ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusThemesInput {
    pub corpus_id: String,
    pub include_edges: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusThemesOutput {
    pub themes: Vec<Theme>,
    pub upheld_by: Vec<Edge>,
    pub violated_by: Vec<Edge>,
}
