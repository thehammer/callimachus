//! Backward-backfill storage infrastructure.
//!
//! Two types live here:
//!
//! ## [`BackfillSupersession`]
//!
//! A per-walk resolver that answers "what is the `superseded_at_sha` for
//! artifact X at the commit currently being filled?"
//!
//! Seeded from the HEAD tables before the walk begins, then updated after each
//! history row is written so that the next (older) iteration sees the freshly-
//! written version as the next-newer anchor.  Iteration is newest-older →
//! oldest-older, so the chain builds itself naturally without any UPDATE of
//! previously written rows.
//!
//! ## [`BackfillStorageWrapper`]
//!
//! A [`StorageBackend`] wrapper that intercepts all artifact-write methods and
//! routes them to the corresponding `*_history` tables, leaving every head table
//! untouched.  Read paths (other than artifact reads that cross passes) are
//! served from an in-memory buffer that is populated as history rows are written,
//! so downstream passes (structure, semantic, …) can read what the chunk pass
//! wrote even though nothing landed in the head tables.
//!
//! [`walk_history_backward`]: crate::indexing::history_walk::walk_history_backward

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::corrections::types::{Correction, CorrectionKind};
use crate::error::{CalError, Result};
use crate::storage::backend::{CascadeStats, StorageBackend};
use crate::storage::edge_store::EdgeDirection;
use crate::storage::embedding_store::StoredEmbedding;
use crate::storage::fts::FtsResult;
use crate::storage::pruning::PruneStats;
use crate::storage::run_log::{PassStats, RunRecord};
use crate::types::pass::RunStatus;
use crate::types::provenance::{
    ArchiveSet, ArchiveStats, CachedArtifact, Layer2CacheKey, Provenance, RefineOutcome, Tombstone,
};
use crate::types::{
    Chunk, Collection, CollectionMember, Corpus, CorpusStatus, Edge, Entity, EntityBlock,
    EntityContract, EntityPurpose, Location, MemberType, Summary, SummaryTargetKind, Theme,
};

// ── BackfillSupersession ──────────────────────────────────────────────────────

/// Tracks `superseded_at_sha` for each artifact kind across a backward
/// backfill walk.
///
/// Seeded from the current HEAD tables so the first write for each artifact
/// correctly records the HEAD version as the supersession target.  After each
/// write the map is updated so older-commit iterations see the newly-written
/// version as their next-newer anchor.
///
/// The invariant: for commit C(k) being filled, the `superseded_at_sha` for
/// artifact A is either
/// (a) the HEAD version of A if A has not yet been backfilled, or
/// (b) the `derived_at_sha` of the most-recently-written backfill row for A.
pub struct BackfillSupersession {
    inner: Mutex<BackfillSupersessionInner>,
}

struct BackfillSupersessionInner {
    /// Version string for the commit currently being processed.
    current_walk_commit: String,
    /// entity_id → next-newer version
    entities: HashMap<String, String>,
    /// edge_id → next-newer version
    edges: HashMap<String, String>,
    /// chunk_id → next-newer version
    chunks: HashMap<String, String>,
    /// target_id → next-newer version
    summaries: HashMap<String, String>,
    /// (entity_id, model) → next-newer version
    purposes: HashMap<(String, String), String>,
    /// (entity_id, model) → next-newer version
    contracts: HashMap<(String, String), String>,
    /// entity_id → next-newer version
    blocks: HashMap<String, String>,
    /// theme_id → next-newer version
    themes: HashMap<String, String>,
}

impl BackfillSupersession {
    /// Seed the resolver from the current HEAD tables of `corpus_id`.
    ///
    /// Must be called once, before the first backfill iteration, with the
    /// real (non-wrapper) backend so that head rows are read.
    pub fn seeded_from(db: &dyn StorageBackend, corpus_id: &str) -> Result<Self> {
        let entities = db
            .entity_head_shas(corpus_id)?
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let chunks = db
            .chunk_head_shas(corpus_id)?
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let edges = db
            .edge_head_shas(corpus_id)?
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let summaries = db
            .summary_head_shas(corpus_id)?
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let purposes = db
            .purpose_head_shas(corpus_id)?
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let contracts = db
            .contract_head_shas(corpus_id)?
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let blocks = db
            .block_head_shas(corpus_id)?
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let themes = db
            .theme_head_shas(corpus_id)?
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();

        Ok(Self {
            inner: Mutex::new(BackfillSupersessionInner {
                current_walk_commit: String::new(),
                entities,
                chunks,
                edges,
                summaries,
                purposes,
                contracts,
                blocks,
                themes,
            }),
        })
    }

    /// Update the current-commit string at the start of each backfill iteration.
    pub fn set_current_commit(&self, oid_str: String) {
        self.inner.lock().unwrap().current_walk_commit = oid_str;
    }

    // ── Lookup helpers ────────────────────────────────────────────────────────

    pub fn superseded_for_entity(&self, entity_id: &str) -> String {
        let g = self.inner.lock().unwrap();
        g.entities
            .get(entity_id)
            .cloned()
            .unwrap_or_else(|| g.current_walk_commit.clone())
    }

    pub fn superseded_for_edge(&self, edge_id: &str) -> String {
        let g = self.inner.lock().unwrap();
        g.edges
            .get(edge_id)
            .cloned()
            .unwrap_or_else(|| g.current_walk_commit.clone())
    }

    pub fn superseded_for_chunk(&self, chunk_id: &str) -> String {
        let g = self.inner.lock().unwrap();
        g.chunks
            .get(chunk_id)
            .cloned()
            .unwrap_or_else(|| g.current_walk_commit.clone())
    }

    pub fn superseded_for_summary(&self, target_id: &str) -> String {
        let g = self.inner.lock().unwrap();
        g.summaries
            .get(target_id)
            .cloned()
            .unwrap_or_else(|| g.current_walk_commit.clone())
    }

    pub fn superseded_for_purpose(&self, entity_id: &str, model: &str) -> String {
        let g = self.inner.lock().unwrap();
        g.purposes
            .get(&(entity_id.to_string(), model.to_string()))
            .cloned()
            .unwrap_or_else(|| g.current_walk_commit.clone())
    }

    pub fn superseded_for_contract(&self, entity_id: &str, model: &str) -> String {
        let g = self.inner.lock().unwrap();
        g.contracts
            .get(&(entity_id.to_string(), model.to_string()))
            .cloned()
            .unwrap_or_else(|| g.current_walk_commit.clone())
    }

    pub fn superseded_for_block(&self, entity_id: &str) -> String {
        let g = self.inner.lock().unwrap();
        g.blocks
            .get(entity_id)
            .cloned()
            .unwrap_or_else(|| g.current_walk_commit.clone())
    }

    pub fn superseded_for_theme(&self, theme_id: &str) -> String {
        let g = self.inner.lock().unwrap();
        g.themes
            .get(theme_id)
            .cloned()
            .unwrap_or_else(|| g.current_walk_commit.clone())
    }

    // ── Record-write helpers ──────────────────────────────────────────────────
    // Called after each history INSERT so older-commit iterations see the
    // freshly-written version as the next-newer anchor.

    pub fn record_write_entity(&self, entity_id: &str, derived_at: &str) {
        self.inner
            .lock()
            .unwrap()
            .entities
            .insert(entity_id.to_string(), derived_at.to_string());
    }

    pub fn record_write_edge(&self, edge_id: &str, derived_at: &str) {
        self.inner
            .lock()
            .unwrap()
            .edges
            .insert(edge_id.to_string(), derived_at.to_string());
    }

    pub fn record_write_chunk(&self, chunk_id: &str, derived_at: &str) {
        self.inner
            .lock()
            .unwrap()
            .chunks
            .insert(chunk_id.to_string(), derived_at.to_string());
    }

    pub fn record_write_summary(&self, target_id: &str, derived_at: &str) {
        self.inner
            .lock()
            .unwrap()
            .summaries
            .insert(target_id.to_string(), derived_at.to_string());
    }

    pub fn record_write_purpose(&self, entity_id: &str, model: &str, derived_at: &str) {
        self.inner.lock().unwrap().purposes.insert(
            (entity_id.to_string(), model.to_string()),
            derived_at.to_string(),
        );
    }

    pub fn record_write_contract(&self, entity_id: &str, model: &str, derived_at: &str) {
        self.inner.lock().unwrap().contracts.insert(
            (entity_id.to_string(), model.to_string()),
            derived_at.to_string(),
        );
    }

    pub fn record_write_block(&self, entity_id: &str, derived_at: &str) {
        self.inner
            .lock()
            .unwrap()
            .blocks
            .insert(entity_id.to_string(), derived_at.to_string());
    }

    pub fn record_write_theme(&self, theme_id: &str, derived_at: &str) {
        self.inner
            .lock()
            .unwrap()
            .themes
            .insert(theme_id.to_string(), derived_at.to_string());
    }
}

// ── BackfillStorageWrapper ────────────────────────────────────────────────────

/// A [`StorageBackend`] wrapper used by [`walk_history_backward`].
///
/// All artifact-upsert methods write to `*_history` tables (via the inner
/// backend's `*_history_insert` helpers) and leave head tables untouched.
///
/// An in-memory buffer (`written_chunks`, `written_entities`) allows downstream
/// passes to read what the chunk/structure passes wrote, even though nothing
/// appeared in the real head tables.
///
/// All other reads delegate to the inner backend.
/// Corpus-write methods that would advance the head version anchor are NO-OPs.
///
/// [`walk_history_backward`]: crate::indexing::history_walk::walk_history_backward
pub struct BackfillStorageWrapper {
    inner: Arc<dyn StorageBackend>,
    derived_at_sha: String,
    supersession: Arc<BackfillSupersession>,
    /// Chunks written during this iteration (in-memory, keyed by chunk ID).
    written_chunks: Mutex<HashMap<String, Chunk>>,
    /// Entities written during this iteration (in-memory, keyed by entity ID).
    written_entities: Mutex<HashMap<String, Entity>>,
}

impl BackfillStorageWrapper {
    pub fn new(
        inner: Arc<dyn StorageBackend>,
        derived_at_sha: String,
        supersession: Arc<BackfillSupersession>,
    ) -> Self {
        Self {
            inner,
            derived_at_sha,
            supersession,
            written_chunks: Mutex::new(HashMap::new()),
            written_entities: Mutex::new(HashMap::new()),
        }
    }
}

// Helper: return a "not supported in backfill mode" error.
fn backfill_no_write(op: &str) -> CalError {
    CalError::Other(format!(
        "BackfillStorageWrapper: '{op}' must not touch head tables during backfill"
    ))
}

impl StorageBackend for BackfillStorageWrapper {
    // ── Corpus ────────────────────────────────────────────────────────────────
    // Reads delegate; version-advancing writes are NO-OPs.

    fn corpus_insert(&self, corpus: &Corpus) -> Result<()> {
        self.inner.corpus_insert(corpus)
    }
    fn corpus_list(&self) -> Result<Vec<Corpus>> {
        self.inner.corpus_list()
    }
    fn corpus_get(&self, id: &str) -> Result<Option<Corpus>> {
        self.inner.corpus_get(id)
    }
    fn corpus_require(&self, id: &str) -> Result<Corpus> {
        self.inner.corpus_require(id)
    }
    fn corpus_update_status(&self, _id: &str, _status: CorpusStatus) -> Result<()> {
        Ok(()) // NO-OP: don't mutate corpus status during backfill
    }
    fn corpus_set_last_indexed(&self, _id: &str, _at: &str) -> Result<()> {
        Ok(()) // NO-OP: head version anchor must remain stable
    }
    fn corpus_set_pipeline_version(&self, _id: &str, _version: u32) -> Result<()> {
        Ok(()) // NO-OP
    }
    fn corpus_set_last_indexed_version(&self, _id: &str, _version: &str) -> Result<()> {
        Ok(()) // NO-OP: head version anchor must remain stable
    }
    fn corpus_get_last_indexed_version(&self, id: &str) -> Result<Option<String>> {
        self.inner.corpus_get_last_indexed_version(id)
    }
    fn corpus_set_backfill_cursor(&self, id: &str, cursor: Option<&str>) -> Result<()> {
        // The cursor tracks backfill progress, not the head anchor — delegate.
        self.inner.corpus_set_backfill_cursor(id, cursor)
    }
    fn corpus_get_backfill_cursor(&self, id: &str) -> Result<Option<String>> {
        self.inner.corpus_get_backfill_cursor(id)
    }
    fn corpus_delete(&self, id: &str) -> Result<bool> {
        self.inner.corpus_delete(id)
    }
    fn corpus_exists(&self, id: &str) -> Result<bool> {
        self.inner.corpus_exists(id)
    }

    // ── Chunk ─────────────────────────────────────────────────────────────────
    // Writes → chunks_history; reads from in-memory buffer.

    fn chunk_upsert(&self, chunk: &Chunk) -> Result<()> {
        let superseded = self.supersession.superseded_for_chunk(&chunk.id);
        self.inner
            .chunk_history_insert(chunk, &self.derived_at_sha, &superseded)?;
        self.supersession
            .record_write_chunk(&chunk.id, &self.derived_at_sha);
        // Buffer for downstream pass reads.
        self.written_chunks
            .lock()
            .unwrap()
            .insert(chunk.id.clone(), chunk.clone());
        Ok(())
    }

    fn chunk_has(&self, id: &str) -> Result<bool> {
        // Always return false so chunk_pass processes every chunk afresh.
        // (We don't skip chunks that already exist in history for a different version.)
        Ok(self.written_chunks.lock().unwrap().contains_key(id))
    }

    fn chunk_get(&self, id: &str) -> Result<Option<Chunk>> {
        Ok(self.written_chunks.lock().unwrap().get(id).cloned())
    }

    fn chunk_get_by_uri(&self, uri: &str) -> Result<Option<Chunk>> {
        let guard = self.written_chunks.lock().unwrap();
        Ok(guard.values().find(|c| c.location.uri == uri).cloned())
    }

    fn chunk_list(&self, corpus_id: &str) -> Result<Vec<Chunk>> {
        let guard = self.written_chunks.lock().unwrap();
        Ok(guard
            .values()
            .filter(|c| c.corpus_id == corpus_id)
            .cloned()
            .collect())
    }

    fn chunk_list_ids(&self, corpus_id: &str) -> Result<Vec<String>> {
        let guard = self.written_chunks.lock().unwrap();
        Ok(guard
            .values()
            .filter(|c| c.corpus_id == corpus_id)
            .map(|c| c.id.clone())
            .collect())
    }

    fn chunk_list_unprocessed(&self, corpus_id: &str) -> Result<Vec<Chunk>> {
        // In backfill mode, all buffered chunks are "unprocessed" since we
        // always start fresh per commit.
        self.chunk_list(corpus_id)
    }

    fn chunk_count(&self, corpus_id: &str) -> Result<u64> {
        let guard = self.written_chunks.lock().unwrap();
        Ok(guard.values().filter(|c| c.corpus_id == corpus_id).count() as u64)
    }

    fn chunk_set_parent_path(&self, chunk_id: &str, parent_path: &str) -> Result<()> {
        // Update in buffer.
        if let Some(c) = self.written_chunks.lock().unwrap().get_mut(chunk_id) {
            c.parent_path = Some(parent_path.to_string());
        }
        Ok(())
    }

    fn chunk_set_semantic_processed(&self, chunk_id: &str) -> Result<()> {
        // No-op for backfill (we don't track semantic_processed in buffer).
        let _ = chunk_id;
        Ok(())
    }

    fn chunk_delete_by_id(&self, _chunk_id: &str) -> Result<bool> {
        // Guard: should not delete head rows during backfill.
        Err(backfill_no_write("chunk_delete_by_id"))
    }

    fn chunk_set_source_hash(&self, chunk_id: &str, hash: &str) -> Result<()> {
        // Update history row.
        self.inner
            .chunk_history_update_source_hash(chunk_id, &self.derived_at_sha, hash)?;
        // Update buffer.
        if let Some(c) = self.written_chunks.lock().unwrap().get_mut(chunk_id) {
            c.source_hash = Some(hash.to_string());
        }
        Ok(())
    }

    fn chunk_set_file_shape(
        &self,
        chunk_id: &str,
        file_shape_hash: &str,
        entity_id_list: &str,
    ) -> Result<()> {
        self.inner
            .chunk_set_file_shape(chunk_id, file_shape_hash, entity_id_list)?;
        if let Some(c) = self.written_chunks.lock().unwrap().get_mut(chunk_id) {
            c.file_shape_hash = file_shape_hash.to_string();
            c.entity_id_list = entity_id_list.to_string();
        }
        Ok(())
    }

    fn chunk_set_history(
        &self,
        chunk_id: &str,
        version: &str,
        commit_message: Option<&str>,
        author: Option<&str>,
    ) -> Result<()> {
        self.inner.chunk_history_update_version(
            chunk_id,
            &self.derived_at_sha,
            version,
            commit_message,
            author,
        )
    }

    fn chunk_list_source_paths(&self, corpus_id: &str) -> Result<Vec<(String, String, String)>> {
        let guard = self.written_chunks.lock().unwrap();
        Ok(guard
            .values()
            .filter(|c| c.corpus_id == corpus_id)
            .map(|c| {
                (
                    c.id.clone(),
                    c.location.uri.clone(),
                    c.source_hash.clone().unwrap_or_default(),
                )
            })
            .collect())
    }

    fn chunk_children_by_uri(&self, corpus_id: &str, parent_uri: &str) -> Result<Vec<Location>> {
        let guard = self.written_chunks.lock().unwrap();
        Ok(guard
            .values()
            .filter(|c| {
                c.corpus_id == corpus_id
                    && c.parent_path
                        .as_deref()
                        .map(|p| p == parent_uri)
                        .unwrap_or(false)
            })
            .map(|c| c.location.clone())
            .collect())
    }

    // ── Entity ────────────────────────────────────────────────────────────────
    // Writes → entities_history; reads from in-memory buffer.

    fn entity_upsert(&self, entity: &Entity) -> Result<()> {
        let superseded = self.supersession.superseded_for_entity(&entity.id);
        self.inner
            .entity_history_insert(entity, &self.derived_at_sha, &superseded)?;
        self.supersession
            .record_write_entity(&entity.id, &self.derived_at_sha);
        self.written_entities
            .lock()
            .unwrap()
            .insert(entity.id.clone(), entity.clone());
        Ok(())
    }

    fn entity_get_by_id(&self, id: &str) -> Result<Option<Entity>> {
        Ok(self.written_entities.lock().unwrap().get(id).cloned())
    }

    fn entity_find_by_name(&self, corpus_id: &str, name: &str) -> Result<Vec<Entity>> {
        let guard = self.written_entities.lock().unwrap();
        let lower = name.to_lowercase();
        Ok(guard
            .values()
            .filter(|e| {
                e.corpus_id == corpus_id
                    && (e.canonical_name.to_lowercase().contains(&lower)
                        || e.aliases.iter().any(|a| a.to_lowercase().contains(&lower)))
            })
            .cloned()
            .collect())
    }

    fn entity_list(&self, corpus_id: &str) -> Result<Vec<Entity>> {
        let guard = self.written_entities.lock().unwrap();
        Ok(guard
            .values()
            .filter(|e| e.corpus_id == corpus_id)
            .cloned()
            .collect())
    }

    fn entity_count(&self, corpus_id: &str) -> Result<u64> {
        let guard = self.written_entities.lock().unwrap();
        Ok(guard.values().filter(|e| e.corpus_id == corpus_id).count() as u64)
    }

    fn entity_top(&self, corpus_id: &str, limit: usize) -> Result<Vec<Entity>> {
        let mut entities = self.entity_list(corpus_id)?;
        entities.sort_by_key(|b| std::cmp::Reverse(b.appearance_count));
        entities.truncate(limit);
        Ok(entities)
    }

    fn entity_merge(&self, _keep_id: &str, _absorb_id: &str) -> Result<()> {
        // NO-OP: merging entities in historical state is out of scope.
        Ok(())
    }

    fn entities_at_location(&self, corpus_id: &str, uri: &str) -> Result<Vec<Entity>> {
        let guard = self.written_entities.lock().unwrap();
        Ok(guard
            .values()
            .filter(|e| {
                e.corpus_id == corpus_id
                    && (e.first_location.as_ref().map(|l| &l.uri) == Some(&uri.to_string())
                        || e.last_location.as_ref().map(|l| &l.uri) == Some(&uri.to_string()))
            })
            .cloned()
            .collect())
    }

    fn entity_list_by_abstract_kind(
        &self,
        corpus_ids: &[&str],
        abstract_kind: &str,
    ) -> Result<Vec<Entity>> {
        let guard = self.written_entities.lock().unwrap();
        Ok(guard
            .values()
            .filter(|e| {
                corpus_ids.contains(&e.corpus_id.as_str()) && e.abstract_kind == abstract_kind
            })
            .cloned()
            .collect())
    }

    fn kind_taxonomy_list(&self) -> Result<Vec<(String, String, String)>> {
        self.inner.kind_taxonomy_list()
    }

    // ── Edge ──────────────────────────────────────────────────────────────────
    // Writes → edges_history (no FK guard needed for history tables).

    fn edge_upsert(&self, edge: &Edge) -> Result<()> {
        let superseded = self.supersession.superseded_for_edge(&edge.id);
        self.inner
            .edge_history_insert(edge, &self.derived_at_sha, &superseded)?;
        self.supersession
            .record_write_edge(&edge.id, &self.derived_at_sha);
        Ok(())
    }

    fn edge_get_for_entity(
        &self,
        _entity_id: &str,
        _direction: EdgeDirection,
        _kind: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<Edge>> {
        Ok(vec![]) // history edges are not needed for pass pipeline reads
    }

    fn edge_list(&self, _corpus_id: &str) -> Result<Vec<Edge>> {
        Ok(vec![])
    }

    fn edge_count(&self, _corpus_id: &str) -> Result<u64> {
        Ok(0)
    }

    fn edge_location_uris_for_entity(&self, _entity_id: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }

    fn edge_entity_ids_at_location(&self, _location_uri: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }

    fn entity_in_degree(&self, _corpus_id: &str, _entity_id: &str) -> Result<u32> {
        Ok(0)
    }

    fn entity_out_degree(&self, _corpus_id: &str, _entity_id: &str) -> Result<u32> {
        Ok(0)
    }

    // ── Summary ───────────────────────────────────────────────────────────────

    fn summary_upsert(&self, summary: &Summary) -> Result<()> {
        let superseded = self.supersession.superseded_for_summary(&summary.target_id);
        self.inner
            .summary_history_insert(summary, &self.derived_at_sha, &superseded)?;
        self.supersession
            .record_write_summary(&summary.target_id, &self.derived_at_sha);
        Ok(())
    }

    fn summary_list(&self, _corpus_id: &str) -> Result<Vec<Summary>> {
        Ok(vec![])
    }

    fn summary_delete_for_target(&self, _corpus_id: &str, _target_id: &str) -> Result<()> {
        Ok(()) // NO-OP
    }

    fn summary_get(
        &self,
        _corpus_id: &str,
        _target_kind: &SummaryTargetKind,
        _target_id: &str,
    ) -> Result<Option<Summary>> {
        Ok(None)
    }

    fn summary_get_for_model(
        &self,
        _corpus_id: &str,
        _target_kind: &SummaryTargetKind,
        _target_id: &str,
        _model: &str,
    ) -> Result<Option<Summary>> {
        Ok(None)
    }

    // ── Run log ───────────────────────────────────────────────────────────────
    // Delegate — run log records for backfill commits are informational.

    fn run_start(&self, corpus_id: &str, pass: &str, provider: Option<&str>) -> Result<String> {
        self.inner.run_start(corpus_id, pass, provider)
    }

    fn run_finish(&self, run_id: &str, status: RunStatus, stats: &PassStats) -> Result<()> {
        self.inner.run_finish(run_id, status, stats)
    }

    fn run_latest(&self, corpus_id: &str, limit: usize) -> Result<Vec<RunRecord>> {
        self.inner.run_latest(corpus_id, limit)
    }

    fn run_abandon_stale(&self, corpus_id: &str) -> Result<u64> {
        self.inner.run_abandon_stale(corpus_id)
    }

    // ── Corrections ───────────────────────────────────────────────────────────

    fn correction_insert(
        &self,
        corpus_id: Option<&str>,
        collection_id: Option<&str>,
        kind: &CorrectionKind,
    ) -> Result<String> {
        self.inner.correction_insert(corpus_id, collection_id, kind)
    }

    fn correction_list(&self, corpus_id: &str) -> Result<Vec<Correction>> {
        self.inner.correction_list(corpus_id)
    }

    fn correction_list_for_collection(&self, collection_id: &str) -> Result<Vec<Correction>> {
        self.inner.correction_list_for_collection(collection_id)
    }

    fn correction_list_all(&self) -> Result<Vec<Correction>> {
        self.inner.correction_list_all()
    }

    fn correction_delete(&self, id: &str) -> Result<bool> {
        self.inner.correction_delete(id)
    }

    // ── FTS / Search ──────────────────────────────────────────────────────────

    fn fts_search(&self, corpus_id: &str, query: &str, limit: usize) -> Result<Vec<FtsResult>> {
        self.inner.fts_search(corpus_id, query, limit)
    }

    fn fts_rebuild(&self, _corpus_id: &str) -> Result<()> {
        Ok(()) // NO-OP: FTS index is over head tables; skip during backfill
    }

    // ── Embeddings ────────────────────────────────────────────────────────────

    fn embedding_upsert(&self, _embedding: &StoredEmbedding) -> Result<()> {
        Ok(()) // NO-OP: embeddings for historical state are out of scope
    }

    fn embedding_get_for_chunk(&self, chunk_id: &str) -> Result<Option<StoredEmbedding>> {
        self.inner.embedding_get_for_chunk(chunk_id)
    }

    fn embedding_list_for_corpus(&self, corpus_id: &str) -> Result<Vec<StoredEmbedding>> {
        self.inner.embedding_list_for_corpus(corpus_id)
    }

    fn embedding_count(&self, _corpus_id: &str) -> Result<u64> {
        Ok(0)
    }

    // ── Collection ────────────────────────────────────────────────────────────

    fn collection_insert(&self, collection: &Collection) -> Result<()> {
        self.inner.collection_insert(collection)
    }

    fn collection_list(&self) -> Result<Vec<Collection>> {
        self.inner.collection_list()
    }

    fn collection_get(&self, id: &str) -> Result<Option<Collection>> {
        self.inner.collection_get(id)
    }

    fn collection_require(&self, id: &str) -> Result<Collection> {
        self.inner.collection_require(id)
    }

    fn collection_add_member(
        &self,
        collection_id: &str,
        member_id: &str,
        member_type: MemberType,
    ) -> Result<()> {
        self.inner
            .collection_add_member(collection_id, member_id, member_type)
    }

    fn collection_remove_member(
        &self,
        collection_id: &str,
        member_id: &str,
        member_type: MemberType,
    ) -> Result<()> {
        self.inner
            .collection_remove_member(collection_id, member_id, member_type)
    }

    fn collection_delete(&self, id: &str) -> Result<bool> {
        self.inner.collection_delete(id)
    }

    fn collection_direct_members(&self, collection_id: &str) -> Result<Vec<CollectionMember>> {
        self.inner.collection_direct_members(collection_id)
    }

    fn collection_resolve_corpus_ids(&self, collection_id: &str) -> Result<Vec<String>> {
        self.inner.collection_resolve_corpus_ids(collection_id)
    }

    // ── Purpose ───────────────────────────────────────────────────────────────

    fn purpose_upsert(&self, p: &EntityPurpose) -> Result<()> {
        let superseded = self
            .supersession
            .superseded_for_purpose(&p.entity_id, &p.model);
        self.inner
            .purpose_history_insert(p, &self.derived_at_sha, &superseded)?;
        self.supersession
            .record_write_purpose(&p.entity_id, &p.model, &self.derived_at_sha);
        Ok(())
    }

    fn purpose_get(&self, _corpus_id: &str, _entity_id: &str) -> Result<Option<EntityPurpose>> {
        Ok(None)
    }

    fn purpose_get_for_model(
        &self,
        _corpus_id: &str,
        _entity_id: &str,
        _model: &str,
    ) -> Result<Option<EntityPurpose>> {
        Ok(None)
    }

    fn purpose_list(&self, _corpus_id: &str) -> Result<Vec<EntityPurpose>> {
        Ok(vec![])
    }

    // ── Block ─────────────────────────────────────────────────────────────────

    fn block_upsert(&self, b: &EntityBlock) -> Result<()> {
        let superseded = self.supersession.superseded_for_block(&b.entity_id);
        self.inner
            .block_history_insert(b, &self.derived_at_sha, &superseded)?;
        self.supersession
            .record_write_block(&b.entity_id, &self.derived_at_sha);
        Ok(())
    }

    fn block_list_for_entity(&self, _entity_id: &str) -> Result<Vec<EntityBlock>> {
        Ok(vec![])
    }

    // ── Contract ──────────────────────────────────────────────────────────────

    fn contract_upsert(&self, c: &EntityContract) -> Result<()> {
        let superseded = self
            .supersession
            .superseded_for_contract(&c.entity_id, &c.model);
        self.inner
            .contract_history_insert(c, &self.derived_at_sha, &superseded)?;
        self.supersession
            .record_write_contract(&c.entity_id, &c.model, &self.derived_at_sha);
        Ok(())
    }

    fn contract_get(&self, _corpus_id: &str, _entity_id: &str) -> Result<Option<EntityContract>> {
        Ok(None)
    }

    fn contract_get_for_model(
        &self,
        _corpus_id: &str,
        _entity_id: &str,
        _model: &str,
    ) -> Result<Option<EntityContract>> {
        Ok(None)
    }

    fn contract_list(&self, _corpus_id: &str) -> Result<Vec<EntityContract>> {
        Ok(vec![])
    }

    fn contract_list_best_per_entity(&self, _corpus_id: &str) -> Result<Vec<EntityContract>> {
        Ok(vec![])
    }

    fn contract_list_inconsistencies(&self, _corpus_id: &str) -> Result<Vec<EntityContract>> {
        Ok(vec![])
    }

    // ── Theme ─────────────────────────────────────────────────────────────────

    fn theme_upsert(&self, t: &Theme) -> Result<()> {
        let superseded = self.supersession.superseded_for_theme(&t.id);
        self.inner
            .theme_history_insert(t, &self.derived_at_sha, &superseded)?;
        self.supersession
            .record_write_theme(&t.id, &self.derived_at_sha);
        Ok(())
    }

    fn theme_list(&self, _corpus_id: &str) -> Result<Vec<Theme>> {
        Ok(vec![])
    }

    fn theme_delete(&self, _theme_id: &str, _corpus_id: &str) -> Result<()> {
        Ok(()) // NO-OP: backfill has no head rows to delete
    }

    // ── History / Archive ─────────────────────────────────────────────────────
    // These are the existing archive helpers; they copy head rows to history.
    // In backfill mode they must not be invoked (they read from head tables
    // which are intentionally empty of the commit's data).

    fn archive_entity(
        &self,
        _entity_id: &str,
        _corpus_id: &str,
        _superseded_at_sha: &str,
    ) -> Result<bool> {
        Ok(false) // NO-OP in backfill
    }

    fn archive_edges_for_entity(
        &self,
        _entity_id: &str,
        _superseded_at_sha: &str,
    ) -> Result<u64> {
        Ok(0)
    }

    fn archive_purposes_for_entity(
        &self,
        _entity_id: &str,
        _superseded_at_sha: &str,
    ) -> Result<u64> {
        Ok(0)
    }

    fn archive_contracts_for_entity(
        &self,
        _entity_id: &str,
        _superseded_at_sha: &str,
    ) -> Result<u64> {
        Ok(0)
    }

    fn archive_blocks_for_entity(
        &self,
        _entity_id: &str,
        _superseded_at_sha: &str,
    ) -> Result<u64> {
        Ok(0)
    }

    fn archive_summaries_for_target(
        &self,
        _corpus_id: &str,
        _target_id: &str,
        _superseded_at_sha: &str,
    ) -> Result<u64> {
        Ok(0)
    }

    fn archive_chunk(&self, _chunk_id: &str, _superseded_at_sha: &str) -> Result<bool> {
        Ok(false) // NO-OP
    }

    fn archive_theme(
        &self,
        _theme_id: &str,
        _corpus_id: &str,
        _superseded_at_sha: &str,
    ) -> Result<bool> {
        Ok(false)
    }

    fn archive_themes_for_corpus(
        &self,
        _corpus_id: &str,
        _superseded_at_sha: &str,
    ) -> Result<u64> {
        Ok(0)
    }

    fn cascade_delete_dirty_subtree(
        &self,
        _corpus_id: &str,
        _dirty_chunk_ids: &[String],
        _superseded_at_sha: &str,
    ) -> Result<CascadeStats> {
        Ok(CascadeStats::default()) // NO-OP: no head rows to cascade
    }

    // ── Graph helpers ─────────────────────────────────────────────────────────

    fn entities_without_inbound_calls(&self, _corpus_id: &str) -> Result<Vec<Entity>> {
        Ok(vec![])
    }

    fn entities_without_verified_by(&self, _corpus_id: &str) -> Result<Vec<Entity>> {
        Ok(vec![])
    }

    // ── Honest provenance (migration 013) — delegate to inner ──────────────────

    fn entity_list_at_sha(
        &self,
        corpus_id: &str,
        target_sha: &str,
        ancestry: Option<&dyn crate::storage::ancestry::AncestryReader>,
    ) -> Result<Vec<Entity>> {
        self.inner
            .entity_list_at_sha(corpus_id, target_sha, ancestry)
    }
    fn is_tombstoned_at(
        &self,
        corpus_id: &str,
        artifact_kind: &str,
        artifact_id: &str,
        target_sha: &str,
        ancestry: Option<&dyn crate::storage::ancestry::AncestryReader>,
    ) -> Result<bool> {
        self.inner
            .is_tombstoned_at(corpus_id, artifact_kind, artifact_id, target_sha, ancestry)
    }
    fn commit_embedding(&self, embedding: &StoredEmbedding, provenance: &Provenance) -> Result<()> {
        self.inner.commit_embedding(embedding, provenance)
    }
    fn migrate_fresh(&self, corpus_id: &str) -> Result<crate::storage::backend::MigrateFreshStats> {
        self.inner.migrate_fresh(corpus_id)
    }
    fn archive_to_history(
        &self,
        corpus_id: &str,
        set: &ArchiveSet,
        provenance: &Provenance,
    ) -> Result<ArchiveStats> {
        self.inner.archive_to_history(corpus_id, set, provenance)
    }
    fn refine_provenance(
        &self,
        corpus_id: &str,
        artifact_kind: &str,
        artifact_id: &str,
        observed: &Provenance,
    ) -> Result<RefineOutcome> {
        self.inner
            .refine_provenance(corpus_id, artifact_kind, artifact_id, observed)
    }
    fn tombstone_insert(
        &self,
        corpus_id: &str,
        artifact_kind: &str,
        artifact_id: &str,
        provenance: &Provenance,
        reason: Option<&str>,
    ) -> Result<()> {
        self.inner
            .tombstone_insert(corpus_id, artifact_kind, artifact_id, provenance, reason)
    }
    fn tombstone_list(
        &self,
        corpus_id: &str,
        artifact_kind: &str,
        artifact_id: &str,
    ) -> Result<Vec<Tombstone>> {
        self.inner
            .tombstone_list(corpus_id, artifact_kind, artifact_id)
    }
    fn layer2_cache_get(&self, key: &Layer2CacheKey) -> Result<Option<CachedArtifact>> {
        self.inner.layer2_cache_get(key)
    }
    fn layer2_cache_put(
        &self,
        key: &Layer2CacheKey,
        payload: &str,
        first_seen_at_sha: &str,
    ) -> Result<()> {
        self.inner.layer2_cache_put(key, payload, first_seen_at_sha)
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    fn schema_version(&self) -> Result<u64> {
        self.inner.schema_version()
    }

    // ── Backfill history writes (delegate to inner) ───────────────────────────

    fn chunk_history_insert(
        &self,
        chunk: &Chunk,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        self.inner
            .chunk_history_insert(chunk, derived_at_sha, superseded_at_sha)
    }

    fn chunk_history_update_source_hash(
        &self,
        chunk_id: &str,
        derived_at_sha: &str,
        source_hash: &str,
    ) -> Result<()> {
        self.inner
            .chunk_history_update_source_hash(chunk_id, derived_at_sha, source_hash)
    }

    fn chunk_history_update_version(
        &self,
        chunk_id: &str,
        derived_at_sha: &str,
        last_modified_at_version: &str,
        commit_message: Option<&str>,
        author: Option<&str>,
    ) -> Result<()> {
        self.inner.chunk_history_update_version(
            chunk_id,
            derived_at_sha,
            last_modified_at_version,
            commit_message,
            author,
        )
    }

    fn entity_history_insert(
        &self,
        entity: &Entity,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        self.inner
            .entity_history_insert(entity, derived_at_sha, superseded_at_sha)
    }

    fn edge_history_insert(
        &self,
        edge: &Edge,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        self.inner
            .edge_history_insert(edge, derived_at_sha, superseded_at_sha)
    }

    fn summary_history_insert(
        &self,
        summary: &Summary,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        self.inner
            .summary_history_insert(summary, derived_at_sha, superseded_at_sha)
    }

    fn purpose_history_insert(
        &self,
        purpose: &EntityPurpose,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        self.inner
            .purpose_history_insert(purpose, derived_at_sha, superseded_at_sha)
    }

    fn contract_history_insert(
        &self,
        contract: &EntityContract,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        self.inner
            .contract_history_insert(contract, derived_at_sha, superseded_at_sha)
    }

    fn block_history_insert(
        &self,
        block: &EntityBlock,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        self.inner
            .block_history_insert(block, derived_at_sha, superseded_at_sha)
    }

    fn theme_history_insert(
        &self,
        theme: &Theme,
        derived_at_sha: &str,
        superseded_at_sha: &str,
    ) -> Result<()> {
        self.inner
            .theme_history_insert(theme, derived_at_sha, superseded_at_sha)
    }

    // ── Backfill seeding helpers (delegate to inner) ──────────────────────────

    fn entity_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        self.inner.entity_head_shas(corpus_id)
    }

    fn chunk_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        self.inner.chunk_head_shas(corpus_id)
    }

    fn edge_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        self.inner.edge_head_shas(corpus_id)
    }

    fn summary_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        self.inner.summary_head_shas(corpus_id)
    }

    fn purpose_head_shas(&self, corpus_id: &str) -> Result<Vec<((String, String), String)>> {
        self.inner.purpose_head_shas(corpus_id)
    }

    fn contract_head_shas(&self, corpus_id: &str) -> Result<Vec<((String, String), String)>> {
        self.inner.contract_head_shas(corpus_id)
    }

    fn block_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        self.inner.block_head_shas(corpus_id)
    }

    fn theme_head_shas(&self, corpus_id: &str) -> Result<Vec<(String, String)>> {
        self.inner.theme_head_shas(corpus_id)
    }

    // ── VirtualHead read helpers (delegate to real inner db) ──────────────────

    fn entity_list_by_sha(&self, corpus_id: &str, sha: &str) -> Result<Vec<Entity>> {
        self.inner.entity_list_by_sha(corpus_id, sha)
    }

    fn entity_count_by_sha(&self, corpus_id: &str, sha: &str) -> Result<u64> {
        self.inner.entity_count_by_sha(corpus_id, sha)
    }

    // ── Pruning ───────────────────────────────────────────────────────────────
    //
    // Pruning is not meaningful during a backfill walk (the wrapper runs against
    // history tables, not head tables).  Delegate to the inner backend.

    fn prune_history(&self, corpus_id: &str, keep: usize, dry_run: bool) -> Result<PruneStats> {
        self.inner.prune_history(corpus_id, keep, dry_run)
    }
}
