//! Storage backend abstraction.
//!
//! The `StorageBackend` trait defines the complete contract for Callimachus storage.
//! `SqliteBackend` is the default implementation. `PostgresBackend` is a compile-only
//! stub that confirms the trait is implementable without SQLite.
//!
//! See `docs/adapting-storage.md` for implementation guidance.

use crate::corrections::types::{Correction, CorrectionKind};
use crate::error::Result;
use crate::storage::edge_store::EdgeDirection;
use crate::storage::embedding_store::StoredEmbedding;
use crate::storage::fts::FtsResult;
use crate::storage::run_log::{PassStats, RunRecord};
use crate::types::pass::RunStatus;
use crate::types::{
    Chunk, Collection, CollectionMember, Corpus, CorpusStatus, Edge, Entity, EntityBlock,
    EntityContract, EntityPurpose, Location, MemberType, Summary, SummaryTargetKind, Theme,
};

/// Statistics returned by `cascade_delete_dirty_subtree`.
#[derive(Debug, Default, Clone)]
pub struct CascadeStats {
    /// Number of chunk rows moved to `chunks_history` and deleted.
    pub chunks_archived: u64,
    /// Number of entity rows moved to `entities_history` and deleted.
    pub entities_archived: u64,
}

/// A swappable storage backend. `SqliteBackend` is the default implementation.
///
/// All methods are synchronous. The backend is responsible for its own concurrency
/// control (e.g. `Mutex<Connection>` for SQLite). Callers hold `Arc<dyn StorageBackend>`.
///
/// Future implementations: `PostgresBackend` (RDS/Aurora), `MemoryBackend` (test-only).
pub trait StorageBackend: Send + Sync {
    // ── Corpus ────────────────────────────────────────────────────────────────

    fn corpus_insert(&self, corpus: &Corpus) -> Result<()>;
    fn corpus_list(&self) -> Result<Vec<Corpus>>;
    fn corpus_get(&self, id: &str) -> Result<Option<Corpus>>;
    fn corpus_require(&self, id: &str) -> Result<Corpus>;
    fn corpus_update_status(&self, id: &str, status: CorpusStatus) -> Result<()>;
    fn corpus_set_last_indexed(&self, id: &str, at: &str) -> Result<()>;
    fn corpus_set_pipeline_version(&self, id: &str, version: u32) -> Result<()>;
    /// Write the version reference (git SHA or v1-tree hash) after a successful
    /// pipeline run that included Pass::History.
    fn corpus_set_last_indexed_version(&self, id: &str, version: &str) -> Result<()>;
    /// Read back the stored version reference (None until first history pass).
    fn corpus_get_last_indexed_version(&self, id: &str) -> Result<Option<String>>;
    fn corpus_delete(&self, id: &str) -> Result<bool>;
    fn corpus_exists(&self, id: &str) -> Result<bool>;

    // ── Chunk ─────────────────────────────────────────────────────────────────

    fn chunk_upsert(&self, chunk: &Chunk) -> Result<()>;
    fn chunk_has(&self, id: &str) -> Result<bool>;
    /// Get a chunk by its content-addressed ID.
    fn chunk_get(&self, id: &str) -> Result<Option<Chunk>>;
    /// Get a chunk by its location URI.
    fn chunk_get_by_uri(&self, uri: &str) -> Result<Option<Chunk>>;
    fn chunk_list(&self, corpus_id: &str) -> Result<Vec<Chunk>>;
    fn chunk_list_ids(&self, corpus_id: &str) -> Result<Vec<String>>;
    fn chunk_list_unprocessed(&self, corpus_id: &str) -> Result<Vec<Chunk>>;
    fn chunk_count(&self, corpus_id: &str) -> Result<u64>;
    fn chunk_set_parent_path(&self, chunk_id: &str, parent_path: &str) -> Result<()>;
    fn chunk_set_semantic_processed(&self, chunk_id: &str) -> Result<()>;
    fn chunk_delete_by_id(&self, chunk_id: &str) -> Result<bool>;
    /// Update the source_hash column for a chunk written by chunk_pass.
    fn chunk_set_source_hash(&self, chunk_id: &str, hash: &str) -> Result<()>;
    /// Write history metadata (version + optional commit info) for a chunk.
    /// Sets `last_modified_at_version`; also sets `introduced_at_version` if
    /// the chunk row does not yet have one.
    fn chunk_set_history(
        &self,
        chunk_id: &str,
        version: &str,
        commit_message: Option<&str>,
        author: Option<&str>,
    ) -> Result<()>;
    /// Return `(chunk_id, location_uri, source_hash)` for all chunks in a corpus.
    /// Used by Stage 0 to compare stored state against fresh adapter output.
    fn chunk_list_source_paths(&self, corpus_id: &str) -> Result<Vec<(String, String, String)>>;
    /// Get location URIs of child chunks (chunks whose parent_path equals `parent_uri`).
    fn chunk_children_by_uri(&self, corpus_id: &str, parent_uri: &str) -> Result<Vec<Location>>;

    // ── Entity ────────────────────────────────────────────────────────────────

    fn entity_upsert(&self, entity: &Entity) -> Result<()>;
    fn entity_get_by_id(&self, id: &str) -> Result<Option<Entity>>;
    fn entity_find_by_name(&self, corpus_id: &str, name: &str) -> Result<Vec<Entity>>;
    fn entity_list(&self, corpus_id: &str) -> Result<Vec<Entity>>;
    fn entity_count(&self, corpus_id: &str) -> Result<u64>;
    fn entity_top(&self, corpus_id: &str, limit: usize) -> Result<Vec<Entity>>;
    fn entity_merge(&self, keep_id: &str, absorb_id: &str) -> Result<()>;
    /// Returns entities whose `first_location_uri` or `last_location_uri` equals `uri`.
    fn entities_at_location(&self, corpus_id: &str, uri: &str) -> Result<Vec<Entity>>;
    /// Returns entities with the given abstract taxonomy kind across the specified corpora.
    fn entity_list_by_abstract_kind(
        &self,
        corpus_ids: &[&str],
        abstract_kind: &str,
    ) -> Result<Vec<Entity>>;
    /// Returns all rows from kind_taxonomy as (concrete_kind, corpus_kind, abstract_kind).
    fn kind_taxonomy_list(&self) -> Result<Vec<(String, String, String)>>;

    // ── Edge ──────────────────────────────────────────────────────────────────

    fn edge_upsert(&self, edge: &Edge) -> Result<()>;
    fn edge_get_for_entity(
        &self,
        entity_id: &str,
        direction: EdgeDirection,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Edge>>;
    fn edge_list(&self, corpus_id: &str) -> Result<Vec<Edge>>;
    fn edge_count(&self, corpus_id: &str) -> Result<u64>;
    /// Returns distinct location URIs for all edges involving `entity_id`.
    fn edge_location_uris_for_entity(&self, entity_id: &str) -> Result<Vec<String>>;
    /// Returns the set of entity IDs (from/to) for edges at `location_uri`.
    fn edge_entity_ids_at_location(&self, location_uri: &str) -> Result<Vec<String>>;
    /// Returns the number of edges pointing *into* `entity_id` (in-degree).
    fn entity_in_degree(&self, corpus_id: &str, entity_id: &str) -> Result<u32>;
    /// Returns the number of edges pointing *out of* `entity_id` (out-degree).
    fn entity_out_degree(&self, corpus_id: &str, entity_id: &str) -> Result<u32>;

    // ── Summary ───────────────────────────────────────────────────────────────

    fn summary_upsert(&self, summary: &Summary) -> Result<()>;
    fn summary_list(&self, corpus_id: &str) -> Result<Vec<Summary>>;
    fn summary_delete_for_target(&self, corpus_id: &str, target_id: &str) -> Result<()>;
    /// Best-tier summary for the target (None ⇒ no row). Transparent to callers.
    fn summary_get(
        &self,
        corpus_id: &str,
        target_kind: &SummaryTargetKind,
        target_id: &str,
    ) -> Result<Option<Summary>>;
    /// Exact-model lookup; returns `None` if no row for that model exists.
    fn summary_get_for_model(
        &self,
        corpus_id: &str,
        target_kind: &SummaryTargetKind,
        target_id: &str,
        model: &str,
    ) -> Result<Option<Summary>>;

    // ── Run log ───────────────────────────────────────────────────────────────

    fn run_start(&self, corpus_id: &str, pass: &str, provider: Option<&str>) -> Result<String>;
    fn run_finish(&self, run_id: &str, status: RunStatus, stats: &PassStats) -> Result<()>;
    fn run_latest(&self, corpus_id: &str, limit: usize) -> Result<Vec<RunRecord>>;
    /// Mark any stale `status='running'` rows for this corpus as `status='failed'`.
    /// Returns the count of rows updated.
    fn run_abandon_stale(&self, corpus_id: &str) -> Result<u64>;

    // ── Corrections ───────────────────────────────────────────────────────────

    /// Insert a correction. Exactly one of `corpus_id` / `collection_id` must be `Some`.
    fn correction_insert(
        &self,
        corpus_id: Option<&str>,
        collection_id: Option<&str>,
        kind: &CorrectionKind,
    ) -> Result<String>;
    fn correction_list(&self, corpus_id: &str) -> Result<Vec<Correction>>;
    fn correction_list_for_collection(&self, collection_id: &str) -> Result<Vec<Correction>>;
    fn correction_list_all(&self) -> Result<Vec<Correction>>;
    fn correction_delete(&self, id: &str) -> Result<bool>;

    // ── FTS / Search ──────────────────────────────────────────────────────────

    fn fts_search(&self, corpus_id: &str, query: &str, limit: usize) -> Result<Vec<FtsResult>>;
    fn fts_rebuild(&self, corpus_id: &str) -> Result<()>;

    // ── Embeddings ────────────────────────────────────────────────────────────

    fn embedding_upsert(&self, embedding: &StoredEmbedding) -> Result<()>;
    fn embedding_get_for_chunk(&self, chunk_id: &str) -> Result<Option<StoredEmbedding>>;
    fn embedding_list_for_corpus(&self, corpus_id: &str) -> Result<Vec<StoredEmbedding>>;
    fn embedding_count(&self, corpus_id: &str) -> Result<u64>;

    // ── Collection ────────────────────────────────────────────────────────────

    fn collection_insert(&self, collection: &Collection) -> Result<()>;
    fn collection_list(&self) -> Result<Vec<Collection>>;
    fn collection_get(&self, id: &str) -> Result<Option<Collection>>;
    fn collection_require(&self, id: &str) -> Result<Collection>;
    fn collection_add_member(
        &self,
        collection_id: &str,
        member_id: &str,
        member_type: MemberType,
    ) -> Result<()>;
    fn collection_remove_member(
        &self,
        collection_id: &str,
        member_id: &str,
        member_type: MemberType,
    ) -> Result<()>;
    fn collection_delete(&self, id: &str) -> Result<bool>;
    fn collection_direct_members(&self, collection_id: &str) -> Result<Vec<CollectionMember>>;
    fn collection_resolve_corpus_ids(&self, collection_id: &str) -> Result<Vec<String>>;

    // ── Purpose ───────────────────────────────────────────────────────────────

    fn purpose_upsert(&self, p: &EntityPurpose) -> Result<()>;
    /// Best-tier artifact for the entity (None ⇒ no row). Transparent to callers.
    fn purpose_get(&self, corpus_id: &str, entity_id: &str) -> Result<Option<EntityPurpose>>;
    /// Exact-model lookup; returns `None` if no row for that model exists.
    fn purpose_get_for_model(
        &self,
        corpus_id: &str,
        entity_id: &str,
        model: &str,
    ) -> Result<Option<EntityPurpose>>;
    fn purpose_list(&self, corpus_id: &str) -> Result<Vec<EntityPurpose>>;

    // ── Block ─────────────────────────────────────────────────────────────────

    fn block_upsert(&self, b: &EntityBlock) -> Result<()>;
    fn block_list_for_entity(&self, entity_id: &str) -> Result<Vec<EntityBlock>>;

    // ── Contract ──────────────────────────────────────────────────────────────

    fn contract_upsert(&self, c: &EntityContract) -> Result<()>;
    /// Best-tier artifact for the entity (None ⇒ no row). Transparent to callers.
    fn contract_get(&self, corpus_id: &str, entity_id: &str) -> Result<Option<EntityContract>>;
    /// Exact-model lookup; returns `None` if no row for that model exists.
    fn contract_get_for_model(
        &self,
        corpus_id: &str,
        entity_id: &str,
        model: &str,
    ) -> Result<Option<EntityContract>>;
    /// All contract rows (multiple per entity in multi-model corpora).
    fn contract_list(&self, corpus_id: &str) -> Result<Vec<EntityContract>>;
    /// One best-tier row per entity. For callers that want exactly one per entity.
    fn contract_list_best_per_entity(&self, corpus_id: &str) -> Result<Vec<EntityContract>>;
    fn contract_list_inconsistencies(&self, corpus_id: &str) -> Result<Vec<EntityContract>>;

    // ── Theme ─────────────────────────────────────────────────────────────────

    fn theme_upsert(&self, t: &Theme) -> Result<()>;
    fn theme_list(&self, corpus_id: &str) -> Result<Vec<Theme>>;

    // ── History / Archive ─────────────────────────────────────────────────────
    //
    // Fine-grained archive methods (one per artifact type). Each copies the
    // current head row(s) into the corresponding `*_history` table without
    // deleting the head. Useful for tests and as building blocks for
    // `cascade_delete_dirty_subtree`.

    /// Archive a single entity row into `entities_history`.
    /// Returns `true` if a history row was inserted.
    fn archive_entity(
        &self,
        entity_id: &str,
        corpus_id: &str,
        superseded_at_version: &str,
    ) -> Result<bool>;
    /// Archive all edges involving `entity_id` (both directions) into `edges_history`.
    fn archive_edges_for_entity(&self, entity_id: &str, superseded_at_version: &str)
    -> Result<u64>;
    /// Archive all purpose rows for `entity_id` into `entity_purposes_history`.
    fn archive_purposes_for_entity(
        &self,
        entity_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64>;
    /// Archive all contract rows for `entity_id` into `entity_contracts_history`.
    fn archive_contracts_for_entity(
        &self,
        entity_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64>;
    /// Archive all block rows for `entity_id` into `entity_blocks_history`.
    fn archive_blocks_for_entity(
        &self,
        entity_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64>;
    /// Archive all summary rows for `target_id` within `corpus_id` into `summaries_history`.
    fn archive_summaries_for_target(
        &self,
        corpus_id: &str,
        target_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64>;
    /// Archive a single chunk row into `chunks_history`.
    fn archive_chunk(&self, chunk_id: &str, superseded_at_version: &str) -> Result<bool>;
    /// Archive a single theme row into `themes_history`.
    fn archive_theme(
        &self,
        theme_id: &str,
        corpus_id: &str,
        superseded_at_version: &str,
    ) -> Result<bool>;
    /// Archive all theme rows for `corpus_id` into `themes_history`.
    fn archive_themes_for_corpus(
        &self,
        corpus_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64>;

    /// Transactionally archive then delete all artifacts (entities, edges,
    /// purposes, contracts, blocks, summaries) associated with the given chunk
    /// IDs, plus the chunks themselves.
    ///
    /// The SQLite implementation executes this inside a single write transaction
    /// via `with_write_tx` for atomicity. On other backends it returns an error.
    ///
    /// `dirty_chunk_ids` — chunk IDs to sweep (typically produced by filtering
    /// all corpus chunks against a `ChangeManifest`).
    /// `superseded_at_version` — the incoming git SHA written to
    /// `*_history.superseded_at_version` for every archived row.
    fn cascade_delete_dirty_subtree(
        &self,
        corpus_id: &str,
        dirty_chunk_ids: &[String],
        superseded_at_version: &str,
    ) -> Result<CascadeStats>;

    // ── Graph helpers ─────────────────────────────────────────────────────────

    /// Entities with no inbound `calls` edges (potentially unreachable code).
    fn entities_without_inbound_calls(&self, corpus_id: &str) -> Result<Vec<Entity>>;
    /// Entities with no inbound `verified_by` edges (no test coverage).
    fn entities_without_verified_by(&self, corpus_id: &str) -> Result<Vec<Entity>>;

    // ── Schema ────────────────────────────────────────────────────────────────

    /// Return the current schema migration version (SQLite `user_version` pragma).
    fn schema_version(&self) -> Result<u64>;
}
