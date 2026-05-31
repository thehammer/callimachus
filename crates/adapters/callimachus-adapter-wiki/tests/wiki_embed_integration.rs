/// Integration test: index the sample_wiki fixture end-to-end with DryRunProvider + embed pass.
use std::sync::Arc;

use callimachus_adapter_wiki::WikiAdapter;
use callimachus_core::{
    adapter::SourceAdapter,
    indexing::{IndexOptions, IndexPipeline},
    storage::{SqliteBackend, StorageBackend},
    types::{Corpus, Pass},
};
use callimachus_llm::{DryRunProvider, EmbeddingProvider};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_wiki");

async fn setup() -> (Arc<dyn StorageBackend>, Corpus) {
    let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus = Corpus::new(
        "testwiki".to_string(),
        "Test Wiki".to_string(),
        "wiki".to_string(),
        FIXTURE.to_string(),
    );
    db.corpus_insert(&corpus).unwrap();
    (db, corpus)
}

/// A custom WikiAdapter that injects corpus_id into meta during chunk().
struct CorpusAwareWikiAdapter {
    inner: WikiAdapter,
    corpus_id: String,
    root: String,
}

impl CorpusAwareWikiAdapter {
    fn new(corpus_id: &str, root: &str) -> Self {
        Self {
            inner: WikiAdapter::new(),
            corpus_id: corpus_id.to_string(),
            root: root.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl SourceAdapter for CorpusAwareWikiAdapter {
    fn kind(&self) -> &str {
        "wiki"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    async fn discover(
        &self,
        source: &str,
    ) -> anyhow::Result<Vec<callimachus_core::adapter::DiscoveredSource>> {
        self.inner.discover(source).await
    }

    async fn chunk(
        &self,
        source: &callimachus_core::adapter::DiscoveredSource,
    ) -> anyhow::Result<Vec<callimachus_core::types::Chunk>> {
        let mut src = source.clone();
        src.meta = serde_json::json!({
            "root": self.root,
            "corpus_id": self.corpus_id,
        });
        self.inner.chunk(&src).await
    }

    async fn extract_structure(
        &self,
        chunk: &callimachus_core::types::Chunk,
    ) -> anyhow::Result<callimachus_core::adapter::ExtractedStructure> {
        self.inner.extract_structure(chunk).await
    }

    async fn extract_with_llm(
        &self,
        chunk: &callimachus_core::types::Chunk,
        llm: &dyn callimachus_llm::LlmProvider,
    ) -> anyhow::Result<Option<callimachus_core::adapter::ExtractedSemantic>> {
        self.inner.extract_with_llm(chunk, llm).await
    }

    async fn summarize(
        &self,
        chunk: &callimachus_core::types::Chunk,
        llm: &dyn callimachus_llm::LlmProvider,
        depth: &str,
    ) -> anyhow::Result<Option<String>> {
        self.inner.summarize(chunk, llm, depth).await
    }

    async fn resolve_aliases(
        &self,
        entities: &[callimachus_core::types::Entity],
        llm: &dyn callimachus_llm::LlmProvider,
    ) -> anyhow::Result<Vec<callimachus_core::adapter::EntityMerge>> {
        self.inner.resolve_aliases(entities, llm).await
    }

    fn format_location(&self, chunk: &callimachus_core::types::Chunk) -> String {
        self.inner.format_location(chunk)
    }

    fn parse_location(&self, uri: &str) -> anyhow::Result<callimachus_core::adapter::LocationRef> {
        self.inner.parse_location(uri)
    }
}

#[tokio::test]
async fn end_to_end_index_with_embed_pass() {
    let (db, corpus) = setup().await;
    let adapter = Arc::new(CorpusAwareWikiAdapter::new(&corpus.id, FIXTURE));
    let dry = Arc::new(DryRunProvider::new());
    let llm: Arc<dyn callimachus_llm::LlmProvider> =
        Arc::clone(&dry) as Arc<dyn callimachus_llm::LlmProvider>;

    // Run chunk + embed passes.
    let opts = IndexOptions {
        passes: vec![Pass::Chunk, Pass::Embed],
        ..Default::default()
    };

    let pipeline = IndexPipeline {
        db: Arc::clone(&db),
        adapter: adapter.clone(),
        llm: Arc::clone(&llm),
        // DryRunProvider implements both LlmProvider and EmbeddingProvider.
        embedder: Some(Arc::clone(&dry) as Arc<dyn EmbeddingProvider>),
    };

    let result = pipeline.run(&corpus, opts).await.unwrap();
    assert!(result.total_chunks > 0, "expected chunks to be indexed");

    // Assert all chunks have embeddings after embed pass.
    let all_chunks = db.chunk_list(&corpus.id).unwrap();
    let embed_count = db.embedding_count(&corpus.id).unwrap();

    assert_eq!(
        embed_count,
        all_chunks.len() as u64,
        "all chunks should have embeddings: chunks={}, embeddings={embed_count}",
        all_chunks.len()
    );

    // Confirm page and section chunks both got embedded.
    let page_count = all_chunks.iter().filter(|c| c.kind == "page").count();
    let section_count = all_chunks.iter().filter(|c| c.kind == "section").count();
    assert!(page_count >= 3, "expected ≥3 page chunks");
    assert!(section_count > 0, "expected section chunks");
}

#[tokio::test]
async fn semantic_search_returns_results_after_embed() {
    let (db, corpus) = setup().await;
    let adapter = Arc::new(CorpusAwareWikiAdapter::new(&corpus.id, FIXTURE));
    let dry = Arc::new(DryRunProvider::new());
    let llm: Arc<dyn callimachus_llm::LlmProvider> = Arc::clone(&dry) as Arc<dyn callimachus_llm::LlmProvider>;

    let opts = IndexOptions {
        passes: vec![Pass::Chunk, Pass::Embed],
        ..Default::default()
    };

    let pipeline = IndexPipeline {
        db: Arc::clone(&db),
        adapter,
        llm: Arc::clone(&llm),
        embedder: Some(Arc::clone(&dry) as Arc<dyn EmbeddingProvider>),
    };

    pipeline.run(&corpus, opts).await.unwrap();

    // Semantic search with a unit vector — DryRunProvider always returns [1.0, 0, 0, ...].
    // All embeddings are identical, so all chunks match with similarity 1.0.
    let query_vector = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]; // DryRun dims = 8
    let results = callimachus_core::query::search::semantic_search(
        db.as_ref(),
        &corpus.id,
        &query_vector,
        None,
        10,
    )
    .unwrap();

    assert!(
        !results.is_empty(),
        "semantic search should return results after embed pass"
    );
}
