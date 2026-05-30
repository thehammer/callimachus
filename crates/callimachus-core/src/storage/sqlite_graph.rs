use crate::error::Result;
use crate::storage::db::Database;
use crate::storage::entity_store::row_to_entity;
use crate::types::Entity;
use rusqlite::params;

/// Entities in `corpus_id` that have no inbound `calls` edges (potentially unreachable).
pub fn entities_without_inbound_calls(db: &Database, corpus_id: &str) -> Result<Vec<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT e.id, e.corpus_id, e.canonical_name, e.kind, e.abstract_kind,
                e.aliases, e.description,
                e.first_location_uri, e.last_location_uri,
                e.appearance_count, e.confidence, e.derived_at_kind, e.derived_at_sha
         FROM entities e
         LEFT JOIN edges ed ON ed.to_entity_id = e.id AND ed.kind = 'calls'
         WHERE e.corpus_id = ?1 AND ed.id IS NULL",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_entity)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::CalError::from)
}

/// Entities in `corpus_id` that have no inbound `verified_by` edges (no test covers them).
pub fn entities_without_verified_by(db: &Database, corpus_id: &str) -> Result<Vec<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT e.id, e.corpus_id, e.canonical_name, e.kind, e.abstract_kind,
                e.aliases, e.description,
                e.first_location_uri, e.last_location_uri,
                e.appearance_count, e.confidence, e.derived_at_kind, e.derived_at_sha
         FROM entities e
         LEFT JOIN edges ed ON ed.to_entity_id = e.id AND ed.kind = 'verified_by'
         WHERE e.corpus_id = ?1 AND ed.id IS NULL",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_entity)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::CalError::from)
}
