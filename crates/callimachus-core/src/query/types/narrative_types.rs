use crate::types::{Edge, Entity, Location};
use serde::{Deserialize, Serialize};

// ── chapter_summary (composite) ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterSummaryInput {
    pub corpus_id: String,
    /// Chapter number ("3"), ordinal word ("Three"), or chapter title.
    pub chapter: String,
}

// ── character_profile (composite) ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterProfileInput {
    pub corpus_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterProfileOutput {
    pub entity: Entity,
    pub edges: Vec<Edge>,
    pub summary: Option<String>,
}

// ── find_scene (composite) ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindSceneInput {
    pub corpus_id: String,
    pub entity_a: String,
    pub entity_b: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindSceneOutput {
    pub location: Location,
    pub content: String,
    pub entities_present: Vec<Entity>,
}
