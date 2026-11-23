//! Behavioural tests for `layout` facts: validation, resolving `layout.field`
//! names to field entities, synthetic per-field facts, and the field-name set
//! handed to source annotation. Neutral names only.

use std::sync::Arc;

use callimachus_core::QueryService;
use callimachus_core::corrections::fact_import::{ImportReport, import};
use callimachus_core::corrections::layouts::field_names_for;
use callimachus_core::corrections::{CorrectionsEngine, FactRecord};
use callimachus_core::query::{EntityEdgesInput, EntityInput};
use callimachus_core::storage::{SqliteBackend, StorageBackend};
use callimachus_core::types::{Corpus, Edge, Entity, Location, ToolResult};
use serde_json::{Value, json};
use tempfile::TempDir;

const C: &str = "game";

fn add_entity(db: &dyn StorageBackend, name: &str, kind: &str) -> String {
    let id = format!("{C}:{}", name.to_lowercase());
    let mut e = Entity::new(id.clone(), C.into(), name.into(), kind.into());
    e.appearance_count = 1;
    db.entity_upsert(&e).unwrap();
    id
}

fn add_edge(db: &dyn StorageBackend, id: &str, from: &str, to: &str, kind: &str) {
    let loc = Location::new(C, "src/a.c");
    db.edge_upsert(&Edge::new(
        id.into(),
        C.into(),
        from.into(),
        to.into(),
        kind.into(),
        loc,
    ))
    .unwrap();
}

fn world() -> Arc<SqliteBackend> {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    db.corpus_insert(&Corpus::new(
        C.into(),
        C.into(),
        "code".into(),
        "/tmp/dummy".into(),
    ))
    .unwrap();
    add_entity(db.as_ref(), "FUN_a", "function");
    add_entity(db.as_ref(), "FUN_b", "function");
    add_entity(db.as_ref(), "unaff_A2+0x10", "field");
    add_entity(db.as_ref(), "unaff_A2+0x12", "field");
    add_edge(
        db.as_ref(),
        "e1",
        "game:fun_a",
        "game:unaff_a2+0x10",
        "reads",
    );
    add_edge(
        db.as_ref(),
        "e2",
        "game:fun_b",
        "game:unaff_a2+0x10",
        "writes",
    );
    db
}

fn layout_json(data_patch: impl FnOnce(&mut Value)) -> Value {
    let mut data = json!({
        "name": "obj",
        "bases": ["unaff_A2", "in_A2"],
        "functions": {"include": [], "exclude": []},
        "fields": [
            {"offset": "0x10", "name": "x", "type": "s16",
             "summary": "x position", "confidence": "confirmed"},
            {"offset": "0x12", "name": "y", "type": "s16"}
        ]
    });
    data_patch(&mut data);
    json!({
        "id": "layout.obj", "kind": "layout", "topic": "other",
        "summary": "Active object record", "data": data,
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

fn imported(patch: impl FnOnce(&mut Value)) -> (Arc<SqliteBackend>, CorrectionsEngine) {
    let db = world();
    let r = run_import(&db, vec![layout_json(patch)]);
    assert!(r.rejected.is_empty(), "{:?}", r.rejected);
    let e = engine(&db);
    (db, e)
}

fn service(db: &Arc<SqliteBackend>) -> QueryService {
    QueryService::with_corrections(db.clone(), engine(db))
}

fn ok<T: std::fmt::Debug>(r: ToolResult<T>) -> T {
    match r {
        ToolResult::Ok(s) => s.data,
        other => panic!("expected ok result, got {other:?}"),
    }
}

fn assert_rejected(patch: impl FnOnce(&mut Value), needle: &str) {
    let db = world();
    let r = run_import(&db, vec![layout_json(patch)]);
    assert_eq!(r.rejected.len(), 1, "{:?}", r.rejected);
    assert!(r.rejected[0].contains("layout.obj"), "{:?}", r.rejected);
    assert!(
        r.rejected[0].to_lowercase().contains(needle),
        "expected {needle:?} in {:?}",
        r.rejected
    );
}

// ── resolution and synthetic facts ───────────────────────────────────────────

#[test]
fn layout_dot_field_name_resolves_to_the_field_entity() {
    let (db, _e) = imported(|_| {});
    let out = ok(service(&db).entity(EntityInput {
        corpus_id: C.into(),
        name_or_id: "obj.x".into(),
        kind: None,
    }));
    assert_eq!(out.id, "game:unaff_a2+0x10");
}

#[test]
fn field_entity_carries_a_synthetic_fact_with_the_field_summary_and_confidence() {
    let (_db, e) = imported(|_| {});
    let facts = e.facts_for_entity("game:unaff_a2+0x10");
    let (f, _anchors) = facts
        .iter()
        .find(|(f, _)| f.id == "layout.obj.x")
        .unwrap_or_else(|| {
            panic!(
                "no synthetic fact: {:?}",
                facts.iter().map(|(f, _)| &f.id).collect::<Vec<_>>()
            )
        });
    assert_eq!(f.kind, "field");
    assert_eq!(f.summary, "x position");
    assert_eq!(f.confidence, "confirmed");
}

#[test]
fn field_without_summary_or_confidence_falls_back_to_name_and_layout_confidence() {
    let (_db, e) = imported(|_| {});
    let facts = e.facts_for_entity("game:unaff_a2+0x12");
    let (f, _) = facts
        .iter()
        .find(|(f, _)| f.id == "layout.obj.y")
        .expect("synthetic fact for y");
    assert_eq!(f.kind, "field");
    assert_eq!(f.summary, "obj.y");
    assert_eq!(f.confidence, "inferred");
}

#[test]
fn entity_edges_of_a_named_field_list_readers_and_writers() {
    let (db, _e) = imported(|_| {});
    let out = ok(service(&db).entity_edges(EntityEdgesInput {
        corpus_id: C.into(),
        entity_id: "obj.x".into(),
        direction: "both".into(),
        kind: None,
        limit: None,
    }));
    assert!(
        out.edges
            .iter()
            .any(|e| e.kind == "reads" && e.from_entity_id == "game:fun_a"),
        "{:?}",
        out.edges
    );
    assert!(
        out.edges
            .iter()
            .any(|e| e.kind == "writes" && e.from_entity_id == "game:fun_b"),
        "{:?}",
        out.edges
    );
}

// ── field_names_for ──────────────────────────────────────────────────────────

fn find<'a>(
    v: &'a [callimachus_adapter_contract::FieldName],
    base: &str,
    field: &str,
) -> &'a callimachus_adapter_contract::FieldName {
    v.iter()
        .find(|f| f.base == base && f.field == field)
        .unwrap_or_else(|| panic!("no {base}/{field} in {v:?}"))
}

#[test]
fn field_names_cover_every_field_and_base_with_derived_registers() {
    let (_db, e) = imported(|_| {});
    let names = field_names_for(&e, "FUN_a");
    assert_eq!(names.len(), 4, "{names:?}");
    let x = find(&names, "unaff_A2", "x");
    assert_eq!(x.layout, "obj");
    assert_eq!(x.offset, 0x10);
    assert_eq!(x.ty.as_deref(), Some("s16"));
    assert_eq!(x.register.as_deref(), Some("a2"));
    assert_eq!(find(&names, "in_A2", "y").offset, 0x12);
    assert_eq!(find(&names, "in_A2", "y").register.as_deref(), Some("a2"));
}

#[test]
fn explicit_registers_map_overrides_base_derivation() {
    let (_db, e) = imported(|d| {
        d["bases"] = json!(["param_1"]);
        d["registers"] = json!({"param_1": "a0"});
    });
    let names = field_names_for(&e, "FUN_a");
    assert_eq!(find(&names, "param_1", "x").register.as_deref(), Some("a0"));
}

#[test]
fn excluded_functions_get_no_field_names() {
    let (_db, e) = imported(|d| d["functions"]["exclude"] = json!(["FUN_b"]));
    assert!(field_names_for(&e, "FUN_b").is_empty());
    assert!(!field_names_for(&e, "FUN_a").is_empty());
}

#[test]
fn non_empty_include_restricts_the_layout_to_those_functions() {
    let (_db, e) = imported(|d| d["functions"]["include"] = json!(["FUN_a"]));
    assert!(!field_names_for(&e, "FUN_a").is_empty());
    assert!(field_names_for(&e, "FUN_b").is_empty());
}

// ── validation ───────────────────────────────────────────────────────────────

#[test]
fn duplicate_field_names_are_rejected() {
    assert_rejected(
        |d| {
            d["fields"] = json!([
                {"offset": "0x10", "name": "x"},
                {"offset": "0x12", "name": "x"}
            ])
        },
        "x",
    );
}

#[test]
fn invalid_layout_name_is_rejected() {
    assert_rejected(|d| d["name"] = json!("Bad Name"), "name");
    assert_rejected(|d| d["name"] = json!("1abc"), "name");
}

#[test]
fn non_hex_offsets_are_rejected() {
    assert_rejected(
        |d| d["fields"] = json!([{"offset": "16", "name": "x"}]),
        "offset",
    );
    assert_rejected(
        |d| d["fields"] = json!([{"offset": "0xZZ", "name": "x"}]),
        "offset",
    );
}

#[test]
fn negative_hex_offsets_are_accepted() {
    let (_db, e) = imported(|d| d["fields"] = json!([{"offset": "-0x4", "name": "f"}]));
    let names = field_names_for(&e, "FUN_a");
    assert_eq!(find(&names, "unaff_A2", "f").offset, -4);
}

#[test]
fn empty_bases_are_rejected() {
    assert_rejected(|d| d["bases"] = json!([]), "base");
}

#[test]
fn empty_or_missing_fields_are_rejected() {
    assert_rejected(|d| d["fields"] = json!([]), "field");
    assert_rejected(|d| d["fields"] = json!("nope"), "field");
}

#[test]
fn from_json_rejects_malformed_layouts() {
    let mut bad = layout_json(|d| d["fields"] = json!([]));
    assert!(FactRecord::from_json(&bad).is_err());
    bad = layout_json(|d| d["bases"] = json!([]));
    assert!(FactRecord::from_json(&bad).is_err());
    bad = layout_json(|d| d["name"] = json!("Obj"));
    assert!(FactRecord::from_json(&bad).is_err());
    assert!(FactRecord::from_json(&layout_json(|_| {})).is_ok());
}
