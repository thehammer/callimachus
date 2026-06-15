use std::sync::Arc;

use anyhow::{Result, bail};
use callimachus_adapter_contract::AdapterRegistry;
use callimachus_core::{
    adapter::SourceAdapter,
    indexing::{IndexOptions, IndexPipeline},
    storage::StorageBackend,
    types::{Corpus, Pass},
};
use callimachus_llm::{
    EmbeddingProviderConfig, ProviderConfig, build_embedding_provider, build_provider,
};

use crate::config::GlobalConfig;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    corpus_id: &str,
    pass: Option<String>,
    from_chunk: Option<String>,
    dry_run: bool,
    full: bool,
    no_git_filter: bool,
    concurrency: Option<usize>,
    stable_sampling: bool,
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

    // Resolve adapter via the registry (single selection path for all commands).
    let registry = default_registry();
    let adapter = resolve_adapter(&corpus, &registry)?;

    // Build index options.
    let passes = resolve_passes(pass)?;

    // Fail-fast: if embed was requested, the embedding config must be usable.
    let embed_requested = passes.contains(&Pass::Embed);
    let embedder = if embed_requested {
        let embed_cfg = build_embedding_provider_config(config);
        match build_embedding_provider(embed_cfg) {
            Ok(Some(p)) => Some(p),
            Ok(None) => {
                bail!(
                    "--pass embed/all requested but [embedding] is disabled or absent \
                     in config; set [embedding] enabled = true with a Voyage api_key_env"
                );
            }
            Err(e) => {
                bail!("embeddings requested via --pass but not usable: {e}");
            }
        }
    } else {
        None
    };

    let opts = IndexOptions {
        passes,
        from_chunk,
        dry_run,
        full,
        no_git_filter,
        concurrency,
        stable_sampling,
        tier_config: config.model_tiers.clone(),
        change_manifest: None,
        ..IndexOptions::default()
    };

    let dry_label = if dry_run { " [dry-run]" } else { "" };
    eprintln!("Indexing corpus '{}'{dry_label}…", corpus.id);

    // Run pipeline.
    let pipeline = IndexPipeline {
        db,
        adapter,
        llm: Arc::new(llm),
        embedder,
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

/// Translate the CLI's `EmbeddingConfig` into the llm crate's
/// `EmbeddingProviderConfig` so the llm crate stays unaware of CLI types.
pub fn build_embedding_provider_config(config: &GlobalConfig) -> EmbeddingProviderConfig {
    match &config.embedding {
        None => EmbeddingProviderConfig::default(),
        Some(e) => EmbeddingProviderConfig {
            enabled: e.enabled,
            provider: e.provider.clone(),
            model: e.model.clone(),
            api_key: e.api_key.clone(),
            api_key_env: e.api_key_env.clone(),
        },
    }
}

/// Corpus kinds Callimachus recognizes as valid, independent of whether an
/// adapter for them is compiled into *this* binary.
///
/// Used by [`resolve_adapter`] and the `corpus` command to distinguish two
/// failure modes: a recognized kind whose adapter simply isn't in this build,
/// versus a kind Callimachus doesn't know about at all. This binary ships the
/// first four; `docs`/`jira`/`sessions` are recognized future/proprietary
/// adapters that other builds may carry.
pub const KNOWN_KINDS: &[&str] = &[
    "book", "capture", "code", "wiki", "docs", "jira", "sessions",
];

/// Build the registry of adapters compiled into the default `calli` binary.
///
/// This is the **single** place adapter constructors are named. Every command
/// resolves its adapter through [`resolve_adapter`] against a registry built
/// here, so adding/removing an adapter from the binary is a one-line change
/// with no `match corpus.kind` to update (PRD A7).
pub fn default_registry() -> AdapterRegistry {
    use callimachus_adapter_book::BookAdapter;
    use callimachus_adapter_capture::CaptureAdapter;
    use callimachus_adapter_code::CodeAdapter;
    use callimachus_adapter_wiki::WikiAdapter;

    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(BookAdapter::new()));
    registry.register(Arc::new(CaptureAdapter::new()));
    registry.register(Arc::new(CodeAdapter::new()));
    registry.register(Arc::new(WikiAdapter::new()));
    registry
}

/// Why a corpus kind is not serviceable by *this* build, or `None` if it is.
///
/// Shared by `corpus add` (which warns but still registers) and
/// `corpus list`/`status` (which annotate inspection output), so the
/// availability story is computed one way and worded consistently (PRD A6).
///
/// This is intentionally a *dynamic* lookup against the live registry — the
/// missing-adapter state is never persisted to the database, because a
/// different binary sharing the same DB may carry the adapter. A persisted
/// "unindexable" flag would be wrong for that binary.
pub fn unavailable_kind_reason(kind: &str, registry: &AdapterRegistry) -> Option<String> {
    if registry.get(kind).is_some() {
        return None;
    }
    if KNOWN_KINDS.contains(&kind) {
        Some(format!(
            "no adapter for kind '{kind}' is compiled into this build of calli"
        ))
    } else {
        Some(format!(
            "'{kind}' is not a recognized Callimachus corpus kind"
        ))
    }
}

/// Resolve the adapter for a corpus through the registry.
///
/// This is the one selection path shared by `index`, `ingest`, `reindex`,
/// `watch`, and `history`. It never panics and never falls back to a default
/// adapter. On a miss it distinguishes two cases (PRD A6b):
///
/// * **Recognized kind, not in this build** — the kind is one Callimachus knows
///   ([`KNOWN_KINDS`]) but no adapter for it was compiled into this binary.
/// * **Unknown kind** — the kind is not a recognized Callimachus corpus kind.
///
/// Both messages name the offending kind and list the kinds this binary
/// supports.
pub fn resolve_adapter(
    corpus: &Corpus,
    registry: &AdapterRegistry,
) -> Result<Arc<dyn SourceAdapter>> {
    if let Some(adapter) = registry.get(&corpus.kind) {
        return Ok(adapter);
    }

    let supported = registry.list().join(", ");
    if KNOWN_KINDS.contains(&corpus.kind.as_str()) {
        bail!(
            "no adapter for corpus kind '{kind}' is compiled into this build of calli. \
             '{kind}' is a recognized Callimachus corpus kind, but this binary was built \
             without its adapter — use (or build) a calli that includes it. \
             Adapters available in this build: {supported}.",
            kind = corpus.kind,
        );
    }
    bail!(
        "unknown corpus kind '{kind}' — not a recognized Callimachus corpus kind. \
         Adapters available in this build: {supported}.",
        kind = corpus.kind,
    )
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
            Pass::History,
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Summarize,
            Pass::Purpose,
            Pass::Contract,
            Pass::Theme,
        ]),
        Some("all") => Ok(vec![
            Pass::History,
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
        Some("history") => Ok(vec![Pass::History]),
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
            "unknown pass '{other}'; use: all, history, chunk, structure, semantic, aliases, summarize, purpose, contract, theme, embed"
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
            false,
            None,
            false, // stable_sampling
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
            false,
            None,
            false, // stable_sampling
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
            false,
            None,
            false, // stable_sampling
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
    async fn php_corpus_selects_code_adapter() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "php-test".to_string(),
            "PHP Test".to_string(),
            "code".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        // Dry run should select CodeAdapter (PHP files are in a code corpus) and complete without error.
        run_index("php-test", db, true).await.unwrap();
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

    // ── A6b: distinct error messages for "not in this build" vs "unknown kind" ──

    /// A corpus of a registered kind resolves its adapter successfully.
    #[test]
    fn resolve_adapter_returns_ok_for_registered_kind() {
        let registry = super::default_registry();
        let corpus = Corpus::new(
            "code-ok".to_string(),
            "Code OK".to_string(),
            "code".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string(),
        );
        let result = super::resolve_adapter(&corpus, &registry);
        assert!(
            result.is_ok(),
            "expected Ok for registered kind 'code', got: {:?}",
            result.err()
        );
    }

    /// A corpus whose kind is in KNOWN_KINDS but has no adapter in this build
    /// returns Err with a message that names the kind, mentions "build", and
    /// lists a kind that IS supported (e.g. "code"). It must NOT contain
    /// "unknown corpus kind".
    #[test]
    fn resolve_adapter_for_known_but_absent_kind_error_distinguishes_from_unknown() {
        let registry = super::default_registry();
        let corpus = Corpus::new(
            "docs-corpus".to_string(),
            "Docs Corpus".to_string(),
            "docs".to_string(),
            "/tmp/docs".to_string(),
        );
        let result = super::resolve_adapter(&corpus, &registry);
        assert!(
            result.is_err(),
            "expected Err for kind 'docs' (not in this build)"
        );

        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("docs"),
            "error must name the offending kind 'docs': {msg}"
        );
        assert!(
            msg.contains("build"),
            "error must indicate the kind is not in this build: {msg}"
        );
        assert!(
            msg.contains("code"),
            "error must list a supported kind so the user knows what is available: {msg}"
        );
        assert!(
            !msg.contains("unknown corpus kind"),
            "error for a known-but-absent kind must not say 'unknown corpus kind' (wrong bucket): {msg}"
        );
    }

    /// A corpus whose kind is not recognized at all returns Err with a message
    /// that names the kind and contains "unknown corpus kind".
    #[test]
    fn resolve_adapter_for_completely_unknown_kind_says_unknown() {
        let registry = super::default_registry();
        let corpus = Corpus::new(
            "pdf-corpus".to_string(),
            "PDF Corpus".to_string(),
            "pdf".to_string(),
            "/tmp/pdf".to_string(),
        );
        let result = super::resolve_adapter(&corpus, &registry);
        assert!(result.is_err(), "expected Err for unrecognized kind 'pdf'");

        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("pdf"),
            "error must name the offending kind 'pdf': {msg}"
        );
        assert!(
            msg.contains("unknown corpus kind"),
            "error for an unrecognized kind must say 'unknown corpus kind': {msg}"
        );
    }

    // ── A6: availability predicate ──────────────────────────────────────────────

    /// A kind present in the registry has no unavailability reason.
    #[test]
    fn unavailable_kind_reason_is_none_for_registered_kind() {
        let registry = super::default_registry();
        let reason = super::unavailable_kind_reason("code", &registry);
        assert!(
            reason.is_none(),
            "expected None for 'code' (registered); got: {:?}",
            reason
        );
    }

    /// A kind in KNOWN_KINDS but absent from this build's registry returns
    /// Some with a message about being compiled into this build.
    #[test]
    fn unavailable_kind_reason_for_known_but_absent_kind_mentions_build() {
        let registry = super::default_registry();
        let reason = super::unavailable_kind_reason("docs", &registry);
        assert!(
            reason.is_some(),
            "expected Some for 'docs' (known but not in this build)"
        );
        let msg = reason.unwrap();
        assert!(
            msg.contains("build"),
            "reason for known-but-absent kind must mention build: {msg}"
        );
    }

    /// A completely unrecognized kind returns Some with a message about not being
    /// a recognized corpus kind.
    #[test]
    fn unavailable_kind_reason_for_unknown_kind_mentions_not_recognized() {
        let registry = super::default_registry();
        let reason = super::unavailable_kind_reason("pdf", &registry);
        assert!(
            reason.is_some(),
            "expected Some for unrecognized kind 'pdf'"
        );
        // The reason should distinguish this case from the "known but absent" case.
        // It must not claim the kind is absent from the build (that implies it's known).
        let msg = reason.unwrap();
        assert!(
            msg.contains("pdf"),
            "reason must name the offending kind: {msg}"
        );
    }

    #[tokio::test]
    async fn embed_pass_without_config_errors_loudly() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "embed-test".to_string(),
            "Embed Test".to_string(),
            "code".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        // Request embed pass with default config (no [embedding] block).
        let result = super::run(
            "embed-test",
            Some("embed".to_string()),
            None,
            false,
            false,
            false,
            None,
            false,
            Some("dry-run".to_string()),
            db,
            &GlobalConfig::default(),
        )
        .await;

        assert!(
            result.is_err(),
            "expected error when embed requested but not configured"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("embed") || msg.contains("embedding") || msg.contains("[embedding]"),
            "error should mention embed/embedding config: {msg}"
        );
    }
}
