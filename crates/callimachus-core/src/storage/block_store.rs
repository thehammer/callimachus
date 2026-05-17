use crate::error::Result;
use crate::storage::db::Database;
use crate::types::EntityBlock;
use rusqlite::params;

pub fn upsert(db: &Database, b: &EntityBlock) -> Result<()> {
    db.conn().execute(
        "INSERT OR REPLACE INTO entity_blocks
         (id, entity_id, corpus_id, label, description, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            b.id,
            b.entity_id,
            b.corpus_id,
            b.label,
            b.description,
            b.position
        ],
    )?;
    Ok(())
}

pub fn list_for_entity(db: &Database, entity_id: &str) -> Result<Vec<EntityBlock>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, entity_id, corpus_id, label, description, position
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
    })
}
