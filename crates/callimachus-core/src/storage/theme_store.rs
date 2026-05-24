use crate::error::Result;
use crate::storage::{db::Database, history};
use crate::types::Theme;
use rusqlite::params;

pub fn upsert(db: &Database, t: &Theme) -> Result<()> {
    let conn = db.conn();
    let new_ver = t.derived_at_version.as_deref();

    // Snapshot before overwrite.
    if let Some(ver) = new_ver {
        history::snapshot_if_version_changed_theme(conn, &t.id, &t.corpus_id, Some(ver), ver)?;
    }

    conn.execute(
        "INSERT INTO themes
         (id, corpus_id, title, statement, confidence, model, model_tier, generated_at,
          derived_at_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
             title              = excluded.title,
             statement          = excluded.statement,
             confidence         = excluded.confidence,
             model              = excluded.model,
             model_tier         = excluded.model_tier,
             generated_at       = excluded.generated_at,
             derived_at_version = COALESCE(excluded.derived_at_version, themes.derived_at_version)",
        params![
            t.id,
            t.corpus_id,
            t.title,
            t.statement,
            t.confidence,
            t.model,
            t.model_tier,
            t.generated_at,
            t.derived_at_version,
        ],
    )?;
    Ok(())
}

pub fn get(db: &Database, id: &str) -> Result<Option<Theme>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, title, statement, confidence, model, model_tier, generated_at,
                derived_at_version
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
        "SELECT id, corpus_id, title, statement, confidence, model, model_tier, generated_at,
                derived_at_version
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
        derived_at_version: row.get(8)?,
    })
}
