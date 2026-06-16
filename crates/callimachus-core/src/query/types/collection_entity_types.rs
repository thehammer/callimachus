use crate::corrections::types::EntityLinkKind;
use crate::types::Entity;
use serde::{Deserialize, Serialize};

use super::collection_browse_types::CollectionLocation;

// ── collection_entity_resolve ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionEntityResolveInput {
    pub collection_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionEntityResolveOutput {
    pub matches: Vec<CollectionEntityMatch>,
    pub related: Vec<CollectionEntityRelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionEntityMatch {
    pub corpus_id: String,
    pub corpus_name: String,
    pub entity: Entity,
    /// (corpus_id, entity_id) pairs in the SameAs equivalence class (excluding self).
    pub same_as: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionEntityRelation {
    pub from: (String, String), // (corpus_id, entity_id)
    pub to: (String, String),   // (corpus_id, entity_id)
    pub kind: EntityLinkKind,
    pub note: Option<String>,
}

// ── collection_entity_meet ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionEntityMeetInput {
    pub collection_id: String,
    pub entity_a: String,
    pub entity_b: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionEntityMeetOutput {
    pub first_co_occurrence: Option<CollectionLocation>,
    pub all: Vec<CollectionLocation>,
    pub count: u64,
}
