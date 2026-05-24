//! PostgreSQL backend stub.
//!
//! This file exists to:
//! 1. Prove the `StorageBackend` trait is implementable without SQLite.
//! 2. Give future contributors a clear starting point.
//! 3. Catch any SQLite-specific assumptions that leaked into the trait signature.
//!
//! Every method returns `Err(CalError::Other("postgres backend not yet implemented"))`.
//!
//! See `docs/adapting-storage.md` for implementation guidance.

use crate::corrections::types::{Correction, CorrectionKind};
use crate::error::{CalError, Result};
use crate::storage::backend::{CascadeStats, StorageBackend};
use crate::storage::edge_store::EdgeDirection;
use crate::storage::embedding_store::StoredEmbedding;
use crate::storage::fts::FtsResult;
use crate::storage::pruning::PruneStats;
use crate::storage::run_log::{PassStats, RunRecord};
use crate::types::pass::RunStatus;
use crate::types::{
    Chunk, Collection, CollectionMember, Corpus, CorpusStatus, Edge, Entity, EntityBlock,
    EntityContract, EntityPurpose, Location, MemberType, Summary, SummaryTargetKind, Theme,
};

fn unimplemented() -> CalError {
    CalError::Other(
        "PostgresBackend is not yet implemented. \
         Contributions welcome — see docs/adapting-storage.md"
            .into(),
    )
}

/// Not-yet-implemented Postgres backend. Compiles; all methods return an error.
///
/// Future implementation notes:
/// - Use `sqlx::PgPool` or `tokio_postgres::Client` for the connection.
/// - FTS: replace `fts_search` with `pg_trgm` + `tsvector`.
/// - Embeddings: replace BLOB column with `pgvector` extension.
/// - Migrations: use your own migration tool (e.g. `sqlx migrate`); do not use
///   the `rusqlite_migration`-based `Database::open` path.
#[derive(Debug)]
pub struct PostgresBackend {
    // future: sqlx::PgPool
    _placeholder: (),
}

impl PostgresBackend {
    /// Not yet implemented. Returns an error at construction time.
    #[allow(unused_variables)]
    pub fn connect(_url: &str) -> Result<Self> {
        Err(CalError::Other(
            "PostgresBackend is not yet implemented. \
             Contributions welcome — see docs/adapting-storage.md"
                .into(),
        ))
    }
}

impl StorageBackend for PostgresBackend {
    fn corpus_insert(&self, _corpus: &Corpus) -> Result<()> {
        Err(unimplemented())
    }
    fn corpus_list(&self) -> Result<Vec<Corpus>> {
        Err(unimplemented())
    }
    fn corpus_get(&self, _id: &str) -> Result<Option<Corpus>> {
        Err(unimplemented())
    }
    fn corpus_require(&self, _id: &str) -> Result<Corpus> {
        Err(unimplemented())
    }
    fn corpus_update_status(&self, _id: &str, _status: CorpusStatus) -> Result<()> {
        Err(unimplemented())
    }
    fn corpus_set_last_indexed(&self, _id: &str, _at: &str) -> Result<()> {
        Err(unimplemented())
    }
    fn corpus_set_pipeline_version(&self, _id: &str, _version: u32) -> Result<()> {
        Err(unimplemented())
    }
    fn corpus_set_last_indexed_version(&self, _id: &str, _version: &str) -> Result<()> {
        Err(unimplemented())
    }
    fn corpus_get_last_indexed_version(&self, _id: &str) -> Result<Option<String>> {
        Err(unimplemented())
    }
    fn corpus_delete(&self, _id: &str) -> Result<bool> {
        Err(unimplemented())
    }
    fn corpus_exists(&self, _id: &str) -> Result<bool> {
        Err(unimplemented())
    }

    fn chunk_upsert(&self, _chunk: &Chunk) -> Result<()> {
        Err(unimplemented())
    }
    fn chunk_has(&self, _id: &str) -> Result<bool> {
        Err(unimplemented())
    }
    fn chunk_get(&self, _id: &str) -> Result<Option<Chunk>> {
        Err(unimplemented())
    }
    fn chunk_get_by_uri(&self, _uri: &str) -> Result<Option<Chunk>> {
        Err(unimplemented())
    }
    fn chunk_list(&self, _corpus_id: &str) -> Result<Vec<Chunk>> {
        Err(unimplemented())
    }
    fn chunk_list_ids(&self, _corpus_id: &str) -> Result<Vec<String>> {
        Err(unimplemented())
    }
    fn chunk_list_unprocessed(&self, _corpus_id: &str) -> Result<Vec<Chunk>> {
        Err(unimplemented())
    }
    fn chunk_count(&self, _corpus_id: &str) -> Result<u64> {
        Err(unimplemented())
    }
    fn chunk_set_parent_path(&self, _chunk_id: &str, _parent_path: &str) -> Result<()> {
        Err(unimplemented())
    }
    fn chunk_set_semantic_processed(&self, _chunk_id: &str) -> Result<()> {
        Err(unimplemented())
    }
    fn chunk_delete_by_id(&self, _chunk_id: &str) -> Result<bool> {
        Err(unimplemented())
    }
    fn chunk_set_source_hash(&self, _chunk_id: &str, _hash: &str) -> Result<()> {
        Err(unimplemented())
    }
    fn chunk_set_history(
        &self,
        _chunk_id: &str,
        _version: &str,
        _commit_message: Option<&str>,
        _author: Option<&str>,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn chunk_list_source_paths(&self, _corpus_id: &str) -> Result<Vec<(String, String, String)>> {
        Err(unimplemented())
    }
    fn chunk_children_by_uri(&self, _corpus_id: &str, _parent_uri: &str) -> Result<Vec<Location>> {
        Err(unimplemented())
    }

    fn entity_upsert(&self, _entity: &Entity) -> Result<()> {
        Err(unimplemented())
    }
    fn entity_get_by_id(&self, _id: &str) -> Result<Option<Entity>> {
        Err(unimplemented())
    }
    fn entity_find_by_name(&self, _corpus_id: &str, _name: &str) -> Result<Vec<Entity>> {
        Err(unimplemented())
    }
    fn entity_list(&self, _corpus_id: &str) -> Result<Vec<Entity>> {
        Err(unimplemented())
    }
    fn entity_count(&self, _corpus_id: &str) -> Result<u64> {
        Err(unimplemented())
    }
    fn entity_top(&self, _corpus_id: &str, _limit: usize) -> Result<Vec<Entity>> {
        Err(unimplemented())
    }
    fn entity_merge(&self, _keep_id: &str, _absorb_id: &str) -> Result<()> {
        Err(unimplemented())
    }
    fn entities_at_location(&self, _corpus_id: &str, _uri: &str) -> Result<Vec<Entity>> {
        Err(unimplemented())
    }
    fn entity_list_by_abstract_kind(
        &self,
        _corpus_ids: &[&str],
        _abstract_kind: &str,
    ) -> Result<Vec<Entity>> {
        Err(unimplemented())
    }
    fn kind_taxonomy_list(&self) -> Result<Vec<(String, String, String)>> {
        Err(unimplemented())
    }

    fn edge_upsert(&self, _edge: &Edge) -> Result<()> {
        Err(unimplemented())
    }
    fn edge_get_for_entity(
        &self,
        _entity_id: &str,
        _direction: EdgeDirection,
        _kind: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<Edge>> {
        Err(unimplemented())
    }
    fn edge_list(&self, _corpus_id: &str) -> Result<Vec<Edge>> {
        Err(unimplemented())
    }
    fn edge_count(&self, _corpus_id: &str) -> Result<u64> {
        Err(unimplemented())
    }
    fn edge_location_uris_for_entity(&self, _entity_id: &str) -> Result<Vec<String>> {
        Err(unimplemented())
    }
    fn edge_entity_ids_at_location(&self, _location_uri: &str) -> Result<Vec<String>> {
        Err(unimplemented())
    }
    fn entity_in_degree(&self, _corpus_id: &str, _entity_id: &str) -> Result<u32> {
        todo!("PostgresBackend::entity_in_degree — not yet implemented")
    }
    fn entity_out_degree(&self, _corpus_id: &str, _entity_id: &str) -> Result<u32> {
        todo!("PostgresBackend::entity_out_degree — not yet implemented")
    }

    fn summary_upsert(&self, _summary: &Summary) -> Result<()> {
        Err(unimplemented())
    }
    fn summary_list(&self, _corpus_id: &str) -> Result<Vec<Summary>> {
        Err(unimplemented())
    }
    fn summary_delete_for_target(&self, _corpus_id: &str, _target_id: &str) -> Result<()> {
        Err(unimplemented())
    }
    fn summary_get_for_model(
        &self,
        _corpus_id: &str,
        _target_kind: &SummaryTargetKind,
        _target_id: &str,
        _model: &str,
    ) -> Result<Option<Summary>> {
        Err(unimplemented())
    }
    fn summary_get(
        &self,
        _corpus_id: &str,
        _target_kind: &SummaryTargetKind,
        _target_id: &str,
    ) -> Result<Option<Summary>> {
        Err(unimplemented())
    }

    fn run_start(&self, _corpus_id: &str, _pass: &str, _provider: Option<&str>) -> Result<String> {
        Err(unimplemented())
    }
    fn run_finish(&self, _run_id: &str, _status: RunStatus, _stats: &PassStats) -> Result<()> {
        Err(unimplemented())
    }
    fn run_latest(&self, _corpus_id: &str, _limit: usize) -> Result<Vec<RunRecord>> {
        Err(unimplemented())
    }
    fn run_abandon_stale(&self, _corpus_id: &str) -> Result<u64> {
        Err(unimplemented())
    }

    fn correction_insert(
        &self,
        _corpus_id: Option<&str>,
        _collection_id: Option<&str>,
        _kind: &CorrectionKind,
    ) -> Result<String> {
        Err(unimplemented())
    }
    fn correction_list(&self, _corpus_id: &str) -> Result<Vec<Correction>> {
        Err(unimplemented())
    }
    fn correction_list_for_collection(&self, _collection_id: &str) -> Result<Vec<Correction>> {
        Err(unimplemented())
    }
    fn correction_list_all(&self) -> Result<Vec<Correction>> {
        Err(unimplemented())
    }
    fn correction_delete(&self, _id: &str) -> Result<bool> {
        Err(unimplemented())
    }

    fn fts_search(&self, _corpus_id: &str, _query: &str, _limit: usize) -> Result<Vec<FtsResult>> {
        Err(unimplemented())
    }
    fn fts_rebuild(&self, _corpus_id: &str) -> Result<()> {
        Err(unimplemented())
    }

    fn embedding_upsert(&self, _embedding: &StoredEmbedding) -> Result<()> {
        Err(unimplemented())
    }
    fn embedding_get_for_chunk(&self, _chunk_id: &str) -> Result<Option<StoredEmbedding>> {
        Err(unimplemented())
    }
    fn embedding_list_for_corpus(&self, _corpus_id: &str) -> Result<Vec<StoredEmbedding>> {
        Err(unimplemented())
    }
    fn embedding_count(&self, _corpus_id: &str) -> Result<u64> {
        Err(unimplemented())
    }

    fn collection_insert(&self, _collection: &Collection) -> Result<()> {
        Err(unimplemented())
    }
    fn collection_list(&self) -> Result<Vec<Collection>> {
        Err(unimplemented())
    }
    fn collection_get(&self, _id: &str) -> Result<Option<Collection>> {
        Err(unimplemented())
    }
    fn collection_require(&self, _id: &str) -> Result<Collection> {
        Err(unimplemented())
    }
    fn collection_add_member(
        &self,
        _collection_id: &str,
        _member_id: &str,
        _member_type: MemberType,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn collection_remove_member(
        &self,
        _collection_id: &str,
        _member_id: &str,
        _member_type: MemberType,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn collection_delete(&self, _id: &str) -> Result<bool> {
        Err(unimplemented())
    }
    fn collection_direct_members(&self, _collection_id: &str) -> Result<Vec<CollectionMember>> {
        Err(unimplemented())
    }
    fn collection_resolve_corpus_ids(&self, _collection_id: &str) -> Result<Vec<String>> {
        Err(unimplemented())
    }

    // ── Purpose ───────────────────────────────────────────────────────────────

    fn purpose_upsert(&self, _p: &EntityPurpose) -> Result<()> {
        Err(unimplemented())
    }
    fn purpose_get(&self, _corpus_id: &str, _entity_id: &str) -> Result<Option<EntityPurpose>> {
        Err(unimplemented())
    }
    fn purpose_get_for_model(
        &self,
        _corpus_id: &str,
        _entity_id: &str,
        _model: &str,
    ) -> Result<Option<EntityPurpose>> {
        Err(unimplemented())
    }
    fn purpose_list(&self, _corpus_id: &str) -> Result<Vec<EntityPurpose>> {
        Err(unimplemented())
    }

    // ── Block ─────────────────────────────────────────────────────────────────

    fn block_upsert(&self, _b: &EntityBlock) -> Result<()> {
        Err(unimplemented())
    }
    fn block_list_for_entity(&self, _entity_id: &str) -> Result<Vec<EntityBlock>> {
        Err(unimplemented())
    }

    // ── Contract ──────────────────────────────────────────────────────────────

    fn contract_upsert(&self, _c: &EntityContract) -> Result<()> {
        Err(unimplemented())
    }
    fn contract_get(&self, _corpus_id: &str, _entity_id: &str) -> Result<Option<EntityContract>> {
        Err(unimplemented())
    }
    fn contract_get_for_model(
        &self,
        _corpus_id: &str,
        _entity_id: &str,
        _model: &str,
    ) -> Result<Option<EntityContract>> {
        Err(unimplemented())
    }
    fn contract_list(&self, _corpus_id: &str) -> Result<Vec<EntityContract>> {
        Err(unimplemented())
    }
    fn contract_list_best_per_entity(&self, _corpus_id: &str) -> Result<Vec<EntityContract>> {
        Err(unimplemented())
    }
    fn contract_list_inconsistencies(&self, _corpus_id: &str) -> Result<Vec<EntityContract>> {
        Err(unimplemented())
    }

    // ── Theme ─────────────────────────────────────────────────────────────────

    fn theme_upsert(&self, _t: &Theme) -> Result<()> {
        Err(unimplemented())
    }
    fn theme_list(&self, _corpus_id: &str) -> Result<Vec<Theme>> {
        Err(unimplemented())
    }

    // ── History / Archive ─────────────────────────────────────────────────────

    fn archive_entity(
        &self,
        _entity_id: &str,
        _corpus_id: &str,
        _superseded_at_version: &str,
    ) -> Result<bool> {
        Err(unimplemented())
    }
    fn archive_edges_for_entity(
        &self,
        _entity_id: &str,
        _superseded_at_version: &str,
    ) -> Result<u64> {
        Err(unimplemented())
    }
    fn archive_purposes_for_entity(
        &self,
        _entity_id: &str,
        _superseded_at_version: &str,
    ) -> Result<u64> {
        Err(unimplemented())
    }
    fn archive_contracts_for_entity(
        &self,
        _entity_id: &str,
        _superseded_at_version: &str,
    ) -> Result<u64> {
        Err(unimplemented())
    }
    fn archive_blocks_for_entity(
        &self,
        _entity_id: &str,
        _superseded_at_version: &str,
    ) -> Result<u64> {
        Err(unimplemented())
    }
    fn archive_summaries_for_target(
        &self,
        _corpus_id: &str,
        _target_id: &str,
        _superseded_at_version: &str,
    ) -> Result<u64> {
        Err(unimplemented())
    }
    fn archive_chunk(&self, _chunk_id: &str, _superseded_at_version: &str) -> Result<bool> {
        Err(unimplemented())
    }
    fn archive_theme(
        &self,
        _theme_id: &str,
        _corpus_id: &str,
        _superseded_at_version: &str,
    ) -> Result<bool> {
        Err(unimplemented())
    }
    fn archive_themes_for_corpus(
        &self,
        _corpus_id: &str,
        _superseded_at_version: &str,
    ) -> Result<u64> {
        Err(unimplemented())
    }
    fn cascade_delete_dirty_subtree(
        &self,
        _corpus_id: &str,
        _dirty_chunk_ids: &[String],
        _superseded_at_version: &str,
    ) -> Result<CascadeStats> {
        Err(unimplemented())
    }

    // ── Graph helpers ─────────────────────────────────────────────────────────

    fn entities_without_inbound_calls(&self, _corpus_id: &str) -> Result<Vec<Entity>> {
        Err(unimplemented())
    }
    fn entities_without_verified_by(&self, _corpus_id: &str) -> Result<Vec<Entity>> {
        Err(unimplemented())
    }

    // ── Pruning ───────────────────────────────────────────────────────────────

    fn prune_history(&self, _corpus_id: &str, _keep: usize, _dry_run: bool) -> Result<PruneStats> {
        unimplemented!("postgres backend does not yet support history pruning")
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    fn schema_version(&self) -> Result<u64> {
        Err(unimplemented())
    }

    // ── Backfill history writes ───────────────────────────────────────────────

    fn chunk_history_insert(
        &self,
        _chunk: &Chunk,
        _derived_at_version: &str,
        _superseded_at_version: &str,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn chunk_history_update_source_hash(
        &self,
        _chunk_id: &str,
        _derived_at_version: &str,
        _source_hash: &str,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn chunk_history_update_version(
        &self,
        _chunk_id: &str,
        _derived_at_version: &str,
        _last_modified_at_version: &str,
        _commit_message: Option<&str>,
        _author: Option<&str>,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn entity_history_insert(
        &self,
        _entity: &Entity,
        _derived_at_version: &str,
        _superseded_at_version: &str,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn edge_history_insert(
        &self,
        _edge: &Edge,
        _derived_at_version: &str,
        _superseded_at_version: &str,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn summary_history_insert(
        &self,
        _summary: &Summary,
        _derived_at_version: &str,
        _superseded_at_version: &str,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn purpose_history_insert(
        &self,
        _purpose: &EntityPurpose,
        _derived_at_version: &str,
        _superseded_at_version: &str,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn contract_history_insert(
        &self,
        _contract: &EntityContract,
        _derived_at_version: &str,
        _superseded_at_version: &str,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn block_history_insert(
        &self,
        _block: &EntityBlock,
        _derived_at_version: &str,
        _superseded_at_version: &str,
    ) -> Result<()> {
        Err(unimplemented())
    }
    fn theme_history_insert(
        &self,
        _theme: &Theme,
        _derived_at_version: &str,
        _superseded_at_version: &str,
    ) -> Result<()> {
        Err(unimplemented())
    }

    // ── Backfill seeding helpers ──────────────────────────────────────────────

    fn entity_head_versions(&self, _corpus_id: &str) -> Result<Vec<(String, String)>> {
        Err(unimplemented())
    }
    fn chunk_head_versions(&self, _corpus_id: &str) -> Result<Vec<(String, String)>> {
        Err(unimplemented())
    }
    fn edge_head_versions(&self, _corpus_id: &str) -> Result<Vec<(String, String)>> {
        Err(unimplemented())
    }
    fn summary_head_versions(&self, _corpus_id: &str) -> Result<Vec<(String, String)>> {
        Err(unimplemented())
    }
    fn purpose_head_versions(&self, _corpus_id: &str) -> Result<Vec<((String, String), String)>> {
        Err(unimplemented())
    }
    fn contract_head_versions(&self, _corpus_id: &str) -> Result<Vec<((String, String), String)>> {
        Err(unimplemented())
    }
    fn block_head_versions(&self, _corpus_id: &str) -> Result<Vec<(String, String)>> {
        Err(unimplemented())
    }
    fn theme_head_versions(&self, _corpus_id: &str) -> Result<Vec<(String, String)>> {
        Err(unimplemented())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_returns_error() {
        let result = PostgresBackend::connect("postgres://localhost/test");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not yet implemented"), "message was: {msg}");
    }

    #[test]
    fn postgres_backend_satisfies_trait() {
        // Compile-time check: PostgresBackend implements StorageBackend.
        fn assert_impl<T: StorageBackend>() {}
        assert_impl::<PostgresBackend>();
    }
}
