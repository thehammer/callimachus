use crate::error::{CalError, Result};
use crate::storage::db::Database;
use crate::types::pass::{Pass, RunStatus};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PassStats {
    pub processed: u64,
    pub skipped: u64,
    pub failed: u64,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub cost_usd: Option<f64>,
    pub errors: Vec<ChunkError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkError {
    pub chunk_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub corpus_id: String,
    pub pass: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub stats: PassStats,
    pub provider: Option<String>,
}

pub fn start_run(
    db: &Database,
    corpus_id: &str,
    pass: &Pass,
    provider: Option<&str>,
) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    db.conn().execute(
        "INSERT INTO runs (id, corpus_id, pass, started_at, status, stats, provider)
         VALUES (?1, ?2, ?3, ?4, 'running', '{}', ?5)",
        params![id, corpus_id, pass.to_string(), now, provider],
    )?;
    Ok(id)
}

pub fn finish_run(db: &Database, run_id: &str, status: RunStatus, stats: &PassStats) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let stats_json = serde_json::to_string(stats)?;
    db.conn().execute(
        "UPDATE runs SET status = ?1, finished_at = ?2, stats = ?3 WHERE id = ?4",
        params![status.to_string(), now, stats_json, run_id],
    )?;
    Ok(())
}

pub fn latest_runs(db: &Database, corpus_id: &str) -> Result<Vec<RunRecord>> {
    latest_runs_n(db, corpus_id, 20)
}

pub fn latest_runs_n(db: &Database, corpus_id: &str, limit: usize) -> Result<Vec<RunRecord>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, pass, started_at, finished_at, status, stats, provider
         FROM runs WHERE corpus_id = ?1
         ORDER BY started_at DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![corpus_id, limit as i64], row_to_run)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

/// Mark any `status='running'` rows for this corpus as `status='failed'`.
/// Returns the number of rows updated.
pub fn abandon_stale(db: &Database, corpus_id: &str) -> Result<u64> {
    let now = chrono::Utc::now().to_rfc3339();
    let updated = db.conn().execute(
        "UPDATE runs SET status = 'failed', finished_at = ?1
         WHERE corpus_id = ?2 AND status = 'running'",
        params![now, corpus_id],
    )?;
    Ok(updated as u64)
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    let stats_json: String = row.get(6)?;
    let stats: PassStats = serde_json::from_str(&stats_json).unwrap_or_default();
    Ok(RunRecord {
        id: row.get(0)?,
        corpus_id: row.get(1)?,
        pass: row.get(2)?,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        status: row.get(5)?,
        stats,
        provider: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{corpus_store, db::Database};
    use crate::types::{Corpus, pass::Pass};

    fn make_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn seed_corpus(db: &Database, id: &str) {
        let corpus = Corpus::new(
            id.to_string(),
            "Test".to_string(),
            "book".to_string(),
            "/tmp".to_string(),
        );
        corpus_store::insert(db, &corpus).unwrap();
    }

    #[test]
    fn abandon_stale_flips_running_to_failed() {
        let db = make_db();
        seed_corpus(&db, "corpus-1");

        // Insert a running row directly.
        let run_id = start_run(&db, "corpus-1", &Pass::Chunk, None).unwrap();

        // Verify it's running.
        let runs = latest_runs(&db, "corpus-1").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "running");
        assert!(runs[0].finished_at.is_none());

        // Abandon stale runs.
        let count = abandon_stale(&db, "corpus-1").unwrap();
        assert_eq!(count, 1, "should have abandoned 1 run");

        // Verify the row is now failed with a finished_at timestamp.
        let runs = latest_runs(&db, "corpus-1").unwrap();
        assert_eq!(runs[0].status, "failed", "run {run_id} should be failed");
        assert!(runs[0].finished_at.is_some(), "finished_at should be set");
    }

    #[test]
    fn abandon_stale_ignores_completed_runs() {
        let db = make_db();
        seed_corpus(&db, "corpus-1");

        let run_id = start_run(&db, "corpus-1", &Pass::Chunk, None).unwrap();
        finish_run(
            &db,
            &run_id,
            crate::types::pass::RunStatus::Completed,
            &PassStats::default(),
        )
        .unwrap();

        let count = abandon_stale(&db, "corpus-1").unwrap();
        assert_eq!(count, 0, "completed runs should not be abandoned");
    }
}
