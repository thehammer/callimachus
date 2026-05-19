use crate::corrections::types::{Correction, CorrectionKind};
use crate::error::{CalError, Result};
use crate::storage::db::Database;
use rusqlite::params;
use uuid::Uuid;

/// Insert a correction. Exactly one of `corpus_id` / `collection_id` must be `Some`.
pub fn insert(
    db: &Database,
    corpus_id: Option<&str>,
    collection_id: Option<&str>,
    kind: &CorrectionKind,
) -> Result<String> {
    match (corpus_id, collection_id) {
        (Some(_), None) | (None, Some(_)) => {}
        _ => {
            return Err(CalError::Other(
                "correction must have exactly one of corpus_id or collection_id".into(),
            ));
        }
    }

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let kind_name = kind.kind_name();
    let payload = serde_json::to_string(kind)?;
    db.conn().execute(
        "INSERT INTO scholia (id, corpus_id, collection_id, kind, payload, applied_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, corpus_id, collection_id, kind_name, payload, now],
    )?;
    Ok(id)
}

/// List corpus-scoped corrections for one corpus, ordered by applied_at ASC.
/// Collection-scoped corrections (EntityLink) are excluded.
pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<Correction>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, collection_id, payload, applied_at FROM scholia
         WHERE corpus_id = ?1 ORDER BY applied_at ASC",
    )?;
    collect_corrections(stmt.query_map(params![corpus_id], row_to_correction)?)
}

/// List collection-scoped corrections (EntityLink records) for one collection.
pub fn list_for_collection(db: &Database, collection_id: &str) -> Result<Vec<Correction>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, collection_id, payload, applied_at FROM scholia
         WHERE collection_id = ?1 ORDER BY applied_at ASC",
    )?;
    collect_corrections(stmt.query_map(params![collection_id], row_to_correction)?)
}

/// List corrections for ALL scopes, ordered by applied_at ASC.
pub fn list_all(db: &Database) -> Result<Vec<Correction>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, collection_id, payload, applied_at FROM scholia
         ORDER BY applied_at ASC",
    )?;
    collect_corrections(stmt.query_map([], row_to_correction)?)
}

/// Delete a correction by ID. Returns `true` if a row was deleted.
pub fn delete(db: &Database, correction_id: &str) -> Result<bool> {
    let n = db
        .conn()
        .execute("DELETE FROM scholia WHERE id = ?1", params![correction_id])?;
    Ok(n > 0)
}

// ── Row mapper ────────────────────────────────────────────────────────────────

fn row_to_correction(row: &rusqlite::Row<'_>) -> rusqlite::Result<Correction> {
    let id: String = row.get(0)?;
    let corpus_id: Option<String> = row.get(1)?;
    let collection_id: Option<String> = row.get(2)?;
    let payload_str: String = row.get(3)?;
    let applied_at: String = row.get(4)?;

    let kind: CorrectionKind = serde_json::from_str(&payload_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(Correction {
        id,
        corpus_id,
        collection_id,
        kind,
        applied_at,
    })
}

fn collect_corrections(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Correction>>,
) -> Result<Vec<Correction>> {
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}
