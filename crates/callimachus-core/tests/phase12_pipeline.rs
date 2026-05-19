//! Integration tests for Phase 12 — Semantic Enrichment.
//!
//! Verifies:
//! - PurposePass stores EntityPurpose and EntityBlock rows for function entities.
//! - ContractPass stores EntityContract rows.
//! - `find_unreachable` returns function entities that have no inbound `calls` edges.
//! - `entities_without_tests` returns function entities that have no inbound `verified_by` edges.
//! - ThemePass skips when entity count < 20 (skipped = 1).
//! - `explain_component` assembles a non-empty narrative from pre-indexed purposes and blocks.

use std::sync::Arc;

use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
        ExtractedSemantic, ExtractedStructure, LocationRef, SourceAdapter,
    },
    indexing::pipeline::{IndexOptions, IndexPipeline},
    query::{
        service::QueryService,
        types::{EntitiesWithoutTestsInput, ExplainComponentInput, FindUnreachableInput},
    },
    storage::{SqliteBackend, StorageBackend},
    types::{Chunk, Corpus, Entity, Location, Pass},
};
use callimachus_llm::DryRunProvider;

// ── Fake adapter ─────────────────────────────────────────────────────────────

/// Produces two chunks — one "production" function and one "test" function.
struct Phase12Adapter;

const CORPUS_ID: &str = "phase12-test";

#[async_trait::async_trait]
impl SourceAdapter for Phase12Adapter {
    fn kind(&self) -> &str {
        "phase12-fake"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }

    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        Ok(vec![DiscoveredSource {
            path: source.to_string(),
            kind: "rust".to_string(),
            meta: serde_json::Value::Null,
        }])
    }

    async fn chunk(&self, _source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        Ok(vec![
            Chunk::new(
                CORPUS_ID.to_string(),
                None,
                "module".to_string(),
                Location::new(CORPUS_ID, "src/compute.rs"),
                "pub fn compute(x: i32) -> Result<i32, String> { Ok(x * 2) }".to_string(),
            ),
            Chunk::new(
                CORPUS_ID.to_string(),
                None,
                "module".to_string(),
                Location::new(CORPUS_ID, "src/tests.rs"),
                "#[test] fn test_compute() { assert!(compute(1).is_ok()); }".to_string(),
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
        chunk: &Chunk,
        _llm: &dyn callimachus_llm::LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        // Produce one function entity per chunk, with first_location pointing to the chunk.
        let (entity_id, name, kind) = if chunk.location.path.contains("tests") {
            (
                format!("{}:test_compute", CORPUS_ID),
                "test_compute".to_string(),
                "function".to_string(),
            )
        } else {
            (
                format!("{}:compute", CORPUS_ID),
                "compute".to_string(),
                "function".to_string(),
            )
        };

        let mut entity = Entity::new(entity_id, CORPUS_ID.to_string(), name, kind);
        entity.first_location = Some(chunk.location.clone());

        Ok(Some(ExtractedSemantic {
            entities: vec![entity],
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
        Ok(None)
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

    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
        Ok(LocationRef {
            corpus_id: CORPUS_ID.to_string(),
            path: uri.to_string(),
        })
    }

    // ── Phase 12 overrides ────────────────────────────────────────────────────

    async fn extract_purpose(
        &self,
        entity: &Entity,
        _content: &str,
        _summary: Option<&str>,
        _llm: &dyn callimachus_llm::LlmProvider,
    ) -> anyhow::Result<Option<ExtractedPurpose>> {
        if entity.canonical_name == "compute" {
            Ok(Some(ExtractedPurpose {
                purpose: "Doubles an integer and returns a Result.".to_string(),
                blocks: vec![ExtractedBlock {
                    label: "Computation".to_string(),
                    description: "Multiplies input by 2.".to_string(),
                }],
            }))
        } else {
            // test functions: no purpose extraction
            Ok(None)
        }
    }

    async fn extract_contract(
        &self,
        entity: &Entity,
        _content: &str,
        _summary: Option<&str>,
        _purpose: Option<&str>,
        _signals: &serde_json::Value,
        _llm: &dyn callimachus_llm::LlmProvider,
    ) -> anyhow::Result<Option<ExtractedContract>> {
        if entity.canonical_name == "compute" {
            Ok(Some(ExtractedContract {
                assumptions: vec!["input is a valid i32".to_string()],
                risks: vec!["overflow for very large inputs".to_string()],
                intent_gap: None,
                caller_notes: Some("Always check the Result".to_string()),
                verified_by_names: vec![],
                discards_result_callees: vec![],
                ..Default::default()
            }))
        } else {
            Ok(None)
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn setup() -> (Arc<dyn StorageBackend>, Corpus) {
    let db = SqliteBackend::open_in_memory().unwrap();
    let corpus = Corpus::new(
        CORPUS_ID.to_string(),
        "Phase 12 Test Corpus".to_string(),
        "phase12-fake".to_string(),
        "/tmp/phase12".to_string(),
    );
    db.corpus_insert(&corpus).unwrap();
    (Arc::new(db), corpus)
}

async fn run_pipeline(db: Arc<dyn StorageBackend>, corpus: &Corpus, passes: Vec<Pass>) {
    let pipeline = IndexPipeline {
        db: db.clone(),
        adapter: Arc::new(Phase12Adapter),
        llm: Arc::new(DryRunProvider::new()),
        embedder: None,
    };
    pipeline
        .run(
            corpus,
            IndexOptions {
                passes,
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn purpose_pass_stores_purpose_and_blocks() {
    let (db, corpus) = setup();
    run_pipeline(
        Arc::clone(&db),
        &corpus,
        vec![
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Purpose,
        ],
    )
    .await;

    // compute entity should have a stored purpose.
    let compute_id = format!("{}:compute", CORPUS_ID);
    let purpose = db.purpose_get(CORPUS_ID, &compute_id).unwrap();
    assert!(purpose.is_some(), "expected purpose for compute entity");
    let p = purpose.unwrap();
    assert!(
        p.purpose.contains("Doubles"),
        "unexpected purpose: {}",
        p.purpose
    );

    // compute entity should have at least one block.
    let blocks = db.block_list_for_entity(&compute_id).unwrap();
    assert!(
        !blocks.is_empty(),
        "expected at least one block for compute"
    );
    assert_eq!(blocks[0].label, "Computation");

    // test_compute should NOT have a purpose (adapter returns None for it).
    let test_id = format!("{}:test_compute", CORPUS_ID);
    let test_purpose = db.purpose_get(CORPUS_ID, &test_id).unwrap();
    assert!(
        test_purpose.is_none(),
        "test_compute should not have a purpose"
    );
}

#[tokio::test]
async fn contract_pass_stores_contract() {
    let (db, corpus) = setup();
    run_pipeline(
        Arc::clone(&db),
        &corpus,
        vec![
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Contract,
        ],
    )
    .await;

    let compute_id = format!("{}:compute", CORPUS_ID);
    let contract = db.contract_get(CORPUS_ID, &compute_id).unwrap();
    assert!(contract.is_some(), "expected contract for compute entity");
    let c = contract.unwrap();
    assert!(
        !c.assumptions.is_empty(),
        "expected at least one assumption"
    );
    assert!(!c.risks.is_empty(), "expected at least one risk");
    assert!(c.caller_notes.is_some(), "expected caller_notes");
}

#[tokio::test]
async fn find_unreachable_returns_entities_without_calls_edges() {
    let (db, corpus) = setup();
    run_pipeline(
        Arc::clone(&db),
        &corpus,
        vec![Pass::Chunk, Pass::Structure, Pass::Semantic, Pass::Aliases],
    )
    .await;

    let qs = QueryService::new(Arc::clone(&db));
    let result = qs.find_unreachable(FindUnreachableInput {
        corpus_id: CORPUS_ID.to_string(),
    });

    match result {
        callimachus_core::types::ToolResult::Ok(s) => {
            let data = s.data;
            // Both compute and test_compute have no inbound calls edges.
            assert!(data.count >= 2, "expected at least 2 unreachable entities");
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[tokio::test]
async fn entities_without_tests_returns_all_when_no_verified_by_edges() {
    let (db, corpus) = setup();
    run_pipeline(
        Arc::clone(&db),
        &corpus,
        vec![
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Contract,
        ],
    )
    .await;

    let qs = QueryService::new(Arc::clone(&db));
    let result = qs.entities_without_tests(EntitiesWithoutTestsInput {
        corpus_id: CORPUS_ID.to_string(),
    });

    match result {
        callimachus_core::types::ToolResult::Ok(s) => {
            let data = s.data;
            // No verified_by edges were created (adapter returns empty verified_by_names).
            assert!(data.count >= 1, "expected at least 1 entity without tests");
        }
        other => panic!("unexpected result: {:?}", other),
    }
}

#[tokio::test]
async fn theme_pass_skips_when_entity_count_below_threshold() {
    let (db, corpus) = setup();
    let pipeline = IndexPipeline {
        db: db.clone(),
        adapter: Arc::new(Phase12Adapter),
        llm: Arc::new(DryRunProvider::new()),
        embedder: None,
    };

    // Run chunk + semantic to populate a small entity set (<20 entities).
    pipeline
        .run(
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure, Pass::Semantic, Pass::Theme],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // The theme pass run-log entry should show skipped ≥ 1 and processed = 0.
    let runs = db.run_latest(&corpus.id, 10).unwrap();
    let theme_run = runs.iter().find(|r| r.pass == "theme");
    assert!(theme_run.is_some(), "expected a theme run-log entry");
    let tr = theme_run.unwrap();
    assert_eq!(tr.status, "completed");
    // skipped = 1, processed = 0 because entity_count < 20.
    assert_eq!(
        tr.stats.processed, 0,
        "expected 0 processed themes (< 20 entities)"
    );
    assert!(
        tr.stats.skipped >= 1,
        "expected at least 1 skipped for theme pass"
    );
}

#[tokio::test]
async fn explain_component_returns_narrative_with_purpose() {
    let (db, corpus) = setup();
    run_pipeline(
        Arc::clone(&db),
        &corpus,
        vec![
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Purpose,
        ],
    )
    .await;

    let qs = QueryService::new(Arc::clone(&db));
    let compute_id = format!("{}:compute", CORPUS_ID);

    let result = qs.explain_component(ExplainComponentInput {
        corpus_id: CORPUS_ID.to_string(),
        entity_id: Some(compute_id),
        module_prefix: None,
        max_depth: Some(2),
    });

    match result {
        callimachus_core::types::ToolResult::Ok(s) => {
            let data = s.data;
            assert!(!data.narrative.is_empty(), "expected non-empty narrative");
            assert!(
                data.narrative.contains("compute"),
                "narrative should mention the entity"
            );
            // Purpose text should appear.
            assert!(
                data.narrative.contains("Doubles"),
                "narrative should contain the stored purpose"
            );
            // Block should appear.
            assert!(
                data.narrative.contains("Computation"),
                "narrative should include block label"
            );
            // Zero LLM calls: the result is purely from pre-indexed data.
            assert!(
                !data.nodes.is_empty(),
                "expected at least one node in explain output"
            );
        }
        other => panic!("unexpected result: {:?}", other),
    }
}
