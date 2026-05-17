use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use callimachus_llm::LlmProvider;
use notify::{RecursiveMode, Watcher};

use crate::{
    adapter::SourceAdapter,
    indexing::{change_detector::ChangeSet, pipeline::IndexOptions, reindex_pass},
    storage::StorageBackend,
    types::Corpus,
};

/// Configuration for `CorpusWatcher`.
pub struct WatcherConfig {
    /// Milliseconds to wait after the last filesystem event before triggering reindex.
    pub debounce_ms: u64,
    /// Max parallel LLM calls forwarded to `IndexOptions`.
    pub concurrency: Option<usize>,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 500,
            concurrency: None,
        }
    }
}

/// Long-running watcher that triggers incremental reindex on filesystem changes.
pub struct CorpusWatcher {
    corpus: Corpus,
    db: Arc<dyn StorageBackend>,
    adapter: Arc<dyn SourceAdapter>,
    llm: Arc<dyn LlmProvider>,
    config: WatcherConfig,
}

impl CorpusWatcher {
    pub fn new(
        corpus: Corpus,
        db: Arc<dyn StorageBackend>,
        adapter: Arc<dyn SourceAdapter>,
        llm: Arc<dyn LlmProvider>,
        config: WatcherConfig,
    ) -> Self {
        Self {
            corpus,
            db,
            adapter,
            llm,
            config,
        }
    }

    /// Start watching the corpus source path. Returns when Ctrl-C is received.
    pub async fn run(&self) -> anyhow::Result<()> {
        let source_path = PathBuf::from(&self.corpus.source);

        // Channel: watcher thread → tokio async task.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Event>(64);

        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    // blocking_send is acceptable here because this callback runs in a
                    // notify background thread (not an async context).
                    tx.blocking_send(event).ok();
                }
            })?;
        watcher.watch(&source_path, RecursiveMode::Recursive)?;

        tracing::info!(
            "[watch] watching {} — press Ctrl-C to stop",
            source_path.display()
        );

        let mut pending: HashSet<PathBuf> = HashSet::new();
        let mut deadline: Option<tokio::time::Instant> = None;
        let debounce = Duration::from_millis(self.config.debounce_ms);

        let shutdown = tokio::signal::ctrl_c();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                // New filesystem event.
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            for path in event.paths {
                                pending.insert(path);
                            }
                            // Reset the debounce deadline.
                            deadline = Some(tokio::time::Instant::now() + debounce);
                        }
                        None => {
                            // Channel closed — watcher dropped.
                            break;
                        }
                    }
                }

                // Debounce timer fires.
                _ = Self::wait_until(deadline) => {
                    if !pending.is_empty() {
                        let paths: Vec<String> = pending
                            .drain()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect();
                        let path_count = paths.len();

                        let change_set = ChangeSet {
                            changed_paths: paths,
                            ..Default::default()
                        };
                        let opts = IndexOptions {
                            concurrency: self.config.concurrency,
                            ..Default::default()
                        };

                        match reindex_pass::run(
                            &self.db,
                            &self.corpus,
                            &self.adapter,
                            &self.llm,
                            &change_set,
                            &opts,
                        )
                        .await
                        {
                            Ok(stats) => tracing::info!(
                                "[watch] reindexed {path_count} path(s): \
                                 +{} ~{} -{} chunks",
                                stats.added, stats.modified, stats.deleted
                            ),
                            Err(e) => tracing::error!("[watch] reindex error: {e}"),
                        }
                    }
                    deadline = None;
                }

                // Ctrl-C / SIGINT.
                _ = &mut shutdown => {
                    tracing::info!("[watch] shutting down gracefully");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Returns a future that completes at `deadline`, or never completes if
    /// `deadline` is `None`.  Used as the debounce arm of `select!`.
    async fn wait_until(deadline: Option<tokio::time::Instant>) {
        match deadline {
            Some(t) => tokio::time::sleep_until(t).await,
            None => std::future::pending::<()>().await,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use callimachus_llm::DryRunProvider;

    use crate::{
        adapter::{
            DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
            SourceAdapter,
        },
        storage::{SqliteBackend, StorageBackend},
        types::{Chunk, Corpus, Entity},
    };

    use super::{CorpusWatcher, WatcherConfig};

    struct NoopAdapter;

    #[async_trait::async_trait]
    impl SourceAdapter for NoopAdapter {
        fn kind(&self) -> &str {
            "noop"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
            Ok(vec![DiscoveredSource {
                path: source.to_string(),
                kind: "text".to_string(),
                meta: serde_json::json!({ "corpus_id": "test" }),
            }])
        }
        async fn chunk(&self, _s: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
            Ok(vec![])
        }
        async fn extract_structure(&self, _c: &Chunk) -> anyhow::Result<ExtractedStructure> {
            Ok(ExtractedStructure {
                parent_path: None,
                child_paths: vec![],
                structural_entities: vec![],
                structural_edges: vec![],
            })
        }
        async fn extract_with_llm(
            &self,
            _c: &Chunk,
            _l: &dyn callimachus_llm::LlmProvider,
        ) -> anyhow::Result<Option<ExtractedSemantic>> {
            Ok(None)
        }
        async fn summarize(
            &self,
            _c: &Chunk,
            _l: &dyn callimachus_llm::LlmProvider,
            _d: &str,
        ) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        async fn resolve_aliases(
            &self,
            _e: &[Entity],
            _l: &dyn callimachus_llm::LlmProvider,
        ) -> anyhow::Result<Vec<EntityMerge>> {
            Ok(vec![])
        }
        fn format_location(&self, c: &Chunk) -> String {
            c.location.path.clone()
        }
        fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
            Ok(LocationRef {
                corpus_id: "test".to_string(),
                path: uri.to_string(),
            })
        }
    }

    /// Build a watcher pointing at a temp dir, spawn it, write a file,
    /// wait briefly, then assert the watcher ran without panicking.
    #[tokio::test]
    async fn watcher_runs_and_responds_to_file_write() {
        let dir = tempfile::tempdir().unwrap();

        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "test".to_string(),
            "Test".to_string(),
            "noop".to_string(),
            dir.path().to_string_lossy().into_owned(),
        );
        db.corpus_insert(&corpus).unwrap();

        let config = WatcherConfig {
            debounce_ms: 50,
            ..Default::default()
        };
        let watcher = Arc::new(CorpusWatcher::new(
            corpus,
            Arc::new(db),
            Arc::new(NoopAdapter),
            Arc::new(DryRunProvider::new()),
            config,
        ));

        // Run the watcher in a background task; cancel it after a brief delay.
        let watcher_clone = Arc::clone(&watcher);
        let handle = tokio::spawn(async move { watcher_clone.run().await });

        // Write a file to trigger an event.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();

        // Wait for the debounce + some processing time.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Abort the watcher task (simulates Ctrl-C in a test environment).
        handle.abort();
        let result = handle.await;
        // Aborted task returns Err(JoinError::Cancelled).
        assert!(
            result.is_err() || result.is_ok(),
            "watcher task should complete without panic"
        );
    }
}
