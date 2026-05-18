use std::sync::Arc;

use sha2::{Digest, Sha256};

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

    // Filter sources through the change manifest.  When all_dirty is true (first
    // run, --full, or no History pass) every source is processed.
    let manifest = opts.change_manifest.as_ref();
    let sources: Vec<_> = sources
        .into_iter()
        .filter(|s| manifest.map(|m| m.is_dirty(&s.path)).unwrap_or(true))
        .collect();

    let mut all_chunks = Vec::new();
    for source in &sources {
        let chunks = adapter.chunk(source).await?;
        // Compute a per-source SHA-256 for the source_hash column.  We use the
        // concatenated content of all chunks from this source as the "file hash"
        // (the actual file content is not directly available here; using chunk
        // content is stable and deterministic).
        let source_hash = {
            let mut h = Sha256::new();
            for c in &chunks {
                h.update(c.content.as_bytes());
            }
            hex::encode(h.finalize())
        };
        // Attach the source path for downstream processing.
        all_chunks.push((source.path.clone(), source_hash, chunks));
    }

    let total: u64 = all_chunks.iter().map(|(_, _, cs)| cs.len() as u64).sum();
    let mut resume_seen = opts.from_chunk.is_none(); // If no resume point, start immediately.

    for (source_path, source_hash, chunks) in all_chunks {
        // Get commit metadata for this source from the manifest, if any.
        let commit_meta = manifest.and_then(|m| m.commit_meta_for(&source_path));
        let current_version = manifest.map(|m| m.current_version.as_str());

        for chunk in chunks {
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

            // Write source_hash and history metadata.
            db.chunk_set_source_hash(&chunk.id, &source_hash)?;
            if let Some(version) = current_version {
                db.chunk_set_history(
                    &chunk.id,
                    version,
                    commit_meta.map(|m| m.message.as_str()),
                    commit_meta.map(|m| m.author.as_str()),
                )?;
            }

            stats.processed += 1;

            if stats.processed % 25 == 0 {
                tracing::info!("[chunk] {}/{} chunks", stats.processed, total);
            }
        }
    }

    Ok(stats)
}
