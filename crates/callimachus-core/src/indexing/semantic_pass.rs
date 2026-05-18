use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use callimachus_llm::{LlmError, LlmProvider};
use tokio::task::JoinSet;

use crate::{
    adapter::{ExtractedSemantic, SourceAdapter},
    storage::{StorageBackend, run_log::PassStats},
    types::{Chunk, Corpus},
};

use super::pipeline::IndexOptions;

const MAX_RETRIES: u32 = 5;

pub async fn run(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    llm: Arc<dyn LlmProvider>,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();

    let chunks = if opts.full {
        db.chunk_list(&corpus.id)?
    } else {
        db.chunk_list_unprocessed(&corpus.id)?
    };
    let total = chunks.len() as u64;

    if llm.supports_parallel() {
        let concurrency = opts.concurrency.unwrap_or(5);
        let mut join_set: JoinSet<(String, Result<Option<ExtractedSemantic>, String>)> =
            JoinSet::new();
        let completed = Arc::new(AtomicUsize::new(0));

        for chunk in chunks {
            // Throttle to `concurrency` in-flight tasks.
            while join_set.len() >= concurrency {
                if let Some(res) = join_set.join_next().await {
                    apply_join_result(res, &db, &mut stats, opts.dry_run)?;
                    let n = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    if (n as u64).is_multiple_of(25) {
                        tracing::info!("[semantic] {}/{} chunks", n, total);
                    }
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
            let n = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if (n as u64).is_multiple_of(25) {
                tracing::info!("[semantic] {}/{} chunks", n, total);
            }
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

                let msg = format!("{e:#}");
                let is_transient_parse = msg.contains("EOF while parsing")
                    || msg.contains("failed to parse LLM JSON")
                    || msg.contains("empty response");
                if is_transient_parse && attempts < MAX_RETRIES {
                    let backoff = 5u64 * 2u64.pow(attempts - 1);
                    tracing::warn!(
                        "transient LLM parse failure; backing off {backoff}s (attempt {attempts}): {msg}"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }

                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use callimachus_llm::DryRunProvider;

    use crate::{
        adapter::{
            DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
            SourceAdapter,
        },
        types::{Chunk, Location},
    };

    use super::extract_with_retry;

    struct FailOnceThenSucceedAdapter {
        calls: AtomicU32,
    }

    impl FailOnceThenSucceedAdapter {
        fn new() -> Self {
            Self {
                calls: AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl SourceAdapter for FailOnceThenSucceedAdapter {
        fn kind(&self) -> &str {
            "test"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }

        async fn discover(&self, _source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
            Ok(vec![])
        }

        async fn chunk(&self, _source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
            Ok(vec![])
        }

        async fn extract_structure(
            &self,
            _chunk: &Chunk,
        ) -> anyhow::Result<ExtractedStructure> {
            Ok(ExtractedStructure {
                parent_path: None,
                child_paths: vec![],
                structural_entities: vec![],
                structural_edges: vec![],
            })
        }

        async fn extract_with_llm(
            &self,
            _chunk: &Chunk,
            _llm: &dyn callimachus_llm::LlmProvider,
        ) -> anyhow::Result<Option<ExtractedSemantic>> {
            let call_n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call_n == 1 {
                Err(anyhow::anyhow!(
                    "failed to parse LLM JSON: EOF while parsing a value at line 1 column 0"
                ))
            } else {
                Ok(Some(ExtractedSemantic {
                    entities: vec![],
                    edges: vec![],
                    summary_text: None,
                }))
            }
        }

        async fn summarize(
            &self,
            _chunk: &Chunk,
            _llm: &dyn callimachus_llm::LlmProvider,
            _depth: &str,
        ) -> anyhow::Result<Option<String>> {
            Ok(None)
        }

        async fn resolve_aliases(
            &self,
            _entities: &[crate::types::Entity],
            _llm: &dyn callimachus_llm::LlmProvider,
        ) -> anyhow::Result<Vec<EntityMerge>> {
            Ok(vec![])
        }

        fn format_location(&self, chunk: &Chunk) -> String {
            chunk.location.path.clone()
        }

        fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
            Ok(LocationRef {
                corpus_id: "test".to_string(),
                path: uri.to_string(),
            })
        }
    }

    #[tokio::test]
    async fn extract_with_retry_retries_on_empty_response() {
        tokio::time::pause();

        let adapter = FailOnceThenSucceedAdapter::new();
        let chunk = Chunk::new(
            "test-corpus".to_string(),
            None,
            "section".to_string(),
            Location::new("test-corpus", "ch/1"),
            "some content".to_string(),
        );
        let llm = DryRunProvider::new();

        let result = extract_with_retry(&adapter, &chunk, &llm).await;

        assert!(result.is_ok(), "expected Ok but got Err");
        assert!(result.unwrap().is_some(), "expected Some(_)");
        assert_eq!(
            adapter.calls.load(Ordering::SeqCst),
            2,
            "adapter should have been called exactly twice"
        );
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
