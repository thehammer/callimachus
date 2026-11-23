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
            "counts": {"functions": 3, "decompile_failed": 1, "tables": 2, "ports": 1, "ram": 1}
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
        "// FUN_00002000 decompiled\nint FUN_00002000(int i)\n{\n  return PTR_DAT_00012000[i] + PTR_LAB_00014000[i];\n}\n",
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
        &[
            json!({"address":"0x012000","name":"PTR_DAT_00012000","kind":"code-pointers","entry":"u32","count":4,"readers":["0x002000"],"source":"dispatch"}),
            json!({"address":"0x014000","name":"PTR_LAB_00014000","kind":"code-pointers","entry":"u32","count":2,"readers":["0x002000"],"source":"dispatch"}),
        ]
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join("\n"),
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

fn set_counts(root: &Path, counts: serde_json::Value) {
    let p = root.join("manifest.json");
    let mut m: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    m["counts"] = counts;
    std::fs::write(p, m.to_string()).unwrap();
}

fn write_bundle(root: &Path) {
    write_bundle_with_format(root, 1);
}

async fn index(bundle: &Path, db: Arc<SqliteBackend>, adapter: Arc<BinExportAdapter>) {
    index_passes(bundle, db, adapter, vec![Pass::Chunk, Pass::Structure]).await;
}

async fn index_passes(
    bundle: &Path,
    db: Arc<SqliteBackend>,
    adapter: Arc<BinExportAdapter>,
    passes: Vec<Pass>,
) {
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
                passes,
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
        ("0x1000", "0x1000"),
        ("0x2000", "0x2000"),
        ("0x3000", "0x3000"),
    ] {
        let e = entity(&db, name);
        assert_eq!(e.kind, "function", "{name}");
        assert!(
            e.aliases.contains(&alias.to_string()),
            "{name}: {:?}",
            e.aliases
        );
    }
    assert!(has_edge(&db, "calls", "0x1000", "0x2000"));
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
    assert!(has_edge(&db, "uses_table", "0x2000", "ptr_dat_00012000"));

    let l = entity(&db, "ptr_lab_00014000");
    assert_eq!(l.kind, "table");
    assert!(
        l.aliases.contains(&"0x14000".to_string()),
        "{:?}",
        l.aliases
    );
    assert!(has_edge(&db, "uses_table", "0x2000", "ptr_lab_00014000"));

    let p = entity(&db, "io_c00004");
    assert_eq!(p.kind, "port");
    assert!(p.aliases.contains(&"0xc00004".to_string()));
    assert!(p.aliases.contains(&"toy.status".to_string()));
    assert!(has_edge(&db, "reads", "0x1000", "io_c00004"));

    let r = entity(&db, "ram_ff0010");
    assert_eq!(r.kind, "variable");
    assert!(r.aliases.contains(&"0xff0010".to_string()));
    assert!(has_edge(&db, "writes", "0x1000", "ram_ff0010"));
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
    assert_eq!(entity(&db, "0x3000").kind, "function");

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

#[tokio::test]
async fn bundle_paths_cannot_escape_the_bundle() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    set_counts(d.path(), json!({"functions": 1}));
    write(
        d.path(),
        "functions.jsonl",
        &json!({"address":"0x001000","name":"FUN_00001000","c":"../outside.c","asm":null,"decompile_ok":true}).to_string(),
    );
    let a = BinExportAdapter::new();
    let mut src = a
        .discover(d.path().to_str().unwrap())
        .await
        .unwrap()
        .remove(0);
    src.meta = json!({"corpus_id": CORP});
    let err = a.chunk(&src).await.unwrap_err().to_string();
    assert!(err.contains("escapes the bundle root"), "{err}");
}

// ── Malformed bundles ────────────────────────────────────────────────────────

async fn discover_err(root: &Path) -> String {
    BinExportAdapter::new()
        .discover(root.to_str().unwrap())
        .await
        .unwrap_err()
        .to_string()
}

#[tokio::test]
async fn manifest_only_bundle_is_an_error_naming_functions_jsonl() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    std::fs::remove_file(d.path().join("functions.jsonl")).unwrap();
    let err = discover_err(d.path()).await;
    assert!(err.contains("functions.jsonl"), "{err}");
}

#[tokio::test]
async fn optional_file_missing_is_ok_only_when_count_is_zero_or_absent() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    std::fs::remove_file(d.path().join("ports.jsonl")).unwrap();
    let err = discover_err(d.path()).await;
    assert!(err.contains("ports.jsonl") && err.contains('1'), "{err}");

    set_counts(
        d.path(),
        json!({"functions": 3, "ports": 0, "tables": 2, "ram": 1}),
    );
    BinExportAdapter::new()
        .discover(d.path().to_str().unwrap())
        .await
        .unwrap();
    set_counts(d.path(), json!({"functions": 3, "tables": 2, "ram": 1}));
    BinExportAdapter::new()
        .discover(d.path().to_str().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn truncated_functions_file_reports_both_counts() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    let p = d.path().join("functions.jsonl");
    let text = std::fs::read_to_string(&p).unwrap();
    let kept: Vec<&str> = text.lines().take(2).collect();
    std::fs::write(&p, kept.join("\n")).unwrap();
    let err = discover_err(d.path()).await;
    assert!(err.contains("functions.jsonl"), "{err}");
    assert!(err.contains("counts 3") && err.contains("2 rows"), "{err}");
}

#[tokio::test]
async fn bad_json_line_names_file_and_line() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    let p = d.path().join("functions.jsonl");
    let text = std::fs::read_to_string(&p).unwrap();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    lines[1] = "{not json".into();
    std::fs::write(&p, lines.join("\n")).unwrap();
    let err = format!(
        "{:#}",
        BinExportAdapter::new()
            .discover(d.path().to_str().unwrap())
            .await
            .unwrap_err()
    );
    assert!(err.contains("functions.jsonl:2"), "{err}");
}

#[tokio::test]
async fn duplicate_function_address_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    write_bundle(d.path());
    let p = d.path().join("functions.jsonl");
    let text = std::fs::read_to_string(&p).unwrap();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    lines[2] = lines[0].replace("FUN_00001000", "FUN_dup");
    std::fs::write(&p, lines.join("\n")).unwrap();
    let err = discover_err(d.path()).await;
    assert!(err.contains("duplicate function address 0x1000"), "{err}");
}

// ── Structure index ──────────────────────────────────────────────────────────

#[tokio::test]
async fn structure_extraction_over_a_large_bundle_uses_the_reader_index() {
    use callimachus_adapter_binexport::Bundle;
    let d = tempfile::tempdir().unwrap();
    let (nf, nr) = (2000usize, 5000usize);
    let funcs: Vec<String> = (0..nf)
        .map(|i| {
            json!({"address": format!("0x{:06x}", 0x1000 + i * 4), "name": format!("FUN_{:08x}", 0x1000 + i * 4), "c": null, "asm": null, "decompile_ok": false}).to_string()
        })
        .collect();
    write(d.path(), "functions.jsonl", &funcs.join("\n"));
    // Record i is written by function i % nf.
    let ram: Vec<String> = (0..nr)
        .map(|i| {
            json!({"address": format!("0x{:06x}", 0xff0000 + i), "name": format!("RAM_{:06x}", 0xff0000 + i),
                   "readers": [], "writers": [format!("0x{:06x}", 0x1000 + (i % nf) * 4)]}).to_string()
        })
        .collect();
    write(d.path(), "ram.jsonl", &ram.join("\n"));
    write(
        d.path(),
        "manifest.json",
        &json!({"format":1,"kind":"binary-export","counts":{"functions":nf,"ram":nr}}).to_string(),
    );

    let bundle = Bundle::load(d.path()).unwrap();
    let mut total = 0;
    for i in 0..nf {
        let n = bundle.ram_used_by((0x1000 + i * 4) as u64).count();
        assert_eq!(n, nr / nf + usize::from(i < nr % nf), "function {i}");
        total += n;
    }
    assert_eq!(
        total, nr,
        "every record is attributed to exactly one function"
    );

    let chunks = chunks_of(d.path()).await;
    assert_eq!(chunks.len(), nf);
    let a = BinExportAdapter::new();
    a.attach_corpus(CORP, d.path().to_str().unwrap());
    let s = a.extract_structure(&chunks[7]).await.unwrap();
    assert_eq!(
        s.structural_edges
            .iter()
            .filter(|e| e.kind == "writes")
            .count(),
        nr / nf + usize::from(7 < nr % nf)
    );
}

// ── Pass behaviour ───────────────────────────────────────────────────────────

#[tokio::test]
async fn structure_pass_alone_works_after_a_separate_chunk_run() {
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path());
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    index_passes(
        dir.path(),
        db.clone(),
        Arc::new(BinExportAdapter::new()),
        vec![Pass::Chunk],
    )
    .await;
    assert!(
        db.entity_get_by_id(&format!("{CORP}:0x1000"))
            .unwrap()
            .is_none()
    );
    // A fresh adapter, as in a new process.
    index_passes(
        dir.path(),
        db.clone(),
        Arc::new(BinExportAdapter::new()),
        vec![Pass::Structure],
    )
    .await;
    assert_eq!(entity(&db, "0x1000").kind, "function");
    assert!(has_edge(&db, "calls", "0x1000", "0x2000"));
    assert!(has_edge(&db, "writes", "0x1000", "ram_ff0010"));
}

#[tokio::test]
async fn missing_per_function_file_is_a_warning_not_a_failed_pass() {
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path());
    std::fs::remove_file(dir.path().join("asm/00002000.s")).unwrap();
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    index(dir.path(), db.clone(), Arc::new(BinExportAdapter::new())).await;

    // Everything that exists is indexed.
    for n in ["0x1000", "0x2000", "0x3000"] {
        assert_eq!(entity(&db, n).kind, "function", "{n}");
    }
    assert!(has_edge(&db, "calls", "0x1000", "0x2000"));

    let runs = db.run_latest(CORP, 20).unwrap();
    let chunk_run = runs.iter().find(|r| r.pass == "chunk").expect("chunk run");
    assert_eq!(chunk_run.status, "completed");
    assert_eq!(
        chunk_run.stats.errors.len(),
        1,
        "{:?}",
        chunk_run.stats.errors
    );
    assert!(chunk_run.stats.errors[0].message.contains("asm/00002000.s"));
    assert_eq!(chunk_run.stats.processed, 3);
}

// ── Struct-field entities and annotation ─────────────────────────────────────

#[tokio::test]
async fn field_access_in_decompiled_c_becomes_a_field_entity() {
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path());
    write(
        dir.path(),
        "c/00002000.c",
        "// FUN_00002000 decompiled\nvoid FUN_00002000(void)\n{\n  *(short *)(unaff_A2 + 0x10) = 1;\n}\n",
    );
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    index(dir.path(), db.clone(), Arc::new(BinExportAdapter::new())).await;

    let f = entity(&db, "unaff_a2+0x10");
    assert_eq!(f.kind, "field");
    assert!(has_edge(&db, "writes", "0x2000", "unaff_a2+0x10"));
}

fn obj_x(register: Option<&str>) -> callimachus_adapter_contract::FieldName {
    callimachus_adapter_contract::FieldName {
        base: "unaff_A2".into(),
        register: register.map(String::from),
        offset: 0x10,
        layout: "obj".into(),
        field: "x".into(),
        ty: Some("s16".into()),
    }
}

fn chunk_with_asm(asm_lines: &[&str]) -> String {
    let mut s = String::from(
        "// FUN_00002000 @ 0x002000  (size 8 bytes; discovered_by analysis)\nvoid FUN_00002000(void)\n{\n  *(short *)(unaff_A2 + 0x10) = 1;\n}\n",
    );
    s.push_str(content::DISASSEMBLY_MARKER);
    s.push('\n');
    for l in asm_lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

#[test]
fn annotate_labels_matching_disassembly_operands_and_the_c_part() {
    let a = BinExportAdapter::new();
    let fields = [obj_x(Some("a2"))];
    for operand in ["$10(a2)", "0x10(a2)", "($10,a2)"] {
        let line = format!("/* 001004  move.w {operand},d0 */");
        let out = a.annotate_source(&chunk_with_asm(&[&line]), "c", &fields);
        let expected = format!("/* 001004  move.w {operand},d0 ; obj.x */");
        assert!(out.contains(&expected), "{operand}: {out}");
        assert!(
            out.contains("unaff_A2->x /* obj+0x10 s16 */ = 1;"),
            "C part annotated: {out}"
        );
    }
}

#[test]
fn annotate_leaves_other_offsets_and_registers_alone() {
    let a = BinExportAdapter::new();
    let fields = [obj_x(Some("a2"))];
    let input = chunk_with_asm(&[
        "/* 001004  move.w $12(a2),d0 */",
        "/* 001006  move.w $10(a3),d0 */",
    ]);
    let out = a.annotate_source(&input, "c", &fields);
    assert!(out.contains("/* 001004  move.w $12(a2),d0 */\n"), "{out}");
    assert!(out.contains("/* 001006  move.w $10(a3),d0 */\n"), "{out}");
    assert!(!out.contains("; obj.x"), "{out}");
}

// ── S10c: cleanup ────────────────────────────────────────────────────────────

fn write_cleanup_bundle(root: &Path) {
    write(
        root,
        "manifest.json",
        &json!({"format":1,"kind":"binary-export","counts":{"functions":1,"tables":1}}).to_string(),
    );
    write(
        root,
        "functions.jsonl",
        &json!({"address":"0x001000","name":"FUN_00001000","size":8,"c":"c/00001000.c","asm":null,"decompile_ok":true,"discovered_by":["analysis"]}).to_string(),
    );
    write(
        root,
        "c/00001000.c",
        "void FUN_00001000(void)\n{\n  uint x;\n  x = (uint)PTR_DAT_00012340[1];\n  x = x + sizeof(undefined4);\n  switch(x) { case 1: goto switchD_00001234; }\n  switchD_00001234:\n  x = register0x0000003c;\n  x = (undefined4)x;\n  return;\n}\n",
    );
    write(
        root,
        "tables.jsonl",
        &json!({"address":"0x012340","name":"DAT_00012340","kind":"code-pointers","entry":"u32","count":2,"readers":["0x001000"]}).to_string(),
    );
}

#[tokio::test]
async fn c_spelling_of_a_table_address_reconciles_with_the_bundle_entity() {
    let dir = tempfile::tempdir().unwrap();
    write_cleanup_bundle(dir.path());
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    index(dir.path(), db.clone(), Arc::new(BinExportAdapter::new())).await;

    let with_addr: Vec<_> = db
        .entity_list(CORP)
        .unwrap()
        .into_iter()
        .filter(|e| e.aliases.contains(&"0x12340".to_string()))
        .collect();
    assert_eq!(with_addr.len(), 1, "{with_addr:?}");
    let t = &with_addr[0];
    assert_eq!(t.id, format!("{CORP}:dat_00012340"));
    assert_eq!(t.canonical_name, "DAT_00012340");
    assert_eq!(t.kind, "table");
    assert!(
        t.aliases.contains(&"PTR_DAT_00012340".to_string()),
        "{:?}",
        t.aliases
    );
    assert!(has_edge(&db, "uses_table", "0x1000", "dat_00012340"));
    assert!(
        db.edge_list(CORP)
            .unwrap()
            .iter()
            .all(|e| !e.to_entity_id.ends_with("ptr_dat_00012340")),
    );
}

#[tokio::test]
async fn type_names_and_ghidra_pseudo_names_are_not_variables() {
    let dir = tempfile::tempdir().unwrap();
    write_cleanup_bundle(dir.path());
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    index(dir.path(), db.clone(), Arc::new(BinExportAdapter::new())).await;
    let names: Vec<String> = db
        .entity_list(CORP)
        .unwrap()
        .into_iter()
        .map(|e| e.canonical_name)
        .collect();
    for bad in [
        "uint",
        "undefined4",
        "switchD_00001234",
        "register0x0000003c",
    ] {
        assert!(!names.iter().any(|n| n == bad), "{bad} in {names:?}");
    }
}

#[tokio::test]
async fn ptr_lab_stub_reconciles_with_table_at_its_address() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "manifest.json",
        &json!({"format":1,"kind":"binary-export","counts":{"functions":1,"tables":1}}).to_string(),
    );
    write(
        root,
        "functions.jsonl",
        &json!({"address":"0x001000","name":"FUN_00001000","size":8,"c":"c/00001000.c","asm":null,"decompile_ok":true,"discovered_by":["analysis"]}).to_string(),
    );
    write(
        root,
        "c/00001000.c",
        "void FUN_00001000(void)\n{\n  g(PTR_LAB_00001234[1]);\n}\n",
    );
    write(
        root,
        "tables.jsonl",
        &json!({"address":"0x001234","name":"DAT_00001234","kind":"code-pointers","entry":"u32","count":2,"readers":["0x001000"]}).to_string(),
    );
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    index(root, db.clone(), Arc::new(BinExportAdapter::new())).await;
    let with_addr: Vec<_> = db
        .entity_list(CORP)
        .unwrap()
        .into_iter()
        .filter(|e| e.aliases.contains(&"0x1234".to_string()))
        .collect();
    assert_eq!(with_addr.len(), 1, "{with_addr:?}");
    assert_eq!(with_addr[0].kind, "table");
    assert!(
        with_addr[0]
            .aliases
            .contains(&"PTR_LAB_00001234".to_string()),
        "{:?}",
        with_addr[0].aliases
    );
}

#[tokio::test]
async fn multi_segment_ghidra_stubs_reconcile_with_bundle_tables() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "manifest.json",
        &json!({"format":1,"kind":"binary-export","counts":{"functions":1,"tables":2}}).to_string(),
    );
    write(
        root,
        "functions.jsonl",
        &json!({"address":"0x001000","name":"FUN_00001000","size":8,"c":"c/00001000.c","asm":null,"decompile_ok":true,"discovered_by":["analysis"]}).to_string(),
    );
    write(
        root,
        "c/00001000.c",
        "void FUN_00001000(void)\n{\n  g(PTR_caseD_21_00010080[1]);\n  g(PTR_LAB_000043c6_2_00060878[1]);\n}\n",
    );
    write(
        root,
        "tables.jsonl",
        &[
            json!({"address":"0x010080","name":"PTR_DAT_00010080","kind":"code-pointers","entry":"u32","count":2,"readers":["0x001000"]}),
            json!({"address":"0x060878","name":"DAT_00060878","kind":"code-pointers","entry":"u32","count":2,"readers":["0x001000"]}),
        ]
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join("\n"),
    );
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    index(root, db.clone(), Arc::new(BinExportAdapter::new())).await;

    for (alias, bundle_id, c_name) in [
        ("0x10080", "ptr_dat_00010080", "PTR_caseD_21_00010080"),
        ("0x60878", "dat_00060878", "PTR_LAB_000043c6_2_00060878"),
    ] {
        let with_addr: Vec<_> = db
            .entity_list(CORP)
            .unwrap()
            .into_iter()
            .filter(|e| e.aliases.contains(&alias.to_string()))
            .collect();
        assert_eq!(with_addr.len(), 1, "{alias}: {with_addr:?}");
        let t = &with_addr[0];
        assert_eq!(t.id, format!("{CORP}:{bundle_id}"));
        assert_eq!(t.kind, "table");
        assert!(t.aliases.contains(&c_name.to_string()), "{:?}", t.aliases);
        assert!(has_edge(&db, "uses_table", "0x1000", bundle_id), "{alias}");
    }
    // No stray C-spelled stub entity survives.
    assert!(
        db.entity_list(CORP)
            .unwrap()
            .iter()
            .all(|e| !e.id.contains("cased_21") && !e.id.contains("ptr_lab_000043c6")),
    );
}

// ── Shared function names ────────────────────────────────────────────────────

/// Three functions all named `caseD_1`, one `caseD_2`, one `FUN_00005000`.
fn write_shared_name_bundle(root: &Path) {
    write(
        root,
        "manifest.json",
        &json!({"format":1,"kind":"binary-export","counts":{"functions":5}}).to_string(),
    );
    let funcs = [
        (
            "0x001000",
            "caseD_1",
            "void caseD_1(void)\n{\n  FUN_00005000();\n}\n",
        ),
        (
            "0x002000",
            "caseD_1",
            "void caseD_1(void)\n{\n  FUN_00005000();\n}\n",
        ),
        (
            "0x003000",
            "caseD_1",
            "void caseD_1(void)\n{\n  caseD_2();\n}\n",
        ),
        (
            "0x004000",
            "caseD_2",
            "void caseD_2(void)\n{\n  caseD_1();\n}\n",
        ),
        (
            "0x005000",
            "FUN_00005000",
            "void FUN_00005000(void)\n{\n  return;\n}\n",
        ),
    ];
    let mut rows = Vec::new();
    for (addr, name, c) in funcs {
        let hex = &addr[2..];
        let rel = format!("c/{hex}.c");
        write(root, &rel, c);
        rows.push(
            json!({"address":addr,"name":name,"size":8,"c":rel,"asm":null,"decompile_ok":true,"discovered_by":["analysis"]})
                .to_string(),
        );
    }
    write(root, "functions.jsonl", &rows.join("\n"));
}

async fn indexed_shared_names() -> (tempfile::TempDir, Arc<SqliteBackend>) {
    let dir = tempfile::tempdir().unwrap();
    write_shared_name_bundle(dir.path());
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    index(dir.path(), db.clone(), Arc::new(BinExportAdapter::new())).await;
    (dir, db)
}

fn edge_confidence(db: &SqliteBackend, kind: &str, from: &str, to: &str) -> f32 {
    db.edge_list(CORP)
        .unwrap()
        .iter()
        .find(|e| {
            e.kind == kind
                && e.from_entity_id == format!("{CORP}:{from}")
                && e.to_entity_id == format!("{CORP}:{to}")
        })
        .unwrap_or_else(|| panic!("missing {kind} edge {from} -> {to}"))
        .confidence
}

#[tokio::test]
async fn functions_sharing_a_name_stay_distinct_address_keyed_entities() {
    let (dir, db) = indexed_shared_names().await;
    let functions: Vec<_> = db
        .entity_list(CORP)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "function")
        .collect();
    let rows = std::fs::read_to_string(dir.path().join("functions.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(functions.len(), rows, "{functions:?}");

    for addr in ["0x1000", "0x2000", "0x3000"] {
        let e = entity(&db, addr);
        assert_eq!(e.kind, "function");
        assert_eq!(e.canonical_name, "caseD_1");
        assert!(e.aliases.contains(&addr.to_string()), "{:?}", e.aliases);
    }
    assert_eq!(entity(&db, "0x4000").canonical_name, "caseD_2");
    assert_eq!(entity(&db, "0x5000").canonical_name, "FUN_00005000");
}

#[tokio::test]
async fn each_same_named_function_keeps_its_own_edges() {
    let (_d, db) = indexed_shared_names().await;
    assert!(has_edge(&db, "calls", "0x1000", "0x5000"));
    assert!(has_edge(&db, "calls", "0x2000", "0x5000"));
    assert!(has_edge(&db, "calls", "0x3000", "0x4000"));
}

#[tokio::test]
async fn looking_up_a_shared_name_returns_every_function_with_it() {
    let (_d, db) = indexed_shared_names().await;
    let mut ids: Vec<String> = db
        .entity_find_by_name(CORP, "caseD_1")
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "function")
        .map(|e| e.id)
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        ["0x1000", "0x2000", "0x3000"].map(|a| format!("{CORP}:{a}"))
    );
}

#[tokio::test]
async fn fun_hex_call_target_resolves_to_that_functions_address_id() {
    let (_d, db) = indexed_shared_names().await;
    assert!(has_edge(&db, "calls", "0x1000", "0x5000"));
    assert!(
        db.edge_list(CORP)
            .unwrap()
            .iter()
            .all(|e| !e.to_entity_id.ends_with(":fun_00005000")),
        "name-keyed FUN_ target leaked"
    );
}

#[tokio::test]
async fn call_to_a_shared_name_reaches_every_candidate_at_half_confidence() {
    let (_d, db) = indexed_shared_names().await;
    for to in ["0x1000", "0x2000", "0x3000"] {
        assert!(has_edge(&db, "calls", "0x4000", to), "missing edge to {to}");
        assert_eq!(edge_confidence(&db, "calls", "0x4000", to), 0.5, "{to}");
    }
}

#[tokio::test]
async fn call_to_a_unique_name_resolves_to_its_address_at_normal_confidence() {
    let (_d, db) = indexed_shared_names().await;
    assert!(has_edge(&db, "calls", "0x3000", "0x4000"));
    assert_eq!(edge_confidence(&db, "calls", "0x3000", "0x4000"), 0.9);
    assert_eq!(edge_confidence(&db, "calls", "0x1000", "0x5000"), 0.9);
    assert!(!has_edge(&db, "calls", "0x3000", "caseD_2"));
}
