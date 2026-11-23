//! Behavioural tests for the binary-export adapter, over a synthetic `toy16`
//! bundle written into a temp dir.

use std::path::Path;
use std::sync::{Arc, Mutex};

use callimachus_adapter_binexport::{BinExportAdapter, content, merge_platform_config};
use callimachus_adapter_contract::SourceAdapter;
use callimachus_core::{
    indexing::{IndexOptions, IndexPipeline},
    storage::{SqliteBackend, StorageBackend},
    types::{Corpus, Entity, Pass},
};
use callimachus_llm::{
    CompletionRequest, CompletionResponse, DryRunProvider, LlmProvider, ProviderUsage,
};
use serde_json::json;

const CORP: &str = "toy";

// ── Fixture ──────────────────────────────────────────────────────────────────

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn write_bundle_with_format(root: &Path, format: u64) {
    write(
        root,
        "manifest.json",
        &json!({
            "format": format, "kind": "binary-export",
            "platform": {"id": "toy16", "profile_sha256": "abc123", "cpu": "toycpu", "address_bits": 24},
            "cartridge": {"sha256": "def456"},
            "tools": {"decompiler": "d", "exporter": "e"}, "rounds": 1,
            "future_key": {"ignored": true},
            "counts": {"functions": 3, "decompile_failed": 1, "tables": 1, "ports": 1, "ram": 1}
        })
        .to_string(),
    );
    let funcs = [
        json!({"address":"0x001000","name":"FUN_00001000","size":40,"c":"c/00001000.c","asm":"asm/00001000.s","decompile_ok":true,"discovered_by":["analysis"]}),
        json!({"address":"0x002000","name":"FUN_00002000","size":60,"c":"c/00002000.c","asm":"asm/00002000.s","decompile_ok":true,"discovered_by":["vector","table:0x012000"]}),
        json!({"address":"0x003000","name":"FUN_00003000","size":12,"c":null,"asm":"asm/00003000.s","decompile_ok":false,"discovered_by":["declared:0x003000"]}),
    ];
    write(
        root,
        "functions.jsonl",
        &funcs
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    write(
        root,
        "c/00001000.c",
        "// FUN_00001000 decompiled\nvoid FUN_00001000(void)\n{\n  RAM_ff0010 = 1;\n  if (IO_c00004 != 0) {\n    FUN_00002000(3);\n  }\n  return;\n}\n",
    );
    write(
        root,
        "c/00002000.c",
        "// FUN_00002000 decompiled\nint FUN_00002000(int i)\n{\n  return PTR_DAT_00012000[i];\n}\n",
    );
    write(
        root,
        "asm/00001000.s",
        "001000\t4e71\tnop\n001002\t13fc\tmove.b #1,$ff0010\n001006\t4e75\trts ; done */\n",
    );
    write(root, "asm/00002000.s", "002000\t4e75\trts\n");
    write(
        root,
        "asm/00003000.s",
        "003000\t4e71\tnop\n003002\t4e75\trts\n",
    );
    write(
        root,
        "tables.jsonl",
        &json!({"address":"0x012000","name":"PTR_DAT_00012000","kind":"code-pointers","entry":"u32","count":4,"readers":["0x002000"],"source":"dispatch"}).to_string(),
    );
    write(
        root,
        "ports.jsonl",
        &json!({"address":"0xc00004","port":"toy.status","name":"IO_c00004","readers":["0x001000"],"writers":[]}).to_string(),
    );
    write(
        root,
        "ram.jsonl",
        &json!({"address":"0xff0010","name":"RAM_ff0010","readers":[],"writers":["0x001000"]})
            .to_string(),
    );
}

fn write_bundle(root: &Path) {
    write_bundle_with_format(root, 1);
}

async fn index(bundle: &Path, db: Arc<SqliteBackend>, adapter: Arc<BinExportAdapter>) {
    let corpus = Corpus::new(
        CORP.into(),
        "Toy".into(),
        "binary-export".into(),
        bundle.to_str().unwrap().into(),
    );
    if !db.corpus_exists(CORP).unwrap() {
        db.corpus_insert(&corpus).unwrap();
    }
    let pipeline = IndexPipeline {
        db: db.clone(),
        adapter,
        llm: Arc::new(DryRunProvider::new()),
        embedder: None,
    };
    pipeline
        .run(
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            },
        )
        .await
        .unwrap();
}

async fn indexed() -> (tempfile::TempDir, Arc<SqliteBackend>) {
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path());
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    index(dir.path(), db.clone(), Arc::new(BinExportAdapter::new())).await;
    (dir, db)
}

fn entity(db: &SqliteBackend, id: &str) -> Entity {
    db.entity_get_by_id(&format!("{CORP}:{id}"))
        .unwrap()
        .unwrap_or_else(|| panic!("missing entity {id}"))
}

fn has_edge(db: &SqliteBackend, kind: &str, from: &str, to: &str) -> bool {
    db.edge_list(CORP).unwrap().iter().any(|e| {
        e.kind == kind
            && e.from_entity_id == format!("{CORP}:{from}")
            && e.to_entity_id == format!("{CORP}:{to}")
    })
}

async fn chunks_of(root: &Path) -> Vec<callimachus_core::types::Chunk> {
    let a = BinExportAdapter::new();
    let src = a.discover(root.to_str().unwrap()).await.unwrap().remove(0);
    let mut src = src;
    src.meta = json!({"corpus_id": CORP});
    a.chunk(&src).await.unwrap()
}

// ── Ingest → entities and edges ──────────────────────────────────────────────

#[tokio::test]
async fn ingest_creates_function_entities_with_address_aliases() {
    let (_d, db) = indexed().await;
    for (name, alias) in [
        ("fun_00001000", "0x1000"),
        ("fun_00002000", "0x2000"),
        ("fun_00003000", "0x3000"),
    ] {
        let e = entity(&db, name);
        assert_eq!(e.kind, "function", "{name}");
        assert!(
            e.aliases.contains(&alias.to_string()),
            "{name}: {:?}",
            e.aliases
        );
    }
    assert!(has_edge(&db, "calls", "fun_00001000", "fun_00002000"));
}

#[tokio::test]
async fn tables_ports_and_ram_become_typed_entities() {
    let (_d, db) = indexed().await;

    let t = entity(&db, "ptr_dat_00012000");
    assert_eq!(t.kind, "table");
    assert_eq!(t.description.as_deref(), Some("code-pointers table"));
    for a in ["0x12000", "entry:u32", "count:4"] {
        assert!(t.aliases.contains(&a.to_string()), "{:?}", t.aliases);
    }
    assert!(has_edge(
        &db,
        "uses_table",
        "fun_00002000",
        "ptr_dat_00012000"
    ));

    let p = entity(&db, "io_c00004");
    assert_eq!(p.kind, "port");
    assert!(p.aliases.contains(&"0xc00004".to_string()));
    assert!(p.aliases.contains(&"toy.status".to_string()));
    assert!(has_edge(&db, "reads", "fun_00001000", "io_c00004"));

    let r = entity(&db, "ram_ff0010");
    assert_eq!(r.kind, "variable");
    assert!(r.aliases.contains(&"0xff0010".to_string()));
    assert!(has_edge(&db, "writes", "fun_00001000", "ram_ff0010"));
}

#[tokio::test]
async fn table_and_port_kinds_beat_a_variable_stub_indexed_first() {
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path());
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    db.corpus_insert(&Corpus::new(
        CORP.into(),
        "Toy".into(),
        "binary-export".into(),
        dir.path().to_str().unwrap().into(),
    ))
    .unwrap();
    // What C analysis of another chunk emits for a free identifier.
    for name in ["io_c00004", "ptr_dat_00012000"] {
        let mut stub = Entity::new(
            format!("{CORP}:{name}"),
            CORP.into(),
            name.to_uppercase(),
            "variable".into(),
        );
        stub.confidence = 0.5;
        stub.appearance_count = 1;
        db.entity_upsert(&stub).unwrap();
    }
    index(dir.path(), db.clone(), Arc::new(BinExportAdapter::new())).await;
    assert_eq!(entity(&db, "io_c00004").kind, "port");
    assert_eq!(entity(&db, "ptr_dat_00012000").kind, "table");
}

#[tokio::test]
async fn function_without_c_still_exists_with_asm_only_chunk() {
    let (d, db) = indexed().await;
    assert_eq!(entity(&db, "fun_00003000").kind, "function");

    let chunks = chunks_of(d.path()).await;
    let c = chunks
        .iter()
        .find(|c| c.location.path == "fn/00003000")
        .unwrap();
    let (c_part, asm) = content::split_content(&c.content);
    assert_eq!(c_part.lines().count(), 1, "header only: {c_part:?}");
    assert!(asm.unwrap().contains("/* 003002  rts */"));
}

// ── Chunk content ────────────────────────────────────────────────────────────

#[tokio::test]
async fn chunk_content_layout_and_escaping() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    let chunks = chunks_of(d.path()).await;
    assert_eq!(chunks.len(), 3);
    let c = chunks
        .iter()
        .find(|c| c.location.path == "fn/00001000")
        .unwrap();
    assert_eq!(c.kind, "function");
    assert_eq!(c.language.as_deref(), Some("c"));
    assert!(
        c.content
            .starts_with("// FUN_00001000 @ 0x001000  (size 40 bytes; discovered_by analysis)\n")
    );
    assert!(c.content.contains("void FUN_00001000(void)"));
    assert!(
        !c.content.contains("decompiled"),
        "first comment line dropped"
    );
    assert!(c.content.contains(content::DISASSEMBLY_MARKER));
    assert!(c.content.contains("/* 001002  move.b #1,$ff0010 */"));
    assert!(c.content.contains("/* 001006  rts ; done *\\/ */"));
    assert!(!c.content.contains("4e71"), "hex bytes dropped");
}

#[tokio::test]
async fn chunk_content_parses_as_c_without_top_level_errors() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .unwrap();
    for c in chunks_of(d.path()).await {
        let tree = parser.parse(&c.content, None).unwrap();
        let root = tree.root_node();
        let mut cur = root.walk();
        assert!(
            root.children(&mut cur).all(|n| n.kind() != "ERROR"),
            "{}: {}",
            c.location.path,
            root.to_sexp()
        );
    }
}

#[test]
fn location_round_trips() {
    let a = BinExportAdapter::new();
    let r = a.parse_location("calli://toy/fn/000071d0").unwrap();
    assert_eq!(
        (r.corpus_id.as_str(), r.path.as_str()),
        ("toy", "fn/000071d0")
    );
    assert_eq!(content::address_of_path(&r.path), Some(0x71d0));
    assert!(a.parse_location("calli://toy/src/x.c").is_err());
}

// ── Manifest and platform ────────────────────────────────────────────────────

#[tokio::test]
async fn unsupported_format_is_a_clear_error() {
    let d = tempfile::tempdir().unwrap();
    write_bundle_with_format(d.path(), 2);
    let err = BinExportAdapter::new()
        .discover(d.path().to_str().unwrap())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unsupported binary-export bundle format 2"),
        "{err}"
    );
}

#[test]
fn platform_is_merged_into_corpus_config() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    let mut cfg = json!({"cpu_spaces": {"a": 1}});
    merge_platform_config(&mut cfg, d.path()).unwrap();
    assert_eq!(cfg["platform"]["id"], "toy16");
    assert_eq!(cfg["platform"]["address_bits"], 24);
    assert_eq!(cfg["address_mask"], "0xffffff");
    assert_eq!(cfg["cartridge_sha256"], "def456");
    assert_eq!(cfg["cpu_spaces"], json!({"a": 1}));

    let mut preset = json!({"address_mask": "0xfffff"});
    merge_platform_config(&mut preset, d.path()).unwrap();
    assert_eq!(preset["address_mask"], "0xfffff", "existing mask is kept");
}

// ── Prompts ──────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl LlmProvider for Recorder {
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<CompletionResponse, callimachus_llm::LlmError> {
        self.0.lock().unwrap().push(req.prompt);
        Ok(CompletionResponse {
            text: "ok".into(),
            input_tokens: 0,
            output_tokens: 0,
            model_used: "rec".into(),
        })
    }
    fn name(&self) -> &str {
        "rec"
    }
    fn supports_parallel(&self) -> bool {
        true
    }
    fn usage(&self) -> ProviderUsage {
        ProviderUsage::default()
    }
    fn reset_usage(&self) {}
}

fn big_asm_content() -> String {
    let c = "// F @ 0x001000  (size 1 bytes; discovered_by analysis)\nvoid F(void)\n{\n}\n";
    let mut s = String::from(c);
    s.push_str(content::DISASSEMBLY_MARKER);
    s.push('\n');
    for i in 0..400 {
        s.push_str(&format!("/* {i:06x}  move.w d0,d1 ; pad pad pad */\n"));
    }
    assert!(s.len() > 10_000);
    s
}

#[test]
fn trimming_keeps_c_and_budgets_the_assembly() {
    let s = big_asm_content();
    let t = content::trim_for_prompt(&s);
    assert!(t.contains("void F(void)"));
    let (c_part, asm) = content::split_content(&t);
    assert!(c_part.len() <= content::C_BUDGET);
    let asm = asm.unwrap();
    assert!(asm.len() <= content::ASM_BUDGET + 64, "{}", asm.len());
    let last = asm.lines().last().unwrap();
    assert!(
        last.starts_with("/* … ") && last.ends_with(" more instructions */"),
        "{last}"
    );
    let kept = asm.lines().count() - 1;
    assert_eq!(last, format!("/* … {} more instructions */", 400 - kept));
}

#[test]
fn trimming_never_cuts_c_to_make_room_for_asm_and_leaves_short_content_alone() {
    let long_c = format!("// h\n{}\n", "int x;\n".repeat(400)); // ~2,800 chars of C
    let s = format!(
        "{long_c}{}\n/* 000000  nop */\n",
        content::DISASSEMBLY_MARKER
    );
    assert_eq!(content::trim_for_prompt(&s), s);
    let huge_c = format!("// h\n{}", "int x;\n".repeat(1000));
    let t = content::trim_for_prompt(&format!(
        "{huge_c}{}\n/* 0  nop */\n",
        content::DISASSEMBLY_MARKER
    ));
    assert!(content::split_content(&t).0.len() >= content::C_BUDGET - 7);
}

#[tokio::test]
async fn summary_prompt_carries_hint_and_trimmed_content() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    let chunk = chunks_of(d.path())
        .await
        .into_iter()
        .find(|c| c.location.path == "fn/00001000")
        .unwrap();
    let rec = Recorder::default();
    BinExportAdapter::new()
        .summarize(&chunk, &rec, "function")
        .await
        .unwrap();
    {
        let prompts = rec.0.lock().unwrap();
        assert!(prompts[0].contains(
            "The disassembly of the same routine follows the C; use it for register parameters, exact offsets and access widths."
        ));
        assert!(prompts[0].contains("/* 001002  move.b #1,$ff0010 */"));
    }

    // A big routine is trimmed in the prompt.
    let mut big = chunk.clone();
    big.content = big_asm_content();
    let rec = Recorder::default();
    BinExportAdapter::new()
        .summarize(&big, &rec, "function")
        .await
        .unwrap();
    assert!(rec.0.lock().unwrap()[0].contains("more instructions */"));
}
