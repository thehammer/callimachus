use std::sync::Arc;

use callimachus_llm::LlmProvider;

use crate::{
    adapter::SourceAdapter,
    storage::{StorageBackend, run_log},
    types::{Corpus, Pass, RunStatus},
};

use super::{
    aliases_pass, chunk_pass, contract_pass, embed_pass, purpose_pass, semantic_pass,
    structure_pass, summarize_pass, theme_pass,
};

/// Which passes the pipeline should execute (default: all except Embed).
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
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            passes: vec![
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

        // Mark any runs that were interrupted mid-pass as failed so the pipeline
        // starts from a consistent state.
        let abandoned = self.db.run_abandon_stale(&corpus.id)?;
        if abandoned > 0 {
            tracing::warn!(
                "marked {abandoned} stale 'running' run(s) as failed for corpus {}",
                corpus.id
            );
        }

        for pass in &opts.passes {
            let run_id = self
                .db
                .run_start(&corpus.id, &pass.to_string(), Some(self.llm.name()))?;

            tracing::info!("[{}] starting…", pass);

            let stats = match pass {
                Pass::Chunk => {
                    chunk_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        &opts,
                    )
                    .await?
                }
                Pass::Structure => {
                    structure_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        &opts,
                    )
                    .await?
                }
                Pass::Semantic => {
                    semantic_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&self.llm),
                        &opts,
                    )
                    .await?
                }
                Pass::Aliases => {
                    aliases_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&self.llm),
                        &opts,
                    )
                    .await?
                }
                Pass::Summarize => {
                    summarize_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&self.llm),
                        &opts,
                    )
                    .await?
                }
                Pass::Embed => {
                    embed_pass::run(self.db.as_ref(), corpus, self.embedder.clone(), &opts).await?
                }
                Pass::Purpose => {
                    purpose_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&self.llm),
                        &opts,
                    )
                    .await?
                }
                Pass::Contract => {
                    contract_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&self.llm),
                        &opts,
                    )
                    .await?
                }
                Pass::Theme => {
                    theme_pass::run(
                        Arc::clone(&self.db),
                        corpus,
                        Arc::clone(&self.adapter),
                        Arc::clone(&self.llm),
                        &opts,
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
        if !opts.dry_run {
            let now = chrono::Utc::now().to_rfc3339();
            self.db.corpus_set_last_indexed(&corpus.id, &now)?;
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
            7,
            "expected 7 run-log entries (chunk, structure, semantic, aliases, summarize, purpose, contract)"
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
}
