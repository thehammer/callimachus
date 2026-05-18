use std::sync::Arc;

use crate::{
    adapter::SourceAdapter,
    storage::{StorageBackend, run_log::PassStats},
    types::Corpus,
};

use super::pipeline::IndexOptions;

pub async fn run(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();

    let mut sources = adapter.discover(&corpus.source).await?;

    // Inject corpus_id and pipeline flags into each source's meta so the adapter can use them.
    for source in &mut sources {
        if let Some(obj) = source.meta.as_object_mut() {
            obj.entry("corpus_id")
                .or_insert_with(|| serde_json::Value::String(corpus.id.clone()));
            obj.insert(
                "no_git_filter".to_string(),
                serde_json::Value::Bool(opts.no_git_filter),
            );
        } else {
            source.meta = serde_json::json!({
                "corpus_id": corpus.id,
                "no_git_filter": opts.no_git_filter,
            });
        }
    }

    let mut all_chunks = Vec::new();
    for source in &sources {
        let chunks = adapter.chunk(source).await?;
        all_chunks.extend(chunks);
    }

    let total = all_chunks.len() as u64;
    let mut resume_seen = opts.from_chunk.is_none(); // If no resume point, start immediately.

    for chunk in all_chunks {
        // Resumability: skip until we see the resume chunk ID.
        if !resume_seen && let Some(ref resume_id) = opts.from_chunk {
            if &chunk.id == resume_id {
                resume_seen = true;
            } else {
                stats.skipped += 1;
                continue;
            }
        }

        if opts.dry_run {
            stats.processed += 1;
            continue;
        }

        // Skip already-written chunks (idempotent), unless --full forces re-upsert.
        if !opts.full && db.chunk_has(&chunk.id)? {
            stats.skipped += 1;
            continue;
        }

        db.chunk_upsert(&chunk)?;
        stats.processed += 1;

        if stats.processed % 25 == 0 {
            tracing::info!("[chunk] {}/{} chunks", stats.processed, total);
        }
    }

    Ok(stats)
}
