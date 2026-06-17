use crate::types::EntityContract;
use serde::{Deserialize, Serialize};

// ── summarize ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SummarizeTarget {
    Corpus,
    Entity { entity_id: String },
    Location { location: String },
    Range { from: String, to: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeInput {
    pub corpus_id: String,
    pub target: SummarizeTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeOutput {
    pub text: String,
    pub generated_at: String,
    pub model: Option<String>,
}

// ── find_inconsistencies ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindInconsistenciesInput {
    pub corpus_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindInconsistenciesOutput {
    pub contracts: Vec<EntityContract>,
    pub count: usize,
}

// ── find_unreachable ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindUnreachableInput {
    pub corpus_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindUnreachableOutput {
    pub entities: Vec<crate::types::Entity>,
    pub count: usize,
}

// ── explain_component ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainComponentInput {
    pub corpus_id: String,
    pub entity_id: Option<String>,
    pub module_prefix: Option<String>,
    pub max_depth: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainNode {
    pub entity_id: String,
    pub name: String,
    pub kind: String,
    pub purpose: Option<String>,
    pub summary: Option<String>,
    pub blocks: Vec<ExplainBlock>,
    pub depth: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainBlock {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainComponentOutput {
    pub narrative: String,
    pub nodes: Vec<ExplainNode>,
}

/// Greek term for a narrative exposition.
/// `Diegesis` is the output of `explain_component` — a multi-paragraph narrative
/// assembled via BFS over call edges using pre-indexed purposes, summaries, and
/// block descriptions, with zero LLM calls at query time.
pub type Diegesis = ExplainComponentOutput;
