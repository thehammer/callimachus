use crate::types::{Edge, Entity, EntityContract, EntityPurpose};
use serde::{Deserialize, Serialize};

// ── entity_contracts ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityContractsInput {
    pub corpus_id: String,
    pub entity_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityContractsOutput {
    pub entity_id: String,
    pub contract: EntityContract,
    pub purpose: Option<EntityPurpose>,
    pub verified_by: Vec<Edge>,
}

// ── entities_without_tests ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitiesWithoutTestsInput {
    pub corpus_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitiesWithoutTestsOutput {
    pub entities: Vec<Entity>,
    pub count: usize,
}
