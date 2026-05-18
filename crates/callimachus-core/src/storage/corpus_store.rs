use crate::error::{CalError, Result};
use crate::storage::db::Database;
use crate::types::corpus::{Corpus, CorpusStatus};
use rusqlite::params;
use std::str::FromStr;

pub fn insert(db: &Database, corpus: &Corpus) -> Result<()> {
    db.conn().execute(
        "INSERT INTO corpora (id, name, kind, source, config, status, created_at, last_indexed_at, pipeline_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            corpus.id,
            corpus.name,
            corpus.kind,
            corpus.source,
            serde_json::to_string(&corpus.config)?,
            corpus.status.to_string(),
            corpus.created_at,
            corpus.last_indexed_at,
            corpus.pipeline_version as i64,
        ],
    )?;
    Ok(())
}

pub fn list(db: &Database) -> Result<Vec<Corpus>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, name, kind, source, config, status, created_at, last_indexed_at, pipeline_version
         FROM corpora ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], row_to_corpus)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

pub fn get(db: &Database, id: &str) -> Result<Option<Corpus>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, name, kind, source, config, status, created_at, last_indexed_at, pipeline_version
         FROM corpora WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_corpus)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn require(db: &Database, id: &str) -> Result<Corpus> {
    get(db, id)?.ok_or_else(|| CalError::CorpusNotFound(id.to_string()))
}

pub fn update_status(db: &Database, id: &str, status: CorpusStatus) -> Result<()> {
    let updated = db.conn().execute(
        "UPDATE corpora SET status = ?1 WHERE id = ?2",
        params![status.to_string(), id],
    )?;
    if updated == 0 {
        return Err(CalError::CorpusNotFound(id.to_string()));
    }
    Ok(())
}

pub fn set_last_indexed(db: &Database, id: &str, at: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE corpora SET last_indexed_at = ?1, status = 'ready' WHERE id = ?2",
        params![at, id],
    )?;
    Ok(())
}

pub fn delete(db: &Database, id: &str) -> Result<bool> {
    let deleted = db
        .conn()
        .execute("DELETE FROM corpora WHERE id = ?1", params![id])?;
    Ok(deleted > 0)
}

pub fn exists(db: &Database, id: &str) -> Result<bool> {
    let count: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM corpora WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

pub fn set_pipeline_version(db: &Database, id: &str, version: u32) -> Result<()> {
    let updated = db.conn().execute(
        "UPDATE corpora SET pipeline_version = ?1 WHERE id = ?2",
        params![version as i64, id],
    )?;
    if updated == 0 {
        return Err(CalError::CorpusNotFound(id.to_string()));
    }
    Ok(())
}

fn row_to_corpus(row: &rusqlite::Row<'_>) -> rusqlite::Result<Corpus> {
    let config_str: String = row.get(4)?;
    let config: serde_json::Value =
        serde_json::from_str(&config_str).unwrap_or(serde_json::Value::Object(Default::default()));
    let status_str: String = row.get(5)?;
    let status = CorpusStatus::from_str(&status_str).unwrap_or(CorpusStatus::Registered);
    Ok(Corpus {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        source: row.get(3)?,
        config,
        status,
        created_at: row.get(6)?,
        last_indexed_at: row.get(7)?,
        pipeline_version: row.get::<_, i64>(8)? as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Database;
    use crate::types::corpus::Corpus;

    #[test]
    fn insert_and_list() {
        let db = Database::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "xenos".into(),
            "Xenos".into(),
            "book".into(),
            "/tmp/xenos.epub".into(),
        );
        insert(&db, &corpus).unwrap();
        let list = list(&db).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "xenos");
    }

    #[test]
    fn get_missing_returns_none() {
        let db = Database::open_in_memory().unwrap();
        assert!(get(&db, "nope").unwrap().is_none());
    }

    #[test]
    fn delete_returns_true_on_success() {
        let db = Database::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "xenos".into(),
            "Xenos".into(),
            "book".into(),
            "/tmp/xenos.epub".into(),
        );
        insert(&db, &corpus).unwrap();
        assert!(delete(&db, "xenos").unwrap());
        assert!(!delete(&db, "xenos").unwrap());
    }
}
