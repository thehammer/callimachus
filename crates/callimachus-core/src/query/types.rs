use crate::corrections::types::EntityLinkKind;
use crate::types::{
    Collection, Edge, Entity, EntityContract, EntityPurpose, Location, Scope, Theme,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    pub top_entities: Vec<Entity>,
    pub summary: Option<String>,
}

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOutput {
    pub results: Vec<SearchResult>,
    pub total: usize,
}

// ── entity ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityInput {
    pub corpus_id: String,
    pub name_or_id: String,
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
}

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

// ── collection_list ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionListInput {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionListOutput {
    pub collections: Vec<CollectionListEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionListEntry {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Number of direct members (corpora + nested collections).
    pub member_count: u64,
    /// Number of leaf corpora reachable transitively.
    pub corpus_count: u64,
}

// ── collection_search ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionSearchInput {
    pub collection_id: String,
    pub query: String,
    #[serde(default)]
    pub mode: SearchMode,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionSearchOutput {
    pub results: Vec<CollectionSearchResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionSearchResult {
    pub corpus_id: String,
    pub corpus_name: String,
    pub location: Location,
    pub snippet: String,
    pub relevance: f32,
}

// ── collection_overview ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionOverviewInput {
    pub collection_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionOverviewOutput {
    pub collection: Collection,
    pub corpora: Vec<CorpusOverviewOutput>,
    /// Direct children of member_type=collection.
    pub nested_collections: Vec<Collection>,
    pub total_chunks: u64,
    pub total_entities: u64,
    /// Count of entity links by kind string (e.g. "same_as" → 3).
    pub cross_corpus_links_by_kind: BTreeMap<String, u64>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionLocation {
    pub corpus_id: String,
    pub corpus_name: String,
    pub location: Location,
}

// ── Phase 12 types ────────────────────────────────────────────────────────────

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
    pub entities: Vec<Entity>,
    pub count: usize,
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
