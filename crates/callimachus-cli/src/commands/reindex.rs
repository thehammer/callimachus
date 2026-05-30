use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use callimachus_adapter_book::BookAdapter;
use callimachus_core::{
    indexing::{ChangeStrategy, change_detector, pipeline::IndexOptions, reindex_pass},
    storage::StorageBackend,
};
use callimachus_llm::build_provider;

use crate::{commands::index::resolve_provider, config::GlobalConfig};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    corpus_id: &str,
    since: Option<String>,
    dry_run: bool,
    provider_override: Option<String>,
    stable_sampling: bool,
    db: Arc<dyn StorageBackend>,
    config: &GlobalConfig,
) -> Result<()> {
    let corpus = db
        .corpus_require(corpus_id)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .map_err(|e| e.context(format!("corpus '{corpus_id}' not found")))?;

    let provider_config = resolve_provider(provider_override, config)?;
    let llm = build_provider(provider_config)
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

    if corpus.kind != "book" {
        bail!(
            "adapter for kind '{}' is not yet available (supports 'book' only)",
            corpus.kind
        );
    }
    let adapter = Arc::new(BookAdapter::new());

    // Detect changes.
    let change_set = change_detector::detect(&corpus, db.as_ref(), since.as_deref())
        .context("change detection failed")?;

    // Summarise strategy for the user.
    let strategy_label = match &change_set.strategy {
        ChangeStrategy::Mtime { since: ts } => format!("mtime since {ts}"),
        ChangeStrategy::Git { since_ref } => format!("git diff since {since_ref}"),
        ChangeStrategy::Full => {
            eprintln!(
                "warning: no change baseline found; running full reindex. \
                 Use `calli index` for initial indexing."
            );
            "full".to_string()
        }
    };

    eprintln!(
        "Reindexing corpus '{}' — strategy: {strategy_label}",
        corpus.id
    );
    eprintln!(
        "  Detected {} changed path(s), {} pre-deleted chunk(s).",
        change_set.changed_paths.len(),
        change_set.deleted_chunk_ids.len()
    );

    if dry_run {
        println!("Dry run — no changes written.");
        for p in &change_set.changed_paths {
            println!("  changed: {p}");
        }
        for id in &change_set.deleted_chunk_ids {
            println!("  delete chunk: {id}");
        }
        return Ok(());
    }

    let start = Instant::now();
    let llm_arc = Arc::new(llm);

    let stats = reindex_pass::run(
        &db,
        &corpus,
        &(adapter as Arc<dyn callimachus_core::SourceAdapter>),
        &(llm_arc as Arc<dyn callimachus_llm::LlmProvider>),
        &change_set,
        &IndexOptions {
            dry_run: false,
            stable_sampling,
            ..Default::default()
        },
    )
    .await?;

    let elapsed = start.elapsed();
    println!("Done ({:.1}s).", elapsed.as_secs_f32());
    println!(
        "  +{} added  ~{} modified  -{} deleted chunks",
        stats.added, stats.modified, stats.deleted
    );

    Ok(())
}
