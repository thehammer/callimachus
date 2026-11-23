//! Run control: entity subsets, caps, estimate (no writes), single-child reuse.
//!
//! A fake adapter makes exactly one `llm.complete` call per summary, purpose
//! and contract, so provider call counts equal units of LLM work.

use std::sync::Arc;

use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
        ExtractedSemantic, ExtractedStructure, LocationRef, SourceAdapter,
    },
    indexing::{
        ChangedSource, EntityFilter, RunCaps,
        pipeline::{IndexOptions, IndexPipeline, IndexResult},
    },
    storage::{SqliteBackend, StorageBackend},
    types::{Chunk, Corpus, Entity, Location, Pass, SummaryTargetKind},
};
use callimachus_llm::{
    CompletionRequest, DryRunProvider, EstimatingProvider, LlmProvider,
    estimating::estimate_input_tokens,
};

const CORPUS: &str = "run-control";

struct Adapter;

#[async_trait::async_trait]
impl SourceAdapter for Adapter {
    fn kind(&self) -> &str {
        "run-control"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn current_version(&self, _p: &str) -> anyhow::Result<String> {
        Ok("v1".into())
    }
    fn changed_sources(
        &self,
        _p: &str,
        _f: Option<&str>,
        _t: &str,
    ) -> anyhow::Result<Vec<ChangedSource>> {
        Ok(vec![])
    }
    fn summary_levels(&self) -> Vec<&'static str> {
        vec!["function", "file"]
    }
    async fn discover(&self, s: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        Ok(vec![DiscoveredSource {
            path: s.into(),
            kind: "c".into(),
            meta: serde_json::Value::Null,
        }])
    }
    async fn chunk(&self, _s: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        Ok(vec![])
    }
    async fn extract_structure(&self, _c: &Chunk) -> anyhow::Result<ExtractedStructure> {
        Ok(ExtractedStructure {
            parent_path: None,
            child_paths: vec![],
            structural_entities: vec![],
            structural_edges: vec![],
        })
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
        c: &Chunk,
        llm: &dyn LlmProvider,
        _depth: &str,
    ) -> anyhow::Result<Option<String>> {
        let r = llm
            .complete(CompletionRequest {
                prompt: format!("summarize {}", c.content),
                max_tokens: Some(200),
                ..Default::default()
            })
            .await?;
        Ok(Some(format!("summary {}", r.text)))
    }
    async fn resolve_aliases(
        &self,
        _e: &[Entity],
        _l: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>> {
        Ok(vec![])
    }
    fn format_location(&self, c: &Chunk) -> String {
        c.location.path.clone()
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
        llm.complete(CompletionRequest {
            prompt: format!("purpose of {}", e.canonical_name),
            max_tokens: Some(100),
            ..Default::default()
        })
        .await?;
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
        llm.complete(CompletionRequest {
            prompt: format!("contract of {}", e.canonical_name),
            max_tokens: Some(100),
            ..Default::default()
        })
        .await?;
        Ok(Some(ExtractedContract::default()))
    }
}

fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("fun_{i:04}")).collect()
}

/// One file per function: a `file` chunk with one `function` child, plus the
/// entity pointing at the function chunk. Entities `fun_0000..`; the first
/// `n_a` are renamed `fun_a<i>` so a glob can select them.
fn seed(n: usize, n_a: usize) -> Arc<SqliteBackend> {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    db.corpus_insert(&Corpus::new(
        CORPUS.into(),
        "rc".into(),
        "run-control".into(),
        "/tmp/rc".into(),
    ))
    .unwrap();
    for (i, name) in names(n).into_iter().enumerate() {
        let name = if i < n_a { format!("fun_a{i}") } else { name };
        let file_path = format!("src/{name}.c");
        db.chunk_upsert(&Chunk::new(
            CORPUS.into(),
            None,
            "file".into(),
            Location::new(CORPUS, file_path.clone()),
            format!("/* {name}.c */\nvoid {name}(void) {{}}"),
        ))
        .unwrap();
        let func = Chunk::new(
            CORPUS.into(),
            Some(file_path.clone()),
            "function".into(),
            Location::new(CORPUS, format!("{file_path}#{name}")),
            format!("void {name}(void) {{}}"),
        );
        db.chunk_upsert(&func).unwrap();
        let mut e = Entity::new(
            format!("{CORPUS}:{name}"),
            CORPUS.into(),
            name,
            "function".into(),
        );
        e.first_location = Some(func.location);
        db.entity_upsert(&e).unwrap();
    }
    db
}

async fn run(
    db: &Arc<SqliteBackend>,
    llm: Arc<dyn LlmProvider>,
    passes: Vec<Pass>,
    tweak: impl FnOnce(&mut IndexOptions),
) -> IndexResult {
    let corpus = db.corpus_get(CORPUS).unwrap().unwrap();
    let mut opts = IndexOptions {
        passes,
        concurrency: Some(4),
        ..Default::default()
    };
    tweak(&mut opts);
    IndexPipeline {
        db: Arc::clone(db) as _,
        adapter: Arc::new(Adapter),
        llm,
        embedder: None,
    }
    .run(&corpus, opts)
    .await
    .unwrap()
}

fn filter(specs: &[&str]) -> EntityFilter {
    EntityFilter::parse(specs).unwrap()
}

#[tokio::test]
async fn purpose_filter_processes_only_matching_entities() {
    let db = seed(10, 3);
    let dry = Arc::new(DryRunProvider::new());
    let res = run(&db, dry.clone(), vec![Pass::Purpose], |o| {
        o.entity_filter = filter(&["name:FUN_A*"]);
    })
    .await;
    assert!(res.stopped_reason.is_none());
    assert_eq!(dry.call_count(), 3);
    assert_eq!(db.purpose_list(CORPUS).unwrap().len(), 3);
    assert!(
        db.purpose_list(CORPUS)
            .unwrap()
            .iter()
            .all(|p| p.entity_id.contains("fun_a"))
    );
}

#[tokio::test]
async fn summarize_filter_selects_chunks_by_entity_and_skips_corpus_summary() {
    let db = seed(6, 2);
    let dry = Arc::new(DryRunProvider::new());
    run(&db, dry.clone(), vec![Pass::Summarize], |o| {
        o.entity_filter = filter(&["name:fun_a*"]);
    })
    .await;
    // Two function chunks summarized; their files reuse the single child
    // summary; no corpus-level summary under a filter.
    assert_eq!(dry.call_count(), 2);
    assert_eq!(db.summary_list(CORPUS).unwrap().len(), 4);
    assert!(
        db.summary_get(CORPUS, &SummaryTargetKind::Corpus, CORPUS)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn single_function_files_reuse_the_function_summary() {
    let db = seed(5, 0);
    let dry = Arc::new(DryRunProvider::new());
    run(&db, dry.clone(), vec![Pass::Summarize], |_| {}).await;
    // 5 function summaries + 1 corpus summary; the 5 file summaries cost nothing.
    assert_eq!(dry.call_count(), 6);
    let sums = db.summary_list(CORPUS).unwrap();
    let files: Vec<_> = sums.iter().filter(|s| s.depth == "file").collect();
    assert_eq!(files.len(), 5);
    for f in files {
        let file_chunk = db
            .chunk_list(CORPUS)
            .unwrap()
            .into_iter()
            .find(|c| c.id == f.target_id)
            .unwrap();
        let func = db
            .chunk_list(CORPUS)
            .unwrap()
            .into_iter()
            .find(|c| c.parent_path.as_deref() == Some(&file_chunk.location.path))
            .unwrap();
        let func_summary = sums.iter().find(|s| s.target_id == func.id).unwrap();
        assert_eq!(f.text, func_summary.text);
    }
}

#[tokio::test]
async fn max_calls_stops_the_run_and_a_rerun_resumes() {
    let db = seed(10, 0);
    let dry = Arc::new(DryRunProvider::new());
    let first = run(&db, dry.clone(), vec![Pass::Purpose], |o| {
        o.caps = RunCaps {
            max_calls: Some(3),
            ..Default::default()
        };
    })
    .await;
    assert_eq!(db.purpose_list(CORPUS).unwrap().len(), 3);
    assert_eq!(dry.call_count(), 3);
    let reason = first.stopped_reason.expect("run stops at the cap");
    assert!(reason.contains("max_calls"), "{reason}");
    assert_eq!(first.runs[0].status, "stopped");
    assert!(
        first.runs[0]
            .stats
            .stopped_reason
            .as_deref()
            .unwrap()
            .contains("max_calls")
    );

    let dry2 = Arc::new(DryRunProvider::new());
    let second = run(&db, dry2.clone(), vec![Pass::Purpose], |_| {}).await;
    assert!(second.stopped_reason.is_none());
    assert_eq!(dry2.call_count(), 7, "only the remaining entities");
    assert_eq!(db.purpose_list(CORPUS).unwrap().len(), 10);
}

#[tokio::test]
async fn caps_span_passes() {
    let db = seed(4, 0);
    let dry = Arc::new(DryRunProvider::new());
    let res = run(&db, dry.clone(), vec![Pass::Purpose, Pass::Contract], |o| {
        o.caps = RunCaps {
            max_calls: Some(6),
            ..Default::default()
        };
    })
    .await;
    assert_eq!(dry.call_count(), 6);
    assert_eq!(db.purpose_list(CORPUS).unwrap().len(), 4);
    assert_eq!(db.contract_list(CORPUS).unwrap().len(), 2);
    assert!(res.stopped_reason.unwrap().contains("max_calls"));
}

#[tokio::test]
async fn estimate_counts_candidates_calls_and_writes_nothing() {
    let db = seed(6, 2);
    let dry = Arc::new(DryRunProvider::new());
    let est: Arc<dyn LlmProvider> = Arc::new(EstimatingProvider::new(dry.clone(), 700));
    let res = run(
        &db,
        est,
        vec![Pass::Summarize, Pass::Purpose, Pass::Contract],
        |o| {
            o.estimate = true;
            o.entity_filter = filter(&["name:fun_a*"]);
        },
    )
    .await;

    assert_eq!(dry.call_count(), 0, "nothing is sent");
    let by_pass = |p: &str| res.estimates.iter().find(|e| e.pass == p).unwrap().clone();
    assert_eq!(by_pass("summarize").calls, 2, "file summaries are reused");
    assert_eq!(by_pass("purpose").entities, 2);
    assert_eq!(by_pass("purpose").calls, 2);
    assert_eq!(by_pass("contract").calls, 2);
    // purpose prompt "purpose of fun_a0" (17 chars), max_tokens 100.
    let p = by_pass("purpose");
    assert_eq!(p.input_tokens, 2 * estimate_input_tokens(17, 700));
    assert_eq!(p.output_tokens, 2 * 50);

    // No artifacts, no run log, no anchor.
    assert!(db.summary_list(CORPUS).unwrap().is_empty());
    assert!(db.purpose_list(CORPUS).unwrap().is_empty());
    assert!(db.contract_list(CORPUS).unwrap().is_empty());
    assert!(db.run_latest(CORPUS, 10).unwrap().is_empty());

    // And no cache entries: a real run afterwards still makes every call.
    let real = Arc::new(DryRunProvider::new());
    run(&db, real.clone(), vec![Pass::Purpose], |o| {
        o.entity_filter = filter(&["name:fun_a*"]);
    })
    .await;
    assert_eq!(real.call_count(), 2);
}

#[tokio::test]
async fn estimate_skips_entities_that_already_have_artifacts() {
    let db = seed(4, 0);
    run(
        &db,
        Arc::new(DryRunProvider::new()),
        vec![Pass::Purpose],
        |o| {
            o.entity_filter = filter(&["id:fun_0000"]);
        },
    )
    .await;
    let est: Arc<dyn LlmProvider> =
        Arc::new(EstimatingProvider::new(Arc::new(DryRunProvider::new()), 0));
    let res = run(&db, est, vec![Pass::Purpose], |o| o.estimate = true).await;
    assert_eq!(res.estimates[0].calls, 3);
}
