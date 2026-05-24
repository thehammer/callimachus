use crate::error::Result;
use crate::storage::{db::Database, history};
use crate::types::EntityBlock;
use rusqlite::params;

pub fn upsert(db: &Database, b: &EntityBlock) -> Result<()> {
    let conn = db.conn();
    let new_ver = b.derived_at_version.as_deref();

    // Snapshot before overwrite.
    if let Some(ver) = new_ver {
        history::snapshot_if_version_changed_block(conn, &b.id, &b.corpus_id, Some(ver), ver)?;
    }

    conn.execute(
        "INSERT INTO entity_blocks
         (id, entity_id, corpus_id, label, description, position, derived_at_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
             label              = excluded.label,
             description        = excluded.description,
             position           = excluded.position,
             derived_at_version = COALESCE(excluded.derived_at_version, entity_blocks.derived_at_version)",
        params![
            b.id,
            b.entity_id,
            b.corpus_id,
            b.label,
            b.description,
            b.position,
            b.derived_at_version,
        ],
    )?;
    Ok(())
}

pub fn list_for_entity(db: &Database, entity_id: &str) -> Result<Vec<EntityBlock>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, entity_id, corpus_id, label, description, position, derived_at_version
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
    Ok(EntityBlock {
        id: row.get(0)?,
        entity_id: row.get(1)?,
        corpus_id: row.get(2)?,
        label: row.get(3)?,
        description: row.get(4)?,
        position: row.get(5)?,
        derived_at_version: row.get(6)?,
    })
}
