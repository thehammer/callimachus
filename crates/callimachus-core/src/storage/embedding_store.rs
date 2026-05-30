use crate::{error::Result, storage::Database, types::provenance::Provenance};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};

/// A stored embedding record.
///
/// The `vector` field holds all `dimensions` float values as `f32` in
/// little-endian byte order (serialized via `bytemuck`).
///
/// ## Memory note
///
/// `list_for_corpus` loads **all** embeddings for a corpus into memory.
/// This is adequate for corpora up to ~50k chunks. For larger corpora
/// consider a future `sqlite-vec`-backed implementation.
#[derive(Debug, Clone)]
pub struct StoredEmbedding {
    pub id: String,
    pub corpus_id: String,
    pub chunk_id: String,
    pub model: String,
    pub vector: Vec<f32>,
    pub dimensions: usize,
}

impl StoredEmbedding {
    pub fn new(
        corpus_id: impl Into<String>,
        chunk_id: impl Into<String>,
        model: impl Into<String>,
        vector: Vec<f32>,
    ) -> Self {
        let dimensions = vector.len();
        // ID: sha256 of chunk_id + model so re-embedding with the same model is idempotent.
        use sha2::{Digest, Sha256};
        let chunk_id = chunk_id.into();
        let model = model.into();
        let mut hasher = Sha256::new();
        hasher.update(chunk_id.as_bytes());
        hasher.update(b"|");
        hasher.update(model.as_bytes());
        let id = hex::encode(hasher.finalize());
        Self {
            id,
            corpus_id: corpus_id.into(),
            chunk_id,
            model,
            vector,
            dimensions,
        }
    }
}

fn encode(v: &[f32]) -> Vec<u8> {
    bytemuck::cast_slice(v).to_vec()
}

fn decode(bytes: &[u8]) -> Vec<f32> {
    bytemuck::cast_slice(bytes).to_vec()
}

/// Insert or replace an embedding (idempotent by `id`).
pub fn upsert(db: &Database, emb: &StoredEmbedding) -> Result<()> {
    let blob = encode(&emb.vector);
    db.conn().execute(
        "INSERT OR REPLACE INTO embeddings
         (id, corpus_id, chunk_id, model, vector, dimensions, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            emb.id,
            emb.corpus_id,
            emb.chunk_id,
            emb.model,
            blob,
            emb.dimensions as i64,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Commit an embedding as the head row for its `(chunk_id, model)` pair,
/// stamping `provenance`, and archive any prior head row whose derivation SHA
/// differs into `embeddings_history` (superseded at `provenance.sha()`).
///
/// This is the history-aware path used by the embed pass. Re-committing an
/// identical embedding (same `(chunk_id, model)` and same provenance SHA) is a
/// no-op archival — the head row is simply rewritten.
pub fn commit(db: &Database, emb: &StoredEmbedding, provenance: &Provenance) -> Result<()> {
    let (kind, sha) = provenance.to_columns();

    // Archive the about-to-be-superseded head row when the derivation SHA
    // changes (e.g. a re-embed under a new commit). When the SHA is unchanged
    // the existing row is just overwritten in place — no history churn.
    let existing_sha: Option<String> = db
        .conn()
        .query_row(
            "SELECT derived_at_sha FROM embeddings WHERE chunk_id = ?1 AND model = ?2",
            rusqlite::params![emb.chunk_id, emb.model],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(prev_sha) = existing_sha
        && prev_sha != sha
    {
        archive_for_chunk_model(db.conn(), &emb.chunk_id, &emb.model, sha)?;
    }

    let blob = encode(&emb.vector);
    db.conn().execute(
        "INSERT OR REPLACE INTO embeddings
         (id, corpus_id, chunk_id, model, vector, dimensions, created_at,
          derived_at_kind, derived_at_sha)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            emb.id,
            emb.corpus_id,
            emb.chunk_id,
            emb.model,
            blob,
            emb.dimensions as i64,
            Utc::now().to_rfc3339(),
            kind,
            sha,
        ],
    )?;
    Ok(())
}

/// Archive every head embedding for `chunk_id` into `embeddings_history`,
/// stamped `superseded_at_sha = superseded_at_version`. Called by the cascade
/// just before a dirty chunk (and, by FK cascade, its embeddings) is deleted, so
/// the vector survives in history with honest supersession provenance.
///
/// Returns the number of history rows written.
pub(crate) fn archive_for_chunk(
    conn: &Connection,
    chunk_id: &str,
    superseded_at_version: &str,
) -> Result<u64> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT OR IGNORE INTO embeddings_history
           (id, corpus_id, chunk_id, model, vector, dimensions,
            surrounding_context_hash, created_at,
            derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at)
         SELECT id, corpus_id, chunk_id, model, vector, dimensions,
                surrounding_context_hash, created_at,
                derived_at_kind, derived_at_sha, ?2, ?3
         FROM embeddings
         WHERE chunk_id = ?1",
        rusqlite::params![chunk_id, superseded_at_version, now],
    )?;
    Ok(rows as u64)
}

/// Archive only the head embedding for a specific `(chunk_id, model)` pair.
/// Used by [`commit`] when a re-embed supersedes a prior vector for the same
/// chunk identity (rather than the chunk dying entirely).
fn archive_for_chunk_model(
    conn: &Connection,
    chunk_id: &str,
    model: &str,
    superseded_at_sha: &str,
) -> Result<u64> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT OR IGNORE INTO embeddings_history
           (id, corpus_id, chunk_id, model, vector, dimensions,
            surrounding_context_hash, created_at,
            derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at)
         SELECT id, corpus_id, chunk_id, model, vector, dimensions,
                surrounding_context_hash, created_at,
                derived_at_kind, derived_at_sha, ?3, ?4
         FROM embeddings
         WHERE chunk_id = ?1 AND model = ?2",
        rusqlite::params![chunk_id, model, superseded_at_sha, now],
    )?;
    Ok(rows as u64)
}

/// Fetch the embedding for a specific chunk, if one exists.
pub fn get_for_chunk(db: &Database, chunk_id: &str) -> Result<Option<StoredEmbedding>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, chunk_id, model, vector, dimensions
         FROM embeddings WHERE chunk_id = ?1 LIMIT 1",
    )?;

    let mut rows = stmt.query(rusqlite::params![chunk_id])?;
    if let Some(row) = rows.next()? {
        let blob: Vec<u8> = row.get(4)?;
        let dimensions: i64 = row.get(5)?;
        Ok(Some(StoredEmbedding {
            id: row.get(0)?,
            corpus_id: row.get(1)?,
            chunk_id: row.get(2)?,
            model: row.get(3)?,
            vector: decode(&blob),
            dimensions: dimensions as usize,
        }))
    } else {
        Ok(None)
    }
}

/// Load **all** embeddings for a corpus.
///
/// Caller is responsible for memory — this loads all vectors for the corpus
/// into RAM for in-process cosine similarity search.
pub fn list_for_corpus(db: &Database, corpus_id: &str) -> Result<Vec<StoredEmbedding>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, chunk_id, model, vector, dimensions
         FROM embeddings WHERE corpus_id = ?1",
    )?;

    let rows = stmt.query_map(rusqlite::params![corpus_id], |row| {
        let blob: Vec<u8> = row.get(4)?;
        let dimensions: i64 = row.get(5)?;
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            blob,
            dimensions,
        ))
    })?;

    let mut result = Vec::new();
    for row in rows {
        let (id, corpus_id, chunk_id, model, blob, dimensions) = row?;
        result.push(StoredEmbedding {
            id,
            corpus_id,
            chunk_id,
            model,
            vector: decode(&blob),
            dimensions: dimensions as usize,
        });
    }
    Ok(result)
}

/// Count embeddings for a corpus.
pub fn count(db: &Database, corpus_id: &str) -> Result<u64> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM embeddings WHERE corpus_id = ?1",
        rusqlite::params![corpus_id],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Database, chunk_store, corpus_store};
    use crate::types::{Chunk, Corpus, Location};

    fn setup() -> (Database, Corpus) {
        let db = Database::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "c1".into(),
            "Test".into(),
            "wiki".into(),
            "/tmp/test".into(),
        );
        corpus_store::insert(&db, &corpus).unwrap();
        (db, corpus)
    }

    fn seed_chunk(db: &Database, corpus_id: &str, path: &str, content: &str) -> Chunk {
        let loc = Location::new(corpus_id, path);
        let chunk = Chunk::new(corpus_id.into(), None, "page".into(), loc, content.into());
        chunk_store::upsert(db, &chunk).unwrap();
        chunk
    }

    #[test]
    fn upsert_and_get_round_trip() {
        let (db, corpus) = setup();
        let chunk = seed_chunk(&db, &corpus.id, "wiki/page-1", "test content");
        let emb = StoredEmbedding::new(&corpus.id, &chunk.id, "test-model", vec![1.0, 0.0, 0.0]);
        upsert(&db, &emb).unwrap();

        let got = get_for_chunk(&db, &chunk.id).unwrap().unwrap();
        assert_eq!(got.corpus_id, corpus.id);
        assert_eq!(got.model, "test-model");
        assert_eq!(got.dimensions, 3);
        assert!((got.vector[0] - 1.0).abs() < 1e-6);
        assert!((got.vector[1]).abs() < 1e-6);
    }

    #[test]
    fn upsert_is_idempotent() {
        let (db, corpus) = setup();
        let chunk = seed_chunk(&db, &corpus.id, "wiki/page-1", "content one");
        let emb = StoredEmbedding::new(&corpus.id, &chunk.id, "model", vec![0.5; 4]);
        upsert(&db, &emb).unwrap();
        upsert(&db, &emb).unwrap(); // second upsert — should not error
        assert_eq!(count(&db, &corpus.id).unwrap(), 1);
    }

    #[test]
    fn list_for_corpus_loads_all() {
        let (db, corpus) = setup();
        for i in 0..3u32 {
            let chunk = seed_chunk(
                &db,
                &corpus.id,
                &format!("wiki/page-{i}"),
                &format!("content {i}"),
            );
            let emb = StoredEmbedding::new(&corpus.id, &chunk.id, "model", vec![i as f32, 0.0]);
            upsert(&db, &emb).unwrap();
        }
        let all = list_for_corpus(&db, &corpus.id).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn get_for_chunk_returns_none_when_missing() {
        let (db, _) = setup();
        let result = get_for_chunk(&db, "nonexistent-chunk-id").unwrap();
        assert!(result.is_none());
    }
}
