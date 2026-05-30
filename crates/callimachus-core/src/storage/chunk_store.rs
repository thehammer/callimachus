use crate::error::{CalError, Result};
use crate::storage::db::Database;
use crate::types::chunk::Chunk;
use crate::types::location::Location;
use rusqlite::params;

pub fn upsert(db: &Database, chunk: &Chunk) -> Result<()> {
    // Use INSERT OR IGNORE so that re-processing an unchanged chunk (same content
    // hash ⇒ same ID) is a no-op.  introduced_at_version is only written on
    // first insert; subsequent runs update it via set_history().
    db.conn().execute(
        "INSERT OR IGNORE INTO chunks
         (id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at,
          source_hash, introduced_at_version, last_modified_at_version, file_shape_hash,
          entity_id_list)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            chunk.id,
            chunk.corpus_id,
            chunk.parent_path,
            chunk.kind,
            chunk.location.uri,
            chunk.content,
            chunk.byte_length as i64,
            chunk.created_at,
            chunk.source_hash,
            chunk.introduced_at_version,
            chunk.last_modified_at_version,
            chunk.file_shape_hash,
            chunk.entity_id_list,
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
        "SELECT id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at, source_hash, introduced_at_version, last_modified_at_version, file_shape_hash, entity_id_list
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
        "SELECT id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at, source_hash, introduced_at_version, last_modified_at_version, file_shape_hash, entity_id_list
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
        "SELECT id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at, source_hash, introduced_at_version, last_modified_at_version, file_shape_hash, entity_id_list
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
        "SELECT id, corpus_id, parent_path, kind, location_uri, content, byte_length, created_at, source_hash, introduced_at_version, last_modified_at_version, file_shape_hash, entity_id_list
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

/// Set the source_hash for a chunk (SHA-256 of the source file content).
pub fn set_source_hash(db: &Database, chunk_id: &str, hash: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE chunks SET source_hash = ?1 WHERE id = ?2",
        params![hash, chunk_id],
    )?;
    Ok(())
}

/// Write history metadata for a chunk.
///
/// Always updates `last_modified_at_version`.  Sets `introduced_at_version`
/// only if the column is currently NULL (i.e. first time we see this chunk).
/// Also sets `last_modified_commit_message` and `last_modified_author` when provided.
pub fn set_history(
    db: &Database,
    chunk_id: &str,
    version: &str,
    commit_message: Option<&str>,
    author: Option<&str>,
) -> Result<()> {
    db.conn().execute(
        "UPDATE chunks
         SET last_modified_at_version = ?1,
             last_modified_commit_message = COALESCE(?2, last_modified_commit_message),
             last_modified_author = COALESCE(?3, last_modified_author),
             introduced_at_version = COALESCE(introduced_at_version, ?1)
         WHERE id = ?4",
        params![version, commit_message, author, chunk_id],
    )?;
    Ok(())
}

/// Return `(chunk_id, location_uri, source_hash)` for all chunks in a corpus.
/// Rows where source_hash is NULL are returned with an empty string.
pub fn list_source_paths(db: &Database, corpus_id: &str) -> Result<Vec<(String, String, String)>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, location_uri, COALESCE(source_hash, '') FROM chunks WHERE corpus_id = ?1",
    )?;
    let rows = stmt.query_map(params![corpus_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
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
        source_hash: row.get(8)?,
        introduced_at_version: row.get(9)?,
        last_modified_at_version: row.get(10)?,
        file_shape_hash: row.get(11)?,
        entity_id_list: row.get(12)?,
    })
}

/// Store the file-shape hash and its entity-id-list JSON on a chunk.
pub fn set_file_shape(
    db: &Database,
    chunk_id: &str,
    file_shape_hash: &str,
    entity_id_list: &str,
) -> Result<()> {
    db.conn().execute(
        "UPDATE chunks SET file_shape_hash = ?1, entity_id_list = ?2 WHERE id = ?3",
        params![file_shape_hash, entity_id_list, chunk_id],
    )?;
    Ok(())
}
