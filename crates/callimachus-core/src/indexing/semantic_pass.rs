use std::sync::Arc;

use callimachus_llm::{LlmError, LlmProvider};
use tokio::task::JoinSet;

use crate::{
    adapter::{ExtractedSemantic, SourceAdapter},
    storage::{StorageBackend, run_log::PassStats},
    types::{Chunk, Corpus},
};

use super::pipeline::IndexOptions;

const MAX_RETRIES: u32 = 3;

pub async fn run(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    llm: Arc<dyn LlmProvider>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();

    let chunks = db.chunk_list_unprocessed(&corpus.id)?;
    let total = chunks.len() as u64;

    if llm.supports_parallel() {
        let concurrency = opts.concurrency.unwrap_or(5);
        let mut join_set: JoinSet<(String, Result<Option<ExtractedSemantic>, String>)> =
            JoinSet::new();

        for chunk in chunks {
            // Throttle to `concurrency` in-flight tasks.
            while join_set.len() >= concurrency {
                if let Some(res) = join_set.join_next().await {
                    apply_join_result(res, &db, &mut stats, opts.dry_run)?;
                }
            }

            let adapter_c = Arc::clone(&adapter);
            let llm_c = Arc::clone(&llm);
            let chunk_id = chunk.id.clone();

            join_set.spawn(async move {
                let result = extract_with_retry(&*adapter_c, &chunk, &*llm_c).await;
                let mapped = match result {
                    Ok(v) => Ok(v),
                    Err(e) => Err(e.to_string()),
                };
                (chunk_id, mapped)
            });
        }

        // Drain remaining tasks.
        while let Some(res) = join_set.join_next().await {
            apply_join_result(res, &db, &mut stats, opts.dry_run)?;
        }
    } else {
        // Sequential path (e.g. ClaudeCodeProvider).
        for (i, chunk) in chunks.iter().enumerate() {
            match extract_with_retry(adapter.as_ref(), chunk, llm.as_ref()).await {
                Ok(Some(sem)) => {
                    if !opts.dry_run {
                        for entity in &sem.entities {
                            db.entity_upsert(entity)?;
                        }
                        for edge in &sem.edges {
                            db.edge_upsert(edge)?;
                        }
                        db.chunk_set_semantic_processed(&chunk.id)?;
                    }
                    stats.processed += 1;
                }
                Ok(None) => {
                    // Adapter says skip — still mark as processed so we don't retry.
                    if !opts.dry_run {
                        db.chunk_set_semantic_processed(&chunk.id)?;
                    }
                    stats.skipped += 1;
                }
                Err(e) => {
                    tracing::warn!("semantic extraction failed for {}: {e}", chunk.id);
                    stats.failed += 1;
                }
            }

            if (i as u64 + 1).is_multiple_of(25) {
                tracing::info!("[semantic] {}/{} chunks", i + 1, total);
            }
        }
    }

    Ok(stats)
}

async fn extract_with_retry(
    adapter: &dyn SourceAdapter,
    chunk: &Chunk,
    llm: &dyn LlmProvider,
) -> anyhow::Result<Option<ExtractedSemantic>> {
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        match adapter.extract_with_llm(chunk, llm).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if let Some(LlmError::RateLimited { retry_after_secs }) =
                    e.downcast_ref::<LlmError>()
                    && attempts < MAX_RETRIES
                {
                    let backoff = *retry_after_secs;
                    tracing::warn!("rate limited; backing off {backoff}s (attempt {attempts})");
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }

                if let Some(LlmError::Timeout { .. }) = e.downcast_ref::<LlmError>()
                    && attempts < MAX_RETRIES
                {
                    let backoff = 5u64 * 2u64.pow(attempts - 1);
                    tracing::warn!("timeout; backing off {backoff}s (attempt {attempts})");
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }

                return Err(e);
            }
        }
    }
}

fn apply_join_result(
    join_result: Result<
        (String, Result<Option<ExtractedSemantic>, String>),
        tokio::task::JoinError,
    >,
    db: &Arc<dyn StorageBackend>,
    stats: &mut PassStats,
    dry_run: bool,
) -> anyhow::Result<()> {
    match join_result {
        Ok((chunk_id, Ok(Some(sem)))) => {
            if !dry_run {
                for entity in &sem.entities {
                    db.entity_upsert(entity)?;
                }
                for edge in &sem.edges {
                    db.edge_upsert(edge)?;
                }
                db.chunk_set_semantic_processed(&chunk_id)?;
            }
            stats.processed += 1;
        }
        Ok((chunk_id, Ok(None))) => {
            if !dry_run {
                db.chunk_set_semantic_processed(&chunk_id)?;
            }
            stats.skipped += 1;
        }
        Ok((_chunk_id, Err(msg))) => {
            tracing::warn!("semantic pass error: {msg}");
            stats.failed += 1;
        }
        Err(e) => {
            tracing::warn!("task join error: {e}");
            stats.failed += 1;
        }
    }
    Ok(())
}
