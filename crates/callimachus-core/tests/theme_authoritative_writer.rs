//! Behavioral test for the theme-pass authoritative writer (PR fix).
//!
//! Regression target: after N runs with different theme sets, the head `themes`
//! table must contain exactly the themes from the most-recent run — not the
//! accumulated union across all runs.
//!
//! Symptom that motivated this test: a 44-commit HEAD-mode walk on the
//! `webster` corpus produced A=7, B=45, M=29 themes on three independent index
//! paths (all should have converged to 7).

use std::sync::{Arc, Mutex};

use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, ExtractedTheme,
        ExtractedThemes, LocationRef, SourceAdapter,
    },
    indexing::pipeline::{IndexOptions, IndexPipeline},
    storage::{SqliteBackend, StorageBackend},
    types::{Chunk, Corpus, Entity, Location, Pass},
};
use callimachus_llm::{DryRunProvider, LlmProvider};

// ── Setup helpers ─────────────────────────────────────────────────────────────

fn setup(corpus_id: &str) -> (Arc<dyn StorageBackend>, Corpus) {
    let db = SqliteBackend::open_in_memory().unwrap();
    let corpus = Corpus::new(
        corpus_id.to_string(),
        format!("Theme Auth Test — {corpus_id}"),
        "theme-auth-fake".to_string(),
        "/tmp/theme-auth-test".to_string(),
    );
    db.corpus_insert(&corpus).unwrap();
    (Arc::new(db), corpus)
}

async fn run_passes(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    dry: Arc<DryRunProvider>,
    passes: Vec<Pass>,
) {
    let pipeline = IndexPipeline {
        db,
        adapter,
        llm: Arc::clone(&dry) as Arc<dyn LlmProvider>,
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

// ── Fixture adapter ───────────────────────────────────────────────────────────

/// Produces 20 function entities (one per file `src/mod0.rs`…`src/mod19.rs`).
///
/// `extract_themes` reads from a shared mutex so the test can swap the theme
/// list between runs — simulating the LLM returning different results at
/// different commits.  Each call makes exactly one `llm.complete(...)` call
/// so `DryRunProvider::call_count()` reflects real LLM invocations.
struct ThemeAuthAdapter {
    corpus_id: String,
    /// (title, statement) pairs returned by `extract_themes`.
    themes: Arc<Mutex<Vec<(String, String)>>>,
}

impl ThemeAuthAdapter {
    fn new(corpus_id: &str) -> (Self, Arc<Mutex<Vec<(String, String)>>>) {
        let themes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(vec![]));
        (
            Self {
                corpus_id: corpus_id.to_string(),
                themes: Arc::clone(&themes),
            },
            themes,
        )
    }
}

#[async_trait::async_trait]
impl SourceAdapter for ThemeAuthAdapter {
    fn kind(&self) -> &str {
        "theme-auth-fake"
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

    /// 20 chunks — one per file — so `MIN_ENTITIES_FOR_THEMES = 20` is met.
    async fn chunk(&self, _source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        let cid = &self.corpus_id;
        let chunks = (0..20)
            .map(|i| {
                Chunk::new(
                    cid.clone(),
                    None,
                    "module".to_string(),
                    Location::new(cid, format!("src/mod{i}.rs")),
                    format!("pub fn fn{i}() {{}}"),
                )
            })
            .collect();
        Ok(chunks)
    }

    async fn extract_structure(&self, _chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
        Ok(ExtractedStructure {
            parent_path: None,
            child_paths: vec![],
            structural_entities: vec![],
            structural_edges: vec![],
        })
    }

    /// One `function` entity per chunk — satisfies the theme-pass precondition.
    async fn extract_with_llm(
        &self,
        chunk: &Chunk,
        _llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>> {
        let cid = &self.corpus_id;
        let n: u32 = chunk
            .location
            .path
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        let entity_id = format!("{cid}:fn{n}");
        let mut entity = Entity::new(
            entity_id,
            cid.clone(),
            format!("fn{n}"),
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
            corpus_id: self.corpus_id.clone(),
            path: uri.to_string(),
        })
    }

    /// Returns whatever title/statement pairs the test has loaded into the mutex.
    /// Makes exactly one `llm.complete(...)` call so cache-miss detection works.
    async fn extract_themes(
        &self,
        corpus: &callimachus_core::types::Corpus,
        _entities: &[Entity],
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedThemes>> {
        use callimachus_llm::CompletionRequest;
        // One LLM call so DryRunProvider::call_count() increments on a cache miss.
        llm.complete(CompletionRequest {
            prompt: format!("extract themes for corpus: {}", corpus.id),
            kind: "theme".to_string(),
            pass: "theme".to_string(),
            ..Default::default()
        })
        .await?;
        let guard = self.themes.lock().unwrap();
        let themes = guard
            .iter()
            .map(|(title, statement)| ExtractedTheme {
                title: title.clone(),
                statement: statement.clone(),
                confidence: 0.9,
                upheld_by_entity_names: vec![],
                violated_by_entity_names: vec![],
            })
            .collect();
        Ok(Some(ExtractedThemes { themes }))
    }
}

// ── Test ──────────────────────────────────────────────────────────────────────

/// After 3 runs with different theme sets, the head table must always contain
/// exactly the 2 themes from the most-recent run — never accumulated from prior
/// runs.
///
/// Without the fix: after run 3, `theme_list` would return 6 rows (2+2+2).
/// With the fix: it returns exactly 2 rows each time.
///
/// Cache-miss mechanism: the Layer-2 theme cache is keyed on the corpus
/// entity-set hash + model. Between runs we insert an extra `function` entity
/// directly into the DB; this changes the non-"theme" entity_id set and
/// therefore the hash, guaranteeing a real `extract_themes` call each time.
#[tokio::test]
async fn theme_head_stays_authoritative_across_runs() {
    let cid = "theme-auth-test";
    let (db, corpus) = setup(cid);
    let (adapter, themes_lock) = ThemeAuthAdapter::new(cid);
    let adapter = Arc::new(adapter);
    let dry = Arc::new(DryRunProvider::new());

    // ── Seed: populate ≥ 20 source entities ───────────────────────────────────
    run_passes(
        Arc::clone(&db),
        &corpus,
        Arc::clone(&adapter) as Arc<dyn SourceAdapter>,
        Arc::clone(&dry),
        vec![Pass::Chunk, Pass::Structure, Pass::Semantic],
    )
    .await;

    let entity_count = db.entity_count(cid).unwrap();
    assert!(
        entity_count >= 20,
        "seed: expected ≥ 20 source entities; got {entity_count}"
    );

    // ── Run 1: Alpha + Beta ────────────────────────────────────────────────────
    {
        let mut g = themes_lock.lock().unwrap();
        *g = vec![
            ("Alpha".to_string(), "The alpha theme.".to_string()),
            ("Beta".to_string(), "The beta theme.".to_string()),
        ];
    }
    dry.reset_usage();
    run_passes(
        Arc::clone(&db),
        &corpus,
        Arc::clone(&adapter) as Arc<dyn SourceAdapter>,
        Arc::clone(&dry),
        vec![Pass::Theme],
    )
    .await;
    assert_eq!(
        dry.call_count(),
        1,
        "run 1: expected exactly 1 LLM call (cache miss); got {}",
        dry.call_count()
    );

    let themes = db.theme_list(cid).unwrap();
    assert_eq!(
        themes.len(),
        2,
        "run 1: expected 2 head themes; got {}",
        themes.len()
    );
    let ids: Vec<String> = themes.iter().map(|t| t.id.clone()).collect();
    assert!(
        ids.contains(&format!("{cid}:theme:alpha")),
        "run 1: alpha theme missing; ids={ids:?}"
    );
    assert!(
        ids.contains(&format!("{cid}:theme:beta")),
        "run 1: beta theme missing; ids={ids:?}"
    );
    let theme_entities: Vec<_> = db
        .entity_list(cid)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "theme")
        .collect();
    assert_eq!(
        theme_entities.len(),
        2,
        "run 1: expected 2 kind=theme entity rows"
    );

    // ── Force cache miss for run 2 by extending the entity set ────────────────
    let extra1 = Entity::new(
        format!("{cid}:extra1"),
        cid.to_string(),
        "extra1".to_string(),
        "function".to_string(),
    );
    db.entity_upsert(&extra1).unwrap();

    // ── Run 2: Gamma + Delta ───────────────────────────────────────────────────
    {
        let mut g = themes_lock.lock().unwrap();
        *g = vec![
            ("Gamma".to_string(), "The gamma theme.".to_string()),
            ("Delta".to_string(), "The delta theme.".to_string()),
        ];
    }
    dry.reset_usage();
    run_passes(
        Arc::clone(&db),
        &corpus,
        Arc::clone(&adapter) as Arc<dyn SourceAdapter>,
        Arc::clone(&dry),
        vec![Pass::Theme],
    )
    .await;
    assert_eq!(
        dry.call_count(),
        1,
        "run 2: expected 1 LLM call (cache miss — entity set grew); got {}",
        dry.call_count()
    );

    let themes = db.theme_list(cid).unwrap();
    assert_eq!(
        themes.len(),
        2,
        "run 2: expected exactly 2 head themes (not accumulated 4); got {}",
        themes.len()
    );
    let ids: Vec<String> = themes.iter().map(|t| t.id.clone()).collect();
    assert!(
        ids.contains(&format!("{cid}:theme:gamma")),
        "run 2: gamma theme missing; ids={ids:?}"
    );
    assert!(
        ids.contains(&format!("{cid}:theme:delta")),
        "run 2: delta theme missing; ids={ids:?}"
    );
    assert!(
        !ids.contains(&format!("{cid}:theme:alpha")),
        "run 2: alpha should have been purged; ids={ids:?}"
    );
    assert!(
        !ids.contains(&format!("{cid}:theme:beta")),
        "run 2: beta should have been purged; ids={ids:?}"
    );
    let theme_entities: Vec<_> = db
        .entity_list(cid)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "theme")
        .collect();
    assert_eq!(
        theme_entities.len(),
        2,
        "run 2: expected 2 kind=theme entity rows"
    );

    // ── Force cache miss for run 3 ─────────────────────────────────────────────
    let extra2 = Entity::new(
        format!("{cid}:extra2"),
        cid.to_string(),
        "extra2".to_string(),
        "function".to_string(),
    );
    db.entity_upsert(&extra2).unwrap();

    // ── Run 3: Epsilon + Zeta ──────────────────────────────────────────────────
    {
        let mut g = themes_lock.lock().unwrap();
        *g = vec![
            ("Epsilon".to_string(), "The epsilon theme.".to_string()),
            ("Zeta".to_string(), "The zeta theme.".to_string()),
        ];
    }
    dry.reset_usage();
    run_passes(
        Arc::clone(&db),
        &corpus,
        Arc::clone(&adapter) as Arc<dyn SourceAdapter>,
        Arc::clone(&dry),
        vec![Pass::Theme],
    )
    .await;
    assert_eq!(
        dry.call_count(),
        1,
        "run 3: expected 1 LLM call (cache miss — entity set grew); got {}",
        dry.call_count()
    );

    let themes = db.theme_list(cid).unwrap();
    assert_eq!(
        themes.len(),
        2,
        "run 3: expected exactly 2 head themes (not accumulated 6); got {}",
        themes.len()
    );
    let ids: Vec<String> = themes.iter().map(|t| t.id.clone()).collect();
    assert!(
        ids.contains(&format!("{cid}:theme:epsilon")),
        "run 3: epsilon theme missing; ids={ids:?}"
    );
    assert!(
        ids.contains(&format!("{cid}:theme:zeta")),
        "run 3: zeta theme missing; ids={ids:?}"
    );
    assert!(
        !ids.contains(&format!("{cid}:theme:gamma")),
        "run 3: gamma should have been purged; ids={ids:?}"
    );
    assert!(
        !ids.contains(&format!("{cid}:theme:delta")),
        "run 3: delta should have been purged; ids={ids:?}"
    );
    let theme_entities: Vec<_> = db
        .entity_list(cid)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "theme")
        .collect();
    assert_eq!(
        theme_entities.len(),
        2,
        "run 3: expected 2 kind=theme entity rows"
    );
}
