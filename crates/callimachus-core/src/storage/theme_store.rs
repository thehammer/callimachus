use crate::error::Result;
use crate::storage::{db::Database, history};
use crate::types::provenance::Provenance;
use crate::types::Theme;
use rusqlite::params;

pub fn upsert(db: &Database, t: &Theme) -> Result<()> {
    let conn = db.conn();
    let new_sha = t.provenance.as_ref().map(|p| p.sha());

    // Snapshot before overwrite.
    if let Some(sha) = new_sha {
        history::snapshot_if_version_changed_theme(conn, &t.id, &t.corpus_id, Some(sha), sha)?;
    }

    let prov_kind = t.provenance.as_ref().map(|p| p.kind_str());
    let prov_sha = t.provenance.as_ref().map(|p| p.sha());

    conn.execute(
        "INSERT INTO themes
         (id, corpus_id, title, statement, confidence, model, model_tier, generated_at,
          derived_at_kind, derived_at_sha)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, COALESCE(?9, 'concrete'), COALESCE(?10, ''))
         ON CONFLICT(id) DO UPDATE SET
             title            = excluded.title,
             statement        = excluded.statement,
             confidence       = excluded.confidence,
             model            = excluded.model,
             model_tier       = excluded.model_tier,
             generated_at     = excluded.generated_at,
             derived_at_kind  = CASE WHEN excluded.derived_at_sha != '' THEN excluded.derived_at_kind ELSE themes.derived_at_kind END,
             derived_at_sha   = CASE WHEN excluded.derived_at_sha != '' THEN excluded.derived_at_sha  ELSE themes.derived_at_sha  END",
        params![
            t.id,
            t.corpus_id,
            t.title,
            t.statement,
            t.confidence,
            t.model,
            t.model_tier,
            t.generated_at,
            prov_kind,
            prov_sha,
        ],
    )?;
    Ok(())
}

pub fn get(db: &Database, id: &str) -> Result<Option<Theme>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, title, statement, confidence, model, model_tier, generated_at,
                derived_at_kind, derived_at_sha
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
                derived_at_kind, derived_at_sha
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
    // Column order: id(0), corpus_id(1), title(2), statement(3), confidence(4),
    //               model(5), model_tier(6), generated_at(7),
    //               derived_at_kind(8), derived_at_sha(9)
    let prov_kind: Option<String> = row.get(8)?;
    let prov_sha: Option<String> = row.get(9)?;
    let provenance = match (prov_kind.as_deref(), prov_sha.as_deref()) {
        (Some(k), Some(s)) if !s.is_empty() => Provenance::from_columns(k, s).ok(),
        _ => None,
    };
    Ok(Theme {
        id: row.get(0)?,
        corpus_id: row.get(1)?,
        title: row.get(2)?,
        statement: row.get(3)?,
        confidence: row.get(4)?,
        model: row.get(5)?,
        model_tier: row.get(6)?,
        generated_at: row.get(7)?,
        provenance,
    })
}
