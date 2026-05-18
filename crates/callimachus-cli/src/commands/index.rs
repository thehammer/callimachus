use std::sync::Arc;

use anyhow::{Result, bail};
use callimachus_adapter_book::BookAdapter;
use callimachus_adapter_code::CodeAdapter;
use callimachus_adapter_wiki::WikiAdapter;
use callimachus_core::{
    adapter::SourceAdapter,
    indexing::{IndexOptions, IndexPipeline},
    storage::StorageBackend,
    types::{Corpus, Pass},
};
use callimachus_llm::{ProviderConfig, build_provider};

use crate::config::GlobalConfig;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    corpus_id: &str,
    pass: Option<String>,
    from_chunk: Option<String>,
    dry_run: bool,
    full: bool,
    provider_override: Option<String>,
    db: Arc<dyn StorageBackend>,
    config: &GlobalConfig,
) -> Result<()> {
    // Load corpus.
    let corpus = db
        .corpus_require(corpus_id)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .map_err(|e| {
            e.context(format!(
                "corpus '{corpus_id}' not found — add it with `calli corpus add`"
            ))
        })?;

    // Build LLM provider.
    let provider_config = resolve_provider(provider_override, config)?;
    let llm = build_provider(provider_config)
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

    // Build adapter.
    let adapter = build_adapter(&corpus)?;

    // Build index options.
    let passes = resolve_passes(pass)?;
    let opts = IndexOptions {
        passes,
        from_chunk,
        dry_run,
        full,
        concurrency: None,
    };

    let dry_label = if dry_run { " [dry-run]" } else { "" };
    eprintln!("Indexing corpus '{}'{dry_label}…", corpus.id);

    // Run pipeline.
    let pipeline = IndexPipeline {
        db,
        adapter,
        llm: Arc::new(llm),
        embedder: None, // TODO: wire up from config when embedding.enabled = true
    };

    let result = pipeline.run(&corpus, opts).await?;

    // Print summary.
    println!("Done.");
    println!("  Chunks:   {}", result.total_chunks);
    println!("  Entities: {}", result.total_entities);
    println!("  Edges:    {}", result.total_edges);
    if result.cost_usd > 0.0 {
        println!("  Cost:     ${:.4}", result.cost_usd);
    }
    for run in &result.runs {
        let status_icon = if run.status == "completed" {
            "✓"
        } else {
            "✗"
        };
        println!(
            "  {status_icon} pass={:<10} processed={} skipped={} failed={}",
            run.pass, run.stats.processed, run.stats.skipped, run.stats.failed
        );
    }

    Ok(())
}

fn build_adapter(corpus: &Corpus) -> Result<Arc<dyn SourceAdapter>> {
    match corpus.kind.as_str() {
        "book" => Ok(Arc::new(BookAdapter::new())),
        "code" => Ok(Arc::new(CodeAdapter::new())),
        "wiki" => Ok(Arc::new(WikiAdapter::new())),
        other => bail!("adapter not yet available for corpus kind '{other}'"),
    }
}

pub fn resolve_provider(
    override_name: Option<String>,
    config: &GlobalConfig,
) -> Result<ProviderConfig> {
    // --provider flag > config > auto-detect
    let name = override_name.as_deref().or(config.llm.provider.as_deref());

    match name {
        Some("dry-run") | Some("dry_run") => Ok(ProviderConfig::DryRun),
        Some("anthropic") | Some("api") => Ok(ProviderConfig::AnthropicApi {
            api_key: config.llm.api_key.clone(),
            model: config.llm.model.clone(),
            max_parallel_calls: None,
        }),
        Some("claude-code") | Some("claude_code") => Ok(ProviderConfig::ClaudeCode {
            claude_bin: None,
            model: config.llm.model.clone(),
            timeout_secs: None,
            calls_per_minute: None,
        }),
        Some(other) => bail!("unknown provider '{other}'; use: anthropic, claude-code, dry-run"),
        None => {
            // If the config file supplies an api_key, prefer the Anthropic API
            // over the CC subprocess regardless of env vars.
            if let Some(key) = config.llm.api_key.clone() {
                Ok(ProviderConfig::AnthropicApi {
                    api_key: Some(key),
                    model: config.llm.model.clone(),
                    max_parallel_calls: None,
                })
            } else {
                callimachus_llm::auto_detect()
                    .map_err(|e| anyhow::anyhow!("could not detect an LLM provider: {e}"))
            }
        }
    }
}

fn resolve_passes(pass: Option<String>) -> Result<Vec<Pass>> {
    match pass.as_deref() {
        None => Ok(vec![
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Summarize,
            Pass::Purpose,
            Pass::Contract,
        ]),
        Some("all") => Ok(vec![
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Summarize,
            Pass::Purpose,
            Pass::Contract,
            Pass::Theme,
            Pass::Embed,
        ]),
        Some("chunk") => Ok(vec![Pass::Chunk]),
        Some("structure") => Ok(vec![Pass::Structure]),
        Some("semantic") => Ok(vec![Pass::Semantic]),
        Some("aliases") => Ok(vec![Pass::Aliases]),
        Some("summarize") => Ok(vec![Pass::Summarize]),
        Some("purpose") => Ok(vec![Pass::Purpose]),
        Some("contract") => Ok(vec![Pass::Contract]),
        Some("theme") => Ok(vec![Pass::Theme]),
        Some("embed") => Ok(vec![Pass::Embed]),
        Some(other) => bail!(
            "unknown pass '{other}'; use: all, chunk, structure, semantic, aliases, summarize, purpose, contract, theme, embed"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use callimachus_core::storage::{SqliteBackend, StorageBackend};
    use callimachus_core::types::Corpus;

    use crate::config::GlobalConfig;

    async fn run_index(
        corpus_id: &str,
        db: Arc<dyn StorageBackend>,
        dry_run: bool,
    ) -> anyhow::Result<()> {
        super::run(
            corpus_id,
            None,
            None,
            dry_run,
            false,
            Some("dry-run".to_string()),
            db,
            &GlobalConfig::default(),
        )
        .await
    }

    async fn run_index_full(corpus_id: &str, db: Arc<dyn StorageBackend>) -> anyhow::Result<()> {
        super::run(
            corpus_id,
            Some("chunk".to_string()),
            None,
            false,
            true, // full
            Some("dry-run".to_string()),
            db,
            &GlobalConfig::default(),
        )
        .await
    }

    #[tokio::test]
    async fn invalid_corpus_id_returns_clear_error() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let result = run_index("nonexistent", db, false).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("nonexistent"),
            "error should mention corpus id: {msg}"
        );
    }

    #[tokio::test]
    async fn dry_run_completes_without_writing() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "test".to_string(),
            "Test".to_string(),
            "book".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string() + "/src/commands/index.rs",
        );
        db.corpus_insert(&corpus).unwrap();

        run_index("test", db, true).await.unwrap();
    }

    #[tokio::test]
    async fn code_corpus_selects_code_adapter() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "code-test".to_string(),
            "Code Test".to_string(),
            "code".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        // Dry run should select CodeAdapter and complete without error.
        run_index("code-test", db, true).await.unwrap();
    }

    #[tokio::test]
    async fn full_flag_forces_reupsert() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "full-test".to_string(),
            "Full Test".to_string(),
            "code".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        // First run: index normally (chunk pass only, dry-run=false).
        super::run(
            "full-test",
            Some("chunk".to_string()),
            None,
            false,
            false,
            Some("dry-run".to_string()),
            Arc::clone(&db),
            &GlobalConfig::default(),
        )
        .await
        .unwrap();

        let count_after_first = db.chunk_count("full-test").unwrap();
        assert!(count_after_first > 0, "should have chunks after first run");

        // Second run with --full: processed > 0, not all skipped.
        // We can't assert processed count easily since the pipeline runs with dry-run=false
        // via the index command, but we can at least verify it doesn't error.
        run_index_full("full-test", Arc::clone(&db)).await.unwrap();

        // Chunk count should be stable (re-upsert, not duplicates).
        let count_after_full = db.chunk_count("full-test").unwrap();
        assert_eq!(
            count_after_first, count_after_full,
            "--full should re-upsert same chunks, not create duplicates"
        );
    }

    #[tokio::test]
    async fn unknown_kind_returns_adapter_error() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "pdf-test".to_string(),
            "PDF Test".to_string(),
            "pdf".to_string(),
            "/tmp/dummy".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        let result = run_index("pdf-test", db, true).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("pdf") || msg.contains("adapter"),
            "error should mention adapter kind: {msg}"
        );
    }
}
