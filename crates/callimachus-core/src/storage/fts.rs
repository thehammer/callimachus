use crate::error::Result;
use crate::storage::db::Database;
use rusqlite::params;

/// Rebuild the FTS index from the chunks table. Use after bulk inserts
/// that bypassed the triggers (e.g. a full corpus reimport).
pub fn rebuild(db: &Database, corpus_id: &str) -> Result<()> {
    // Delete existing FTS rows for this corpus by joining on rowid.
    db.conn().execute_batch(&format!(
        "INSERT INTO chunks_fts(chunks_fts) VALUES ('delete-all');
         INSERT INTO chunks_fts(rowid, content)
             SELECT rowid, content FROM chunks WHERE corpus_id = '{corpus_id}';"
    ))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct FtsResult {
    pub location_uri: String,
    pub snippet: String,
    pub rank: f64,
}

/// Full-text keyword search across chunks for a corpus.
pub fn search(db: &Database, corpus_id: &str, query: &str, limit: usize) -> Result<Vec<FtsResult>> {
    // Map the FTS rowid back to the chunks table to get the location_uri and corpus_id filter.
    let mut stmt = db.conn().prepare(
        "SELECT c.location_uri,
                snippet(chunks_fts, 0, '...', '...', '...', 20) AS snip,
                fts.rank
         FROM chunks_fts fts
         JOIN chunks c ON c.rowid = fts.rowid
         WHERE chunks_fts MATCH ?1
           AND c.corpus_id = ?2
         ORDER BY fts.rank
         LIMIT ?3",
    )?;
    let mut results = vec![];
    let rows = stmt.query_map(params![query, corpus_id, limit as i64], |row| {
        Ok(FtsResult {
            location_uri: row.get(0)?,
            snippet: row.get(1)?,
            rank: row.get(2)?,
        })
    })?;
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}
