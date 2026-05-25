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
use rusqlite::OptionalExtension;

use crate::storage::{
    backend::{CascadeStats, StorageBackend},
    block_store, chunk_store, collection_store, contract_store, corpus_store, correction_store,
    db::Database,
    edge_store, embedding_store, entity_store, fts, history,
    pruning::PruneStats,
    purpose_store, run_log, sqlite_graph, summary_store, theme_store,
};
use crate::types::pass::{Pass, RunStatus};
use crate::types::{
    Chunk, Collection, CollectionMember, Corpus, CorpusStatus, Edge, Entity, EntityBlock,
    EntityContract, EntityPurpose, Location, MemberType, Summary, SummaryTargetKind, Theme,
};

/// SQLite-backed storage. Thread-safe via `Arc<Mutex<Database>>`.
pub struct SqliteBackend {
    pub(crate) db: Arc<Mutex<Database>>,
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

    /// Test-only: acquire the database lock and return the guard for raw SQL assertions.
    #[cfg(test)]
    pub fn db_for_test(&self) -> std::sync::MutexGuard<'_, Database> {
        self.db.lock().expect("database lock poisoned")
    }

    /// Execute a closure inside a write transaction.
    ///
    /// The transaction is committed when `f` returns `Ok`, rolled back on `Err`.
    /// This acquires the database lock for the duration of the closure.
    pub fn with_write_tx<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<R>,
    {
        let mut guard = self.db.lock().expect("database lock poisoned");
        let tx = guard.conn_mut().transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
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

    fn corpus_set_last_indexed_version(&self, id: &str, version: &str) -> Result<()> {
        corpus_store::set_last_indexed_version(&db!(self), id, version)
    }

    fn corpus_get_last_indexed_version(&self, id: &str) -> Result<Option<String>> {
        corpus_store::get_last_indexed_version(&db!(self), id)
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

    fn chunk_set_source_hash(&self, chunk_id: &str, hash: &str) -> Result<()> {
        chunk_store::set_source_hash(&db!(self), chunk_id, hash)
    }

    fn chunk_set_history(
        &self,
        chunk_id: &str,
        version: &str,
        commit_message: Option<&str>,
        author: Option<&str>,
    ) -> Result<()> {
        chunk_store::set_history(&db!(self), chunk_id, version, commit_message, author)
    }

    fn chunk_list_source_paths(&self, corpus_id: &str) -> Result<Vec<(String, String, String)>> {
        chunk_store::list_source_paths(&db!(self), corpus_id)
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

    fn entity_list_by_abstract_kind(
        &self,
        corpus_ids: &[&str],
        abstract_kind: &str,
    ) -> Result<Vec<Entity>> {
        entity_store::list_by_abstract_kind(&db!(self), corpus_ids, abstract_kind)
    }

    fn kind_taxonomy_list(&self) -> Result<Vec<(String, String, String)>> {
        entity_store::list_taxonomy(&db!(self))
    }

    fn entity_list_at_version(&self, corpus_id: &str, version: &str) -> Result<Vec<Entity>> {
        let guard = db!(self);
        let conn = guard.conn();
        // entities_history lacks abstract_kind; substitute '' so row_to_entity's
        // column offsets stay consistent with the head-table query.
        let mut stmt = conn.prepare(
            "SELECT id, corpus_id, canonical_name, kind, abstract_kind,
                    aliases, description,
                    first_location_uri, last_location_uri,
                    appearance_count, confidence, derived_at_version
             FROM entities
             WHERE corpus_id = ?1 AND derived_at_version = ?2
             UNION ALL
             SELECT id, corpus_id, canonical_name, kind, '' AS abstract_kind,
                    aliases, description,
                    first_location_uri, last_location_uri,
                    appearance_count, confidence, derived_at_version
             FROM entities_history
             WHERE corpus_id = ?1 AND derived_at_version = ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![corpus_id, version],
            entity_store::row_to_entity,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn entity_count_at_version(&self, corpus_id: &str, version: &str) -> Result<u64> {
        let guard = db!(self);
        let n: i64 = guard.conn().query_row(
            "SELECT COUNT(*) FROM (
               SELECT id FROM entities
                 WHERE corpus_id = ?1 AND derived_at_version = ?2
               UNION ALL
               SELECT id FROM entities_history
                 WHERE corpus_id = ?1 AND derived_at_version = ?2
             )",
            rusqlite::params![corpus_id, version],
            |r| r.get(0),
        )?;
        Ok(n as u64)
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

    fn entity_in_degree(&self, corpus_id: &str, entity_id: &str) -> Result<u32> {
        edge_store::in_degree(&db!(self), corpus_id, entity_id)
    }

    fn entity_out_degree(&self, corpus_id: &str, entity_id: &str) -> Result<u32> {
        edge_store::out_degree(&db!(self), corpus_id, entity_id)
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
        summary_store::get_best(&db!(self), corpus_id, target_kind, target_id)
    }

    fn summary_get_for_model(
        &self,
        corpus_id: &str,
        target_kind: &SummaryTargetKind,
        target_id: &str,
        model: &str,
    ) -> Result<Option<Summary>> {
        summary_store::get_for_model(&db!(self), corpus_id, target_kind, target_id, model)
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
        purpose_store::get_best(&db!(self), corpus_id, entity_id)
    }

    fn purpose_get_for_model(
        &self,
        corpus_id: &str,
        entity_id: &str,
        model: &str,
    ) -> Result<Option<EntityPurpose>> {
        purpose_store::get_for_model(&db!(self), corpus_id, entity_id, model)
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
        contract_store::get_best(&db!(self), corpus_id, entity_id)
    }

    fn contract_get_for_model(
        &self,
        corpus_id: &str,
        entity_id: &str,
        model: &str,
    ) -> Result<Option<EntityContract>> {
        contract_store::get_for_model(&db!(self), corpus_id, entity_id, model)
    }

    fn contract_list(&self, corpus_id: &str) -> Result<Vec<EntityContract>> {
        contract_store::list(&db!(self), corpus_id)
    }

    fn contract_list_best_per_entity(&self, corpus_id: &str) -> Result<Vec<EntityContract>> {
        contract_store::list_best_per_entity(&db!(self), corpus_id)
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

    // ── History / Archive ─────────────────────────────────────────────────────

    fn archive_entity(
        &self,
        entity_id: &str,
        corpus_id: &str,
        superseded_at_version: &str,
    ) -> Result<bool> {
        let guard = db!(self);
        history::archive_entity(guard.conn(), entity_id, corpus_id, superseded_at_version)
    }

    fn archive_edges_for_entity(
        &self,
        entity_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_edges_for_entity(guard.conn(), entity_id, superseded_at_version)
    }

    fn archive_purposes_for_entity(
        &self,
        entity_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_purposes_for_entity(guard.conn(), entity_id, superseded_at_version)
    }

    fn archive_contracts_for_entity(
        &self,
        entity_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_contracts_for_entity(guard.conn(), entity_id, superseded_at_version)
    }

    fn archive_blocks_for_entity(
        &self,
        entity_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_blocks_for_entity(guard.conn(), entity_id, superseded_at_version)
    }

    fn archive_summaries_for_target(
        &self,
        corpus_id: &str,
        target_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_summaries_for_target(
            guard.conn(),
            corpus_id,
            target_id,
            superseded_at_version,
        )
    }

    fn archive_chunk(&self, chunk_id: &str, superseded_at_version: &str) -> Result<bool> {
        let guard = db!(self);
        history::archive_chunk(guard.conn(), chunk_id, superseded_at_version)
    }

    fn archive_theme(
        &self,
        theme_id: &str,
        corpus_id: &str,
        superseded_at_version: &str,
    ) -> Result<bool> {
        let guard = db!(self);
        history::archive_theme(guard.conn(), theme_id, corpus_id, superseded_at_version)
    }

    fn archive_themes_for_corpus(
        &self,
        corpus_id: &str,
        superseded_at_version: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        let conn = guard.conn();
        let now = chrono::Utc::now().to_rfc3339();
        let rows = conn.execute(
            "INSERT INTO themes_history
               (id, corpus_id, title, statement, confidence,
                model, model_tier, generated_at,
                derived_at_version, superseded_at_version, superseded_at)
             SELECT id, corpus_id, title, statement, confidence,
                    model, model_tier, generated_at,
                    derived_at_version, ?2, ?3
             FROM themes WHERE corpus_id = ?1",
            rusqlite::params![corpus_id, superseded_at_version, now],
        )?;
        Ok(rows as u64)
    }

    fn cascade_delete_dirty_subtree(
        &self,
        corpus_id: &str,
        dirty_chunk_ids: &[String],
        superseded_at_version: &str,
    ) -> Result<CascadeStats> {
        self.with_write_tx(|tx| {
            let mut stats = CascadeStats::default();

            for chunk_id in dirty_chunk_ids {
                // Resolve the location URI for this chunk.
                let location_uri: Option<String> = tx
                    .query_row(
                        "SELECT location_uri FROM chunks WHERE id = ?1",
                        rusqlite::params![chunk_id],
                        |r| r.get(0),
                    )
                    .optional()?;

                if let Some(uri) = location_uri {
                    // Find all entities whose first or last location is at this URI.
                    let entity_ids: Vec<String> = {
                        let mut stmt = tx.prepare(
                            "SELECT id FROM entities
                             WHERE corpus_id = ?1
                               AND (first_location_uri = ?2 OR last_location_uri = ?2)",
                        )?;
                        let rows = stmt.query_map(rusqlite::params![corpus_id, uri], |r| {
                            r.get::<_, String>(0)
                        })?;
                        rows.collect::<std::result::Result<Vec<_>, _>>()?
                    };

                    for entity_id in &entity_ids {
                        // Archive before FK-cascade delete wipes the head rows.
                        history::archive_edges_for_entity(tx, entity_id, superseded_at_version)?;
                        history::archive_purposes_for_entity(tx, entity_id, superseded_at_version)?;
                        history::archive_contracts_for_entity(
                            tx,
                            entity_id,
                            superseded_at_version,
                        )?;
                        history::archive_blocks_for_entity(tx, entity_id, superseded_at_version)?;
                        history::archive_summaries_for_target(
                            tx,
                            corpus_id,
                            entity_id,
                            superseded_at_version,
                        )?;
                        history::archive_entity(tx, entity_id, corpus_id, superseded_at_version)?;

                        // Delete entity — FK ON DELETE CASCADE removes edges/purposes/contracts/blocks.
                        tx.execute(
                            "DELETE FROM entities WHERE id = ?1",
                            rusqlite::params![entity_id],
                        )?;
                        // Delete summaries explicitly (no FK cascade from entities → summaries).
                        tx.execute(
                            "DELETE FROM summaries WHERE corpus_id = ?1 AND target_id = ?2",
                            rusqlite::params![corpus_id, entity_id],
                        )?;
                        stats.entities_archived += 1;
                    }
                }

                // Archive chunk summaries before deleting the chunk.
                history::archive_summaries_for_target(
                    tx,
                    corpus_id,
                    chunk_id,
                    superseded_at_version,
                )?;
                history::archive_chunk(tx, chunk_id, superseded_at_version)?;

                // Delete chunk — FK ON DELETE CASCADE removes embeddings.
                tx.execute(
                    "DELETE FROM chunks WHERE id = ?1",
                    rusqlite::params![chunk_id],
                )?;
                // Delete summaries for chunk explicitly.
                tx.execute(
                    "DELETE FROM summaries WHERE corpus_id = ?1 AND target_id = ?2",
                    rusqlite::params![corpus_id, chunk_id],
                )?;
                stats.chunks_archived += 1;
            }

            Ok(stats)
        })
    }

    // ── Graph helpers ─────────────────────────────────────────────────────────

    fn entities_without_inbound_calls(&self, corpus_id: &str) -> Result<Vec<Entity>> {
        sqlite_graph::entities_without_inbound_calls(&db!(self), corpus_id)
    }

    fn entities_without_verified_by(&self, corpus_id: &str) -> Result<Vec<Entity>> {
        sqlite_graph::entities_without_verified_by(&db!(self), corpus_id)
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    fn schema_version(&self) -> Result<u64> {
        let guard = db!(self);
        let v: i64 = guard
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        Ok(v as u64)
    }

    // ── Backfill history writes ───────────────────────────────────────────────

    fn chunk_history_insert(
        &self,
        chunk: &Chunk,
        derived_at_version: &str,
        superseded_at_version: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT OR IGNORE INTO chunks_history
             (id, corpus_id, parent_path, kind, location_uri, content,
              byte_length, created_at, semantic_processed, source_hash,
              introduced_at_version, last_modified_at_version,
              last_modified_commit_message, last_modified_author,
              superseded_at_version, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,0,?9,?10,?11,NULL,NULL,?12,?13)",
            rusqlite::params![
                chunk.id,
                chunk.corpus_id,
                chunk.parent_path,
                chunk.kind,
                chunk.location.uri,
                chunk.content,
                chunk.byte_length as i64,
                chunk.created_at,
                chunk.source_hash,
                derived_at_version,
                derived_at_version,
                superseded_at_version,
                now,
            ],
        )?;
        Ok(())
    }

    fn chunk_history_update_source_hash(
        &self,
        chunk_id: &str,
        derived_at_version: &str,
        source_hash: &str,
    ) -> Result<()> {
        let guard = db!(self);
        guard.conn().execute(
            "UPDATE chunks_history SET source_hash = ?1
             WHERE id = ?2 AND introduced_at_version = ?3",
            rusqlite::params![source_hash, chunk_id, derived_at_version],
        )?;
        Ok(())
    }

    fn chunk_history_update_version(
        &self,
        chunk_id: &str,
        derived_at_version: &str,
        last_modified_at_version: &str,
        commit_message: Option<&str>,
        author: Option<&str>,
    ) -> Result<()> {
        let guard = db!(self);
        guard.conn().execute(
            "UPDATE chunks_history
             SET last_modified_at_version = ?1,
                 last_modified_commit_message = COALESCE(?2, last_modified_commit_message),
                 last_modified_author = COALESCE(?3, last_modified_author),
                 introduced_at_version = COALESCE(introduced_at_version, ?1)
             WHERE id = ?4 AND introduced_at_version = ?5",
            rusqlite::params![
                last_modified_at_version,
                commit_message,
                author,
                chunk_id,
                derived_at_version,
            ],
        )?;
        Ok(())
    }

    fn entity_history_insert(
        &self,
        entity: &Entity,
        derived_at_version: &str,
        superseded_at_version: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        let aliases_json = serde_json::to_string(&entity.aliases)?;
        let first_uri = entity.first_location.as_ref().map(|l| &l.uri);
        let last_uri = entity.last_location.as_ref().map(|l| &l.uri);
        guard.conn().execute(
            "INSERT OR IGNORE INTO entities_history
             (id, corpus_id, canonical_name, kind, aliases, description,
              first_location_uri, last_location_uri, appearance_count, confidence,
              derived_at_version, superseded_at_version, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                entity.id,
                entity.corpus_id,
                entity.canonical_name,
                entity.kind,
                aliases_json,
                entity.description,
                first_uri,
                last_uri,
                entity.appearance_count as i64,
                entity.confidence as f64,
                derived_at_version,
                superseded_at_version,
                now,
            ],
        )?;
        Ok(())
    }

    fn edge_history_insert(
        &self,
        edge: &Edge,
        derived_at_version: &str,
        superseded_at_version: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        // No FK guard: history tables have no foreign key constraints.
        guard.conn().execute(
            "INSERT OR IGNORE INTO edges_history
             (id, corpus_id, from_entity_id, to_entity_id, kind,
              location_uri, confidence, derived_at_version,
              superseded_at_version, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                edge.id,
                edge.corpus_id,
                edge.from_entity_id,
                edge.to_entity_id,
                edge.kind,
                edge.location.uri,
                edge.confidence as f64,
                derived_at_version,
                superseded_at_version,
                now,
            ],
        )?;
        Ok(())
    }

    fn summary_history_insert(
        &self,
        summary: &Summary,
        derived_at_version: &str,
        superseded_at_version: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        let target_kind_str = summary.target_kind.to_string();
        guard.conn().execute(
            "INSERT OR IGNORE INTO summaries_history
             (id, corpus_id, target_kind, target_id, depth, text,
              model, model_tier, generated_at,
              derived_at_version, superseded_at_version, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                summary.id,
                summary.corpus_id,
                target_kind_str,
                summary.target_id,
                summary.depth,
                summary.text,
                summary.model,
                summary.model_tier,
                summary.generated_at,
                derived_at_version,
                superseded_at_version,
                now,
            ],
        )?;
        Ok(())
    }

    fn purpose_history_insert(
        &self,
        purpose: &EntityPurpose,
        derived_at_version: &str,
        superseded_at_version: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT OR IGNORE INTO entity_purposes_history
             (entity_id, corpus_id, purpose, model, model_tier, generated_at,
              derived_at_version, superseded_at_version, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                purpose.entity_id,
                purpose.corpus_id,
                purpose.purpose,
                purpose.model,
                purpose.model_tier,
                purpose.generated_at,
                derived_at_version,
                superseded_at_version,
                now,
            ],
        )?;
        Ok(())
    }

    fn contract_history_insert(
        &self,
        contract: &EntityContract,
        derived_at_version: &str,
        superseded_at_version: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        let debt_json =
            serde_json::to_string(&contract.debt_markers).unwrap_or_else(|_| "[]".into());
        let assumptions_json =
            serde_json::to_string(&contract.assumptions).unwrap_or_else(|_| "[]".into());
        let risks_json = serde_json::to_string(&contract.risks).unwrap_or_else(|_| "[]".into());
        guard.conn().execute(
            "INSERT OR IGNORE INTO entity_contracts_history
             (entity_id, corpus_id,
              is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
              is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
              panic_call_count, debt_markers, assumptions, risks,
              intent_gap, caller_notes, model, model_tier, generated_at,
              derived_at_version, superseded_at_version, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)",
            rusqlite::params![
                contract.entity_id,
                contract.corpus_id,
                contract.is_public as i64,
                contract.is_must_use as i64,
                contract.is_deprecated as i64,
                contract.is_fallible as i64,
                contract.is_nullable as i64,
                contract.is_mutating as i64,
                contract.is_diverging as i64,
                contract.has_panic_risk as i64,
                contract.has_unsafe as i64,
                contract.is_incomplete as i64,
                contract.panic_call_count,
                debt_json,
                assumptions_json,
                risks_json,
                contract.intent_gap,
                contract.caller_notes,
                contract.model,
                contract.model_tier,
                contract.generated_at,
                derived_at_version,
                superseded_at_version,
                now,
            ],
        )?;
        Ok(())
    }

    fn block_history_insert(
        &self,
        block: &EntityBlock,
        derived_at_version: &str,
        superseded_at_version: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT OR IGNORE INTO entity_blocks_history
             (id, entity_id, corpus_id, label, description, position,
              derived_at_version, superseded_at_version, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                block.id,
                block.entity_id,
                block.corpus_id,
                block.label,
                block.description,
                block.position,
                derived_at_version,
                superseded_at_version,
                now,
            ],
        )?;
        Ok(())
    }

    fn theme_history_insert(
        &self,
        theme: &Theme,
        derived_at_version: &str,
        superseded_at_version: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT OR IGNORE INTO themes_history
             (id, corpus_id, title, statement, confidence,
              model, model_tier, generated_at,
              derived_at_version, superseded_at_version, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            rusqlite::params![
                theme.id,
                theme.corpus_id,
                theme.title,
                theme.statement,
                theme.confidence as f64,
                theme.model,
                theme.model_tier,
                theme.generated_at,
                derived_at_version,
                superseded_at_version,
                now,
            ],
        )?;
        Ok(())
    }

    // ── Backfill seeding helpers ──────────────────────────────────────────────

    fn entity_head_versions(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT id, COALESCE(derived_at_version, '') FROM entities WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn chunk_head_versions(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT id, COALESCE(last_modified_at_version, '') FROM chunks WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn edge_head_versions(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT id, COALESCE(derived_at_version, '') FROM edges WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn summary_head_versions(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT target_id, COALESCE(derived_at_version, '') FROM summaries WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn purpose_head_versions(&self, corpus_id: &str) -> Result<Vec<((String, String), String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT entity_id, model, COALESCE(derived_at_version, '') FROM entity_purposes WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((
                (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn contract_head_versions(&self, corpus_id: &str) -> Result<Vec<((String, String), String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT entity_id, model, COALESCE(derived_at_version, '') FROM entity_contracts WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((
                (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn block_head_versions(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        // Return one entry per entity_id (max derived_at_version across blocks).
        let mut stmt = guard.conn().prepare(
            "SELECT entity_id, COALESCE(MAX(derived_at_version), '')
             FROM entity_blocks WHERE corpus_id = ?1 GROUP BY entity_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn theme_head_versions(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT id, COALESCE(derived_at_version, '') FROM themes WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    // ── Pruning ───────────────────────────────────────────────────────────────

    fn prune_history(&self, corpus_id: &str, keep: usize, dry_run: bool) -> Result<PruneStats> {
        self.with_write_tx(|tx| {
            // Step 1: Collect the ordered list of distinct supersession SHAs for
            // this corpus by taking MAX(superseded_at) per SHA across all 8 history
            // tables, then ordering ascending by that timestamp.
            let ordered_shas: Vec<String> = {
                let mut stmt = tx.prepare(
                    "WITH all_events AS (
                         SELECT superseded_at_version AS sha, MAX(superseded_at) AS ts
                           FROM entities_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_version
                         UNION ALL
                         SELECT superseded_at_version, MAX(superseded_at)
                           FROM edges_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_version
                         UNION ALL
                         SELECT superseded_at_version, MAX(superseded_at)
                           FROM entity_purposes_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_version
                         UNION ALL
                         SELECT superseded_at_version, MAX(superseded_at)
                           FROM entity_contracts_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_version
                         UNION ALL
                         SELECT superseded_at_version, MAX(superseded_at)
                           FROM entity_blocks_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_version
                         UNION ALL
                         SELECT superseded_at_version, MAX(superseded_at)
                           FROM summaries_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_version
                         UNION ALL
                         SELECT superseded_at_version, MAX(superseded_at)
                           FROM chunks_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_version
                         UNION ALL
                         SELECT superseded_at_version, MAX(superseded_at)
                           FROM themes_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_version
                     )
                     SELECT sha, MAX(ts) AS ts
                       FROM all_events
                       GROUP BY sha
                       ORDER BY ts ASC",
                )?;
                let rows =
                    stmt.query_map(rusqlite::params![corpus_id], |r| r.get::<_, String>(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };

            let total = ordered_shas.len();

            // Step 2: If we have <= keep SHAs, nothing to prune.
            if total <= keep {
                return Ok(PruneStats {
                    supersession_shas_kept: total,
                    supersession_shas_pruned: 0,
                    ..Default::default()
                });
            }

            // Step 3: Partition — oldest (prune) vs newest (keep).
            let prune_count = total - keep;
            let prune_set: Vec<String> = ordered_shas.into_iter().take(prune_count).collect();

            // Step 4: For each history table, count then optionally delete.
            // We chunk the IN clause to ≤500 parameters for SQLite compatibility.
            const CHUNK_SIZE: usize = 500;

            // Table name paired with a fn pointer that accumulates counts into PruneStats.
            type Acc = fn(&mut PruneStats, usize);
            let tables: &[(&str, Acc)] = &[
                ("entities_history", |s, n| {
                    s.rows_pruned_entities_history += n
                }),
                ("edges_history", |s, n| s.rows_pruned_edges_history += n),
                ("entity_purposes_history", |s, n| {
                    s.rows_pruned_entity_purposes_history += n
                }),
                ("entity_contracts_history", |s, n| {
                    s.rows_pruned_entity_contracts_history += n
                }),
                ("entity_blocks_history", |s, n| {
                    s.rows_pruned_entity_blocks_history += n
                }),
                ("summaries_history", |s, n| {
                    s.rows_pruned_summaries_history += n
                }),
                ("chunks_history", |s, n| s.rows_pruned_chunks_history += n),
                ("themes_history", |s, n| s.rows_pruned_themes_history += n),
            ];

            let mut stats = PruneStats {
                supersession_shas_kept: keep,
                supersession_shas_pruned: prune_set.len(),
                ..Default::default()
            };

            for (table, accumulate) in tables {
                let mut table_count: usize = 0;

                for chunk in prune_set.chunks(CHUNK_SIZE) {
                    // Build the IN (?, ?, …) placeholder list for this chunk.
                    let placeholders: String = chunk
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("?{}", i + 2))
                        .collect::<Vec<_>>()
                        .join(", ");

                    // COUNT query — runs regardless of dry_run.
                    let count_sql = format!(
                        "SELECT COUNT(*) FROM {table} \
                         WHERE corpus_id = ?1 \
                           AND superseded_at_version IN ({placeholders})"
                    );
                    let count: usize = {
                        let mut stmt = tx.prepare(&count_sql)?;
                        let params_iter =
                            std::iter::once(&corpus_id as &dyn rusqlite::types::ToSql)
                                .chain(chunk.iter().map(|s| s as &dyn rusqlite::types::ToSql));
                        stmt.query_row(rusqlite::params_from_iter(params_iter), |r| r.get(0))?
                    };
                    table_count += count;

                    // DELETE — only in real mode.
                    if !dry_run {
                        let delete_sql = format!(
                            "DELETE FROM {table} \
                             WHERE corpus_id = ?1 \
                               AND superseded_at_version IN ({placeholders})"
                        );
                        let mut stmt = tx.prepare(&delete_sql)?;
                        let params_iter =
                            std::iter::once(&corpus_id as &dyn rusqlite::types::ToSql)
                                .chain(chunk.iter().map(|s| s as &dyn rusqlite::types::ToSql));
                        stmt.execute(rusqlite::params_from_iter(params_iter))?;
                    }
                }

                accumulate(&mut stats, table_count);
            }

            // Step 5: Commit (handled by with_write_tx on Ok) or rollback on Err.
            // In dry-run mode the transaction wraps the SELECTs; it will be rolled
            // back if we return Ok without deleting — which is fine for read consistency.
            Ok(stats)
        })
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
