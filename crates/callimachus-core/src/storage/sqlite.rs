//! SQLite implementation of `StorageBackend`.
//!
//! `SqliteBackend` wraps `Arc<Mutex<Database>>` and implements `StorageBackend` by
//! delegating to the existing `*_store` modules. The store modules contain all SQL;
//! this file contains only delegation.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::corrections::types::{Correction, CorrectionKind};
use crate::error::Result;
use crate::storage::edge_store::EdgeDirection;
use crate::storage::embedding_store::StoredEmbedding;
use crate::storage::fts::FtsResult;
use crate::storage::run_log::{PassStats, RunRecord};
use crate::storage::{
    backend::StorageBackend, block_store, chunk_store, collection_store, contract_store,
    corpus_store, correction_store, db::Database, edge_store, embedding_store, entity_store, fts,
    purpose_store, run_log, sqlite_graph, summary_store, theme_store,
};
use crate::types::pass::{Pass, RunStatus};
use crate::types::{
    Chunk, Collection, CollectionMember, Corpus, CorpusStatus, Edge, Entity, EntityBlock,
    EntityContract, EntityPurpose, Location, MemberType, Summary, SummaryTargetKind, Theme,
};

/// SQLite-backed storage. Thread-safe via `Arc<Mutex<Database>>`.
pub struct SqliteBackend {
    db: Arc<Mutex<Database>>,
}

impl SqliteBackend {
    /// Open (or create) a database at `path`, running any pending migrations.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            db: Arc::new(Mutex::new(Database::open(path)?)),
        })
    }

    /// Open an in-memory database. Useful for tests.
    pub fn open_in_memory() -> Result<Self> {
        Ok(Self {
            db: Arc::new(Mutex::new(Database::open_in_memory()?)),
        })
    }
}

// Convenience macro to lock the mutex and propagate the poison error.
macro_rules! db {
    ($self:expr) => {
        $self.db.lock().expect("database lock poisoned")
    };
}

impl StorageBackend for SqliteBackend {
    // ── Corpus ────────────────────────────────────────────────────────────────

    fn corpus_insert(&self, corpus: &Corpus) -> Result<()> {
        corpus_store::insert(&db!(self), corpus)
    }

    fn corpus_list(&self) -> Result<Vec<Corpus>> {
        corpus_store::list(&db!(self))
    }

    fn corpus_get(&self, id: &str) -> Result<Option<Corpus>> {
        corpus_store::get(&db!(self), id)
    }

    fn corpus_require(&self, id: &str) -> Result<Corpus> {
        corpus_store::require(&db!(self), id)
    }

    fn corpus_update_status(&self, id: &str, status: CorpusStatus) -> Result<()> {
        corpus_store::update_status(&db!(self), id, status)
    }

    fn corpus_set_last_indexed(&self, id: &str, at: &str) -> Result<()> {
        corpus_store::set_last_indexed(&db!(self), id, at)
    }

    fn corpus_set_pipeline_version(&self, id: &str, version: u32) -> Result<()> {
        corpus_store::set_pipeline_version(&db!(self), id, version)
    }

    fn corpus_delete(&self, id: &str) -> Result<bool> {
        corpus_store::delete(&db!(self), id)
    }

    fn corpus_exists(&self, id: &str) -> Result<bool> {
        corpus_store::exists(&db!(self), id)
    }

    // ── Chunk ─────────────────────────────────────────────────────────────────

    fn chunk_upsert(&self, chunk: &Chunk) -> Result<()> {
        chunk_store::upsert(&db!(self), chunk)
    }

    fn chunk_has(&self, id: &str) -> Result<bool> {
        chunk_store::has(&db!(self), id)
    }

    fn chunk_get(&self, id: &str) -> Result<Option<Chunk>> {
        chunk_store::get_by_id(&db!(self), id)
    }

    fn chunk_get_by_uri(&self, uri: &str) -> Result<Option<Chunk>> {
        chunk_store::get(&db!(self), uri)
    }

    fn chunk_list(&self, corpus_id: &str) -> Result<Vec<Chunk>> {
        chunk_store::list(&db!(self), corpus_id)
    }

    fn chunk_list_ids(&self, corpus_id: &str) -> Result<Vec<String>> {
        chunk_store::list_ids_for_corpus(&db!(self), corpus_id)
    }

    fn chunk_list_unprocessed(&self, corpus_id: &str) -> Result<Vec<Chunk>> {
        chunk_store::list_unprocessed(&db!(self), corpus_id)
    }

    fn chunk_count(&self, corpus_id: &str) -> Result<u64> {
        chunk_store::count(&db!(self), corpus_id)
    }

    fn chunk_set_parent_path(&self, chunk_id: &str, parent_path: &str) -> Result<()> {
        chunk_store::set_parent_path(&db!(self), chunk_id, parent_path)
    }

    fn chunk_set_semantic_processed(&self, chunk_id: &str) -> Result<()> {
        chunk_store::set_semantic_processed(&db!(self), chunk_id)
    }

    fn chunk_delete_by_id(&self, chunk_id: &str) -> Result<bool> {
        chunk_store::delete_by_id(&db!(self), chunk_id)
    }

    fn chunk_children_by_uri(&self, corpus_id: &str, parent_uri: &str) -> Result<Vec<Location>> {
        chunk_store::children_by_uri(&db!(self), corpus_id, parent_uri)
    }

    // ── Entity ────────────────────────────────────────────────────────────────

    fn entity_upsert(&self, entity: &Entity) -> Result<()> {
        entity_store::upsert(&db!(self), entity)
    }

    fn entity_get_by_id(&self, id: &str) -> Result<Option<Entity>> {
        entity_store::get_by_id(&db!(self), id)
    }

    fn entity_find_by_name(&self, corpus_id: &str, name: &str) -> Result<Vec<Entity>> {
        entity_store::find_by_name(&db!(self), corpus_id, name)
    }

    fn entity_list(&self, corpus_id: &str) -> Result<Vec<Entity>> {
        entity_store::list(&db!(self), corpus_id)
    }

    fn entity_count(&self, corpus_id: &str) -> Result<u64> {
        entity_store::count(&db!(self), corpus_id)
    }

    fn entity_top(&self, corpus_id: &str, limit: usize) -> Result<Vec<Entity>> {
        entity_store::top(&db!(self), corpus_id, limit)
    }

    fn entity_merge(&self, keep_id: &str, absorb_id: &str) -> Result<()> {
        entity_store::merge(&db!(self), keep_id, absorb_id)
    }

    fn entities_at_location(&self, corpus_id: &str, uri: &str) -> Result<Vec<Entity>> {
        entity_store::at_location(&db!(self), corpus_id, uri)
    }

    // ── Edge ──────────────────────────────────────────────────────────────────

    fn edge_upsert(&self, edge: &Edge) -> Result<()> {
        edge_store::upsert(&db!(self), edge)
    }

    fn edge_get_for_entity(
        &self,
        entity_id: &str,
        direction: EdgeDirection,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Edge>> {
        edge_store::get_for_entity(&db!(self), entity_id, direction, kind, limit)
    }

    fn edge_list(&self, corpus_id: &str) -> Result<Vec<Edge>> {
        edge_store::list(&db!(self), corpus_id)
    }

    fn edge_count(&self, corpus_id: &str) -> Result<u64> {
        edge_store::count(&db!(self), corpus_id)
    }

    fn edge_location_uris_for_entity(&self, entity_id: &str) -> Result<Vec<String>> {
        edge_store::location_uris_for_entity(&db!(self), entity_id)
    }

    fn edge_entity_ids_at_location(&self, location_uri: &str) -> Result<Vec<String>> {
        edge_store::entity_ids_at_location(&db!(self), location_uri)
    }

    // ── Summary ───────────────────────────────────────────────────────────────

    fn summary_upsert(&self, summary: &Summary) -> Result<()> {
        summary_store::upsert(&db!(self), summary)
    }

    fn summary_list(&self, corpus_id: &str) -> Result<Vec<Summary>> {
        summary_store::list(&db!(self), corpus_id)
    }

    fn summary_delete_for_target(&self, corpus_id: &str, target_id: &str) -> Result<()> {
        summary_store::delete_for_target(&db!(self), corpus_id, target_id)
    }

    fn summary_get(
        &self,
        corpus_id: &str,
        target_kind: &SummaryTargetKind,
        target_id: &str,
    ) -> Result<Option<Summary>> {
        summary_store::get(&db!(self), corpus_id, target_kind, target_id)
    }

    // ── Run log ───────────────────────────────────────────────────────────────

    fn run_start(&self, corpus_id: &str, pass: &str, provider: Option<&str>) -> Result<String> {
        let pass_enum: Pass = pass
            .parse()
            .map_err(|e: String| crate::error::CalError::Other(e))?;
        run_log::start_run(&db!(self), corpus_id, &pass_enum, provider)
    }

    fn run_finish(&self, run_id: &str, status: RunStatus, stats: &PassStats) -> Result<()> {
        run_log::finish_run(&db!(self), run_id, status, stats)
    }

    fn run_latest(&self, corpus_id: &str, limit: usize) -> Result<Vec<RunRecord>> {
        run_log::latest_runs_n(&db!(self), corpus_id, limit)
    }

    fn run_abandon_stale(&self, corpus_id: &str) -> Result<u64> {
        run_log::abandon_stale(&db!(self), corpus_id)
    }

    // ── Corrections ───────────────────────────────────────────────────────────

    fn correction_insert(
        &self,
        corpus_id: Option<&str>,
        collection_id: Option<&str>,
        kind: &CorrectionKind,
    ) -> Result<String> {
        correction_store::insert(&db!(self), corpus_id, collection_id, kind)
    }

    fn correction_list(&self, corpus_id: &str) -> Result<Vec<Correction>> {
        correction_store::list(&db!(self), corpus_id)
    }

    fn correction_list_for_collection(&self, collection_id: &str) -> Result<Vec<Correction>> {
        correction_store::list_for_collection(&db!(self), collection_id)
    }

    fn correction_list_all(&self) -> Result<Vec<Correction>> {
        correction_store::list_all(&db!(self))
    }

    fn correction_delete(&self, id: &str) -> Result<bool> {
        correction_store::delete(&db!(self), id)
    }

    // ── FTS / Search ──────────────────────────────────────────────────────────

    fn fts_search(&self, corpus_id: &str, query: &str, limit: usize) -> Result<Vec<FtsResult>> {
        fts::search(&db!(self), corpus_id, query, limit)
    }

    fn fts_rebuild(&self, corpus_id: &str) -> Result<()> {
        fts::rebuild(&db!(self), corpus_id)
    }

    // ── Embeddings ────────────────────────────────────────────────────────────

    fn embedding_upsert(&self, embedding: &StoredEmbedding) -> Result<()> {
        embedding_store::upsert(&db!(self), embedding)
    }

    fn embedding_get_for_chunk(&self, chunk_id: &str) -> Result<Option<StoredEmbedding>> {
        embedding_store::get_for_chunk(&db!(self), chunk_id)
    }

    fn embedding_list_for_corpus(&self, corpus_id: &str) -> Result<Vec<StoredEmbedding>> {
        embedding_store::list_for_corpus(&db!(self), corpus_id)
    }

    fn embedding_count(&self, corpus_id: &str) -> Result<u64> {
        embedding_store::count(&db!(self), corpus_id)
    }

    // ── Collection ────────────────────────────────────────────────────────────

    fn collection_insert(&self, collection: &Collection) -> Result<()> {
        collection_store::insert(&db!(self), collection)
    }

    fn collection_list(&self) -> Result<Vec<Collection>> {
        collection_store::list(&db!(self))
    }

    fn collection_get(&self, id: &str) -> Result<Option<Collection>> {
        collection_store::get(&db!(self), id)
    }

    fn collection_require(&self, id: &str) -> Result<Collection> {
        collection_store::require(&db!(self), id)
    }

    fn collection_add_member(
        &self,
        collection_id: &str,
        member_id: &str,
        member_type: MemberType,
    ) -> Result<()> {
        collection_store::add_member(&db!(self), collection_id, member_id, member_type)
    }

    fn collection_remove_member(
        &self,
        collection_id: &str,
        member_id: &str,
        member_type: MemberType,
    ) -> Result<()> {
        collection_store::remove_member(&db!(self), collection_id, member_id, member_type)
    }

    fn collection_delete(&self, id: &str) -> Result<bool> {
        collection_store::delete(&db!(self), id)
    }

    fn collection_direct_members(&self, collection_id: &str) -> Result<Vec<CollectionMember>> {
        collection_store::direct_members(&db!(self), collection_id)
    }

    fn collection_resolve_corpus_ids(&self, collection_id: &str) -> Result<Vec<String>> {
        collection_store::resolve_corpus_ids(&db!(self), collection_id)
    }

    // ── Purpose ───────────────────────────────────────────────────────────────

    fn purpose_upsert(&self, p: &EntityPurpose) -> Result<()> {
        purpose_store::upsert(&db!(self), p)
    }

    fn purpose_get(&self, corpus_id: &str, entity_id: &str) -> Result<Option<EntityPurpose>> {
        purpose_store::get(&db!(self), corpus_id, entity_id)
    }

    fn purpose_list(&self, corpus_id: &str) -> Result<Vec<EntityPurpose>> {
        purpose_store::list(&db!(self), corpus_id)
    }

    // ── Block ─────────────────────────────────────────────────────────────────

    fn block_upsert(&self, b: &EntityBlock) -> Result<()> {
        block_store::upsert(&db!(self), b)
    }

    fn block_list_for_entity(&self, entity_id: &str) -> Result<Vec<EntityBlock>> {
        block_store::list_for_entity(&db!(self), entity_id)
    }

    // ── Contract ──────────────────────────────────────────────────────────────

    fn contract_upsert(&self, c: &EntityContract) -> Result<()> {
        contract_store::upsert(&db!(self), c)
    }

    fn contract_get(&self, corpus_id: &str, entity_id: &str) -> Result<Option<EntityContract>> {
        contract_store::get(&db!(self), corpus_id, entity_id)
    }

    fn contract_list(&self, corpus_id: &str) -> Result<Vec<EntityContract>> {
        contract_store::list(&db!(self), corpus_id)
    }

    fn contract_list_inconsistencies(&self, corpus_id: &str) -> Result<Vec<EntityContract>> {
        contract_store::list_with_inconsistencies(&db!(self), corpus_id)
    }

    // ── Theme ─────────────────────────────────────────────────────────────────

    fn theme_upsert(&self, t: &Theme) -> Result<()> {
        theme_store::upsert(&db!(self), t)
    }

    fn theme_list(&self, corpus_id: &str) -> Result<Vec<Theme>> {
        theme_store::list(&db!(self), corpus_id)
    }

    // ── Graph helpers ─────────────────────────────────────────────────────────

    fn entities_without_inbound_calls(&self, corpus_id: &str) -> Result<Vec<Entity>> {
        sqlite_graph::entities_without_inbound_calls(&db!(self), corpus_id)
    }

    fn entities_without_verified_by(&self, corpus_id: &str) -> Result<Vec<Entity>> {
        sqlite_graph::entities_without_verified_by(&db!(self), corpus_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::backend::StorageBackend;
    use crate::types::Corpus;

    fn make_backend() -> SqliteBackend {
        SqliteBackend::open_in_memory().unwrap()
    }

    #[test]
    fn sqlite_backend_satisfies_trait() {
        // Compile-time check: SqliteBackend implements StorageBackend.
        let backend: &dyn StorageBackend = &make_backend();
        let _ = backend.corpus_list().unwrap();
    }

    #[test]
    fn corpus_round_trip() {
        let backend = make_backend();
        let corpus = Corpus::new("c1".into(), "Test".into(), "book".into(), "/tmp".into());
        backend.corpus_insert(&corpus).unwrap();
        let got = backend.corpus_get("c1").unwrap().unwrap();
        assert_eq!(got.id, "c1");
    }

    #[test]
    fn open_in_memory_runs_migrations() {
        let backend = SqliteBackend::open_in_memory().unwrap();
        // If migrations didn't run, corpus_list would fail.
        let result = backend.corpus_list();
        assert!(result.is_ok());
    }
}
