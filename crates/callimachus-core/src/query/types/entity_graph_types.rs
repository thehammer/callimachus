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

/// A fact anchored to an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityFact {
    pub id: String,
    pub kind: String,
    pub summary: String,
    pub confidence: String,
    pub evidence: Vec<String>,
    pub notes: Option<String>,
}

/// A reference-corpus entity linked to an entity by address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityReference {
    pub corpus_id: String,
    pub entity_id: String,
    pub name: String,
    pub summary: Option<String>,
}

/// A hardware/behaviour variant reachable from a linked reference entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityVariant {
    pub id: String,
    pub summary: Option<String>,
}

/// `entity` output: the entity plus its facts, linked reference entities and variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityOutput {
    #[serde(flatten)]
    pub entity: Entity,
    #[serde(default)]
    pub facts: Vec<EntityFact>,
    #[serde(default)]
    pub reference: Vec<EntityReference>,
    #[serde(default)]
    pub variants: Vec<EntityVariant>,
}

impl std::ops::Deref for EntityOutput {
    type Target = Entity;
    fn deref(&self) -> &Entity {
        &self.entity
    }
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
