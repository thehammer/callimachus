use std::sync::Arc;

use callimachus_llm::LlmProvider;

use crate::{
    indexing::pipeline::IndexOptions,
    storage::{StorageBackend, embedding_store::StoredEmbedding, run_log::PassStats},
    types::Corpus,
};

const PROGRESS_EVERY: u64 = 25;

/// Run the embed pass for a corpus.
///
/// Iterates all chunks that don't yet have a stored embedding, calls
/// `embedder.embed(content)`, and stores the result. Already-embedded
/// chunks are skipped, making the pass idempotent.
///
/// If `embedder` is `None` (no embedding provider configured), this
/// function logs a warning and returns immediately with `processed = 0`.
///
/// The embed pass is **not** in `IndexOptions::default().passes`.
/// Request it explicitly via `--pass=embed` or `--pass=all`.
pub async fn run(
    db: &dyn StorageBackend,
    corpus: &Corpus,
    embedder: Option<Arc<dyn LlmProvider>>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let embedder = match embedder {
        Some(e) => e,
        None => {
            tracing::warn!(
                corpus_id = %corpus.id,
                "embed pass requested but no embedder configured; skipping"
            );
            return Ok(PassStats::default());
        }
    };

    if !embedder.supports_embeddings() {
        tracing::warn!(
            corpus_id = %corpus.id,
            provider = %embedder.name(),
            "embed pass: provider does not support embeddings; skipping"
        );
        return Ok(PassStats::default());
    }

    // Load all chunks for this corpus.
    let mut chunks = db.chunk_list(&corpus.id)?;

    // Skip chunks whose source file is unchanged according to the manifest.
    if let Some(m) = opts.change_manifest.as_ref() {
        chunks.retain(|c| m.is_dirty_for_chunk(c));
    }

    let mut stats = PassStats::default();

    for chunk in &chunks {
        // Skip if already embedded (unless --full).
        if !opts.full && db.embedding_get_for_chunk(&chunk.id)?.is_some() {
            stats.skipped += 1;
            continue;
        }

        // Generate embedding.
        match embedder.embed(&chunk.content).await {
            Ok(vector) => {
                let emb = StoredEmbedding::new(&corpus.id, &chunk.id, embedder.name(), vector);
                db.embedding_upsert(&emb)?;
                stats.processed += 1;

                if stats.processed % PROGRESS_EVERY == 0 {
                    tracing::info!(
                        corpus_id = %corpus.id,
                        processed = stats.processed,
                        "embed pass: progress"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    corpus_id = %corpus.id,
                    chunk_id = %chunk.id,
                    error = %e,
                    "embed pass: failed to embed chunk"
                );
                stats.failed += 1;
            }
        }
    }

    tracing::info!(
        corpus_id = %corpus.id,
        processed = stats.processed,
        skipped = stats.skipped,
        failed = stats.failed,
        "embed pass complete"
    );

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SqliteBackend;
    use crate::types::{Chunk, Corpus, Location};
    use callimachus_llm::DryRunProvider;

    fn setup() -> (Arc<dyn StorageBackend>, Corpus) {
        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "test".into(),
            "Test".into(),
            "wiki".into(),
            "/tmp/test".into(),
        );
        db.corpus_insert(&corpus).unwrap();

        // Seed 3 chunks.
        for i in 0..3u32 {
            let loc = Location::new("test", format!("wiki/page-{i}"));
            let chunk = Chunk::new(
                "test".into(),
                None,
                "page".into(),
                loc,
                format!("Content of page {i}"),
            );
            db.chunk_upsert(&chunk).unwrap();
        }

        (Arc::new(db), corpus)
    }

    #[tokio::test]
    async fn embeds_three_chunks() {
        let (db, corpus) = setup();
        let embedder: Arc<dyn LlmProvider> = Arc::new(DryRunProvider::new());
        let opts = IndexOptions::default();

        let stats = run(db.as_ref(), &corpus, Some(embedder), &opts)
            .await
            .unwrap();
        assert_eq!(stats.processed, 3);
        assert_eq!(stats.skipped, 0);
        assert_eq!(stats.failed, 0);

        let count = db.embedding_count(&corpus.id).unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn embed_pass_is_idempotent() {
        let (db, corpus) = setup();
        let embedder: Arc<dyn LlmProvider> = Arc::new(DryRunProvider::new());
        let opts = IndexOptions::default();

        // First run.
        let s1 = run(db.as_ref(), &corpus, Some(Arc::clone(&embedder)), &opts)
            .await
            .unwrap();
        assert_eq!(s1.processed, 3);

        // Second run — all chunks already embedded.
        let s2 = run(db.as_ref(), &corpus, Some(embedder), &opts)
            .await
            .unwrap();
        assert_eq!(s2.processed, 0);
        assert_eq!(s2.skipped, 3);

        // Count unchanged.
        let count = db.embedding_count(&corpus.id).unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn embed_pass_with_no_embedder_skips_gracefully() {
        let (db, corpus) = setup();
        let opts = IndexOptions::default();

        let stats = run(db.as_ref(), &corpus, None, &opts).await.unwrap();
        assert_eq!(stats.processed, 0);
        assert_eq!(stats.skipped, 0);

        let count = db.embedding_count(&corpus.id).unwrap();
        assert_eq!(count, 0);
    }
}
