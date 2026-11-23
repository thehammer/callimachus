//! The C corpus option `decompiler_names` must survive a structure-only run
//! driven through the real index pipeline (fresh adapter, no chunk pass).

use std::sync::Arc;

use callimachus_adapter_code::CodeAdapter;
use callimachus_core::{
    indexing::{IndexOptions, IndexPipeline},
    storage::{SqliteBackend, StorageBackend},
    types::{Corpus, Pass},
};
use callimachus_llm::DryRunProvider;
use serde_json::json;

const CORP: &str = "dc";
const SRC: &str = "void f(void) { code = 1; addr = word; }\n";

async fn run(db: &Arc<SqliteBackend>, corpus: &Corpus, adapter: CodeAdapter, passes: Vec<Pass>) {
    let pipeline = IndexPipeline {
        db: db.clone(),
        adapter: Arc::new(adapter),
        llm: Arc::new(DryRunProvider::new()),
        embedder: None,
    };
    pipeline
        .run(
            corpus,
            IndexOptions {
                passes,
                ..IndexOptions::default()
            },
        )
        .await
        .unwrap();
}

/// Chunk with one adapter, then run Structure alone with a brand-new adapter.
async fn variable_names(config: Option<serde_json::Value>) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.c"), SRC).unwrap();
    let mut corpus = Corpus::new(
        CORP.into(),
        "DC".into(),
        "code".into(),
        dir.path().to_str().unwrap().into(),
    );
    if let Some(c) = config {
        corpus.config = c;
    }
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    db.corpus_insert(&corpus).unwrap();

    run(&db, &corpus, CodeAdapter::new(), vec![Pass::Chunk]).await;
    run(&db, &corpus, CodeAdapter::new(), vec![Pass::Structure]).await;

    db.entity_list(CORP)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "variable")
        .map(|e| e.canonical_name)
        .collect()
}

#[tokio::test]
async fn structure_only_run_honours_decompiler_names_option() {
    let vars = variable_names(Some(json!({"decompiler_names": true}))).await;
    for bad in ["code", "addr", "word"] {
        assert!(!vars.iter().any(|v| v == bad), "{bad} in {vars:?}");
    }
}

#[tokio::test]
async fn without_the_option_ghidra_type_names_stay_variables() {
    let vars = variable_names(None).await;
    for good in ["code", "addr", "word"] {
        assert!(
            vars.iter().any(|v| v == good),
            "{good} missing from {vars:?}"
        );
    }
}
