//! Integration tests for multi-model artifact storage.
//!
//! Verifies that:
//! - Multiple models can each produce artifacts for the same entity without
//!   overwriting one another (composite PK on entity_id + model).
//! - `*_get` (no model arg) returns the highest-tier artifact.
//! - `*_get_for_model` returns the row for an exact model name.
//! - Same-model upsert replaces the existing row (idempotent write).
//! - The `unknown` tier ranks lower than any named tier.
//! - The pipeline idempotency check is now per-model: re-running with the
//!   same LLM skips; re-running with a different LLM adds new rows.

use std::sync::{Arc, Mutex};

use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
        ExtractedSemantic, ExtractedStructure, LocationRef, SourceAdapter,
    },
    indexing::pipeline::{IndexOptions, IndexPipeline},
    storage::{SqliteBackend, StorageBackend},
    types::{
        Chunk, Corpus, Entity, EntityContract, EntityPurpose, Location, Pass, Summary,
        SummaryTargetKind,
    },
};
use callimachus_llm::{CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_backend() -> Arc<dyn StorageBackend> {
    Arc::new(SqliteBackend::open_in_memory().unwrap())
}

/// Insert a minimal entity row so FK constraints are satisfied.
fn seed_entity(db: &dyn StorageBackend, corpus_id: &str, entity_id: &str) {
    let corpus = Corpus::new(
        corpus_id.to_string(),
        "test corpus".to_string(),
        "code".to_string(),
        "/tmp".to_string(),
    );
    // Ignore if corpus already exists.
    let _ = db.corpus_insert(&corpus);

    let entity = Entity::new(
        entity_id.to_string(),
        corpus_id.to_string(),
        entity_id.to_string(),
        "function".to_string(),
    );
    db.entity_upsert(&entity).unwrap();
}

fn make_purpose(entity_id: &str, corpus_id: &str, model: &str) -> EntityPurpose {
    EntityPurpose {
        entity_id: entity_id.to_string(),
        corpus_id: corpus_id.to_string(),
        purpose: format!("purpose by {model}"),
        model: model.to_string(),
        model_tier: callimachus_llm::model_tier(model).to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        provenance: None,
    }
}

fn make_contract(entity_id: &str, corpus_id: &str, model: &str) -> EntityContract {
    EntityContract {
        entity_id: entity_id.to_string(),
        corpus_id: corpus_id.to_string(),
        assumptions: vec![format!("assumption by {model}")],
        model: model.to_string(),
        model_tier: callimachus_llm::model_tier(model).to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        ..EntityContract::default()
    }
}

fn make_summary(corpus_id: &str, target_id: &str, model: &str) -> Summary {
    Summary {
        id: uuid::Uuid::new_v4().to_string(),
        corpus_id: corpus_id.to_string(),
        target_kind: SummaryTargetKind::Entity,
        target_id: target_id.to_string(),
        depth: "entity".to_string(),
        text: format!("summary by {model}"),
        model: model.to_string(),
        model_tier: callimachus_llm::model_tier(model).to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        provenance: None,
    }
}

// ── Test 1: Round-trip — two contracts, two models ────────────────────────────

#[test]
fn two_contracts_two_models_both_stored() {
    let db = make_backend();
    let corpus_id = "c1";
    let entity_id = "e1";
    seed_entity(db.as_ref(), corpus_id, entity_id);

    let haiku = "claude-haiku-4-5-20251001";
    let sonnet = "claude-sonnet-4-5-20250929";

    db.contract_upsert(&make_contract(entity_id, corpus_id, haiku))
        .unwrap();
    db.contract_upsert(&make_contract(entity_id, corpus_id, sonnet))
        .unwrap();

    // Both rows should exist.
    let all = db.contract_list(corpus_id).unwrap();
    let for_entity: Vec<_> = all.iter().filter(|c| c.entity_id == entity_id).collect();
    assert_eq!(
        for_entity.len(),
        2,
        "expected 2 contract rows, one per model"
    );

    // Best-tier read should return the sonnet row (sonnet > haiku).
    let best = db.contract_get(corpus_id, entity_id).unwrap().unwrap();
    assert_eq!(best.model, sonnet, "expected sonnet to be best-tier");
    assert_eq!(best.model_tier, "sonnet");
}

// ── Test 2: Best-tier read — opus wins over sonnet and haiku ─────────────────

#[test]
fn best_tier_purpose_returns_opus() {
    let db = make_backend();
    let corpus_id = "c2";
    let entity_id = "e2";
    seed_entity(db.as_ref(), corpus_id, entity_id);

    db.purpose_upsert(&make_purpose(
        entity_id,
        corpus_id,
        "claude-haiku-4-5-20251001",
    ))
    .unwrap();
    db.purpose_upsert(&make_purpose(
        entity_id,
        corpus_id,
        "claude-sonnet-4-5-20250929",
    ))
    .unwrap();
    db.purpose_upsert(&make_purpose(
        entity_id,
        corpus_id,
        "claude-opus-4-20250514",
    ))
    .unwrap();

    let best = db.purpose_get(corpus_id, entity_id).unwrap().unwrap();
    assert_eq!(best.model_tier, "opus", "expected opus to win");
    assert!(best.model.contains("opus"));
}

// ── Test 3: Same-model upsert replaces, does not duplicate ───────────────────

#[test]
fn same_model_upsert_does_not_duplicate() {
    let db = make_backend();
    let corpus_id = "c3";
    let entity_id = "e3";
    seed_entity(db.as_ref(), corpus_id, entity_id);

    let model = "claude-haiku-4-5-20251001";

    // Upsert twice with the same model.
    let mut p = make_purpose(entity_id, corpus_id, model);
    p.purpose = "first version".to_string();
    db.purpose_upsert(&p).unwrap();

    let mut p2 = make_purpose(entity_id, corpus_id, model);
    p2.purpose = "second version".to_string();
    db.purpose_upsert(&p2).unwrap();

    // Only one row should exist.
    let all = db.purpose_list(corpus_id).unwrap();
    let for_entity: Vec<_> = all.iter().filter(|p| p.entity_id == entity_id).collect();
    assert_eq!(
        for_entity.len(),
        1,
        "expected exactly 1 row after same-model upsert"
    );
    assert_eq!(
        for_entity[0].purpose, "second version",
        "upsert should replace with latest version"
    );
}

// ── Test 4: get_for_model exact match ────────────────────────────────────────

#[test]
fn get_for_model_exact_match() {
    let db = make_backend();
    let corpus_id = "c4";
    let entity_id = "e4";
    seed_entity(db.as_ref(), corpus_id, entity_id);

    let haiku = "claude-haiku-4-5-20251001";
    let sonnet = "claude-sonnet-4-5-20250929";

    db.contract_upsert(&make_contract(entity_id, corpus_id, haiku))
        .unwrap();
    db.contract_upsert(&make_contract(entity_id, corpus_id, sonnet))
        .unwrap();

    // Exact match for haiku.
    let haiku_row = db
        .contract_get_for_model(corpus_id, entity_id, haiku)
        .unwrap();
    assert!(haiku_row.is_some(), "expected haiku contract");
    assert_eq!(haiku_row.unwrap().model, haiku);

    // Exact match for sonnet.
    let sonnet_row = db
        .contract_get_for_model(corpus_id, entity_id, sonnet)
        .unwrap();
    assert!(sonnet_row.is_some(), "expected sonnet contract");
    assert_eq!(sonnet_row.unwrap().model, sonnet);

    // Mismatched model returns None.
    let missing = db
        .contract_get_for_model(corpus_id, entity_id, "claude-opus-4-20250514")
        .unwrap();
    assert!(
        missing.is_none(),
        "expected None for a model that was not inserted"
    );
}

// ── Test 5: unknown tier ranks lowest ────────────────────────────────────────

#[test]
fn unknown_tier_ranks_below_named_tiers() {
    let db = make_backend();
    let corpus_id = "c5";
    let entity_id = "e5";
    seed_entity(db.as_ref(), corpus_id, entity_id);

    // Insert unknown-tier first, then haiku.
    db.purpose_upsert(&make_purpose(entity_id, corpus_id, "unknown"))
        .unwrap();
    db.purpose_upsert(&make_purpose(
        entity_id,
        corpus_id,
        "claude-haiku-4-5-20251001",
    ))
    .unwrap();

    let best = db.purpose_get(corpus_id, entity_id).unwrap().unwrap();
    assert_eq!(best.model_tier, "haiku", "haiku should outrank unknown");
}

// ── Test 5b: Summary multi-model ─────────────────────────────────────────────

#[test]
fn summary_multi_model_best_tier() {
    let db = make_backend();
    let corpus_id = "c5b";
    let entity_id = "e5b";
    seed_entity(db.as_ref(), corpus_id, entity_id);

    db.summary_upsert(&make_summary(corpus_id, entity_id, "unknown"))
        .unwrap();
    db.summary_upsert(&make_summary(
        corpus_id,
        entity_id,
        "claude-sonnet-4-5-20250929",
    ))
    .unwrap();

    let best = db
        .summary_get(corpus_id, &SummaryTargetKind::Entity, entity_id)
        .unwrap()
        .unwrap();
    assert_eq!(best.model_tier, "sonnet");

    // get_for_model works too.
    let exact = db
        .summary_get_for_model(corpus_id, &SummaryTargetKind::Entity, entity_id, "unknown")
        .unwrap();
    assert!(exact.is_some());
    assert_eq!(exact.unwrap().model, "unknown");
}

// ── Test 6: Idempotency-by-model in the pipeline ─────────────────────────────

/// A named LLM provider whose `name()` is configurable at construction time.
/// Returns canned purpose/contract responses like DryRunProvider.
struct NamedDryRunProvider {
    name: &'static str,
    usage: Arc<Mutex<ProviderUsage>>,
}

impl NamedDryRunProvider {
    fn new(name: &'static str) -> Self {
        NamedDryRunProvider {
            name,
            usage: Arc::new(Mutex::new(ProviderUsage::default())),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for NamedDryRunProvider {
    async fn complete(
        &self,
        _req: CompletionRequest,
    ) -> Result<CompletionResponse, callimachus_llm::LlmError> {
        let mut u = self.usage.lock().unwrap();
        u.calls += 1;
        Ok(CompletionResponse {
            text: r#"{"purpose":"test purpose","blocks":[]}"#.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model_used: self.name.to_string(),
        })
    }

    fn name(&self) -> &str {
        self.name
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    fn usage(&self) -> ProviderUsage {
        self.usage.lock().unwrap().clone()
    }

    fn reset_usage(&self) {
        *self.usage.lock().unwrap() = ProviderUsage::default();
    }
}

/// Minimal adapter that produces one function entity per run.
struct PipelineTestAdapter;

const PIPE_CORPUS: &str = "pipe-test";

#[async_trait::async_trait]
impl SourceAdapter for PipelineTestAdapter {
    fn kind(&self) -> &str {
        "pipe-test"
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
        Ok(vec![Chunk::new(
            PIPE_CORPUS.to_string(),
            None,
            "module".to_string(),
            Location::new(PIPE_CORPUS, "src/lib.rs"),
            "pub fn foo() {}".to_string(),
        )])
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
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        let mut entity = Entity::new(
            format!("{}:foo", PIPE_CORPUS),
            PIPE_CORPUS.to_string(),
            "foo".to_string(),
            "function".to_string(),
        );
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
        _llm: &dyn LlmProvider,
        _depth: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    async fn resolve_aliases(
        &self,
        _entities: &[Entity],
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>> {
        Ok(vec![])
    }

    fn format_location(&self, chunk: &Chunk) -> String {
        chunk.location.path.clone()
    }

    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
        Ok(LocationRef {
            corpus_id: PIPE_CORPUS.to_string(),
            path: uri.to_string(),
        })
    }

    async fn extract_purpose(
        &self,
        _entity: &Entity,
        _content: &str,
        _summary: Option<&str>,
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedPurpose>> {
        Ok(Some(ExtractedPurpose {
            purpose: "a purpose".to_string(),
            blocks: vec![ExtractedBlock {
                label: "main logic".to_string(),
                description: "does stuff".to_string(),
            }],
        }))
    }

    async fn extract_contract(
        &self,
        _entity: &Entity,
        _content: &str,
        _summary: Option<&str>,
        _purpose: Option<&str>,
        _signals: &serde_json::Value,
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedContract>> {
        Ok(None)
    }
}

async fn run_purpose_pass(db: Arc<dyn StorageBackend>, corpus: &Corpus, llm: Arc<dyn LlmProvider>) {
    let pipeline = IndexPipeline {
        db,
        adapter: Arc::new(PipelineTestAdapter),
        llm,
        embedder: None,
    };
    pipeline
        .run(
            corpus,
            IndexOptions {
                passes: vec![
                    Pass::Chunk,
                    Pass::Structure,
                    Pass::Semantic,
                    Pass::Aliases,
                    Pass::Purpose,
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn pipeline_idempotency_by_model() {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus = Corpus::new(
        PIPE_CORPUS.to_string(),
        "Pipeline Test".to_string(),
        "pipe-test".to_string(),
        "/tmp/pipe".to_string(),
    );
    db.corpus_insert(&corpus).unwrap();

    let haiku_llm = Arc::new(NamedDryRunProvider::new("claude-haiku-4-5-20251001"));
    let sonnet_llm = Arc::new(NamedDryRunProvider::new("claude-sonnet-4-5-20250929"));

    let entity_id = format!("{}:foo", PIPE_CORPUS);

    // Run #1: haiku. Should produce 1 purpose row.
    run_purpose_pass(Arc::clone(&db) as _, &corpus, Arc::clone(&haiku_llm) as _).await;

    let purposes = db.purpose_list(PIPE_CORPUS).unwrap();
    assert_eq!(
        purposes.len(),
        1,
        "expected 1 purpose row after first haiku run"
    );
    assert_eq!(purposes[0].model, "claude-haiku-4-5-20251001");

    // Run #2: haiku again (same model). Idempotent — still 1 row.
    run_purpose_pass(Arc::clone(&db) as _, &corpus, haiku_llm).await;

    let purposes = db.purpose_list(PIPE_CORPUS).unwrap();
    let entity_purposes: Vec<_> = purposes
        .iter()
        .filter(|p| p.entity_id == entity_id)
        .collect();
    assert_eq!(
        entity_purposes.len(),
        1,
        "same-model re-run should not add a second row"
    );

    // Run #3: sonnet (different model). Should add a second row.
    run_purpose_pass(Arc::clone(&db) as _, &corpus, sonnet_llm).await;

    let purposes = db.purpose_list(PIPE_CORPUS).unwrap();
    let entity_purposes: Vec<_> = purposes
        .iter()
        .filter(|p| p.entity_id == entity_id)
        .collect();
    assert_eq!(
        entity_purposes.len(),
        2,
        "different-model run should add a second purpose row"
    );

    // Best-tier should now be sonnet.
    let best = db.purpose_get(PIPE_CORPUS, &entity_id).unwrap().unwrap();
    assert_eq!(
        best.model_tier, "sonnet",
        "sonnet should be the best-tier artifact"
    );
}
