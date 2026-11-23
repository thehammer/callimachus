//! Behavioural tests for field-name annotation of C source: the adapter-level
//! `annotate_source` and the `read` tool's `annotate` flag.

use std::sync::Arc;

use callimachus_adapter_code::CodeAdapter;
use callimachus_adapter_contract::{AdapterRegistry, FieldName, SourceAdapter};
use callimachus_core::QueryService;
use callimachus_core::corrections::CorrectionsEngine;
use callimachus_core::corrections::fact_import::import;
use callimachus_core::query::{ReadDepth, ReadInput};
use callimachus_core::storage::{SqliteBackend, StorageBackend};
use callimachus_core::types::{Chunk, Corpus, Entity, Location, ToolResult};
use serde_json::json;

fn fname(base: &str, offset: i64, field: &str, ty: Option<&str>) -> FieldName {
    FieldName {
        base: base.into(),
        register: None,
        offset,
        layout: "obj".into(),
        field: field.into(),
        ty: ty.map(String::from),
    }
}

// ── annotate_source ──────────────────────────────────────────────────────────

#[test]
fn matching_derefs_are_rewritten_to_field_access_with_a_layout_comment() {
    let a = CodeAdapter::new();
    let out = a.annotate_source(
        "*(short *)(unaff_A2 + 0x10) = *(short *)(unaff_A2 + 0x10) + 1;",
        "c",
        &[fname("unaff_A2", 0x10, "x", Some("s16"))],
    );
    assert_eq!(
        out,
        "unaff_A2->x /* obj+0x10 s16 */ = unaff_A2->x /* obj+0x10 s16 */ + 1;"
    );
}

#[test]
fn surrounding_text_is_byte_identical() {
    let a = CodeAdapter::new();
    let src = "// header  comment\nvoid  FUN_a( void )\n{\t\n  int   y ;   /* keep */\n\n    *(short *)(unaff_A2 + 0x10)=3;\n  y = *(int *)(unaff_A2 + 0x20);  \n}\n";
    let out = a.annotate_source(src, "c", &[fname("unaff_A2", 0x10, "x", Some("s16"))]);
    let expected = src.replace(
        "*(short *)(unaff_A2 + 0x10)",
        "unaff_A2->x /* obj+0x10 s16 */",
    );
    assert_eq!(out, expected);
}

#[test]
fn unmatched_offsets_are_untouched() {
    let a = CodeAdapter::new();
    let src = "x = *(short *)(unaff_A2 + 0x12);\ny = *(short *)(unaff_A3 + 0x10);\n";
    let out = a.annotate_source(src, "c", &[fname("unaff_A2", 0x10, "x", Some("s16"))]);
    assert_eq!(out, src);
}

#[test]
fn missing_type_omits_it_from_the_comment() {
    let a = CodeAdapter::new();
    let out = a.annotate_source(
        "*(short *)(unaff_A2 + 0x10) = 1;",
        "c",
        &[fname("unaff_A2", 0x10, "x", None)],
    );
    assert_eq!(out, "unaff_A2->x /* obj+0x10 */ = 1;");
}

#[test]
fn negative_offsets_render_with_a_minus_sign() {
    let a = CodeAdapter::new();
    let out = a.annotate_source(
        "y = *(short *)(unaff_A2 - 0x4);",
        "c",
        &[fname("unaff_A2", -4, "f", Some("s16"))],
    );
    assert_eq!(out, "y = unaff_A2->f /* obj-0x4 s16 */;");
}

#[test]
fn other_languages_are_returned_unchanged() {
    let a = CodeAdapter::new();
    let src = "*(short *)(unaff_A2 + 0x10) = 1";
    let out = a.annotate_source(src, "python", &[fname("unaff_A2", 0x10, "x", Some("s16"))]);
    assert_eq!(out, src);
}

// ── read(annotate = true) ────────────────────────────────────────────────────

const CORP: &str = "game";
const SRC: &str = "void FUN_a(void)\n{\n  *(short *)(unaff_A2 + 0x10) = 1;\n}\n";

fn world() -> (QueryService, String) {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    db.corpus_insert(&Corpus::new(
        CORP.into(),
        CORP.into(),
        "code".into(),
        "/tmp/dummy".into(),
    ))
    .unwrap();

    let loc = Location::new(CORP, "src/FUN_a.c");
    let mut chunk = Chunk::new(
        CORP.into(),
        None,
        "function".into(),
        loc.clone(),
        SRC.into(),
    );
    chunk.language = Some("c".into());
    db.chunk_upsert(&chunk).unwrap();

    let mut func = Entity::new(
        format!("{CORP}:fun_a"),
        CORP.into(),
        "FUN_a".into(),
        "function".into(),
    );
    func.appearance_count = 1;
    func.first_location = Some(loc.clone());
    func.last_location = Some(loc.clone());
    db.entity_upsert(&func).unwrap();

    let mut fld = Entity::new(
        format!("{CORP}:unaff_a2+0x10"),
        CORP.into(),
        "unaff_A2+0x10".into(),
        "field".into(),
    );
    fld.appearance_count = 1;
    db.entity_upsert(&fld).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("l.json");
    std::fs::write(
        &p,
        json!({"format": 1, "facts": [{
            "id": "layout.obj", "kind": "layout", "topic": "other",
            "summary": "Active object record",
            "data": {"name": "obj", "bases": ["unaff_A2"],
                "functions": {"include": [], "exclude": []},
                "fields": [{"offset": "0x10", "name": "x", "type": "s16"}]},
            "confidence": "inferred", "evidence": ["ev.x"]
        }]})
        .to_string(),
    )
    .unwrap();
    let report = import(db.as_ref(), CORP, &[p], false).unwrap();
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);

    let engine = CorrectionsEngine::load(db.as_ref(), CORP).unwrap();
    let mut reg = AdapterRegistry::new();
    reg.register(Arc::new(CodeAdapter::new()));
    // The registry key is the adapter's own kind; the corpus kind is "code".
    assert!(reg.get("code").is_some(), "{:?}", reg.list());
    let svc = QueryService::with_corrections(db, engine).with_adapters(Arc::new(reg));
    (svc, loc.uri())
}

fn read_content(svc: &QueryService, uri: &str, annotate: Option<bool>) -> String {
    let mut v = json!({"corpus_id": CORP, "location": uri, "depth": "full"});
    if let Some(a) = annotate {
        v["annotate"] = json!(a);
    }
    let input: ReadInput = serde_json::from_value(v).unwrap();
    match svc.read(input) {
        ToolResult::Ok(s) => s.data.content.expect("content"),
        other => panic!("expected ok, got {other:?}"),
    }
}

#[test]
fn read_with_annotate_returns_field_names_in_the_source() {
    let (svc, uri) = world();
    let out = read_content(&svc, &uri, Some(true));
    assert!(out.contains("unaff_A2->x /* obj+0x10 s16 */ = 1;"), "{out}");
}

#[test]
fn read_without_annotate_returns_the_original_source() {
    let (svc, uri) = world();
    assert_eq!(read_content(&svc, &uri, Some(false)), SRC);
    assert_eq!(read_content(&svc, &uri, None), SRC);
}

#[test]
fn read_annotate_is_ignored_when_depth_is_not_full() {
    let (svc, uri) = world();
    let input = ReadInput {
        corpus_id: Some(CORP.into()),
        location: uri,
        depth: ReadDepth::Summary,
        annotate: true,
    };
    match svc.read(input) {
        ToolResult::Ok(s) => assert!(s.data.content.is_none()),
        other => panic!("expected ok, got {other:?}"),
    }
}
