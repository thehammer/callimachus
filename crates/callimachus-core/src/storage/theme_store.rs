use crate::error::Result;
use crate::storage::db::Database;
use crate::types::Theme;
use rusqlite::params;

pub fn upsert(db: &Database, t: &Theme) -> Result<()> {
    db.conn().execute(
        "INSERT OR REPLACE INTO themes
         (id, corpus_id, title, statement, confidence, model, model_tier, generated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            t.id,
            t.corpus_id,
            t.title,
            t.statement,
            t.confidence,
            t.model,
            t.model_tier,
            t.generated_at
        ],
    )?;
    Ok(())
}

pub fn get(db: &Database, id: &str) -> Result<Option<Theme>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, title, statement, confidence, model, model_tier, generated_at
         FROM themes WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_theme)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<Theme>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, title, statement, confidence, model, model_tier, generated_at
         FROM themes WHERE corpus_id = ?1 ORDER BY confidence DESC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_theme)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::CalError::from)
}

pub fn delete_for_corpus(db: &Database, corpus_id: &str) -> Result<()> {
    db.conn().execute(
        "DELETE FROM themes WHERE corpus_id = ?1",
        params![corpus_id],
    )?;
    Ok(())
}

fn row_to_theme(row: &rusqlite::Row<'_>) -> rusqlite::Result<Theme> {
    Ok(Theme {
        id: row.get(0)?,
        corpus_id: row.get(1)?,
        title: row.get(2)?,
        statement: row.get(3)?,
        confidence: row.get(4)?,
        model: row.get(5)?,
        model_tier: row.get(6)?,
        generated_at: row.get(7)?,
    })
}
