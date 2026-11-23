//! Behavioural tests for `layout` facts describing a fixed-address array of
//! records (`data.array`): validation, anchoring slot-field entities, stride
//! checks, and the field-name set handed to source annotation. Fictional
//! addresses only.

use std::sync::Arc;

use callimachus_adapter_contract::{FieldArray, FieldName};
use callimachus_core::QueryService;
use callimachus_core::corrections::fact_import::{ImportReport, import};
use callimachus_core::corrections::layouts::field_names_for;
use callimachus_core::corrections::{CorrectionsEngine, FactRecord};
use callimachus_core::query::EntityInput;
use callimachus_core::storage::{SqliteBackend, StorageBackend};
use callimachus_core::types::{Corpus, Entity, ToolResult};
use serde_json::{Value, json};
use tempfile::TempDir;

const C: &str = "game";

fn db_with(mask: Option<&str>) -> Arc<SqliteBackend> {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    db.corpus_insert(&Corpus::new(
        C.into(),
        C.into(),
        "code".into(),
        "/tmp/dummy".into(),
    ))
    .unwrap();
    if let Some(m) = mask {
        db.corpus_set_config(C, &json!({"address_mask": m}))
            .unwrap();
    }
    db
}

/// A slot field entity as the C adapter emits it.
fn add_slot(db: &dyn StorageBackend, name: &str, stride: Option<&str>) -> String {
    let id = format!("{C}:{}", name.to_lowercase());
    let mut e = Entity::new(id.clone(), C.into(), name.into(), "field".into());
    e.appearance_count = 1;
    e.aliases.push("width:2".into());
    if let Some(s) = stride {
        e.aliases.push(format!("stride:{s}"));
    }
    db.entity_upsert(&e).unwrap();
    id
}

fn enemy(patch: impl FnOnce(&mut Value)) -> Value {
    let mut data = json!({
        "name": "enemy",
        "array": {"address": "0xFF8A00", "stride": "0x40", "count": 40},
        "fields": [
            {"offset": "0x1c", "name": "h", "type": "s16", "summary": "hit points",
             "confidence": "confirmed"},
            {"offset": "0x0", "name": "state", "type": "u8"}
        ]
    });
    patch(&mut data);
    json!({
        "id": "layout.enemy", "kind": "layout", "topic": "other",
        "summary": "Enemy slots", "data": data,
        "confidence": "inferred", "evidence": ["ev.x"]
    })
}

fn run_import(db: &Arc<SqliteBackend>, facts: Vec<Value>) -> ImportReport {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("l.json");
    std::fs::write(&p, json!({"format": 1, "facts": facts}).to_string()).unwrap();
    import(db.as_ref(), C, &[p], false).unwrap()
}

fn engine(db: &Arc<SqliteBackend>) -> CorrectionsEngine {
    CorrectionsEngine::load(db.as_ref(), C).unwrap()
}

fn anchors(db: &Arc<SqliteBackend>) -> Vec<String> {
    engine(db).anchors_for_fact(C, "layout.enemy")
}

fn service(db: &Arc<SqliteBackend>) -> QueryService {
    QueryService::with_corrections(db.clone(), engine(db))
}

fn find<'a>(v: &'a [FieldName], field: &str) -> &'a FieldName {
    v.iter()
        .find(|f| f.field == field)
        .unwrap_or_else(|| panic!("no field {field} in {v:?}"))
}

// ── anchoring ────────────────────────────────────────────────────────────────

#[test]
fn array_layout_anchors_the_matching_slot_field_and_names_it_enemy_h() {
    let db = db_with(Some("0xffffff"));
    let id = add_slot(db.as_ref(), "slot_ff8a00+0x1c", Some("0x40"));
    let r = run_import(&db, vec![enemy(|_| {})]);
    assert!(r.rejected.is_empty(), "{:?}", r.rejected);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(anchors(&db), vec![id.clone()]);

    let out = match service(&db).entity(EntityInput {
        corpus_id: C.into(),
        name_or_id: "enemy.h".into(),
        kind: None,
    }) {
        ToolResult::Ok(s) => s.data,
        other => panic!("expected ok, got {other:?}"),
    };
    assert_eq!(out.id, id);
}

#[test]
fn anchored_slot_field_carries_the_synthetic_field_fact() {
    let db = db_with(Some("0xffffff"));
    let id = add_slot(db.as_ref(), "slot_ff8a00+0x1c", Some("0x40"));
    run_import(&db, vec![enemy(|_| {})]);
    let e = engine(&db);
    let facts = e.facts_for_entity(&id);
    let (f, _) = facts
        .iter()
        .find(|(f, _)| f.id == "layout.enemy.h")
        .unwrap_or_else(|| {
            panic!(
                "no synthetic fact: {:?}",
                facts.iter().map(|(f, _)| &f.id).collect::<Vec<_>>()
            )
        });
    assert_eq!(f.kind, "field");
    assert_eq!(f.summary, "hit points");
    assert_eq!(f.confidence, "confirmed");
}

#[test]
fn slots_with_other_offsets_or_other_addresses_are_not_anchored() {
    let db = db_with(Some("0xffffff"));
    let hit = add_slot(db.as_ref(), "slot_ff8a00+0x1c", Some("0x40"));
    add_slot(db.as_ref(), "slot_ff8a00+0x1e", Some("0x40"));
    add_slot(db.as_ref(), "slot_ff9000+0x1c", Some("0x40"));
    run_import(&db, vec![enemy(|_| {})]);
    assert_eq!(anchors(&db), vec![hit]);
}

#[test]
fn every_declared_field_anchors_its_own_slot_entity() {
    let db = db_with(Some("0xffffff"));
    let h = add_slot(db.as_ref(), "slot_ff8a00+0x1c", Some("0x40"));
    let s = add_slot(db.as_ref(), "slot_ff8a00+0x0", Some("0x40"));
    run_import(&db, vec![enemy(|_| {})]);
    let mut got = anchors(&db);
    got.sort();
    let mut want = vec![h, s];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn a_slot_entity_without_a_stride_alias_still_anchors() {
    let db = db_with(Some("0xffffff"));
    let id = add_slot(db.as_ref(), "slot_ff8a00+0x1c", None);
    let r = run_import(&db, vec![enemy(|_| {})]);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(anchors(&db), vec![id]);
}

#[test]
fn a_mirror_spelling_anchors_through_the_corpus_address_mask() {
    let db = db_with(Some("0xFFFFFF"));
    let id = add_slot(db.as_ref(), "slot_ffff8a00+0x1c", Some("0x40"));
    run_import(&db, vec![enemy(|_| {})]);
    assert_eq!(anchors(&db), vec![id.clone()]);

    // Without a mask the two spellings are different addresses.
    let db = db_with(None);
    add_slot(db.as_ref(), "slot_ffff8a00+0x1c", Some("0x40"));
    run_import(&db, vec![enemy(|_| {})]);
    assert!(anchors(&db).is_empty());
}

#[test]
fn a_mirror_layout_address_anchors_the_plain_slot_through_the_mask() {
    let db = db_with(Some("0xFFFFFF"));
    let id = add_slot(db.as_ref(), "slot_ff8a00+0x1c", Some("0x40"));
    run_import(
        &db,
        vec![enemy(|d| d["array"]["address"] = json!("0xFFFF8A00"))],
    );
    assert_eq!(anchors(&db), vec![id]);
}

#[test]
fn a_stride_mismatch_is_not_anchored_and_warns_naming_the_layout() {
    let db = db_with(Some("0xffffff"));
    add_slot(db.as_ref(), "slot_ff8a00+0x1c", Some("0x40"));
    let r = run_import(&db, vec![enemy(|d| d["array"]["stride"] = json!("0x20"))]);
    assert!(r.rejected.is_empty(), "{:?}", r.rejected);
    assert!(anchors(&db).is_empty());
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("stride") && w.contains("layout.enemy")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn array_layouts_do_not_need_functions_to_anchor() {
    let db = db_with(Some("0xffffff"));
    let id = add_slot(db.as_ref(), "slot_ff8a00+0x1c", Some("0x40"));
    let r = run_import(&db, vec![enemy(|_| {})]);
    assert!(
        !r.warnings.iter().any(|w| w.contains("per-function")),
        "{:?}",
        r.warnings
    );
    assert_eq!(anchors(&db), vec![id]);
}

// ── field_names_for ──────────────────────────────────────────────────────────

#[test]
fn field_names_for_an_array_layout_carry_the_slot_base_and_array_spec() {
    let db = db_with(Some("0xffffff"));
    let r = run_import(&db, vec![enemy(|_| {})]);
    assert!(r.rejected.is_empty(), "{:?}", r.rejected);
    let names = field_names_for(&engine(&db), "FUN_a");
    assert_eq!(names.len(), 2, "{names:?}");
    let h = find(&names, "h");
    assert_eq!(h.base, "slot_ff8a00");
    assert_eq!(h.layout, "enemy");
    assert_eq!(h.offset, 0x1c);
    assert_eq!(h.ty.as_deref(), Some("s16"));
    assert_eq!(
        h.array,
        Some(FieldArray {
            address: 0xff8a00,
            stride: 0x40,
            mask: Some(0xffffff)
        })
    );
    let s = find(&names, "state");
    assert_eq!(s.base, "slot_ff8a00");
    assert_eq!(s.offset, 0);
}

#[test]
fn array_address_is_masked_and_mask_is_absent_without_corpus_mask() {
    let db = db_with(Some("0xffffff"));
    run_import(
        &db,
        vec![enemy(|d| d["array"]["address"] = json!("0xFFFF8A00"))],
    );
    let names = field_names_for(&engine(&db), "FUN_a");
    let h = find(&names, "h");
    assert_eq!(h.base, "slot_ff8a00");
    assert_eq!(h.array.as_ref().unwrap().address, 0xff8a00);

    let db = db_with(None);
    run_import(&db, vec![enemy(|_| {})]);
    let names = field_names_for(&engine(&db), "FUN_a");
    assert_eq!(find(&names, "h").array.as_ref().unwrap().mask, None);
}

#[test]
fn register_base_layouts_have_no_array_spec() {
    let db = db_with(None);
    let r = run_import(
        &db,
        vec![json!({
            "id": "layout.obj", "kind": "layout", "topic": "other",
            "summary": "obj",
            "data": {"name": "obj", "bases": ["unaff_A2"],
                     "functions": ["FUN_a"],
                     "fields": [{"offset": "0x10", "name": "x"}]},
            "confidence": "inferred", "evidence": ["ev.x"]
        })],
    );
    assert!(r.rejected.is_empty(), "{:?}", r.rejected);
    let names = field_names_for(&engine(&db), "FUN_a");
    assert!(find(&names, "x").array.is_none());
}

// ── validation ───────────────────────────────────────────────────────────────

#[test]
fn layout_without_bases_but_with_an_array_is_accepted() {
    let db = db_with(None);
    let r = run_import(
        &db,
        vec![enemy(|d| {
            assert!(d.get("bases").is_none());
        })],
    );
    assert!(r.rejected.is_empty(), "{:?}", r.rejected);
    assert!(FactRecord::from_json(&enemy(|_| {})).is_ok());
}

#[test]
fn count_is_optional() {
    let db = db_with(None);
    let r = run_import(
        &db,
        vec![enemy(|d| {
            d["array"].as_object_mut().unwrap().remove("count");
        })],
    );
    assert!(r.rejected.is_empty(), "{:?}", r.rejected);
}

fn assert_rejected_any(patch: impl FnOnce(&mut Value), needles: &[&str]) {
    let db = db_with(None);
    let r = run_import(&db, vec![enemy(patch)]);
    assert_eq!(r.rejected.len(), 1, "{:?}", r.rejected);
    assert!(r.rejected[0].contains("layout.enemy"), "{:?}", r.rejected);
    let msg = r.rejected[0].to_lowercase();
    assert!(
        needles.iter().any(|n| msg.contains(n)),
        "expected one of {needles:?} in {msg:?}"
    );
}

#[test]
fn a_non_hex_array_address_is_rejected() {
    for bad in ["ff8a00", "0xZZ", "65418752", ""] {
        assert_rejected_any(|d| d["array"]["address"] = json!(bad), &["address"]);
    }
    assert_rejected_any(|d| d["array"]["address"] = json!(0xff8a00), &["address"]);
}

#[test]
fn a_zero_or_non_hex_stride_is_rejected() {
    assert_rejected_any(|d| d["array"]["stride"] = json!("0x0"), &["stride"]);
    assert_rejected_any(|d| d["array"]["stride"] = json!("64"), &["stride"]);
    assert_rejected_any(|d| d["array"]["stride"] = json!(64), &["stride"]);
    assert_rejected_any(
        |d| {
            d["array"].as_object_mut().unwrap().remove("stride");
        },
        &["stride"],
    );
}

#[test]
fn a_non_integer_count_is_rejected() {
    assert_rejected_any(|d| d["array"]["count"] = json!("forty"), &["count"]);
    assert_rejected_any(|d| d["array"]["count"] = json!(40.5), &["count"]);
    assert_rejected_any(|d| d["array"]["count"] = json!(-1), &["count"]);
}

#[test]
fn an_array_that_is_not_an_object_is_rejected() {
    assert_rejected_any(|d| d["array"] = json!("0xff8a00"), &["array"]);
}

#[test]
fn a_layout_with_neither_bases_nor_array_is_rejected() {
    assert_rejected_any(
        |d| {
            d.as_object_mut().unwrap().remove("array");
        },
        &["base", "array"],
    );
    let bad = enemy(|d| {
        d.as_object_mut().unwrap().remove("array");
    });
    assert!(FactRecord::from_json(&bad).is_err());
}

#[test]
fn array_layouts_still_validate_fields() {
    assert_rejected_any(|d| d["fields"] = json!([]), &["field"]);
    assert_rejected_any(
        |d| d["fields"] = json!([{"offset": "28", "name": "h"}]),
        &["offset"],
    );
}
