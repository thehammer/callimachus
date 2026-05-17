use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, bail};
use callimachus_adapter_book::BookAdapter;
use callimachus_core::{
    indexing::watcher::{CorpusWatcher, WatcherConfig},
    storage::{SqliteBackend, StorageBackend},
};
use callimachus_llm::build_provider;

use crate::{commands::index::resolve_provider, config::GlobalConfig};

pub async fn run(
    corpus_id: &str,
    debounce_ms: u64,
    provider_override: Option<String>,
    db_path: &Path,
    config: &GlobalConfig,
) -> Result<()> {
    // Open a fresh DB connection to avoid WAL contention with other CLI sessions.
    let db: Arc<dyn StorageBackend> = Arc::new(
        SqliteBackend::open(db_path)
            .with_context(|| format!("opening database at {}", db_path.display()))?,
    );

    let corpus = db
        .corpus_require(corpus_id)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .map_err(|e| e.context(format!("corpus '{corpus_id}' not found")))?;

    if corpus.kind != "book" {
        bail!(
            "adapter for kind '{}' is not yet available (supports 'book' only)",
            corpus.kind
        );
    }

    let provider_config = resolve_provider(provider_override, config)?;
    let llm = build_provider(provider_config)
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

    let source_path = &corpus.source;
    println!("Watching {source_path} for changes. Press Ctrl+C to stop.");

    let watcher = CorpusWatcher::new(
        corpus,
        db,
        Arc::new(BookAdapter::new()),
        Arc::new(llm),
        WatcherConfig {
            debounce_ms,
            concurrency: None,
        },
    );

    watcher.run().await
}
