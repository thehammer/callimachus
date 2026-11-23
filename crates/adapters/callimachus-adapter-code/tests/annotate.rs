//! Behavioural tests for field-name annotation of C source: the adapter-level
//! `annotate_source` and the `read` tool's `annotate` flag.

use std::sync::Arc;

use callimachus_adapter_code::CodeAdapter;
use callimachus_adapter_contract::{AdapterRegistry, FieldArray, FieldName, SourceAdapter};
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
        array: None,
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

// ── array layouts (S5c) ──────────────────────────────────────────────────────

fn enemy_h(ty: Option<&str>, stride: u64, mask: Option<u64>) -> FieldName {
    FieldName {
        base: "slot_ff8a00".into(),
        register: None,
        offset: 0x1c,
        layout: "enemy".into(),
        field: "h".into(),
        ty: ty.map(String::from),
        array: Some(FieldArray {
            address: 0xff8a00,
            stride,
            mask,
        }),
    }
}

const WALK: &str = "void FUN_a(int i)\n{\n  byte *p;\n  p = &RAM_ff8a00;\n  do {\n    *(short *)(p + 0x1c) = 1;\n    p = p + 0x40;\n  } while (i-- != 0);\n}\n";

#[test]
fn walking_pointer_access_is_annotated_as_an_array_field() {
    let a = CodeAdapter::new();
    let out = a.annotate_source(WALK, "c", &[enemy_h(Some("s16"), 0x40, None)]);
    assert_eq!(
        out,
        WALK.replace("*(short *)(p + 0x1c)", "p->h /* enemy[].h +0x1c s16 */")
    );
}

#[test]
fn array_annotation_omits_a_missing_type() {
    let a = CodeAdapter::new();
    let out = a.annotate_source(WALK, "c", &[enemy_h(None, 0x40, None)]);
    assert!(out.contains("p->h /* enemy[].h +0x1c */ = 1;"), "{out}");
}

#[test]
fn a_walk_with_a_different_stride_than_the_layout_is_not_annotated() {
    let a = CodeAdapter::new();
    let out = a.annotate_source(WALK, "c", &[enemy_h(Some("s16"), 0x20, None)]);
    assert_eq!(out, WALK);
}

#[test]
fn a_pointer_with_other_assignments_is_not_annotated() {
    let a = CodeAdapter::new();
    let src = "  p = &RAM_ff8a00;\n  p = other;\n  i = *(short *)(p + 0x1c);\n";
    let out = a.annotate_source(src, "c", &[enemy_h(Some("s16"), 0x40, None)]);
    assert_eq!(out, src);
}

#[test]
fn other_offsets_and_other_array_addresses_are_untouched() {
    let a = CodeAdapter::new();
    let src = "  p = &RAM_ff8a00;\n  i = *(short *)(p + 0x1e);\n  q = &RAM_ff9000;\n  i = *(short *)(q + 0x1c);\n";
    let out = a.annotate_source(src, "c", &[enemy_h(Some("s16"), 0x40, None)]);
    assert_eq!(out, src);
}

#[test]
fn indexed_access_is_annotated_as_enemy_index_field() {
    let a = CodeAdapter::new();
    let out = a.annotate_source(
        "x = *(short *)(&RAM_ff8a00 + i * 0x40 + 0x1c);\n",
        "c",
        &[enemy_h(Some("s16"), 0x40, None)],
    );
    assert_eq!(out, "x = enemy[i].h;\n");
}

#[test]
fn constant_slot_access_is_annotated_with_the_slot_number() {
    let a = CodeAdapter::new();
    let out = a.annotate_source(
        "x = *(short *)(&RAM_ff8a00 + 3 * 0x40 + 0x1c);\n",
        "c",
        &[enemy_h(Some("s16"), 0x40, None)],
    );
    assert_eq!(out, "x = enemy[3].h;\n");
}

#[test]
fn indexed_access_with_a_different_stride_is_not_annotated() {
    let a = CodeAdapter::new();
    let src = "x = *(short *)(&RAM_ff8a00 + i * 0x40 + 0x1c);\n";
    let out = a.annotate_source(src, "c", &[enemy_h(Some("s16"), 0x20, None)]);
    assert_eq!(out, src);
}

#[test]
fn a_mirror_spelling_of_the_array_address_is_annotated_when_masked() {
    let a = CodeAdapter::new();
    let src = "void FUN_a(void)\n{\n  p = (byte *)0xffff8a00;\n  i = *(short *)(p + 0x1c);\n  p = p + 0x40;\n}\n";
    let masked = a.annotate_source(src, "c", &[enemy_h(Some("s16"), 0x40, Some(0xffffff))]);
    assert!(
        masked.contains("i = p->h /* enemy[].h +0x1c s16 */;"),
        "{masked}"
    );
    let unmasked = a.annotate_source(src, "c", &[enemy_h(Some("s16"), 0x40, None)]);
    assert_eq!(unmasked, src);
}

#[test]
fn array_fields_do_not_disturb_register_base_annotation() {
    let a = CodeAdapter::new();
    let out = a.annotate_source(
        "void FUN_a(void)\n{\n  *(short *)(unaff_A2 + 0x10) = 1;\n  p = &RAM_ff8a00;\n  i = *(short *)(p + 0x1c);\n  p = p + 0x40;\n}\n",
        "c",
        &[
            fname("unaff_A2", 0x10, "x", Some("s16")),
            enemy_h(Some("s16"), 0x40, None),
        ],
    );
    assert!(out.contains("unaff_A2->x /* obj+0x10 s16 */ = 1;"), "{out}");
    assert!(out.contains("i = p->h /* enemy[].h +0x1c s16 */;"), "{out}");
}

// ── read(annotate = true) over an array layout ───────────────────────────────

const ARRAY_SRC: &str = "void FUN_a(int i)\n{\n  byte *p;\n  p = &RAM_ff8a00;\n  do {\n    i = *(short *)(p + 0x1c);\n    p = p + 0x40;\n  } while (i != 0);\n}\n";

fn array_world(stride: &str) -> (QueryService, String) {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    db.corpus_insert(&Corpus::new(
        CORP.into(),
        CORP.into(),
        "code".into(),
        "/tmp/dummy".into(),
    ))
    .unwrap();
    db.corpus_set_config(CORP, &json!({"address_mask": "0xffffff"}))
        .unwrap();
    let loc = Location::new(CORP, "src/FUN_a.c");
    let mut chunk = Chunk::new(
        CORP.into(),
        None,
        "function".into(),
        loc.clone(),
        ARRAY_SRC.into(),
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
        format!("{CORP}:slot_ff8a00+0x1c"),
        CORP.into(),
        "slot_ff8a00+0x1c".into(),
        "field".into(),
    );
    fld.appearance_count = 1;
    fld.aliases = vec!["width:2".into(), "stride:0x40".into()];
    db.entity_upsert(&fld).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("l.json");
    std::fs::write(
        &p,
        json!({"format": 1, "facts": [{
            "id": "layout.enemy", "kind": "layout", "topic": "other",
            "summary": "Enemy slots",
            "data": {"name": "enemy",
                "array": {"address": "0xFF8A00", "stride": stride, "count": 40},
                "fields": [{"offset": "0x1c", "name": "h", "type": "s16"}]},
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
    let svc = QueryService::with_corrections(db, engine).with_adapters(Arc::new(reg));
    (svc, loc.uri())
}

#[test]
fn read_with_annotate_names_array_fields_in_the_source() {
    let (svc, uri) = array_world("0x40");
    let out = read_content(&svc, &uri, Some(true));
    assert!(out.contains("i = p->h /* enemy[].h +0x1c s16 */;"), "{out}");
    assert_eq!(read_content(&svc, &uri, None), ARRAY_SRC);
}

#[test]
fn read_with_annotate_leaves_a_stride_mismatch_unannotated() {
    let (svc, uri) = array_world("0x20");
    assert_eq!(read_content(&svc, &uri, Some(true)), ARRAY_SRC);
}
