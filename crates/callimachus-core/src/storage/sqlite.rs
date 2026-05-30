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
    ancestry::{self, AncestryReader},
    backend::{CascadeStats, MigrateFreshStats, StorageBackend},
    block_store, chunk_store, collection_store, contract_store, corpus_store, correction_store,
    db::Database,
    edge_store, embedding_store, entity_store, fts, history,
    pruning::PruneStats,
    purpose_store, run_log, sqlite_graph, summary_store, theme_store,
};
use crate::types::pass::{Pass, RunStatus};
use crate::types::provenance::{
    ArchiveSet, ArchiveStats, CachedArtifact, Layer2CacheKey, Provenance, RefineOutcome, Tombstone,
};
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

    /// Shared body for the tombstone-ancestry check, usable from a held
    /// connection (so callers that already hold the lock — e.g.
    /// `entity_list_at_sha` — don't re-acquire it).
    ///
    /// An artifact is tombstoned at `target_sha` when any of its tombstones has a
    /// death SHA that is an ancestor-or-equal of `target_sha`.
    fn is_tombstoned_at_conn(
        conn: &rusqlite::Connection,
        corpus_id: &str,
        artifact_kind: &str,
        artifact_id: &str,
        target_sha: &str,
        ancestry: Option<&dyn AncestryReader>,
    ) -> Result<bool> {
        let mut stmt = conn.prepare(
            "SELECT derived_at_sha FROM artifact_tombstones
             WHERE corpus_id = ?1 AND artifact_kind = ?2 AND artifact_id = ?3",
        )?;
        let death_shas = stmt.query_map(
            rusqlite::params![corpus_id, artifact_kind, artifact_id],
            |row| row.get::<_, String>(0),
        )?;
        for sha in death_shas {
            let sha = sha?;
            if ancestry::is_ancestor_or_equal(ancestry, &sha, target_sha) {
                return Ok(true);
            }
        }
        Ok(false)
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

    fn corpus_set_backfill_cursor(&self, id: &str, cursor: Option<&str>) -> Result<()> {
        corpus_store::set_backfill_cursor(&db!(self), id, cursor)
    }

    fn corpus_get_backfill_cursor(&self, id: &str) -> Result<Option<String>> {
        corpus_store::get_backfill_cursor(&db!(self), id)
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

    fn chunk_set_file_shape(
        &self,
        chunk_id: &str,
        file_shape_hash: &str,
        entity_id_list: &str,
    ) -> Result<()> {
        chunk_store::set_file_shape(&db!(self), chunk_id, file_shape_hash, entity_id_list)
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

    fn entity_list_by_sha(&self, corpus_id: &str, sha: &str) -> Result<Vec<Entity>> {
        let guard = db!(self);
        let conn = guard.conn();
        // entities_history lacks abstract_kind; substitute '' so row_to_entity's
        // column offsets stay consistent with the head-table query.
        let mut stmt = conn.prepare(
            "SELECT id, corpus_id, canonical_name, kind, abstract_kind,
                    aliases, description,
                    first_location_uri, last_location_uri,
                    appearance_count, confidence, derived_at_kind, derived_at_sha
             FROM entities
             WHERE corpus_id = ?1 AND derived_at_sha = ?2
             UNION ALL
             SELECT id, corpus_id, canonical_name, kind, '' AS abstract_kind,
                    aliases, description,
                    first_location_uri, last_location_uri,
                    appearance_count, confidence, derived_at_kind, derived_at_sha
             FROM entities_history
             WHERE corpus_id = ?1 AND derived_at_sha = ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![corpus_id, sha],
            entity_store::row_to_entity,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn entity_count_by_sha(&self, corpus_id: &str, sha: &str) -> Result<u64> {
        let guard = db!(self);
        let n: i64 = guard.conn().query_row(
            "SELECT COUNT(*) FROM (
               SELECT id FROM entities
                 WHERE corpus_id = ?1 AND derived_at_sha = ?2
               UNION ALL
               SELECT id FROM entities_history
                 WHERE corpus_id = ?1 AND derived_at_sha = ?2
             )",
            rusqlite::params![corpus_id, sha],
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

    fn theme_delete(&self, theme_id: &str, corpus_id: &str) -> Result<()> {
        theme_store::delete_one(&db!(self), theme_id, corpus_id)
    }

    // ── History / Archive ─────────────────────────────────────────────────────

    fn archive_entity(
        &self,
        entity_id: &str,
        corpus_id: &str,
        superseded_at_sha: &str,
    ) -> Result<bool> {
        let guard = db!(self);
        history::archive_entity(guard.conn(), entity_id, corpus_id, superseded_at_sha)
    }

    fn archive_edges_for_entity(
        &self,
        entity_id: &str,
        superseded_at_sha: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_edges_for_entity(guard.conn(), entity_id, superseded_at_sha)
    }

    fn archive_purposes_for_entity(
        &self,
        entity_id: &str,
        superseded_at_sha: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_purposes_for_entity(guard.conn(), entity_id, superseded_at_sha)
    }

    fn archive_contracts_for_entity(
        &self,
        entity_id: &str,
        superseded_at_sha: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_contracts_for_entity(guard.conn(), entity_id, superseded_at_sha)
    }

    fn archive_blocks_for_entity(
        &self,
        entity_id: &str,
        superseded_at_sha: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_blocks_for_entity(guard.conn(), entity_id, superseded_at_sha)
    }

    fn archive_summaries_for_target(
        &self,
        corpus_id: &str,
        target_id: &str,
        superseded_at_sha: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        history::archive_summaries_for_target(
            guard.conn(),
            corpus_id,
            target_id,
            superseded_at_sha,
        )
    }

    fn archive_chunk(&self, chunk_id: &str, superseded_at_sha: &str) -> Result<bool> {
        let guard = db!(self);
        history::archive_chunk(guard.conn(), chunk_id, superseded_at_sha)
    }

    fn archive_theme(
        &self,
        theme_id: &str,
        corpus_id: &str,
        superseded_at_sha: &str,
    ) -> Result<bool> {
        let guard = db!(self);
        history::archive_theme(guard.conn(), theme_id, corpus_id, superseded_at_sha)
    }

    fn archive_themes_for_corpus(
        &self,
        corpus_id: &str,
        superseded_at_sha: &str,
    ) -> Result<u64> {
        let guard = db!(self);
        let conn = guard.conn();
        let now = chrono::Utc::now().to_rfc3339();
        let rows = conn.execute(
            "INSERT OR IGNORE INTO themes_history
               (id, corpus_id, title, statement, confidence,
                model, model_tier, generated_at,
                derived_at_kind, derived_at_sha,
                superseded_at_sha, superseded_at)
             SELECT id, corpus_id, title, statement, confidence,
                    model, model_tier, generated_at,
                    derived_at_kind, derived_at_sha,
                    ?2, ?3
             FROM themes WHERE corpus_id = ?1",
            rusqlite::params![corpus_id, superseded_at_sha, now],
        )?;
        Ok(rows as u64)
    }

    fn cascade_delete_dirty_subtree(
        &self,
        corpus_id: &str,
        dirty_chunk_ids: &[String],
        superseded_at_sha: &str,
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
                        history::archive_edges_for_entity(tx, entity_id, superseded_at_sha)?;
                        history::archive_purposes_for_entity(tx, entity_id, superseded_at_sha)?;
                        history::archive_contracts_for_entity(
                            tx,
                            entity_id,
                            superseded_at_sha,
                        )?;
                        history::archive_blocks_for_entity(tx, entity_id, superseded_at_sha)?;
                        history::archive_summaries_for_target(
                            tx,
                            corpus_id,
                            entity_id,
                            superseded_at_sha,
                        )?;
                        history::archive_entity(tx, entity_id, corpus_id, superseded_at_sha)?;

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
                    superseded_at_sha,
                )?;
                // Archive this chunk's embeddings before the FK cascade wipes
                // them, so they survive in embeddings_history with honest
                // supersession provenance (closes embeddings-no-history-archival).
                embedding_store::archive_for_chunk(tx, chunk_id, superseded_at_sha)?;
                history::archive_chunk(tx, chunk_id, superseded_at_sha)?;

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

    // ── Honest provenance (migration 013) ──────────────────────────────────────

    fn entity_list_at_sha(
        &self,
        corpus_id: &str,
        target_sha: &str,
        ancestry: Option<&dyn AncestryReader>,
    ) -> Result<Vec<Entity>> {
        let guard = db!(self);
        let conn = guard.conn();

        // Gather every (id, provenance) the corpus has ever carried for an
        // entity — head rows plus archived rows — then keep an id when ANY of
        // its provenance tags is valid at the target and it is not tombstoned
        // ancestrally. The head row's data is preferred for the returned entity;
        // otherwise a valid archived row is materialised.
        let mut stmt = conn.prepare(
            "SELECT id, corpus_id, canonical_name, kind, abstract_kind,
                    aliases, description,
                    first_location_uri, last_location_uri,
                    appearance_count, confidence,
                    derived_at_kind, derived_at_sha,
                    1 AS is_head
             FROM entities
             WHERE corpus_id = ?1
             UNION ALL
             SELECT id, corpus_id, canonical_name, kind, '' AS abstract_kind,
                    aliases, description,
                    first_location_uri, last_location_uri,
                    appearance_count, confidence,
                    derived_at_kind, derived_at_sha,
                    0 AS is_head
             FROM entities_history
             WHERE corpus_id = ?1",
        )?;

        // Each row → (Entity, provenance, is_head). row_to_entity reads the
        // first 13 columns; is_head is column 13.
        let rows = stmt.query_map(rusqlite::params![corpus_id], |row| {
            let entity = entity_store::row_to_entity(row)?;
            let kind: String = row.get(11)?;
            let sha: String = row.get(12)?;
            let is_head: i64 = row.get(13)?;
            Ok((entity, kind, sha, is_head != 0))
        })?;

        // id → (chosen entity, chose_head). Prefer a head row's representation;
        // among archived rows the first valid one wins (PR-4 fixtures have a
        // single derivation per id — full multi-version reconstruction is
        // deferred).
        use std::collections::HashMap;
        let mut chosen: HashMap<String, (Entity, bool)> = HashMap::new();
        for row in rows {
            let (entity, kind, sha, is_head) = row?;
            let provenance = Provenance::from_columns(&kind, &sha)?;
            if !provenance.is_valid_at(target_sha, |a, b| {
                ancestry::is_ancestor_or_equal(ancestry, a, b)
            }) {
                continue;
            }
            match chosen.get(&entity.id) {
                // A head row always wins over an archived one.
                Some((_, true)) => {}
                _ => {
                    chosen.insert(entity.id.clone(), (entity, is_head));
                }
            }
        }

        // Drop ids whose tombstone is ancestral to the target.
        let mut out = Vec::new();
        for (id, (entity, _)) in chosen {
            if Self::is_tombstoned_at_conn(conn, corpus_id, "entity", &id, target_sha, ancestry)? {
                continue;
            }
            out.push(entity);
        }
        Ok(out)
    }

    fn is_tombstoned_at(
        &self,
        corpus_id: &str,
        artifact_kind: &str,
        artifact_id: &str,
        target_sha: &str,
        ancestry: Option<&dyn AncestryReader>,
    ) -> Result<bool> {
        let guard = db!(self);
        Self::is_tombstoned_at_conn(
            guard.conn(),
            corpus_id,
            artifact_kind,
            artifact_id,
            target_sha,
            ancestry,
        )
    }

    fn commit_embedding(&self, embedding: &StoredEmbedding, provenance: &Provenance) -> Result<()> {
        embedding_store::commit(&db!(self), embedding, provenance)
    }

    fn migrate_fresh(&self, corpus_id: &str) -> Result<MigrateFreshStats> {
        self.with_write_tx(|tx| {
            let mut stats = MigrateFreshStats::default();

            // Head tables. Order children-before-parents is unnecessary (each
            // DELETE is scoped by corpus_id and FK cascades cover the rest), but
            // we delete every table explicitly so the wipe is exhaustive and
            // self-documenting rather than relying on cascade fan-out.
            const HEAD_TABLES: &[&str] = &[
                "embeddings",
                "edges",
                "entity_purposes",
                "entity_contracts",
                "entity_blocks",
                "summaries",
                "themes",
                "entities",
                "chunks",
            ];
            for table in HEAD_TABLES {
                stats.head_rows_deleted += tx.execute(
                    &format!("DELETE FROM {table} WHERE corpus_id = ?1"),
                    rusqlite::params![corpus_id],
                )? as u64;
            }

            // History tables.
            const HISTORY_TABLES: &[&str] = &[
                "entities_history",
                "edges_history",
                "entity_purposes_history",
                "entity_contracts_history",
                "entity_blocks_history",
                "summaries_history",
                "themes_history",
                "chunks_history",
                "embeddings_history",
            ];
            for table in HISTORY_TABLES {
                stats.history_rows_deleted += tx.execute(
                    &format!("DELETE FROM {table} WHERE corpus_id = ?1"),
                    rusqlite::params![corpus_id],
                )? as u64;
            }

            // Tombstones for this corpus.
            stats.tombstones_deleted += tx.execute(
                "DELETE FROM artifact_tombstones WHERE corpus_id = ?1",
                rusqlite::params![corpus_id],
            )? as u64;

            // Layer-2 cache is content-addressed and carries no corpus_id, so it
            // cannot be scoped to one corpus. Clearing it entirely is safe — it
            // is a memoisation cache; the next index rebuilds it — and matches
            // the from-scratch intent of migrate-fresh.
            stats.layer2_cache_deleted += tx.execute("DELETE FROM layer2_cache", [])? as u64;

            // Preserve the corpora row; reset the rebuild anchors.
            tx.execute(
                "UPDATE corpora SET backfill_cursor = NULL, last_indexed_version = NULL
                 WHERE id = ?1",
                rusqlite::params![corpus_id],
            )?;

            Ok(stats)
        })
    }

    fn archive_to_history(
        &self,
        corpus_id: &str,
        set: &ArchiveSet,
        provenance: &Provenance,
    ) -> Result<ArchiveStats> {
        // Naive facade: fan out to the existing per-artifact archive helpers,
        // using the provenance SHA as the supersession version.
        let sha = provenance.sha();
        let mut stats = ArchiveStats::default();
        for entity_id in &set.entity_ids {
            if self.archive_entity(entity_id, corpus_id, sha)? {
                stats.entities_archived += 1;
            }
            stats.edges_archived += self.archive_edges_for_entity(entity_id, sha)?;
            stats.purposes_archived += self.archive_purposes_for_entity(entity_id, sha)?;
            stats.contracts_archived += self.archive_contracts_for_entity(entity_id, sha)?;
            stats.blocks_archived += self.archive_blocks_for_entity(entity_id, sha)?;
        }
        for chunk_id in &set.chunk_ids {
            if self.archive_chunk(chunk_id, sha)? {
                stats.chunks_archived += 1;
            }
        }
        for target_id in &set.summary_target_ids {
            stats.summaries_archived +=
                self.archive_summaries_for_target(corpus_id, target_id, sha)?;
        }
        for theme_id in &set.theme_ids {
            if self.archive_theme(theme_id, corpus_id, sha)? {
                stats.themes_archived += 1;
            }
        }
        Ok(stats)
    }

    fn refine_provenance(
        &self,
        corpus_id: &str,
        artifact_kind: &str,
        artifact_id: &str,
        observed: &Provenance,
    ) -> Result<RefineOutcome> {
        // Map the artifact kind to the id-keyed head table it lives in. Only the
        // Layer-1 artifacts (entity / chunk / edge) carry a single-row provenance
        // tag refinable by id; Layer-2 artifacts (purpose/contract/…) are keyed
        // by (entity_id, model) and are refined through the Layer-2 cache (PR 3),
        // so they are treated as Unchanged here.
        let table = match artifact_kind {
            "entity" => "entities",
            "chunk" => "chunks",
            "edge" => "edges",
            "theme" => "themes",
            _ => return Ok(RefineOutcome::Unchanged),
        };

        let guard = db!(self);
        let conn = guard.conn();

        // Read the current tag.
        let current: Option<(String, String)> = conn
            .query_row(
                &format!(
                    "SELECT derived_at_kind, derived_at_sha FROM {table}
                     WHERE id = ?1 AND corpus_id = ?2"
                ),
                rusqlite::params![artifact_id, corpus_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;

        let Some((kind, sha)) = current else {
            return Ok(RefineOutcome::Unchanged); // no head row to refine
        };
        let current = Provenance::from_columns(&kind, &sha)?;

        // Monotonicity (Q2): Concrete is maximally specific and never widened by
        // a RangePredating observation. A RangePredating upper bound only narrows.
        match (&current, observed) {
            // Already concrete — observing anything cannot make it more specific.
            (Provenance::Concrete(_), _) => Ok(RefineOutcome::Unchanged),
            // Range → Concrete: a proven substrate touch. Tighten.
            (Provenance::RangePredating(_), Provenance::Concrete(_)) => {
                let (k, s) = observed.to_columns();
                conn.execute(
                    &format!(
                        "UPDATE {table} SET derived_at_kind = ?1, derived_at_sha = ?2
                         WHERE id = ?3 AND corpus_id = ?4"
                    ),
                    rusqlite::params![k, s, artifact_id, corpus_id],
                )?;
                Ok(RefineOutcome::Refined)
            }
            // Range → Range: narrowing the one-sided upper bound is only honest
            // when the observation is strictly older. We cannot prove ancestry
            // here, so accept a *different* observed SHA as a narrowing (the
            // walker only ever calls this with an ancestor SHA — its contract)
            // and reject an identical one as Unchanged.
            (Provenance::RangePredating(cur), Provenance::RangePredating(obs)) => {
                if cur == obs {
                    Ok(RefineOutcome::Unchanged)
                } else {
                    let (k, s) = observed.to_columns();
                    conn.execute(
                        &format!(
                            "UPDATE {table} SET derived_at_kind = ?1, derived_at_sha = ?2
                             WHERE id = ?3 AND corpus_id = ?4"
                        ),
                        rusqlite::params![k, s, artifact_id, corpus_id],
                    )?;
                    Ok(RefineOutcome::Refined)
                }
            }
        }
    }

    fn tombstone_insert(
        &self,
        corpus_id: &str,
        artifact_kind: &str,
        artifact_id: &str,
        provenance: &Provenance,
        reason: Option<&str>,
    ) -> Result<()> {
        let guard = db!(self);
        let (kind, sha) = provenance.to_columns();
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT OR IGNORE INTO artifact_tombstones
             (corpus_id, artifact_kind, artifact_id, derived_at_kind, derived_at_sha, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![corpus_id, artifact_kind, artifact_id, kind, sha, reason, now],
        )?;
        Ok(())
    }

    fn tombstone_list(
        &self,
        corpus_id: &str,
        artifact_kind: &str,
        artifact_id: &str,
    ) -> Result<Vec<Tombstone>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT corpus_id, artifact_kind, artifact_id, derived_at_kind, derived_at_sha,
                    reason, created_at
             FROM artifact_tombstones
             WHERE corpus_id = ?1 AND artifact_kind = ?2 AND artifact_id = ?3
             ORDER BY created_at DESC, tombstone_id DESC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![corpus_id, artifact_kind, artifact_id],
            |row| {
                let kind: String = row.get(3)?;
                let sha: String = row.get(4)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    kind,
                    sha,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (corpus_id, artifact_kind, artifact_id, kind, sha, reason, created_at) = row?;
            out.push(Tombstone {
                corpus_id,
                artifact_kind,
                artifact_id,
                provenance: Provenance::from_columns(&kind, &sha)?,
                reason,
                created_at,
            });
        }
        Ok(out)
    }

    fn layer2_cache_get(&self, key: &Layer2CacheKey) -> Result<Option<CachedArtifact>> {
        let guard = db!(self);
        let cache_key = key.cache_key();
        let row = guard
            .conn()
            .query_row(
                "SELECT cache_key, artifact_kind, entity_id, content_hash, file_shape_hash,
                        model, stable_sampling, payload, created_at, first_seen_at_sha, hit_count
                 FROM layer2_cache WHERE cache_key = ?1",
                rusqlite::params![cache_key],
                |row| {
                    Ok(CachedArtifact {
                        cache_key: row.get(0)?,
                        artifact_kind: row.get(1)?,
                        entity_id: row.get(2)?,
                        content_hash: row.get(3)?,
                        file_shape_hash: row.get(4)?,
                        model: row.get(5)?,
                        stable_sampling: row.get::<_, i64>(6)? != 0,
                        payload: row.get(7)?,
                        created_at: row.get(8)?,
                        first_seen_at_sha: row.get(9)?,
                        hit_count: row.get(10)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    fn layer2_cache_put(
        &self,
        key: &Layer2CacheKey,
        payload: &str,
        first_seen_at_sha: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let cache_key = key.cache_key();
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT INTO layer2_cache
             (cache_key, artifact_kind, entity_id, content_hash, file_shape_hash, model,
              stable_sampling, payload, created_at, first_seen_at_sha, hit_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0)
             ON CONFLICT(cache_key) DO UPDATE SET
                 artifact_kind   = excluded.artifact_kind,
                 entity_id       = excluded.entity_id,
                 content_hash    = excluded.content_hash,
                 file_shape_hash = excluded.file_shape_hash,
                 model           = excluded.model,
                 stable_sampling = excluded.stable_sampling,
                 payload         = excluded.payload",
            rusqlite::params![
                cache_key,
                key.artifact_kind,
                key.entity_id,
                key.content_hash,
                key.file_shape_hash,
                key.model,
                key.stable_sampling as i64,
                payload,
                now,
                first_seen_at_sha,
            ],
        )?;
        Ok(())
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
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT OR IGNORE INTO chunks_history
             (id, corpus_id, parent_path, kind, location_uri, content,
              byte_length, created_at, semantic_processed, source_hash,
              introduced_at_version, last_modified_at_version,
              last_modified_commit_message, last_modified_author,
              derived_at_kind, derived_at_sha,
              superseded_at_sha, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,0,?9,?10,?11,NULL,NULL,'concrete',?10,?12,?13)",
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
                derived_at_sha,
                derived_at_sha,
                superseded_at_sha,
                now,
            ],
        )?;
        Ok(())
    }

    fn chunk_history_update_source_hash(
        &self,
        chunk_id: &str,
        derived_at_sha: &str,
        source_hash: &str,
    ) -> Result<()> {
        let guard = db!(self);
        guard.conn().execute(
            "UPDATE chunks_history SET source_hash = ?1
             WHERE id = ?2 AND introduced_at_version = ?3",
            rusqlite::params![source_hash, chunk_id, derived_at_sha],
        )?;
        Ok(())
    }

    fn chunk_history_update_version(
        &self,
        chunk_id: &str,
        derived_at_sha: &str,
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
                derived_at_sha,
            ],
        )?;
        Ok(())
    }

    fn entity_history_insert(
        &self,
        entity: &Entity,
        derived_at_sha: &str,
        superseded_at_sha: &str,
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
              derived_at_kind, derived_at_sha,
              superseded_at_sha, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'concrete',?11,?12,?13)",
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
                derived_at_sha,
                superseded_at_sha,
                now,
            ],
        )?;
        Ok(())
    }

    fn edge_history_insert(
        &self,
        edge: &Edge,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        // No FK guard: history tables have no foreign key constraints.
        guard.conn().execute(
            "INSERT OR IGNORE INTO edges_history
             (id, corpus_id, from_entity_id, to_entity_id, kind,
              location_uri, confidence,
              derived_at_kind, derived_at_sha,
              superseded_at_sha, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'concrete',?8,?9,?10)",
            rusqlite::params![
                edge.id,
                edge.corpus_id,
                edge.from_entity_id,
                edge.to_entity_id,
                edge.kind,
                edge.location.uri,
                edge.confidence as f64,
                derived_at_sha,
                superseded_at_sha,
                now,
            ],
        )?;
        Ok(())
    }

    fn summary_history_insert(
        &self,
        summary: &Summary,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        let target_kind_str = summary.target_kind.to_string();
        guard.conn().execute(
            "INSERT OR IGNORE INTO summaries_history
             (id, corpus_id, target_kind, target_id, depth, text,
              model, model_tier, generated_at,
              derived_at_kind, derived_at_sha,
              superseded_at_sha, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'concrete',?10,?11,?12)",
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
                derived_at_sha,
                superseded_at_sha,
                now,
            ],
        )?;
        Ok(())
    }

    fn purpose_history_insert(
        &self,
        purpose: &EntityPurpose,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT OR IGNORE INTO entity_purposes_history
             (entity_id, corpus_id, purpose, model, model_tier, generated_at,
              derived_at_kind, derived_at_sha,
              superseded_at_sha, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,'concrete',?7,?8,?9)",
            rusqlite::params![
                purpose.entity_id,
                purpose.corpus_id,
                purpose.purpose,
                purpose.model,
                purpose.model_tier,
                purpose.generated_at,
                derived_at_sha,
                superseded_at_sha,
                now,
            ],
        )?;
        Ok(())
    }

    fn contract_history_insert(
        &self,
        contract: &EntityContract,
        derived_at_sha: &str,
        superseded_at_sha: &str,
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
              derived_at_kind, derived_at_sha,
              superseded_at_sha, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,'concrete',?22,?23,?24)",
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
                derived_at_sha,
                superseded_at_sha,
                now,
            ],
        )?;
        Ok(())
    }

    fn block_history_insert(
        &self,
        block: &EntityBlock,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT OR IGNORE INTO entity_blocks_history
             (id, entity_id, corpus_id, label, description, position,
              derived_at_kind, derived_at_sha,
              superseded_at_sha, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,'concrete',?7,?8,?9)",
            rusqlite::params![
                block.id,
                block.entity_id,
                block.corpus_id,
                block.label,
                block.description,
                block.position,
                derived_at_sha,
                superseded_at_sha,
                now,
            ],
        )?;
        Ok(())
    }

    fn theme_history_insert(
        &self,
        theme: &Theme,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        let guard = db!(self);
        let now = chrono::Utc::now().to_rfc3339();
        guard.conn().execute(
            "INSERT OR IGNORE INTO themes_history
             (id, corpus_id, title, statement, confidence,
              model, model_tier, generated_at,
              derived_at_kind, derived_at_sha,
              superseded_at_sha, superseded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'concrete',?9,?10,?11)",
            rusqlite::params![
                theme.id,
                theme.corpus_id,
                theme.title,
                theme.statement,
                theme.confidence as f64,
                theme.model,
                theme.model_tier,
                theme.generated_at,
                derived_at_sha,
                superseded_at_sha,
                now,
            ],
        )?;
        Ok(())
    }

    // ── Backfill seeding helpers ──────────────────────────────────────────────

    fn entity_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT id, derived_at_sha FROM entities WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn chunk_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT id, derived_at_sha FROM chunks WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn edge_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT id, derived_at_sha FROM edges WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn summary_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT target_id, derived_at_sha FROM summaries WHERE corpus_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn purpose_head_shas(&self, corpus_id: &str) -> Result<Vec<((String, String), String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT entity_id, model, derived_at_sha FROM entity_purposes WHERE corpus_id = ?1",
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

    fn contract_head_shas(&self, corpus_id: &str) -> Result<Vec<((String, String), String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT entity_id, model, derived_at_sha FROM entity_contracts WHERE corpus_id = ?1",
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

    fn block_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        // Return one entry per entity_id (max derived_at_sha across blocks).
        let mut stmt = guard.conn().prepare(
            "SELECT entity_id, MAX(derived_at_sha)
             FROM entity_blocks WHERE corpus_id = ?1 GROUP BY entity_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![corpus_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::CalError::from)
    }

    fn theme_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        let guard = db!(self);
        let mut stmt = guard.conn().prepare(
            "SELECT id, derived_at_sha FROM themes WHERE corpus_id = ?1",
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
                         SELECT superseded_at_sha AS sha, MAX(superseded_at) AS ts
                           FROM entities_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_sha
                         UNION ALL
                         SELECT superseded_at_sha, MAX(superseded_at)
                           FROM edges_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_sha
                         UNION ALL
                         SELECT superseded_at_sha, MAX(superseded_at)
                           FROM entity_purposes_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_sha
                         UNION ALL
                         SELECT superseded_at_sha, MAX(superseded_at)
                           FROM entity_contracts_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_sha
                         UNION ALL
                         SELECT superseded_at_sha, MAX(superseded_at)
                           FROM entity_blocks_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_sha
                         UNION ALL
                         SELECT superseded_at_sha, MAX(superseded_at)
                           FROM summaries_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_sha
                         UNION ALL
                         SELECT superseded_at_sha, MAX(superseded_at)
                           FROM chunks_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_sha
                         UNION ALL
                         SELECT superseded_at_sha, MAX(superseded_at)
                           FROM themes_history
                          WHERE corpus_id = ?1
                          GROUP BY superseded_at_sha
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
                           AND superseded_at_sha IN ({placeholders})"
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
                               AND superseded_at_sha IN ({placeholders})"
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

    // ── PR 4: embeddings history + tombstone-aware reads + migrate-fresh ────────

    use crate::types::{Chunk, Entity, Layer2CacheKey, Location};

    /// Fake ancestry over a linear chain `git:C1 → git:C2 → git:C3`:
    /// `is_ancestor_or_equal(a, b)` iff `rank(a) <= rank(b)`.
    struct LinearAncestry;
    impl AncestryReader for LinearAncestry {
        fn is_ancestor_or_equal(&self, ancestor: &str, descendant: &str) -> bool {
            let rank = |s: &str| match s {
                "git:C1" => 1,
                "git:C2" => 2,
                "git:C3" => 3,
                _ => 0,
            };
            rank(ancestor) <= rank(descendant)
        }
    }

    fn seed_corpus(backend: &SqliteBackend, id: &str) -> Corpus {
        let corpus = Corpus::new(id.into(), "Test".into(), "code".into(), "/tmp".into());
        backend.corpus_insert(&corpus).unwrap();
        corpus
    }

    /// When a chunk is superseded (content change → the cascade sweeps it), its
    /// embedding is archived into `embeddings_history` with honest supersession
    /// provenance, while the live chunk's head embedding carries the new vector.
    #[test]
    fn embedding_archived_to_history_on_chunk_supersession() {
        let backend = make_backend();
        seed_corpus(&backend, "c1");

        // C1: a chunk and its embedding, committed at git:C1.
        let chunk1 = Chunk::new(
            "c1".into(),
            None,
            "file".into(),
            Location::new("c1", "a.txt"),
            "content v1".into(),
        );
        backend.chunk_upsert(&chunk1).unwrap();
        let emb1 = StoredEmbedding::new("c1", &chunk1.id, "test-model", vec![1.0, 0.0, 0.0]);
        backend
            .commit_embedding(&emb1, &Provenance::concrete("git:C1"))
            .unwrap();

        // C2 supersedes a.txt: the cascade archives + deletes the chunk (and,
        // by FK, its head embedding — archived first).
        backend
            .cascade_delete_dirty_subtree("c1", std::slice::from_ref(&chunk1.id), "git:C2")
            .unwrap();
        assert!(
            backend
                .embedding_get_for_chunk(&chunk1.id)
                .unwrap()
                .is_none(),
            "the superseded chunk's head embedding must be gone"
        );

        // C2: new content → new (content-addressed) chunk + embedding.
        let chunk2 = Chunk::new(
            "c1".into(),
            None,
            "file".into(),
            Location::new("c1", "a.txt"),
            "content v2".into(),
        );
        backend.chunk_upsert(&chunk2).unwrap();
        let emb2 = StoredEmbedding::new("c1", &chunk2.id, "test-model", vec![0.0, 1.0, 0.0]);
        backend
            .commit_embedding(&emb2, &Provenance::concrete("git:C2"))
            .unwrap();

        // Head embedding for the live chunk carries the C2 vector.
        let head = backend
            .embedding_get_for_chunk(&chunk2.id)
            .unwrap()
            .unwrap();
        assert!(
            (head.vector[1] - 1.0).abs() < 1e-6,
            "head embedding must be the C2 vector"
        );

        // embeddings_history holds the C1 vector: derived at C1, superseded at C2.
        let guard = backend.db_for_test();
        let (derived, superseded, blob): (String, String, Vec<u8>) = guard
            .conn()
            .query_row(
                "SELECT derived_at_sha, superseded_at_sha, vector
                 FROM embeddings_history WHERE chunk_id = ?1",
                rusqlite::params![chunk1.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("embeddings_history must contain the superseded C1 embedding");
        assert_eq!(derived, "git:C1", "archived embedding derived at C1");
        assert_eq!(superseded, "git:C2", "archived embedding superseded at C2");
        let archived_vec: &[f32] = bytemuck::cast_slice(&blob);
        assert!(
            (archived_vec[0] - 1.0).abs() < 1e-6,
            "archived vector must be the C1 vector"
        );
    }

    /// `entity_list_at_sha` excludes an entity once its tombstone is ancestral to
    /// the query SHA: present at the death commit's parent, absent at the death
    /// commit and every descendant.
    #[test]
    fn entity_list_at_sha_excludes_ancestrally_tombstoned() {
        let backend = make_backend();
        seed_corpus(&backend, "c1");

        // Entity derived at C1 — provenance carries the honest SHA.
        let mut e = Entity::new(
            "ent-1".into(),
            "c1".into(),
            "Alpha".into(),
            "function".into(),
        );
        e.provenance = Some(Provenance::concrete("git:C1"));
        backend.entity_upsert(&e).unwrap();

        // It dies at C2.
        backend
            .tombstone_insert(
                "c1",
                "entity",
                "ent-1",
                &Provenance::concrete("git:C2"),
                Some("removed"),
            )
            .unwrap();

        let anc = LinearAncestry;
        let anc: Option<&dyn AncestryReader> = Some(&anc);

        let present = |sha: &str| {
            backend
                .entity_list_at_sha("c1", sha, anc)
                .unwrap()
                .iter()
                .any(|x| x.id == "ent-1")
        };

        assert!(present("git:C1"), "entity present at C1 (before its death)");
        assert!(!present("git:C2"), "entity absent at C2 (its death commit)");
        assert!(
            !present("git:C3"),
            "entity absent at C3 (tombstone is ancestral)"
        );
    }

    /// `migrate_fresh` wipes every head row, every history row, all tombstones,
    /// and the Layer-2 cache for a corpus, resets the rebuild anchors, and
    /// preserves the `corpora` registration row.
    #[test]
    fn migrate_fresh_wipes_corpus_but_preserves_registration() {
        let backend = make_backend();
        seed_corpus(&backend, "c1");

        // Head rows: chunk, entity, embedding.
        let chunk = Chunk::new(
            "c1".into(),
            None,
            "file".into(),
            Location::new("c1", "a.txt"),
            "v1".into(),
        );
        backend.chunk_upsert(&chunk).unwrap();
        let mut e = Entity::new(
            "ent-1".into(),
            "c1".into(),
            "Alpha".into(),
            "function".into(),
        );
        e.provenance = Some(Provenance::concrete("git:C1"));
        backend.entity_upsert(&e).unwrap();
        let emb = StoredEmbedding::new("c1", &chunk.id, "m", vec![1.0, 2.0]);
        backend
            .commit_embedding(&emb, &Provenance::concrete("git:C1"))
            .unwrap();

        // A history row, a tombstone, a cache entry, and the rebuild anchors.
        backend
            .chunk_history_insert(&chunk, "git:C0", "git:C1")
            .unwrap();
        backend
            .tombstone_insert(
                "c1",
                "entity",
                "dead",
                &Provenance::concrete("git:C2"),
                None,
            )
            .unwrap();
        let key = Layer2CacheKey {
            artifact_kind: "purpose".into(),
            entity_id: Some("e".into()),
            content_hash: "c".into(),
            file_shape_hash: "f".into(),
            model: "m".into(),
            stable_sampling: false,
        };
        backend.layer2_cache_put(&key, "{}", "git:C1").unwrap();
        backend
            .corpus_set_backfill_cursor("c1", Some("git:C1"))
            .unwrap();
        backend
            .corpus_set_last_indexed_version("c1", "git:C1")
            .unwrap();

        let stats = backend.migrate_fresh("c1").unwrap();
        assert!(stats.head_rows_deleted > 0, "head rows must be wiped");
        assert!(stats.history_rows_deleted > 0, "history rows must be wiped");

        // Head tables empty.
        assert_eq!(backend.chunk_count("c1").unwrap(), 0);
        assert_eq!(backend.entity_count("c1").unwrap(), 0);
        assert_eq!(backend.embedding_count("c1").unwrap(), 0);

        // Tombstones + cache gone.
        assert!(
            backend
                .tombstone_list("c1", "entity", "dead")
                .unwrap()
                .is_empty(),
            "tombstones must be wiped"
        );
        assert!(
            backend.layer2_cache_get(&key).unwrap().is_none(),
            "layer-2 cache must be cleared"
        );

        // Corpus registration preserved; rebuild anchors reset to NULL.
        assert!(
            backend.corpus_get("c1").unwrap().is_some(),
            "corpora row must be preserved"
        );
        assert_eq!(backend.corpus_get_backfill_cursor("c1").unwrap(), None);
        assert_eq!(backend.corpus_get_last_indexed_version("c1").unwrap(), None);

        // Every *_history table empty for this corpus.
        let guard = backend.db_for_test();
        for t in [
            "entities_history",
            "edges_history",
            "chunks_history",
            "summaries_history",
            "themes_history",
            "entity_purposes_history",
            "entity_contracts_history",
            "entity_blocks_history",
            "embeddings_history",
        ] {
            let n: i64 = guard
                .conn()
                .query_row(
                    &format!("SELECT COUNT(*) FROM {t} WHERE corpus_id = 'c1'"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "{t} must be empty after migrate_fresh");
        }
    }
}
