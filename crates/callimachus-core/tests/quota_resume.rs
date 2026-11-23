//! Quota-stop behaviour beyond the basic purpose stop: resuming a stopped run
//! with History enabled, Contract / Semantic / Aliases stops, and in-flight
//! entities at concurrency > 1.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};
use std::time::Duration;

use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
        ExtractedSemantic, ExtractedStructure, LocationRef, SourceAdapter,
    },
    indexing::{
        ChangedSource,
        pipeline::{IndexOptions, IndexPipeline, IndexResult},
    },
    storage::{SqliteBackend, StorageBackend},
    types::{Chunk, Corpus, Entity, Location, Pass},
};
use callimachus_llm::{
    CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderUsage,
};

const CORPUS: &str = "quota-resume";
const FILES: usize = 8;

/// Succeeds `successes` times, then reports quota exhaustion forever.
struct QuotaAfterN {
    successes: u32,
    parallel: bool,
    calls: AtomicU32,
}

impl QuotaAfterN {
    fn new(successes: u32) -> Arc<Self> {
        Arc::new(Self {
            successes,
            parallel: false,
            calls: AtomicU32::new(0),
        })
    }
    fn parallel(successes: u32) -> Arc<Self> {
        Arc::new(Self {
            successes,
            parallel: true,
            calls: AtomicU32::new(0),
        })
    }
}

#[async_trait::async_trait]
impl LlmProvider for QuotaAfterN {
    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        // Small delay so concurrent calls overlap.
        tokio::time::sleep(Duration::from_millis(15)).await;
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n >= self.successes {
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
        self.parallel
    }
    fn usage(&self) -> ProviderUsage {
        ProviderUsage::default()
    }
    fn reset_usage(&self) {}
}

#[derive(Default)]
struct Adapter {
    /// Entity ids whose purpose/contract LLM call succeeded, in order.
    purposes: Mutex<Vec<String>>,
    contracts: Mutex<Vec<String>>,
}

fn file_chunk(i: usize) -> Chunk {
    Chunk::new(
        CORPUS.into(),
        None,
        "module".into(),
        Location::new(CORPUS, format!("src/f{i}.rs")),
        format!("fn f{i}() {{}}"),
    )
}

#[async_trait::async_trait]
impl SourceAdapter for Adapter {
    fn kind(&self) -> &str {
        "quota-resume"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn current_version(&self, _source_path: &str) -> anyhow::Result<String> {
        Ok("v1".into())
    }
    fn changed_sources(
        &self,
        _source_path: &str,
        _from: Option<&str>,
        _to: &str,
    ) -> anyhow::Result<Vec<ChangedSource>> {
        Ok(vec![])
    }
    async fn discover(&self, s: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        Ok(vec![DiscoveredSource {
            path: s.to_string(),
            kind: "rust".into(),
            meta: serde_json::Value::Null,
        }])
    }
    async fn chunk(&self, _s: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        Ok((0..FILES).map(file_chunk).collect())
    }
    async fn extract_structure(&self, c: &Chunk) -> anyhow::Result<ExtractedStructure> {
        let name = c
            .location
            .path
            .trim_start_matches("src/")
            .replace(".rs", "");
        let mut e = Entity::new(
            format!("{CORPUS}:{name}"),
            CORPUS.into(),
            name,
            "function".into(),
        );
        e.first_location = Some(c.location.clone());
        Ok(ExtractedStructure {
            parent_path: None,
            child_paths: vec![],
            structural_entities: vec![e],
            structural_edges: vec![],
        })
    }
    async fn extract_with_llm(
        &self,
        _c: &Chunk,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        llm.complete(CompletionRequest::default()).await?;
        Ok(Some(ExtractedSemantic {
            entities: vec![],
            edges: vec![],
            summary_text: None,
        }))
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
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>> {
        llm.complete(CompletionRequest::default()).await?;
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
        e: &Entity,
        _content: &str,
        _summary: Option<&str>,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedPurpose>> {
        llm.complete(CompletionRequest::default()).await?;
        self.purposes.lock().unwrap().push(e.id.clone());
        Ok(Some(ExtractedPurpose {
            purpose: "p".into(),
            blocks: Vec::<ExtractedBlock>::new(),
        }))
    }
    async fn extract_contract(
        &self,
        e: &Entity,
        _content: &str,
        _summary: Option<&str>,
        _purpose: Option<&str>,
        _signals: &serde_json::Value,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedContract>> {
        llm.complete(CompletionRequest::default()).await?;
        self.contracts.lock().unwrap().push(e.id.clone());
        Ok(Some(ExtractedContract::default()))
    }
}

fn new_db() -> (Arc<SqliteBackend>, Corpus) {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus = Corpus::new(
        CORPUS.into(),
        "Quota".into(),
        "quota-resume".into(),
        "/tmp/q".into(),
    );
    db.corpus_insert(&corpus).unwrap();
    (db, corpus)
}

/// Seed chunks and entities directly (no History / Chunk / Structure passes).
fn seed(db: &SqliteBackend) {
    for i in 0..FILES {
        let c = file_chunk(i);
        db.chunk_upsert(&c).unwrap();
        let mut e = Entity::new(
            format!("{CORPUS}:f{i}"),
            CORPUS.into(),
            format!("f{i}"),
            "function".into(),
        );
        e.first_location = Some(c.location);
        db.entity_upsert(&e).unwrap();
    }
}

async fn run(
    db: &Arc<SqliteBackend>,
    adapter: &Arc<Adapter>,
    llm: Arc<QuotaAfterN>,
    passes: Vec<Pass>,
    concurrency: usize,
) -> IndexResult {
    // Re-read the corpus so `last_indexed_version` reflects earlier runs.
    let corpus = db.corpus_get(CORPUS).unwrap().unwrap();
    IndexPipeline {
        db: Arc::clone(db) as _,
        adapter: Arc::clone(adapter) as _,
        llm,
        embedder: None,
    }
    .run(
        &corpus,
        IndexOptions {
            passes,
            concurrency: Some(concurrency),
            ..Default::default()
        },
    )
    .await
    .expect("a quota stop is not an error")
}

#[tokio::test]
async fn resume_with_history_completes_every_entity_exactly_once() {
    let (db, _) = new_db();
    let adapter = Arc::new(Adapter::default());
    let passes = vec![Pass::History, Pass::Chunk, Pass::Structure, Pass::Purpose];

    // First run: quota hits after 3 purposes.
    let first = run(&db, &adapter, QuotaAfterN::new(3), passes.clone(), 1).await;
    assert!(first.stopped_reason.is_some());
    assert_eq!(db.purpose_list(CORPUS).unwrap().len(), 3);
    assert_eq!(
        db.corpus_get_last_indexed_version(CORPUS).unwrap(),
        None,
        "a stopped run must not advance the version anchor"
    );

    // Same command, no --full, quota restored.
    let second = run(&db, &adapter, QuotaAfterN::new(1000), passes, 1).await;
    assert!(second.stopped_reason.is_none());

    assert_eq!(db.purpose_list(CORPUS).unwrap().len(), FILES);
    let mut ids = adapter.purposes.lock().unwrap().clone();
    assert_eq!(ids.len(), FILES, "every entity processed exactly once");
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), FILES, "no entity processed twice");
    assert_eq!(
        db.corpus_get_last_indexed_version(CORPUS)
            .unwrap()
            .as_deref(),
        Some("v1"),
        "a completed run advances the anchor"
    );
}

#[tokio::test]
async fn unchanged_files_without_output_are_picked_up() {
    // Anchor already current (nothing dirty) but some entities have no purpose.
    let (db, _) = new_db();
    seed(&db);
    db.corpus_set_last_indexed_version(CORPUS, "v1").unwrap();
    let adapter = Arc::new(Adapter::default());
    let res = run(
        &db,
        &adapter,
        QuotaAfterN::new(1000),
        vec![Pass::History, Pass::Purpose, Pass::Contract],
        1,
    )
    .await;
    assert!(res.stopped_reason.is_none());
    assert_eq!(db.purpose_list(CORPUS).unwrap().len(), FILES);
    assert_eq!(db.contract_list(CORPUS).unwrap().len(), FILES);
}

#[tokio::test]
async fn contract_pass_stops_cleanly_on_quota() {
    let (db, _) = new_db();
    seed(&db);
    let adapter = Arc::new(Adapter::default());
    let res = run(&db, &adapter, QuotaAfterN::new(2), vec![Pass::Contract], 1).await;
    assert!(res.stopped_reason.is_some());
    let run = &db.run_latest(CORPUS, 5).unwrap()[0];
    assert_eq!(run.status, "stopped");
    assert_eq!(run.stats.processed, 2);
    assert!(run.stats.failed <= 1, "failed = {}", run.stats.failed);
    assert_eq!(db.contract_list(CORPUS).unwrap().len(), 2);
}

#[tokio::test]
async fn concurrent_in_flight_quota_errors_are_not_failures() {
    for pass in [Pass::Purpose, Pass::Contract] {
        let (db, _) = new_db();
        seed(&db);
        let adapter = Arc::new(Adapter::default());
        let res = run(&db, &adapter, QuotaAfterN::new(3), vec![pass.clone()], 4).await;
        assert!(res.stopped_reason.is_some(), "{pass}");
        let run = &db.run_latest(CORPUS, 5).unwrap()[0];
        assert_eq!(run.status, "stopped", "{pass}");
        assert_eq!(run.stats.failed, 0, "{pass}: quota errors are not failures");
        assert_eq!(run.stats.processed, 3, "{pass}: completed work is kept");
        let stored = match pass {
            Pass::Purpose => db.purpose_list(CORPUS).unwrap().len(),
            _ => db.contract_list(CORPUS).unwrap().len(),
        };
        assert_eq!(stored, 3, "{pass}");
    }
}

#[tokio::test]
async fn semantic_pass_stops_cleanly_on_quota() {
    for llm in [QuotaAfterN::new(2), QuotaAfterN::parallel(2)] {
        let parallel = llm.parallel;
        let (db, _) = new_db();
        seed(&db);
        let adapter = Arc::new(Adapter::default());
        let res = run(&db, &adapter, llm, vec![Pass::Semantic], 2).await;
        assert!(res.stopped_reason.is_some(), "parallel={parallel}");
        let run = &db.run_latest(CORPUS, 5).unwrap()[0];
        assert_eq!(run.status, "stopped", "parallel={parallel}");
        assert_eq!(run.stats.failed, 0, "parallel={parallel}");
        assert_eq!(run.stats.processed, 2, "parallel={parallel}");
        assert_eq!(
            db.chunk_list_unprocessed(CORPUS).unwrap().len(),
            FILES - 2,
            "remaining chunks stay unprocessed (parallel={parallel})"
        );
    }
}

#[tokio::test]
async fn aliases_pass_stops_cleanly_on_quota() {
    let (db, _) = new_db();
    seed(&db);
    let adapter = Arc::new(Adapter::default());
    let res = run(&db, &adapter, QuotaAfterN::new(0), vec![Pass::Aliases], 1).await;
    assert_eq!(res.stopped_reason.as_deref(), Some("quota_exhausted: 3pm"));
    let run = &db.run_latest(CORPUS, 5).unwrap()[0];
    assert_eq!(run.status, "stopped");
    assert_eq!(run.stats.failed, 0);
}
