use crate::error::Result;
use crate::storage::db::Database;
use crate::types::summary::{Summary, SummaryTargetKind};
use rusqlite::params;
use std::str::FromStr;

pub fn upsert(db: &Database, summary: &Summary) -> Result<()> {
    db.conn().execute(
        "INSERT OR REPLACE INTO summaries
         (id, corpus_id, target_kind, target_id, depth, text, model, generated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            summary.id,
            summary.corpus_id,
            summary.target_kind.to_string(),
            summary.target_id,
            summary.depth,
            summary.text,
            summary.model,
            summary.generated_at,
        ],
    )?;
    Ok(())
}

pub fn get(
    db: &Database,
    corpus_id: &str,
    target_kind: &SummaryTargetKind,
    target_id: &str,
) -> Result<Option<Summary>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, target_kind, target_id, depth, text, model, generated_at
         FROM summaries
         WHERE corpus_id = ?1 AND target_kind = ?2 AND target_id = ?3
         ORDER BY generated_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map(
        params![corpus_id, target_kind.to_string(), target_id],
        row_to_summary,
    )?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<Summary>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, target_kind, target_id, depth, text, model, generated_at
         FROM summaries WHERE corpus_id = ?1 ORDER BY generated_at ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_summary)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::CalError::from)
}

/// Delete all summaries whose `target_id` matches the given ID.
/// Used during reindex to remove orphaned summaries before chunk deletion.
pub fn delete_for_target(db: &Database, corpus_id: &str, target_id: &str) -> Result<()> {
    db.conn().execute(
        "DELETE FROM summaries WHERE corpus_id = ?1 AND target_id = ?2",
        params![corpus_id, target_id],
    )?;
    Ok(())
}

fn row_to_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    let target_kind_str: String = row.get(2)?;
    let target_kind =
        SummaryTargetKind::from_str(&target_kind_str).unwrap_or(SummaryTargetKind::Chunk);
    Ok(Summary {
        id: row.get(0)?,
        corpus_id: row.get(1)?,
        target_kind,
        target_id: row.get(3)?,
        depth: row.get(4)?,
        text: row.get(5)?,
        model: row.get(6)?,
        generated_at: row.get(7)?,
    })
}
