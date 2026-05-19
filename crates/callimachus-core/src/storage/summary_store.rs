use crate::error::Result;
use crate::storage::db::Database;
use crate::types::summary::{Summary, SummaryTargetKind};
use rusqlite::params;
use std::str::FromStr;

pub fn upsert(db: &Database, summary: &Summary) -> Result<()> {
    db.conn().execute(
        "INSERT INTO summaries
         (id, corpus_id, target_kind, target_id, depth, text, model, model_tier, generated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(corpus_id, target_kind, target_id, model) DO UPDATE SET
             text         = excluded.text,
             model_tier   = excluded.model_tier,
             generated_at = excluded.generated_at,
             depth        = excluded.depth",
        params![
            summary.id,
            summary.corpus_id,
            summary.target_kind.to_string(),
            summary.target_id,
            summary.depth,
            summary.text,
            summary.model,
            summary.model_tier,
            summary.generated_at,
        ],
    )?;
    Ok(())
}

/// Return the highest-tier summary for the target. opus > sonnet > haiku > unknown.
/// Ties broken by `generated_at DESC`.
pub fn get_best(
    db: &Database,
    corpus_id: &str,
    target_kind: &SummaryTargetKind,
    target_id: &str,
) -> Result<Option<Summary>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, target_kind, target_id, depth, text, model, model_tier, generated_at
         FROM summaries
         WHERE corpus_id = ?1 AND target_kind = ?2 AND target_id = ?3
         ORDER BY CASE model_tier
                    WHEN 'opus'    THEN 3
                    WHEN 'sonnet'  THEN 2
                    WHEN 'haiku'   THEN 1
                    ELSE 0
                  END DESC,
                  generated_at DESC
         LIMIT 1",
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

/// Return the summary produced by an exact model name, or `None` if not found.
pub fn get_for_model(
    db: &Database,
    corpus_id: &str,
    target_kind: &SummaryTargetKind,
    target_id: &str,
    model: &str,
) -> Result<Option<Summary>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, target_kind, target_id, depth, text, model, model_tier, generated_at
         FROM summaries
         WHERE corpus_id = ?1 AND target_kind = ?2 AND target_id = ?3 AND model = ?4",
    )?;
    let mut rows = stmt.query_map(
        params![corpus_id, target_kind.to_string(), target_id, model],
        row_to_summary,
    )?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<Summary>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, target_kind, target_id, depth, text, model, model_tier, generated_at
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
        model_tier: row.get(7)?,
        generated_at: row.get(8)?,
    })
}
