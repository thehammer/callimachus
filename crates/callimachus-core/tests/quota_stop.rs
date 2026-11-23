//! A `QuotaExhausted` error stops the purpose pass cleanly (no failure flood)
//! and the pipeline does not start later LLM passes.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};

use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
        ExtractedSemantic, ExtractedStructure, LocationRef, SourceAdapter,
    },
    indexing::pipeline::{IndexOptions, IndexPipeline},
    storage::{SqliteBackend, StorageBackend},
    types::{Chunk, Corpus, Entity, Location, Pass},
};
use callimachus_llm::{
    CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderUsage,
};

const CORPUS: &str = "quota-test";
const SUCCESSES: u32 = 3;
const ENTITIES: usize = 8;

/// Succeeds `SUCCESSES` times, then reports quota exhaustion forever.
struct QuotaAfterN {
    calls: AtomicU32,
}

#[async_trait::async_trait]
impl LlmProvider for QuotaAfterN {
    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n >= SUCCESSES {
            return Err(LlmError::QuotaExhausted {
                message: "usage limit reached".into(),
                resets_at: Some("3pm".into()),
            });
        }
        Ok(CompletionResponse {
            text: "ok".into(),
            input_tokens: 0,
            output_tokens: 0,
            model_used: "fake".into(),
        })
    }
    fn name(&self) -> &str {
        "fake"
    }
    fn supports_parallel(&self) -> bool {
        false
    }
    fn usage(&self) -> ProviderUsage {
        ProviderUsage::default()
    }
    fn reset_usage(&self) {}
}

/// Adapter whose purpose/contract extraction goes through the LLM.
#[derive(Default)]
struct Adapter {
    contract_calls: Mutex<u32>,
}

#[async_trait::async_trait]
impl SourceAdapter for Adapter {
    fn kind(&self) -> &str {
        "quota-test"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    async fn discover(&self, _s: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        Ok(vec![])
    }
    async fn chunk(&self, _s: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        Ok(vec![])
    }
    async fn extract_structure(&self, _c: &Chunk) -> anyhow::Result<ExtractedStructure> {
        unreachable!()
    }
    async fn extract_with_llm(
        &self,
        _c: &Chunk,
        _l: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        Ok(None)
    }
    async fn summarize(
        &self,
        _c: &Chunk,
        _l: &dyn LlmProvider,
        _d: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
    async fn resolve_aliases(
        &self,
        _e: &[Entity],
        _l: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>> {
        Ok(vec![])
    }
    fn format_location(&self, chunk: &Chunk) -> String {
        chunk.location.path.clone()
    }
    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
        Ok(LocationRef {
            corpus_id: CORPUS.into(),
            path: uri.into(),
        })
    }
    async fn extract_purpose(
        &self,
        _e: &Entity,
        _content: &str,
        _summary: Option<&str>,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedPurpose>> {
        llm.complete(CompletionRequest::default()).await?;
        Ok(Some(ExtractedPurpose {
            purpose: "p".into(),
            blocks: Vec::<ExtractedBlock>::new(),
        }))
    }
    async fn extract_contract(
        &self,
        _e: &Entity,
        _content: &str,
        _summary: Option<&str>,
        _purpose: Option<&str>,
        _signals: &serde_json::Value,
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedContract>> {
        *self.contract_calls.lock().unwrap() += 1;
        Ok(None)
    }
}

#[tokio::test]
async fn quota_exhaustion_stops_pass_and_skips_later_passes() {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus = Corpus::new(
        CORPUS.into(),
        "Quota".into(),
        "quota-test".into(),
        "/tmp/q".into(),
    );
    db.corpus_insert(&corpus).unwrap();
    for i in 0..ENTITIES {
        let loc = Location::new(CORPUS, format!("src/f{i}.rs"));
        db.chunk_upsert(&Chunk::new(
            CORPUS.into(),
            None,
            "module".into(),
            loc.clone(),
            format!("fn f{i}() {{}}"),
        ))
        .unwrap();
        let mut e = Entity::new(
            format!("{CORPUS}:f{i}"),
            CORPUS.into(),
            format!("f{i}"),
            "function".into(),
        );
        e.first_location = Some(loc);
        db.entity_upsert(&e).unwrap();
    }

    let adapter = Arc::new(Adapter::default());
    let pipeline = IndexPipeline {
        db: Arc::clone(&db) as _,
        adapter: Arc::clone(&adapter) as _,
        llm: Arc::new(QuotaAfterN {
            calls: AtomicU32::new(0),
        }),
        embedder: None,
    };
    let result = pipeline
        .run(
            &corpus,
            IndexOptions {
                passes: vec![Pass::Purpose, Pass::Contract],
                concurrency: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("a quota stop is not an error");

    assert_eq!(
        result.stopped_reason.as_deref(),
        Some("quota_exhausted: 3pm")
    );
    assert_eq!(
        *adapter.contract_calls.lock().unwrap(),
        0,
        "contract pass must not start after a stop"
    );
    let runs = db.run_latest(CORPUS, 10).unwrap();
    assert_eq!(runs.len(), 1, "only the purpose run is recorded");
    let run = &runs[0];
    assert_eq!(run.status, "stopped");
    assert_eq!(run.stats.processed, SUCCESSES as u64);
    assert!(run.stats.failed <= 1, "failed = {}", run.stats.failed);
    assert_eq!(
        run.stats.stopped_reason.as_deref(),
        Some("quota_exhausted: 3pm")
    );
}
