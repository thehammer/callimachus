use crate::error::Result;
use crate::storage::{db::Database, history};
use crate::types::provenance::Provenance;
use crate::types::EntityBlock;
use rusqlite::params;

pub fn upsert(db: &Database, b: &EntityBlock) -> Result<()> {
    let conn = db.conn();
    let new_sha = b.provenance.as_ref().map(|p| p.sha());

    // Snapshot before overwrite.
    if let Some(sha) = new_sha {
        history::snapshot_if_version_changed_block(conn, &b.id, &b.corpus_id, Some(sha), sha)?;
    }

    let prov_kind = b.provenance.as_ref().map(|p| p.kind_str());
    let prov_sha = b.provenance.as_ref().map(|p| p.sha());

    conn.execute(
        "INSERT INTO entity_blocks
         (id, entity_id, corpus_id, label, description, position, derived_at_kind, derived_at_sha)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, 'concrete'), COALESCE(?8, ''))
         ON CONFLICT(id) DO UPDATE SET
             label            = excluded.label,
             description      = excluded.description,
             position         = excluded.position,
             derived_at_kind  = CASE WHEN excluded.derived_at_sha != '' THEN excluded.derived_at_kind ELSE entity_blocks.derived_at_kind END,
             derived_at_sha   = CASE WHEN excluded.derived_at_sha != '' THEN excluded.derived_at_sha  ELSE entity_blocks.derived_at_sha  END",
        params![
            b.id,
            b.entity_id,
            b.corpus_id,
            b.label,
            b.description,
            b.position,
            prov_kind,
            prov_sha,
        ],
    )?;
    Ok(())
}

pub fn list_for_entity(db: &Database, entity_id: &str) -> Result<Vec<EntityBlock>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, entity_id, corpus_id, label, description, position, derived_at_kind, derived_at_sha
         FROM entity_blocks WHERE entity_id = ?1 ORDER BY position ASC",
    )?;
    let rows = stmt.query_map(params![entity_id], row_to_block)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::CalError::from)
}

pub fn delete_for_entity(db: &Database, entity_id: &str) -> Result<()> {
    db.conn().execute(
        "DELETE FROM entity_blocks WHERE entity_id = ?1",
        params![entity_id],
    )?;
    Ok(())
}

fn row_to_block(row: &rusqlite::Row<'_>) -> rusqlite::Result<EntityBlock> {
    // Column order: id(0), entity_id(1), corpus_id(2), label(3), description(4),
    //               position(5), derived_at_kind(6), derived_at_sha(7)
    let prov_kind: Option<String> = row.get(6)?;
    let prov_sha: Option<String> = row.get(7)?;
    let provenance = match (prov_kind.as_deref(), prov_sha.as_deref()) {
        (Some(k), Some(s)) if !s.is_empty() => Provenance::from_columns(k, s).ok(),
        _ => None,
    };
    Ok(EntityBlock {
        id: row.get(0)?,
        entity_id: row.get(1)?,
        corpus_id: row.get(2)?,
        label: row.get(3)?,
        description: row.get(4)?,
        position: row.get(5)?,
        provenance,
    })
}
