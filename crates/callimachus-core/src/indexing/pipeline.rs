//! Indexing pipeline orchestrator.
//!
//! [`IndexPipeline`] owns a corpus, a storage backend, an adapter, and an LLM
//! provider.  Calling [`IndexPipeline::run`] executes a sequence of passes in
//! order, each pass enriching the index with pre-built understanding:
//!
//! | # | Pass | LLM | What it does |
//! |---|------|-----|--------------|
//! | 0 | [`Pass::History`] | No | Compares current source state against the last-indexed-version anchor; produces a [`ChangeManifest`] that downstream passes use to skip unchanged files. |
//! | 1 | [`Pass::Chunk`] | No | Walks the source and emits [`Chunk`](crate::types::Chunk) records with location URIs. |
//! | 2 | [`Pass::Structure`] | No | Parser-driven entity and edge extraction (tree-sitter for code, section hierarchy for books/wikis). |
//! | 3 | [`Pass::Semantic`] | Yes | LLM-powered entity and edge extraction over chunk content. |
//! | 4 | [`Pass::Aliases`] | Yes | Merges entity name variants into canonical entities. |
//! | 5 | [`Pass::Summarize`] | Yes | Bottom-up LLM summarization: function → file → corpus for code; scene → chapter → corpus for books. |
//! | 6 | [`Pass::Purpose`] | Yes | Asks the LLM why each entity exists; stores an `entity_purpose` row. |
//! | 7 | [`Pass::Contract`] | Yes | Static signals (is_public, is_fallible, …) plus LLM-inferred assumptions, risks, and caller notes; stores an `entity_contract` row. |
//! | 8 | [`Pass::Theme`] | Yes | Corpus-level architectural invariants (opt-in; not in the default pass list). |
//!
//! Passes are epistemically ordered: each depends on the outputs of earlier
//! passes.  They can be run individually with `calli index --pass <name>` or
//! skipped by omitting them from [`IndexOptions::passes`].
//!
//! [`ChangeManifest`]: crate::indexing::change_manifest::ChangeManifest

use std::sync::Arc;

use callimachus_llm::LlmProvider;

use crate::{
    adapter::SourceAdapter,
    indexing::{change_manifest::ChangeManifest, model_tier::TierConfig},
    storage::{StorageBackend, run_log},
    types::{Corpus, Pass, RunStatus},
};

use super::{
    aliases_pass, cascade, chunk_pass, contract_pass, embed_pass, history_pass, purpose_pass,
    semantic_pass, structure_pass, summarize_pass, theme_pass,
};

/// Which passes the pipeline should execute (default: all except Embed).
///
/// When `passes` omits `Pass::History`, the pipeline treats every source as
/// dirty (synthesises an all-dirty manifest) so that downstream passes
/// behave identically to a pre-Stage-0 run.
#[derive(Debug, Clone)]
pub struct IndexOptions {
    pub passes: Vec<Pass>,
    /// If set, chunk pass skips chunks until this chunk ID is first seen.
    pub from_chunk: Option<String>,
    /// Count but don't write anything.
    pub dry_run: bool,
    /// Max parallel LLM calls for providers that support concurrency.
    pub concurrency: Option<usize>,
    /// Force a full reindex: bypass all "skip if already processed" guards in
    /// every pass so previously-indexed chunks and entities are re-upserted.
    pub full: bool,
    /// If true, disable git-aware file walking in the code adapter.
    pub no_git_filter: bool,
    /// Tier-routing configuration.  When `enabled = false` (default), all
    /// passes use a single provider.  When enabled, the pipeline builds three
    /// provider variants (haiku/sonnet/opus) and the passes route per entity.
    pub tier_config: TierConfig,
    /// Manifest produced by Pass::History.  When None the pipeline uses an
    /// all-dirty sentinel so downstream passes process everything.
    /// Set by the orchestrator after Pass::History runs; callers constructing
    /// IndexOptions for a subset of passes may leave this None.
    pub change_manifest: Option<ChangeManifest>,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            passes: vec![
                Pass::History,
                Pass::Chunk,
                Pass::Structure,
                Pass::Semantic,
                Pass::Aliases,
                Pass::Summarize,
                Pass::Purpose,
                Pass::Contract,
                // Pass::Theme is opt-in; not included by default.
            ],
            from_chunk: None,
            dry_run: false,
            concurrency: None,
            full: false,
            no_git_filter: false,
            tier_config: TierConfig::default(),
            change_manifest: None,
        }
    }
}

/// Three provider variants, one per tier.
///
/// When tier routing is disabled, all three point to the same underlying
/// provider (three `Arc::clone`s — cheap).  When enabled, they are built
/// via `AnthropicApiProvider::with_model` and share a connection pool and
/// usage accounting `Arc`.
pub struct TierProviders {
    pub haiku: Arc<dyn LlmProvider>,
    pub sonnet: Arc<dyn LlmProvider>,
    pub opus: Arc<dyn LlmProvider>,
}

/// Build three tier providers from `base`.
///
/// If `tier_config.enabled` is true and `base` supports `with_model_override`
/// (i.e. it is an `AnthropicApiProvider`), produces three variants with the
/// configured model names sharing the same connection pool and usage `Arc`.
///
/// For non-Anthropic providers (DryRun, ClaudeCode), logs a WARN and returns
/// the same provider for all three tiers — indexing still completes correctly,
/// just without tier routing.
pub fn build_tier_providers(base: Arc<dyn LlmProvider>, cfg: &TierConfig) -> TierProviders {
    if !cfg.enabled {
        return TierProviders {
            haiku: Arc::clone(&base),
            sonnet: Arc::clone(&base),
            opus: Arc::clone(&base),
        };
    }

    let haiku_opt = base.with_model_override(&cfg.haiku_model);
    let sonnet_opt = base.with_model_override(&cfg.sonnet_model);
    let opus_opt = base.with_model_override(&cfg.opus_model);

    match (haiku_opt, sonnet_opt, opus_opt) {
        (Some(haiku), Some(sonnet), Some(opus)) => TierProviders {
            haiku,
            sonnet,
            opus,
        },
        _ => {
            tracing::warn!(
                "tier routing is enabled but provider '{}' does not support \
                 with_model_override; falling back to single-provider mode for all tiers",
                base.name()
            );
            TierProviders {
                haiku: Arc::clone(&base),
                sonnet: Arc::clone(&base),
                opus: Arc::clone(&base),
            }
        }
    }
}

/// Aggregate result returned after a pipeline run.
#[derive(Debug, Default)]
pub struct IndexResult {
    pub total_chunks: u64,
    pub total_entities: u64,
    pub total_edges: u64,
    pub cost_usd: f64,
    pub runs: Vec<run_log::RunRecord>,
}

/// The orchestrator.  Runs each requested pass in order, recording a run-log
/// entry per pass.
pub struct IndexPipeline {
    pub db: Arc<dyn StorageBackend>,
    pub adapter: Arc<dyn SourceAdapter>,
    pub llm: Arc<dyn LlmProvider>,
    /// Optional embedding provider. When `None`, the embed pass is skipped with a warning.
    pub embedder: Option<Arc<dyn LlmProvider>>,
}

impl IndexPipeline {
    pub async fn run(&self, corpus: &Corpus, opts: IndexOptions) -> anyhow::Result<IndexResult> {
        let mut result = IndexResult::default();

        // Build tier providers once for the whole run.
        let tiers = build_tier_providers(Arc::clone(&self.llm), &opts.tier_config);

        // ── Run-time visibility: log the resolved provider + tier + concurrency
        // configuration so it's obvious from the log how this run is wired.
        let uses_budget = tiers.haiku.budget().is_some();
        let provider_kind = if uses_budget {
            "Anthropic API"
        } else {
            "Claude Code CLI subprocess (no API key in config — auto-detected)"
        };
        tracing::info!(
            "[pipeline] provider = {provider_kind}  base_model = {}",
            self.llm.name()
        );
        if opts.tier_config.enabled {
            tracing::info!(
                "[pipeline] tier routing: ENABLED  haiku={}  sonnet={}  opus={}",
                opts.tier_config.haiku_model,
                opts.tier_config.sonnet_model,
                opts.tier_config.opus_model
            );
        } else {
            tracing::info!(
                "[pipeline] tier routing: DISABLED  all entities → {}",
                self.llm.name()
            );
        }
        tracing::info!(
            "[pipeline] concurrency cap (semaphore) = {}  budget admission = {}",
            opts.concurrency
                .map(|n| n.to_string())
                .unwrap_or_else(|| "64 (default)".to_string()),
            if uses_budget {
                "ACTIVE (TokenBudget)"
            } else {
                "n/a (claude CLI path)"
            }
        );

        // Probe rate limits so the token budget is seeded for each model family
        // before the first LLM-heavy pass begins.  Each probe fires a 1-token
        // request which seeds that family's budget from response headers.
        // Failures are logged as warnings and ignored — the budget will seed on
        // the first real request instead.
        //
        // We probe only if any tier provider exposes a budget (i.e. it is an
        // AnthropicApiProvider).  Non-budget providers are no-ops anyway.
        if uses_budget {
            tiers.haiku.probe_rate_limits().await;
            tiers.sonnet.probe_rate_limits().await;
            tiers.opus.probe_rate_limits().await;
        }

        // Mark any runs that were interrupted mid-pass as failed so the pipeline
        // starts from a consistent state.
        let abandoned = self.db.run_abandon_stale(&corpus.id)?;
        if abandoned > 0 {
            tracing::warn!(
                "marked {abandoned} stale 'running' run(s) as failed for corpus {}",
                corpus.id
            );
        }

        // Build a mutable copy of opts so we can inject the ChangeManifest after
        // Pass::History runs.  If passes don't include History, synthesise an
        // all-dirty manifest so downstream passes process everything.
        let mut opts_local = opts.clone();
        if !opts_local.passes.contains(&Pass::History) && opts_local.change_manifest.is_none() {
            opts_local.change_manifest = Some(ChangeManifest::all_dirty("synthetic"));
        }

        // Track the final manifest version so we can write it back after success.
        let mut history_version: Option<String> = None;

        for pass in &opts_local.passes.clone() {
            let run_id = self
                .db
                .run_start(&corpus.id, &pass.to_string(), Some(self.llm.name()))?;

            tracing::info!("[{}] starting…", pass);

            let stats = match pass {
                Pass::History => {
                    let (manifest, stats) = history_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        &opts_local,
                    )
                    .await?;
                    // Persist the version anchor as soon as history succeeds.
                    self.db
                        .corpus_set_last_indexed_version(&corpus.id, &manifest.current_version)?;
                    history_version = Some(manifest.current_version.clone());
                    opts_local.change_manifest = Some(manifest);

                    // Cascade-invalidate stale artifacts for dirty source files.
                    // This must run before chunk/structure/semantic passes so they
                    // start with a blank slate for the changed files.
                    if let Some(m) = opts_local.change_manifest.as_ref() {
                        cascade::run(Arc::clone(&self.db), corpus, m).await?;
                    }

                    stats
                }
                Pass::Chunk => {
                    chunk_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        &opts_local,
                    )
                    .await?
                }
                Pass::Structure => {
                    structure_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        &opts_local,
                    )
                    .await?
                }
                Pass::Semantic => {
                    semantic_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&self.llm),
                        &opts_local,
                    )
                    .await?
                }
                Pass::Aliases => {
                    aliases_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&self.llm),
                        &opts_local,
                    )
                    .await?
                }
                Pass::Summarize => {
                    summarize_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&tiers.haiku),
                        Arc::clone(&tiers.sonnet),
                        Arc::clone(&tiers.opus),
                        &opts_local,
                    )
                    .await?
                }
                Pass::Embed => {
                    embed_pass::run(self.db.as_ref(), corpus, self.embedder.clone(), &opts_local)
                        .await?
                }
                Pass::Purpose => {
                    purpose_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&tiers.haiku),
                        Arc::clone(&tiers.sonnet),
                        Arc::clone(&tiers.opus),
                        &opts_local,
                    )
                    .await?
                }
                Pass::Contract => {
                    contract_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&tiers.haiku),
                        Arc::clone(&tiers.sonnet),
                        Arc::clone(&tiers.opus),
                        &opts_local,
                    )
                    .await?
                }
                Pass::Theme => {
                    theme_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&self.llm),
                        &opts_local,
                    )
                    .await?
                }
            };

            self.db.run_finish(&run_id, RunStatus::Completed, &stats)?;

            tracing::info!(
                "[{}] done — processed={} skipped={} failed={}",
                pass,
                stats.processed,
                stats.skipped,
                stats.failed
            );

            // Accumulate cost.
            result.cost_usd += stats.cost_usd.unwrap_or(0.0);
        }

        // Tally final counts from DB.
        result.total_chunks = self.db.chunk_count(&corpus.id)?;
        result.total_entities = self.db.entity_count(&corpus.id)?;
        result.total_edges = self.db.edge_count(&corpus.id)?;
        result.runs = self.db.run_latest(&corpus.id, 20)?;

        // Mark the corpus as ready with a last-indexed timestamp.
        if !opts_local.dry_run {
            let now = chrono::Utc::now().to_rfc3339();
            self.db.corpus_set_last_indexed(&corpus.id, &now)?;

            // Write back the version anchor only after all passes succeed and
            // only when Pass::History was actually run.  Partial failures
            // must not advance the anchor so the next run replays correctly.
            if let Some(version) = history_version {
                self.db
                    .corpus_set_last_indexed_version(&corpus.id, &version)?;
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use callimachus_llm::DryRunProvider;

    use crate::{
        adapter::{
            DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, SourceAdapter,
        },
        storage::{SqliteBackend, StorageBackend},
        types::{Chunk, Corpus, Entity, Location},
    };

    use super::{IndexOptions, IndexPipeline};

    /// Minimal in-memory adapter that produces two chunks from a text string.
    struct FakeAdapter;

    #[async_trait::async_trait]
    impl SourceAdapter for FakeAdapter {
        fn kind(&self) -> &str {
            "fake"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }

        async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
            Ok(vec![DiscoveredSource {
                path: source.to_string(),
                kind: "text".to_string(),
                meta: serde_json::Value::Null,
            }])
        }

        async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
            let corpus_id = "test-corpus";
            Ok(vec![
                Chunk::new(
                    corpus_id.to_string(),
                    None,
                    "chapter".to_string(),
                    Location::new(corpus_id, "ch/1"),
                    format!("Chapter one content from {}", source.path),
                ),
                Chunk::new(
                    corpus_id.to_string(),
                    Some("ch/1".to_string()),
                    "scene".to_string(),
                    Location::new(corpus_id, "ch/1/sc/1"),
                    "Scene one content".to_string(),
                ),
            ])
        }

        async fn extract_structure(&self, _chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
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
            Ok(Some(ExtractedSemantic {
                entities: vec![],
                edges: vec![],
                summary_text: None,
            }))
        }

        async fn summarize(
            &self,
            _chunk: &Chunk,
            _llm: &dyn callimachus_llm::LlmProvider,
            _depth: &str,
        ) -> anyhow::Result<Option<String>> {
            Ok(Some("[test summary]".to_string()))
        }

        async fn resolve_aliases(
            &self,
            _entities: &[Entity],
            _llm: &dyn callimachus_llm::LlmProvider,
        ) -> anyhow::Result<Vec<EntityMerge>> {
            Ok(vec![])
        }

        fn format_location(&self, chunk: &Chunk) -> String {
            chunk.location.path.clone()
        }

        fn parse_location(&self, uri: &str) -> anyhow::Result<crate::adapter::LocationRef> {
            Ok(crate::adapter::LocationRef {
                corpus_id: "test-corpus".to_string(),
                path: uri.to_string(),
            })
        }
    }

    fn setup() -> (Arc<dyn StorageBackend>, Corpus) {
        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "test-corpus".to_string(),
            "Test Corpus".to_string(),
            "fake".to_string(),
            "/tmp/test.txt".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();
        (Arc::new(db), corpus)
    }

    #[tokio::test]
    async fn all_passes_complete_without_error() {
        let (db, corpus) = setup();
        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(FakeAdapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

        let result = pipeline
            .run(&corpus, IndexOptions::default())
            .await
            .unwrap();
        assert!(result.total_chunks > 0, "expected chunks after indexing");
        assert_eq!(
            result.runs.len(),
            8,
            "expected 8 run-log entries (history, chunk, structure, semantic, aliases, summarize, purpose, contract)"
        );
        for run in &result.runs {
            assert_eq!(run.status, "completed");
        }

        // Corpus status should now be 'ready' with a last_indexed_at timestamp.
        let updated = db.corpus_get(&corpus.id).unwrap().unwrap();
        assert_eq!(updated.status.to_string(), "ready");
        assert!(
            updated.last_indexed_at.is_some(),
            "last_indexed_at should be set after pipeline run"
        );
    }

    #[tokio::test]
    async fn chunk_pass_is_idempotent() {
        let (db, corpus) = setup();
        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(FakeAdapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

        let opts_chunk_only = IndexOptions {
            passes: vec![crate::types::Pass::Chunk],
            ..Default::default()
        };

        // First run: process chunks.
        let r1 = pipeline
            .run(&corpus, opts_chunk_only.clone())
            .await
            .unwrap();
        let processed_first = db.chunk_count(&corpus.id).unwrap();

        // Second run: all chunks should be skipped.
        let r2 = pipeline.run(&corpus, opts_chunk_only).await.unwrap();
        let processed_second = db.chunk_count(&corpus.id).unwrap();

        assert_eq!(
            processed_first, processed_second,
            "chunk count should not change on re-run"
        );
        assert_eq!(r1.total_chunks, r2.total_chunks);
    }

    #[tokio::test]
    async fn dry_run_writes_no_chunks() {
        let (db, corpus) = setup();
        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(FakeAdapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

        let opts = IndexOptions {
            passes: vec![crate::types::Pass::Chunk],
            dry_run: true,
            ..Default::default()
        };
        pipeline.run(&corpus, opts).await.unwrap();

        let count = db.chunk_count(&corpus.id).unwrap();
        assert_eq!(count, 0, "dry-run should not write chunks");
    }

    /// When tier routing is enabled and the base provider supports
    /// `with_model_override`, `build_tier_providers` returns three distinct
    /// variants — each using the model name from the tier config.
    #[test]
    fn tier_routing_uses_correct_provider_per_tier() {
        use callimachus_llm::{AnthropicApiProvider, LlmProvider};

        use crate::indexing::model_tier::TierConfig;

        use super::build_tier_providers;

        let cfg = TierConfig {
            enabled: true,
            haiku_model: "haiku-test".to_string(),
            sonnet_model: "sonnet-test".to_string(),
            opus_model: "opus-test".to_string(),
            ..TierConfig::default()
        };

        // AnthropicApiProvider supports with_model_override.
        let base: Arc<dyn LlmProvider> = Arc::new(AnthropicApiProvider::new(
            "test-key".to_string(),
            Some("sonnet-test".to_string()),
            None,
        ));

        let tiers = build_tier_providers(Arc::clone(&base), &cfg);

        assert_eq!(tiers.haiku.name(), "haiku-test");
        assert_eq!(tiers.sonnet.name(), "sonnet-test");
        assert_eq!(tiers.opus.name(), "opus-test");
    }

    /// When tier routing is enabled but the provider does not support
    /// `with_model_override` (e.g. DryRunProvider), `build_tier_providers`
    /// falls back gracefully — all three tiers use the base provider's name.
    #[test]
    fn tier_routing_falls_back_for_provider_without_override() {
        use callimachus_llm::LlmProvider;

        use crate::indexing::model_tier::TierConfig;

        use super::build_tier_providers;

        let cfg = TierConfig {
            enabled: true,
            ..TierConfig::default()
        };

        // DryRunProvider returns None from with_model_override.
        let base: Arc<dyn LlmProvider> = Arc::new(DryRunProvider::new());
        let tiers = build_tier_providers(Arc::clone(&base), &cfg);

        // All three should fall back to the base provider's name.
        assert_eq!(tiers.haiku.name(), "dry-run");
        assert_eq!(tiers.sonnet.name(), "dry-run");
        assert_eq!(tiers.opus.name(), "dry-run");
    }

    /// Verifies that running the pipeline with explicit `concurrency = Some(N)`
    /// produces identical processed/skipped/failed counts to `concurrency = None`
    /// (which defaults to 4 for DryRun providers).
    ///
    /// Since DryRun instantly returns without LLM work, both paths exercise the
    /// same per-entity logic; what changes is only how many entities run concurrently.
    #[tokio::test]
    async fn concurrent_run_matches_sequential_counts() {
        let (db, corpus) = setup();
        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(FakeAdapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

        // Run with default concurrency (None → 4).
        let r1 = pipeline
            .run(&corpus, IndexOptions::default())
            .await
            .unwrap();

        // Reset corpus and re-run with concurrency = 1 (effectively sequential).
        // We use a fresh in-memory DB to get a clean slate.
        let (db2, corpus2) = setup();
        let pipeline2 = IndexPipeline {
            db: db2.clone(),
            adapter: Arc::new(FakeAdapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };
        let r2 = pipeline2
            .run(
                &corpus2,
                IndexOptions {
                    concurrency: Some(1),
                    ..IndexOptions::default()
                },
            )
            .await
            .unwrap();

        // Entity and chunk counts must be identical regardless of concurrency.
        assert_eq!(
            r1.total_chunks, r2.total_chunks,
            "chunk counts should match"
        );
        assert_eq!(
            r1.total_entities, r2.total_entities,
            "entity counts should match"
        );
        assert_eq!(r1.total_edges, r2.total_edges, "edge counts should match");

        // Run counts should be equal.
        assert_eq!(r1.runs.len(), r2.runs.len(), "run counts should match");

        // All runs should complete successfully in both cases.
        for run in r1.runs.iter().chain(r2.runs.iter()) {
            assert_eq!(
                run.status, "completed",
                "all runs should be completed; got {:?}",
                run
            );
        }
    }

    /// DryRun pipeline with concurrency = None (uses default 4).
    #[tokio::test]
    async fn pipeline_runs_with_default_concurrency() {
        let (db, corpus) = setup();
        let pipeline = IndexPipeline {
            db,
            adapter: Arc::new(FakeAdapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

        let result = pipeline
            .run(
                &corpus,
                IndexOptions {
                    concurrency: None,
                    ..IndexOptions::default()
                },
            )
            .await
            .unwrap();

        for run in &result.runs {
            assert_eq!(run.status, "completed");
        }
    }
}
