use crate::types::{Edge, Entity, Location};
use serde::{Deserialize, Serialize};

// ── entity ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityInput {
    pub corpus_id: String,
    pub name_or_id: String,
    /// Optional adapter-defined kind filter (e.g. "class", "method", "file").
    /// When set, name matches are filtered to this kind before the ambiguity check.
    #[serde(default)]
    pub kind: Option<String>,
}

// ── entity_edges ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityEdgesInput {
    pub corpus_id: String,
    pub entity_id: String,
    /// "inbound" | "outbound" | "both"
    #[serde(default = "default_direction")]
    pub direction: String,
    pub kind: Option<String>,
    pub limit: Option<u32>,
}

fn default_direction() -> String {
    "both".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityEdgesOutput {
    pub entity_id: String,
    pub edges: Vec<Edge>,
    pub count: usize,
}

// ── entity_meet ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityMeetInput {
    pub corpus_id: String,
    pub entity_a: String,
    pub entity_b: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityMeetOutput {
    pub first_co_occurrence: Location,
    pub all: Vec<Location>,
    pub count: u32,
}

// ── entity_search_by_abstract_kind ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySearchByAbstractKindInput {
    pub corpus_ids: Vec<String>,
    pub abstract_kind: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySearchByAbstractKindOutput {
    pub entities: Vec<Entity>,
    pub count: usize,
}

// ── list_abstract_kinds ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListAbstractKindsInput {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxonomyRow {
    pub concrete_kind: String,
    pub corpus_kind: String,
    pub abstract_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAbstractKindsOutput {
    pub rows: Vec<TaxonomyRow>,
    pub count: usize,
}
