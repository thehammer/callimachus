use crate::error::Result;
use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};
use std::path::Path;

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("../../migrations/001_initial.sql")),
        M::up(include_str!("../../migrations/002_fts.sql")),
        M::up(include_str!("../../migrations/003_embeddings.sql")),
        M::up(include_str!("../../migrations/004_libraries.sql")),
        M::up(include_str!("../../migrations/005_collections.sql")),
        M::up(include_str!("../../migrations/006_contracts_themes.sql")),
        M::up(include_str!("../../migrations/007_pipeline_version.sql")),
        M::up(include_str!("../../migrations/008_kind_taxonomy.sql")),
        M::up(include_str!("../../migrations/009_change_manifest.sql")),
        M::up(include_str!(
            "../../migrations/010_multi_model_artifacts.sql"
        )),
        M::up(include_str!("../../migrations/011_scholia.sql")),
        M::up(include_str!(
            "../../migrations/012_provenance_and_history.sql"
        )),
        M::up(include_str!("../../migrations/013_honest_provenance.sql")),
    ])
}

/// Owned database handle. Wraps a `rusqlite::Connection` and ensures
/// all schema migrations have been applied on open.
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open (or create) a database at the given path, running any pending migrations.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut conn = Connection::open(path)?;
        Self::configure(&conn)?;
        migrations().to_latest(&mut conn)?;
        Ok(Self { conn })
    }

    /// Open an in-memory database. Useful for tests.
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        migrations().to_latest(&mut conn)?;
        Ok(Self { conn })
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "foreign_keys", "on")?;
        conn.pragma_update(None, "synchronous", "normal")?;
        Ok(())
    }

    /// Access the underlying connection for store operations.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mutable access for transactions.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_in_memory() {
        let db = Database::open_in_memory().unwrap();
        // Verify schema exists
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM corpora", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
