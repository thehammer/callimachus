use crate::types::{Collection, Location};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::corpus_types::CorpusOverviewOutput;
use super::search_read_types::SearchMode;

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

// ── collection_entity_meet (location helper) ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionLocation {
    pub corpus_id: String,
    pub corpus_name: String,
    pub location: Location,
}
