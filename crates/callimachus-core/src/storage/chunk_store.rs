use crate::error::{CalError, Result};
use crate::storage::db::Database;
use crate::types::chunk::Chunk;
use crate::types::location::Location;
use rusqlite::params;

pub fn upsert(db: &Database, chunk: &Chunk) -> Result<()> {
    db.conn().execute(
        "INSERT OR IGNORE INTO chunks
         (id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            chunk.id,
            chunk.corpus_id,
            chunk.parent_path,
            chunk.kind,
            chunk.location.uri,
            chunk.content,
            chunk.byte_length as i64,
            chunk.created_at,
        ],
    )?;
    Ok(())
}

pub fn has(db: &Database, id: &str) -> Result<bool> {
    let count: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM chunks WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

pub fn get(db: &Database, uri: &str) -> Result<Option<Chunk>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at
         FROM chunks WHERE location_uri = ?1",
    )?;
    let mut rows = stmt.query_map(params![uri], row_to_chunk)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn get_by_id(db: &Database, id: &str) -> Result<Option<Chunk>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at
         FROM chunks WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_chunk)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<Chunk>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at
         FROM chunks WHERE corpus_id = ?1 ORDER BY location_uri ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_chunk)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

pub fn count(db: &Database, corpus_id: &str) -> Result<u64> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM chunks WHERE corpus_id = ?1",
        params![corpus_id],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}

/// Return chunks that have not yet been semantically processed.
pub fn list_unprocessed(db: &Database, corpus_id: &str) -> Result<Vec<Chunk>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at
         FROM chunks WHERE corpus_id = ?1 AND semantic_processed = 0
         ORDER BY location_uri ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_chunk)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

pub fn set_semantic_processed(db: &Database, chunk_id: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE chunks SET semantic_processed = 1 WHERE id = ?1",
        params![chunk_id],
    )?;
    Ok(())
}

pub fn set_parent_path(db: &Database, chunk_id: &str, parent_path: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE chunks SET parent_path = ?1 WHERE id = ?2",
        params![parent_path, chunk_id],
    )?;
    Ok(())
}

/// Return all chunk IDs for a corpus (for orphan detection during reindex).
pub fn list_ids_for_corpus(db: &Database, corpus_id: &str) -> Result<Vec<String>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT id FROM chunks WHERE corpus_id = ?1")?;
    let rows = stmt.query_map(params![corpus_id], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

/// Delete a single chunk by ID. Also removes associated summary rows.
pub fn delete_by_id(db: &Database, chunk_id: &str) -> Result<bool> {
    let n = db
        .conn()
        .execute("DELETE FROM chunks WHERE id = ?1", params![chunk_id])?;
    Ok(n > 0)
}

/// Reset semantic_processed = 0 so the semantic pass will re-process this chunk.
pub fn reset_semantic_processed(db: &Database, chunk_id: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE chunks SET semantic_processed = 0 WHERE id = ?1",
        params![chunk_id],
    )?;
    Ok(())
}

/// Returns Location of chunks whose `parent_path` equals `parent_uri`, ordered by location.
pub fn children_by_uri(db: &Database, corpus_id: &str, parent_uri: &str) -> Result<Vec<Location>> {
    let mut stmt = db.conn().prepare(
        "SELECT location_uri FROM chunks WHERE corpus_id = ?1 AND parent_path = ?2
         ORDER BY location_uri ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id, parent_uri], |row| {
        row.get::<_, String>(0)
    })?;
    let mut locs = Vec::new();
    for r in rows {
        let uri = r.map_err(crate::error::CalError::from)?;
        let loc = Location::parse(&uri).unwrap_or_else(|_| Location {
            corpus_id: corpus_id.to_string(),
            path: uri.clone(),
            uri,
        });
        locs.push(loc);
    }
    Ok(locs)
}

fn row_to_chunk(row: &rusqlite::Row<'_>) -> rusqlite::Result<Chunk> {
    let uri: String = row.get(4)?;
    let location = Location::parse(&uri).unwrap_or_else(|_| Location {
        corpus_id: String::new(),
        path: uri.clone(),
        uri: uri.clone(),
    });
    Ok(Chunk {
        id: row.get(0)?,
        corpus_id: row.get(1)?,
        parent_path: row.get(2)?,
        kind: row.get(3)?,
        location,
        content: row.get(5)?,
        byte_length: row.get::<_, i64>(6)? as usize,
        created_at: row.get(7)?,
    })
}
