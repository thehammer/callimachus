use crate::error::{CalError, Result};
use crate::storage::db::Database;
use crate::types::entity::Entity;
use crate::types::location::Location;
use rusqlite::params;

pub fn upsert(db: &Database, entity: &Entity) -> Result<()> {
    let aliases_json = serde_json::to_string(&entity.aliases)?;
    let first_uri = entity.first_location.as_ref().map(|l| &l.uri);
    let last_uri = entity.last_location.as_ref().map(|l| &l.uri);

    // On conflict: merge aliases, increment appearance_count, update location pointers,
    // take max confidence.
    db.conn().execute(
        "INSERT INTO entities
             (id, corpus_id, canonical_name, kind, aliases, description,
              first_location_uri, last_location_uri, appearance_count, confidence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
             aliases        = json_patch(aliases, excluded.aliases),
             description    = COALESCE(excluded.description, description),
             last_location_uri = COALESCE(excluded.last_location_uri, last_location_uri),
             appearance_count  = appearance_count + excluded.appearance_count,
             confidence        = MAX(confidence, excluded.confidence)",
        params![
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
        ],
    )?;
    Ok(())
}

pub fn get_by_id(db: &Database, id: &str) -> Result<Option<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, canonical_name, kind, aliases, description,
                first_location_uri, last_location_uri, appearance_count, confidence
         FROM entities WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_entity)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

pub fn find_by_name(db: &Database, corpus_id: &str, name: &str) -> Result<Vec<Entity>> {
    let pattern = format!("%{}%", name.to_lowercase());
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, canonical_name, kind, aliases, description,
                first_location_uri, last_location_uri, appearance_count, confidence
         FROM entities
         WHERE corpus_id = ?1
           AND (LOWER(canonical_name) LIKE ?2 OR LOWER(aliases) LIKE ?2)
         ORDER BY appearance_count DESC
         LIMIT 20",
    )?;
    let rows = stmt.query_map(params![corpus_id, pattern], row_to_entity)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, canonical_name, kind, aliases, description,
                first_location_uri, last_location_uri, appearance_count, confidence
         FROM entities WHERE corpus_id = ?1
         ORDER BY appearance_count DESC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_entity)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

pub fn count(db: &Database, corpus_id: &str) -> Result<u64> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM entities WHERE corpus_id = ?1",
        params![corpus_id],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}

pub fn list_all(db: &Database, corpus_id: &str) -> Result<Vec<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, canonical_name, kind, aliases, description,
                first_location_uri, last_location_uri, appearance_count, confidence
         FROM entities WHERE corpus_id = ?1
         ORDER BY canonical_name ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_entity)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

/// Merge `absorb_id` into `keep_id`: transfer all edges, delete the absorbed entity.
pub fn merge(db: &Database, keep_id: &str, absorb_id: &str) -> Result<()> {
    // Transfer edges referencing the absorbed entity.
    db.conn().execute(
        "UPDATE edges SET from_entity_id = ?1 WHERE from_entity_id = ?2",
        params![keep_id, absorb_id],
    )?;
    db.conn().execute(
        "UPDATE edges SET to_entity_id = ?1 WHERE to_entity_id = ?2",
        params![keep_id, absorb_id],
    )?;
    // Merge appearance_count into keeper.
    db.conn().execute(
        "UPDATE entities SET appearance_count = appearance_count +
         (SELECT appearance_count FROM entities WHERE id = ?2)
         WHERE id = ?1",
        params![keep_id, absorb_id],
    )?;
    // Delete the absorbed entity.
    db.conn()
        .execute("DELETE FROM entities WHERE id = ?1", params![absorb_id])?;
    Ok(())
}

pub fn top(db: &Database, corpus_id: &str, limit: usize) -> Result<Vec<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, canonical_name, kind, aliases, description,
                first_location_uri, last_location_uri, appearance_count, confidence
         FROM entities WHERE corpus_id = ?1
         ORDER BY appearance_count DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![corpus_id, limit as i64], row_to_entity)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

/// Returns entities whose `first_location_uri` or `last_location_uri` equals `uri`.
pub fn at_location(db: &Database, corpus_id: &str, uri: &str) -> Result<Vec<Entity>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, canonical_name, kind, aliases, description,
                first_location_uri, last_location_uri, appearance_count, confidence
         FROM entities
         WHERE corpus_id = ?1 AND (first_location_uri = ?2 OR last_location_uri = ?2)",
    )?;
    let rows = stmt.query_map(params![corpus_id, uri], row_to_entity)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

pub(crate) fn row_to_entity(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entity> {
    let aliases_json: String = row.get(4)?;
    let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
    let first_uri: Option<String> = row.get(6)?;
    let last_uri: Option<String> = row.get(7)?;
    Ok(Entity {
        id: row.get(0)?,
        corpus_id: row.get(1)?,
        canonical_name: row.get(2)?,
        kind: row.get(3)?,
        aliases,
        description: row.get(5)?,
        first_location: first_uri.and_then(|u| Location::parse(&u).ok()),
        last_location: last_uri.and_then(|u| Location::parse(&u).ok()),
        appearance_count: row.get::<_, i64>(8)? as u32,
        confidence: row.get::<_, f64>(9)? as f32,
    })
}
