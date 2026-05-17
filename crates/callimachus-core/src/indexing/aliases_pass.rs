use std::sync::Arc;

use callimachus_llm::{LlmError, LlmProvider};

use crate::{
    adapter::SourceAdapter,
    storage::{StorageBackend, run_log::PassStats},
    types::Corpus,
};

use super::pipeline::IndexOptions;

const MAX_RETRIES: u32 = 8;

/// Resolve entity aliases across the whole corpus in one pass.
///
/// Calls `adapter.resolve_aliases` (with retry on rate-limit / timeout), then
/// applies each returned `EntityMerge` via `db.entity_merge`.  Failures per
/// merge increment `stats.failed`; a final LLM failure after all retries also
/// increments `stats.failed` and returns `Ok(stats)` so the pipeline run-log
/// records `failed > 0` without aborting the remaining passes.
pub async fn run(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    llm: Arc<dyn LlmProvider>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();

    let all_entities = db.entity_list(&corpus.id)?;
    if all_entities.is_empty() || opts.dry_run {
        return Ok(stats);
    }

    match resolve_with_retry(adapter.as_ref(), &all_entities, llm.as_ref()).await {
        Ok(merges) => {
            for merge in merges {
                match db.entity_merge(&merge.keep_id, &merge.absorb_id) {
                    Ok(()) => stats.processed += 1,
                    Err(e) => {
                        tracing::warn!(
                            "entity merge failed (keep={} absorb={}): {e}",
                            merge.keep_id,
                            merge.absorb_id
                        );
                        stats.failed += 1;
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!("alias resolution failed after retries: {e}");
            stats.failed += 1;
        }
    }

    Ok(stats)
}

async fn resolve_with_retry(
    adapter: &dyn SourceAdapter,
    entities: &[crate::types::Entity],
    llm: &dyn LlmProvider,
) -> anyhow::Result<Vec<crate::adapter::EntityMerge>> {
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        match adapter.resolve_aliases(entities, llm).await {
            Ok(merges) => return Ok(merges),
            Err(e) => {
                if let Some(LlmError::RateLimited { retry_after_secs }) =
                    e.downcast_ref::<LlmError>()
                    && attempts < MAX_RETRIES
                {
                    let backoff = *retry_after_secs;
                    tracing::warn!(
                        "alias pass rate limited; backing off {backoff}s (attempt {attempts})"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }

                if let Some(LlmError::Timeout { .. }) = e.downcast_ref::<LlmError>()
                    && attempts < MAX_RETRIES
                {
                    let backoff = 5u64 * 2u64.pow(attempts - 1);
                    tracing::warn!(
                        "alias pass timeout; backing off {backoff}s (attempt {attempts})"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }

                return Err(e);
            }
        }
    }
}
