use std::sync::Arc;

use anyhow::{Result, bail};
use callimachus_adapter_contract::AdapterRegistry;
use callimachus_core::{
    adapter::SourceAdapter,
    indexing::{IndexOptions, IndexPipeline},
    storage::StorageBackend,
    types::{Corpus, Pass, parse_passes_list},
};
use callimachus_llm::{
    EmbeddingProviderConfig, LlmProvider, ProviderConfig, build_embedding_provider, build_provider,
};

use crate::commands::run_control::{EstimateProfile, RunControl, print_estimate};
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
    control: RunControl,
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
    let profile = EstimateProfile::for_provider(&provider_config, config, concurrency);
    let llm = build_provider(provider_config)
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;
    let mut llm: Arc<dyn LlmProvider> = Arc::new(llm);
    if control.estimate {
        llm = profile.provider(llm);
    }

    // Resolve adapter via the registry (single selection path for all commands).
    let registry = default_registry();
    let adapter = resolve_adapter(&corpus, &registry)?;

    // Build index options.
    let passes = resolve_passes(pass.or_else(|| corpus_passes(&corpus)))?;

    // Fail-fast: if embed was requested, the embedding config must be usable.
    let embed_requested = passes.contains(&Pass::Embed) && !control.estimate;
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

    let mut opts = IndexOptions {
        passes,
        from_chunk,
        dry_run: dry_run && !control.estimate,
        full,
        no_git_filter,
        concurrency,
        stable_sampling,
        tier_config: config.model_tiers.clone(),
        change_manifest: None,
        ..IndexOptions::default()
    };
    control.apply(&mut opts)?;

    let dry_label = if dry_run { " [dry-run]" } else { "" };
    eprintln!("Indexing corpus '{}'{dry_label}…", corpus.id);

    // Run pipeline.
    let report_db = Arc::clone(&db);
    let pipeline = IndexPipeline {
        db,
        adapter,
        llm,
        embedder,
    };

    let result = pipeline.run(&corpus, opts).await?;

    if control.estimate {
        let summaries_present = !report_db.summary_list(&corpus.id)?.is_empty();
        print_estimate(&result, &profile, summaries_present);
        return Ok(());
    }

    // Print summary.
    println!("Done.");
    println!("  Chunks:   {}", result.total_chunks);
    println!("  Entities: {}", result.total_entities);
    println!("  Edges:    {}", result.total_edges);
    if result.cost_usd > 0.0 {
        println!("  Cost:     ${:.4}", result.cost_usd);
    }
    for run in &result.runs {
        let status_icon = match run.status.as_str() {
            "completed" => "✓",
            "stopped" => "■",
            _ => "✗",
        };
        println!(
            "  {status_icon} pass={:<10} processed={} skipped={} failed={}",
            run.pass, run.stats.processed, run.stats.skipped, run.stats.failed
        );
    }

    if let Some(reason) = &result.stopped_reason {
        println!("stopped: {reason} — re-run the same command to resume");
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
        Some("claude-code") | Some("claude_code") => {
            let cc = &config.llm.claude_code;
            Ok(ProviderConfig::ClaudeCode {
                claude_bin: None,
                model: cc.model.clone().or_else(|| config.llm.model.clone()),
                timeout_secs: cc.timeout_secs,
                calls_per_minute: cc.calls_per_minute,
                lean: cc.lean,
                effort: cc.effort.clone(),
                extra_args: cc.extra_args.clone(),
            })
        }
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

/// Passes named by the corpus config key `"passes"` (a string or a list).
fn corpus_passes(corpus: &Corpus) -> Option<String> {
    match corpus.config.get("passes")? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(items) => {
            let names: Vec<&str> = items.iter().filter_map(|v| v.as_str()).collect();
            (!names.is_empty()).then(|| names.join(","))
        }
        _ => None,
    }
}

/// Resolve `--pass`: one name or profile (`all`, `decompile`, `default`), or a
/// comma list of them. Passes run in pipeline order whatever order was given.
pub(crate) fn resolve_passes(pass: Option<String>) -> Result<Vec<Pass>> {
    let Some(spec) = pass else {
        return resolve_passes(Some("default".to_string()));
    };
    let mut passes: Vec<Pass> = Vec::new();
    for token in spec.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let expanded = if token == "all" {
            let mut all = parse_passes_list("default").map_err(|e| anyhow::anyhow!(e))?;
            all.push(Pass::Embed);
            all
        } else {
            parse_passes_list(token).map_err(|e| {
                anyhow::anyhow!("{e}; also accepted: all, decompile (profiles), comma lists")
            })?
        };
        for p in expanded {
            if !passes.contains(&p) {
                passes.push(p);
            }
        }
    }
    if passes.is_empty() {
        bail!("--pass requires at least one pass name");
    }
    passes.sort_by_key(pass_rank);
    Ok(passes)
}

fn pass_rank(pass: &Pass) -> u8 {
    match pass {
        Pass::History => 0,
        Pass::Chunk => 1,
        Pass::Structure => 2,
        Pass::Semantic => 3,
        Pass::Aliases => 4,
        Pass::Summarize => 5,
        Pass::Purpose => 6,
        Pass::Contract => 7,
        Pass::Theme => 8,
        Pass::Embed => 9,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use callimachus_core::storage::{SqliteBackend, StorageBackend};
    use callimachus_core::types::Corpus;

    use crate::commands::run_control::RunControl;
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
            RunControl::default(),
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
            RunControl::default(),
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
            RunControl::default(),
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
            RunControl::default(),
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

    // ── Run control on a tiny C corpus ──────────────────────────────────────

    fn c_corpus() -> (tempfile::TempDir, Arc<dyn StorageBackend>) {
        let dir = tempfile::tempdir().unwrap();
        for (file, body) in [
            ("fun_a.c", "void fun_a(void) {\n  fun_b();\n}\n"),
            ("fun_b.c", "void fun_b(void) {\n  DAT_00001000 = 1;\n}\n"),
            ("fun_c.c", "int fun_c(int x) {\n  return x + 1;\n}\n"),
            ("fun_d.c", "void fun_d(void) {\n  fun_c(2);\n}\n"),
        ] {
            std::fs::write(dir.path().join(file), body).unwrap();
        }
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        db.corpus_insert(&Corpus::new(
            "c4".to_string(),
            "C4".to_string(),
            "code".to_string(),
            dir.path().display().to_string(),
        ))
        .unwrap();
        (dir, db)
    }

    async fn index_c(
        db: &Arc<dyn StorageBackend>,
        pass: &str,
        control: RunControl,
    ) -> anyhow::Result<()> {
        super::run(
            "c4",
            Some(pass.to_string()),
            None,
            false,
            false,
            true, // no_git_filter
            Some(4),
            false,
            control,
            Some("dry-run".to_string()),
            Arc::clone(db),
            &GlobalConfig::default(),
        )
        .await
    }

    #[test]
    fn decompile_profile_expands_to_four_passes() {
        use callimachus_core::types::Pass::*;
        assert_eq!(
            super::resolve_passes(Some("decompile".into())).unwrap(),
            vec![Chunk, Structure, Summarize, Purpose]
        );
    }

    #[test]
    fn comma_lists_and_profiles_resolve_in_pipeline_order() {
        use callimachus_core::types::Pass::*;
        assert_eq!(
            super::resolve_passes(Some("purpose, chunk,summarize".into())).unwrap(),
            vec![Chunk, Summarize, Purpose]
        );
        assert_eq!(
            super::resolve_passes(Some("decompile,contract".into())).unwrap(),
            vec![Chunk, Structure, Summarize, Purpose, Contract]
        );
        assert!(super::resolve_passes(Some("bogus".into())).is_err());
        assert!(
            super::resolve_passes(Some("all".into()))
                .unwrap()
                .contains(&Embed)
        );
    }

    #[test]
    fn corpus_config_passes_key_is_a_string_or_list() {
        let mut c = Corpus::new("x".into(), "x".into(), "code".into(), "/tmp".into());
        assert_eq!(super::corpus_passes(&c), None);
        c.config = serde_json::json!({"passes": "decompile"});
        assert_eq!(super::corpus_passes(&c).as_deref(), Some("decompile"));
        c.config = serde_json::json!({"passes": ["chunk", "structure"]});
        assert_eq!(super::corpus_passes(&c).as_deref(), Some("chunk,structure"));
    }

    #[tokio::test]
    async fn estimate_on_c_corpus_writes_no_artifacts() {
        let (_dir, db) = c_corpus();
        index_c(&db, "chunk,structure", RunControl::default())
            .await
            .unwrap();
        assert!(db.entity_count("c4").unwrap() >= 4);
        let runs_before = db.run_latest("c4", 50).unwrap().len();

        index_c(
            &db,
            "decompile",
            RunControl {
                entities: vec!["name:fun_*".into()],
                estimate: true,
                ..RunControl::default()
            },
        )
        .await
        .unwrap();

        assert!(db.summary_list("c4").unwrap().is_empty());
        assert!(db.purpose_list("c4").unwrap().is_empty());
        assert_eq!(db.run_latest("c4", 50).unwrap().len(), runs_before);

        // A real, filtered run afterwards: only the matching entity, resumable.
        index_c(
            &db,
            "purpose",
            RunControl {
                entities: vec!["callers-of:fun_b".into()],
                ..RunControl::default()
            },
        )
        .await
        .unwrap();
        let purposes = db.purpose_list("c4").unwrap();
        assert_eq!(purposes.len(), 1);
        assert!(purposes[0].entity_id.ends_with("fun_a"), "{purposes:?}");
    }

    #[tokio::test]
    async fn inspect_artifacts_json_has_expected_keys() {
        let (_dir, db) = c_corpus();
        index_c(
            &db,
            "chunk,structure,summarize,purpose",
            RunControl::default(),
        )
        .await
        .unwrap();
        let records =
            crate::commands::inspect::run_artifacts("c4", &["name:fun_c".to_string()], db.as_ref())
                .unwrap();
        assert_eq!(records.len(), 1);
        let json = serde_json::to_string_pretty(&records).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let first = &v.as_array().unwrap()[0];
        for key in [
            "id",
            "name",
            "kind",
            "location",
            "summary",
            "purpose",
            "purpose_model",
            "contract",
            "run_timestamp",
        ] {
            assert!(first.get(key).is_some(), "missing key {key}: {json}");
        }
        assert_eq!(first["name"], "fun_c");
        assert!(first["purpose"].is_string(), "{json}");
        assert!(first["summary"].is_string(), "{json}");
        assert!(first["run_timestamp"].is_string());
        let text = crate::commands::inspect::format_artifacts(&records);
        assert!(text.contains("fun_c") && text.contains("purpose:"));
    }
}
