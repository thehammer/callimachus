use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use callimachus_core::{
    indexing::{ChangeStrategy, change_detector, pipeline::IndexOptions, reindex_pass},
    storage::StorageBackend,
};
use callimachus_llm::{build_embedding_provider, build_provider};

use crate::{
    commands::index::{build_adapter, build_embedding_provider_config, resolve_provider},
    config::GlobalConfig,
};

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

    let adapter = build_adapter(&corpus)?;

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

    // Build embedder if embeddings are enabled. Fail loudly if enabled-but-broken.
    let embedder = {
        let embedding_enabled = config.embedding.as_ref().is_some_and(|e| e.enabled);
        if embedding_enabled {
            let embed_cfg = build_embedding_provider_config(config);
            match build_embedding_provider(embed_cfg) {
                Ok(Some(p)) => Some(p),
                Ok(None) => None,
                Err(e) => {
                    bail!("embeddings enabled in config but not usable: {e}");
                }
            }
        } else {
            None
        }
    };

    let start = Instant::now();
    let llm_arc = Arc::new(llm);

    let stats = reindex_pass::run(
        &db,
        &corpus,
        &(adapter as Arc<dyn callimachus_core::SourceAdapter>),
        &(llm_arc as Arc<dyn callimachus_llm::LlmProvider>),
        embedder,
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use callimachus_core::storage::{SqliteBackend, StorageBackend};
    use callimachus_core::types::Corpus;

    use crate::config::GlobalConfig;

    /// Regression: `reindex` previously hardcoded the book adapter and bailed
    /// with "supports 'book' only" for any other corpus kind. It must now select
    /// the code adapter via `build_adapter` and complete a dry-run without error.
    #[tokio::test]
    async fn code_corpus_reindex_selects_code_adapter() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "code-test".to_string(),
            "Code Test".to_string(),
            "code".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        super::run(
            "code-test",
            None,  // since
            true,  // dry_run
            None,  // provider_override
            false, // stable_sampling
            db,
            &GlobalConfig::default(),
        )
        .await
        .unwrap();
    }
}
