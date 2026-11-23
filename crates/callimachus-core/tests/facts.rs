//! Behavioural tests for fact import, anchoring, address linking and the
//! fact-aware query surface. Fictional platform `toy16`, fictional addresses.
//!
//! Game entities are built by hand exactly as the C adapter emits them for
//! decompiler-style names: id `{corpus}:{lowercased name}`, kind `function` or
//! `variable`, canonical alias `0x{lowercase hex, no padding}`. (The code
//! adapter dev-depends on core, so core cannot depend on it in return.)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use callimachus_core::corrections::fact_import::{ImportReport, import};
use callimachus_core::corrections::link_addresses::link_addresses;
use callimachus_core::corrections::{CorrectionKind, CorrectionsEngine};
use callimachus_core::query::{CorpusOverviewInput, EntityInput, ListFactsInput};
use callimachus_core::storage::{SqliteBackend, StorageBackend};
use callimachus_core::types::{CollectionKind, Corpus, Entity, MemberType, ToolResult};
use callimachus_core::{Collection, QueryService};
use serde_json::{Value, json};
use tempfile::TempDir;

const GAME: &str = "game";
const REF: &str = "ref";
const COLL: &str = "toy16";

// ── helpers ──────────────────────────────────────────────────────────────────

fn config(role: Option<&str>, mask: Option<&str>) -> Value {
    let mut c = json!({ "cpu_spaces": ["rom", "wram", "io"] });
    if let Some(r) = role {
        c["role"] = json!(r);
    }
    if let Some(m) = mask {
        c["address_mask"] = json!(m);
    }
    c
}

fn add_corpus(db: &dyn StorageBackend, id: &str, cfg: Value) {
    db.corpus_insert(&Corpus::new(
        id.to_string(),
        id.to_string(),
        "code".to_string(),
        "/tmp/dummy".to_string(),
    ))
    .unwrap();
    db.corpus_set_config(id, &cfg).unwrap();
}

fn add_entity(db: &dyn StorageBackend, corpus: &str, name: &str, kind: &str, addr: u64) {
    let id = format!("{corpus}:{}", name.to_lowercase());
    let mut e = Entity::new(id, corpus.to_string(), name.to_string(), kind.to_string());
    e.aliases.push(format!("0x{addr:x}"));
    db.entity_upsert(&e).unwrap();
}

/// Game corpus with the three decompiler-style C files' entities.
fn game_db(mask: Option<&str>) -> Arc<SqliteBackend> {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    add_corpus(db.as_ref(), GAME, config(None, mask));
    add_entity(db.as_ref(), GAME, "FUN_00001000", "function", 0x1000);
    add_entity(db.as_ref(), GAME, "RAM_ff0010", "variable", 0xff0010);
    add_entity(db.as_ref(), GAME, "_DAT_00c00004", "variable", 0xc00004);
    add_entity(db.as_ref(), GAME, "FUN_00002000", "function", 0x2000);
    add_entity(db.as_ref(), GAME, "DAT_00012000", "variable", 0x12000);
    add_entity(db.as_ref(), GAME, "FUN_00003000", "function", 0x3000);
    add_entity(db.as_ref(), GAME, "DAT_00a10001", "variable", 0xa10001);
    db
}

fn fact(id: &str, kind: &str, summary: &str, data: Value) -> Value {
    json!({
        "id": id, "kind": kind, "topic": "memory", "summary": summary,
        "data": data, "confidence": "confirmed", "evidence": ["test"],
    })
}

fn var_fact(id: &str, summary: &str, addr: &str, space: &str) -> Value {
    fact(
        id,
        "variable",
        summary,
        json!({"address": addr, "space": space}),
    )
}

fn write_facts(dir: &Path, name: &str, facts: Vec<Value>) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, json!({"format": 1, "facts": facts}).to_string()).unwrap();
    p
}

fn game_facts() -> Vec<Value> {
    vec![
        var_fact("var.counter", "Frame counter", "0xFF0010", "wram"),
        fact(
            "table.spawn",
            "table",
            "Spawn table",
            json!({"address": "0x12000", "space": "rom"}),
        ),
    ]
}

fn query(db: &Arc<SqliteBackend>, corpus: &str) -> QueryService {
    let engine = CorrectionsEngine::load(db.as_ref(), corpus).unwrap();
    QueryService::with_corrections(db.clone(), engine)
}

fn ok<T: std::fmt::Debug>(r: ToolResult<T>) -> T {
    match r {
        ToolResult::Ok(s) => s.data,
        other => panic!("expected ok result, got {other:?}"),
    }
}

fn entity_of(
    db: &Arc<SqliteBackend>,
    corpus: &str,
    name: &str,
) -> callimachus_core::query::EntityOutput {
    ok(query(db, corpus).entity(EntityInput {
        corpus_id: corpus.into(),
        name_or_id: name.into(),
        kind: None,
    }))
}

fn anchors_of(db: &Arc<SqliteBackend>, corpus: &str, fact_id: &str) -> Vec<String> {
    let engine = CorrectionsEngine::load(db.as_ref(), corpus).unwrap();
    engine
        .facts_in_corpus(corpus)
        .into_iter()
        .find(|(f, _)| f.id == fact_id)
        .map(|(_, a)| a.to_vec())
        .unwrap_or_else(|| panic!("fact {fact_id} not found"))
}

fn fact_ids(db: &Arc<SqliteBackend>, corpus: &str) -> Vec<String> {
    let mut ids: Vec<String> = CorrectionsEngine::load(db.as_ref(), corpus)
        .unwrap()
        .facts_in_corpus(corpus)
        .into_iter()
        .map(|(f, _)| f.id.clone())
        .collect();
    ids.sort();
    ids
}

fn run_import(db: &Arc<SqliteBackend>, corpus: &str, p: &Path, prune: bool) -> ImportReport {
    import(db.as_ref(), corpus, &[p.to_path_buf()], prune).unwrap()
}

fn scholia_count(db: &Arc<SqliteBackend>, corpus: &str) -> usize {
    db.correction_list(corpus).unwrap().len()
}

/// Reference corpus facts: two io registers, a vram fact, and a variant.
fn reference_facts() -> Vec<Value> {
    vec![
        var_fact("io.video-control", "Video control port", "0xC00004", "io"),
        var_fact("io.version", "Version register", "0xA10001", "io"),
        var_fact(
            "vram.sprite-table",
            "Sprite table in video memory",
            "0x0010",
            "vram",
        ),
        fact(
            "variant.region-bits",
            "variant",
            "Region bits differ between hardware variants",
            json!({"variant": "region", "affects": ["io.version"]}),
        ),
    ]
}

/// Game + reference corpora in one collection, reference facts imported.
fn linked_world(dir: &Path) -> Arc<SqliteBackend> {
    let db = game_db(Some("0xffffff"));
    add_corpus(
        db.as_ref(),
        REF,
        config(Some("reference"), Some("0xffffff")),
    );
    db.collection_insert(&Collection::new(COLL, COLL, CollectionKind::Domain))
        .unwrap();
    db.collection_add_member(COLL, GAME, MemberType::Corpus)
        .unwrap();
    db.collection_add_member(COLL, REF, MemberType::Corpus)
        .unwrap();
    let p = write_facts(dir, "ref.json", reference_facts());
    run_import(&db, REF, &p, false);
    db
}

// ── anchoring ────────────────────────────────────────────────────────────────

#[test]
fn variable_and_table_facts_anchor_to_entities_at_their_address() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    let p = write_facts(dir.path(), "game.json", game_facts());
    let report = run_import(&db, GAME, &p, false);

    assert_eq!(report.added, 2);
    assert_eq!(report.anchored, 2);
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    assert_eq!(
        anchors_of(&db, GAME, "var.counter"),
        vec!["game:ram_ff0010"]
    );
    assert_eq!(
        anchors_of(&db, GAME, "table.spawn"),
        vec!["game:dat_00012000"]
    );
}

#[test]
fn entity_lookup_by_fact_id_resolves_to_the_anchored_entity_with_its_fact() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    run_import(
        &db,
        GAME,
        &write_facts(dir.path(), "g.json", game_facts()),
        false,
    );

    let e = entity_of(&db, GAME, "var.counter");
    assert_eq!(e.canonical_name, "RAM_ff0010");
    assert_eq!(e.facts.len(), 1);
    assert_eq!(e.facts[0].id, "var.counter");
    assert_eq!(e.facts[0].summary, "Frame counter");
    assert_eq!(e.facts[0].confidence, "confirmed");

    // Looking the entity up by its own name shows the same fact.
    let by_name = entity_of(&db, GAME, "RAM_ff0010");
    assert_eq!(by_name.facts.len(), 1);
}

#[test]
fn a_mirrored_address_anchors_only_when_the_corpus_has_an_address_mask() {
    let dir = TempDir::new().unwrap();
    let mirrored = vec![var_fact(
        "var.counter",
        "Frame counter",
        "0xFFFF0010",
        "wram",
    )];

    let masked = game_db(Some("0xffffff"));
    let p = write_facts(dir.path(), "m.json", mirrored.clone());
    run_import(&masked, GAME, &p, false);
    assert_eq!(
        anchors_of(&masked, GAME, "var.counter"),
        vec!["game:ram_ff0010"]
    );

    let unmasked = game_db(None);
    run_import(&unmasked, GAME, &p, false);
    assert!(anchors_of(&unmasked, GAME, "var.counter").is_empty());
}

#[test]
fn facts_in_non_cpu_spaces_do_not_anchor() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    // 0xff0010 exists as an entity, but the fact lives in `vram`.
    let facts = vec![var_fact(
        "vram.thing",
        "Something in video memory",
        "0xff0010",
        "vram",
    )];
    let report = run_import(&db, GAME, &write_facts(dir.path(), "v.json", facts), false);
    assert_eq!(report.anchored, 0);
    assert!(anchors_of(&db, GAME, "vram.thing").is_empty());
}

// ── validation ───────────────────────────────────────────────────────────────

#[test]
fn record_missing_evidence_is_rejected_by_id_and_others_still_import() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    let mut bad = var_fact("var.noevidence", "No evidence given", "0xFF0010", "wram");
    bad.as_object_mut().unwrap().remove("evidence");
    let mut facts = game_facts();
    facts.push(bad);
    let report = run_import(&db, GAME, &write_facts(dir.path(), "g.json", facts), false);

    assert_eq!(report.added, 2);
    assert_eq!(report.rejected.len(), 1);
    assert!(
        report.rejected[0].contains("var.noevidence"),
        "{}",
        report.rejected[0]
    );
    assert!(
        report.rejected[0].contains("evidence"),
        "{}",
        report.rejected[0]
    );
    assert!(
        report.rejected[0].contains("g.json"),
        "{}",
        report.rejected[0]
    );
    assert_eq!(fact_ids(&db, GAME), vec!["table.spawn", "var.counter"]);
}

// ── idempotence, change, prune ───────────────────────────────────────────────

#[test]
fn reimporting_the_same_file_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    let p = write_facts(dir.path(), "g.json", game_facts());
    run_import(&db, GAME, &p, false);
    let before = scholia_count(&db, GAME);

    let report = run_import(&db, GAME, &p, false);
    assert_eq!(report.unchanged, 2);
    assert_eq!((report.added, report.changed, report.retracted), (0, 0, 0));
    assert_eq!(scholia_count(&db, GAME), before);
}

#[test]
fn changing_one_summary_reports_one_change_and_serves_the_new_text() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    run_import(
        &db,
        GAME,
        &write_facts(dir.path(), "g.json", game_facts()),
        false,
    );

    let mut facts = game_facts();
    facts[0]["summary"] = json!("Frame counter, wraps at 256");
    let report = run_import(&db, GAME, &write_facts(dir.path(), "g.json", facts), false);

    assert_eq!(report.changed, 1);
    assert_eq!(report.unchanged, 1);
    let e = entity_of(&db, GAME, "RAM_ff0010");
    assert_eq!(e.facts.len(), 1);
    assert_eq!(e.facts[0].summary, "Frame counter, wraps at 256");
}

#[test]
fn prune_retracts_facts_missing_from_the_import_and_they_disappear() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    run_import(
        &db,
        GAME,
        &write_facts(dir.path(), "g.json", game_facts()),
        false,
    );

    let remaining = vec![game_facts().remove(1)];
    let p = write_facts(dir.path(), "g.json", remaining);

    // Without prune, nothing is retracted.
    let report = run_import(&db, GAME, &p, false);
    assert_eq!(report.retracted, 0);
    assert_eq!(entity_of(&db, GAME, "RAM_ff0010").facts.len(), 1);

    let report = run_import(&db, GAME, &p, true);
    assert_eq!(report.retracted, 1);
    assert!(entity_of(&db, GAME, "RAM_ff0010").facts.is_empty());
    assert_eq!(fact_ids(&db, GAME), vec!["table.spawn"]);
}

#[test]
fn prune_keeps_a_fact_whose_updated_record_was_rejected() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    run_import(
        &db,
        GAME,
        &write_facts(dir.path(), "g.json", game_facts()),
        false,
    );

    let mut facts = game_facts();
    facts[0].as_object_mut().unwrap().remove("evidence");
    let report = run_import(&db, GAME, &write_facts(dir.path(), "g.json", facts), true);
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(report.retracted, 0);
    assert!(fact_ids(&db, GAME).contains(&"var.counter".to_string()));
}

// ── materialisation ──────────────────────────────────────────────────────────

#[test]
fn unanchored_fact_materialises_an_entity_carrying_its_address() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    let facts = vec![var_fact(
        "var.unused",
        "Not referenced by any code",
        "0xFF0F00",
        "wram",
    )];
    let report = run_import(&db, GAME, &write_facts(dir.path(), "u.json", facts), false);
    assert_eq!(report.anchored, 0);
    assert_eq!(report.materialised, 1);

    // The entity id is `{corpus}:fact.{fact-id}`; its name is the bare fact id.
    let e = entity_of(&db, GAME, &format!("{GAME}:fact.var.unused"));
    assert_eq!(e.canonical_name, "var.unused");
    assert_eq!(entity_of(&db, GAME, "var.unused").id, e.id);
    assert_eq!(e.kind, "fact_variable");
    assert!(
        e.aliases.contains(&"0xff0f00".to_string()),
        "{:?}",
        e.aliases
    );
}

#[test]
fn anchored_fact_does_not_materialise_a_duplicate_entity() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    let report = run_import(
        &db,
        GAME,
        &write_facts(dir.path(), "g.json", game_facts()),
        false,
    );
    assert_eq!(report.materialised, 0);
    assert!(
        db.entity_get_by_id("game:fact.var.counter")
            .unwrap()
            .is_none()
    );
}

#[test]
fn variant_region_constant_and_platform_facts_always_materialise() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    let facts = vec![
        fact("variant.x", "variant", "A variant", json!({"variant": "x"})),
        fact(
            "region.io",
            "region",
            "IO block",
            json!({"space": "io", "start": "0xA10000", "end": "0xA1FFFF"}),
        ),
        fact("const.max", "constant", "Max sprites", json!({"value": 80})),
        fact(
            "platform.toy16",
            "platform",
            "The toy16 console",
            json!({"cpu": "toy"}),
        ),
    ];
    let report = run_import(&db, GAME, &write_facts(dir.path(), "p.json", facts), false);
    assert_eq!(report.materialised, 4);
    for (id, kind) in [
        ("variant.x", "fact_variant"),
        ("region.io", "fact_region"),
        ("const.max", "fact_constant"),
        ("platform.toy16", "fact_platform"),
    ] {
        let e = db
            .entity_get_by_id(&format!("game:fact.{id}"))
            .unwrap()
            .expect(id);
        assert_eq!(e.kind, kind);
    }
}

// ── reference corpus + affects edges ─────────────────────────────────────────

fn affects_edges(db: &Arc<SqliteBackend>, corpus: &str) -> Vec<(String, String)> {
    let mut v: Vec<_> = db
        .edge_list(corpus)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "affects")
        .map(|e| (e.from_entity_id, e.to_entity_id))
        .collect();
    v.sort();
    v
}

#[test]
fn reference_import_materialises_fact_entities_and_an_affects_edge() {
    let dir = TempDir::new().unwrap();
    let db = linked_world(dir.path());

    for id in [
        "io.video-control",
        "io.version",
        "vram.sprite-table",
        "variant.region-bits",
    ] {
        assert!(
            db.entity_get_by_id(&format!("ref:fact.{id}"))
                .unwrap()
                .is_some(),
            "missing entity for {id}"
        );
    }
    assert_eq!(
        affects_edges(&db, REF),
        vec![(
            "ref:fact.variant.region-bits".into(),
            "ref:fact.io.version".into()
        )]
    );
}

#[test]
fn reimporting_reference_facts_does_not_duplicate_affects_edges() {
    let dir = TempDir::new().unwrap();
    let db = linked_world(dir.path());
    let p = dir.path().join("ref.json");
    run_import(&db, REF, &p, false);
    run_import(&db, REF, &p, true);
    assert_eq!(affects_edges(&db, REF).len(), 1);
}

#[test]
fn affects_edge_is_dropped_when_the_variant_is_pruned() {
    let dir = TempDir::new().unwrap();
    let db = linked_world(dir.path());
    let without_variant: Vec<Value> = reference_facts().into_iter().take(3).collect();
    let p = write_facts(dir.path(), "ref.json", without_variant);
    run_import(&db, REF, &p, true);
    assert!(affects_edges(&db, REF).is_empty());
}

// ── link_addresses ───────────────────────────────────────────────────────────

fn links(db: &Arc<SqliteBackend>) -> Vec<(String, String)> {
    let mut v: Vec<_> = db
        .correction_list_for_collection(COLL)
        .unwrap()
        .into_iter()
        .filter_map(|c| match c.kind {
            CorrectionKind::EntityLink {
                entity_a_id,
                entity_b_id,
                ..
            } => Some((entity_a_id, entity_b_id)),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

#[test]
fn link_addresses_links_game_entities_to_reference_facts_at_the_same_address() {
    let dir = TempDir::new().unwrap();
    let db = linked_world(dir.path());
    let added = link_addresses(db.as_ref(), COLL).unwrap();

    assert_eq!(added, 2);
    assert_eq!(
        links(&db),
        vec![
            (
                "game:_dat_00c00004".into(),
                "ref:fact.io.video-control".into()
            ),
            ("game:dat_00a10001".into(), "ref:fact.io.version".into()),
        ]
    );
}

#[test]
fn link_addresses_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let db = linked_world(dir.path());
    link_addresses(db.as_ref(), COLL).unwrap();
    assert_eq!(link_addresses(db.as_ref(), COLL).unwrap(), 0);
    assert_eq!(links(&db).len(), 2);
}

#[test]
fn link_addresses_does_not_link_facts_from_non_cpu_spaces() {
    let dir = TempDir::new().unwrap();
    let db = linked_world(dir.path());
    // A game entity at the vram fact's address must not be linked to it.
    add_entity(db.as_ref(), GAME, "DAT_0010", "variable", 0x0010);
    link_addresses(db.as_ref(), COLL).unwrap();
    assert!(
        links(&db)
            .iter()
            .all(|(_, b)| b != "ref:fact.vram.sprite-table")
    );
}

#[test]
fn entity_shows_linked_reference_and_the_variants_that_affect_it() {
    let dir = TempDir::new().unwrap();
    let db = linked_world(dir.path());
    link_addresses(db.as_ref(), COLL).unwrap();

    let e = entity_of(&db, GAME, "DAT_00a10001");
    assert_eq!(e.reference.len(), 1);
    assert_eq!(e.reference[0].corpus_id, REF);
    assert_eq!(e.reference[0].entity_id, "ref:fact.io.version");
    assert!(
        e.reference[0]
            .summary
            .as_deref()
            .unwrap_or("")
            .contains("Version register"),
        "{:?}",
        e.reference[0]
    );
    assert_eq!(e.variants.len(), 1);
    assert_eq!(e.variants[0].id, "variant.region-bits");

    // The register with no variant shows reference but no variants.
    let v = entity_of(&db, GAME, "_DAT_00c00004");
    assert_eq!(v.reference.len(), 1);
    assert!(v.variants.is_empty());
}

#[test]
fn entities_without_a_reference_counterpart_have_no_reference_or_variants() {
    let dir = TempDir::new().unwrap();
    let db = linked_world(dir.path());
    link_addresses(db.as_ref(), COLL).unwrap();
    let e = entity_of(&db, GAME, "RAM_ff0010");
    assert!(e.reference.is_empty());
    assert!(e.variants.is_empty());
}

// ── index files ──────────────────────────────────────────────────────────────

#[test]
fn index_file_imports_every_facts_file_it_lists() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    let mut f = game_facts();
    write_facts(dir.path(), "a.json", vec![f.remove(0)]);
    write_facts(dir.path(), "b.json", vec![f.remove(0)]);
    let idx = dir.path().join("index.json");
    std::fs::write(&idx, json!({"facts": ["a.json", "b.json"]}).to_string()).unwrap();

    let report = run_import(&db, GAME, &idx, false);
    assert_eq!(report.added, 2);
    assert_eq!(fact_ids(&db, GAME), vec!["table.spawn", "var.counter"]);
}

#[test]
fn index_file_refuses_names_that_escape_its_folder() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    write_facts(&sub, "a.json", game_facts());
    let db = game_db(Some("0xffffff"));

    for bad in ["sub/a.json", "../a.json", "..", "."] {
        let idx = dir.path().join("index.json");
        std::fs::write(&idx, json!({"facts": [bad]}).to_string()).unwrap();
        let r = import(db.as_ref(), GAME, &[idx], false);
        assert!(r.is_err(), "`{bad}` should be refused");
    }
    assert!(fact_ids(&db, GAME).is_empty());
}

// ── list_facts / overview ────────────────────────────────────────────────────

#[test]
fn list_facts_filters_by_kind_and_topic() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    let mut facts = game_facts();
    facts[1]["topic"] = json!("graphics");
    run_import(&db, GAME, &write_facts(dir.path(), "g.json", facts), false);
    let svc = query(&db, GAME);
    let list = |kind: Option<&str>, topic: Option<&str>| -> Vec<String> {
        let mut ids: Vec<String> = ok(svc.list_facts(ListFactsInput {
            corpus_id: GAME.into(),
            kind: kind.map(String::from),
            topic: topic.map(String::from),
        }))
        .facts
        .into_iter()
        .map(|f| f.fact.id)
        .collect();
        ids.sort();
        ids
    };

    assert_eq!(list(None, None), vec!["table.spawn", "var.counter"]);
    assert_eq!(list(Some("table"), None), vec!["table.spawn"]);
    assert_eq!(list(None, Some("memory")), vec!["var.counter"]);
    assert_eq!(list(None, Some("graphics")), vec!["table.spawn"]);
    assert!(list(Some("table"), Some("memory")).is_empty());
}

#[test]
fn list_facts_reports_each_facts_anchors() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    run_import(
        &db,
        GAME,
        &write_facts(dir.path(), "g.json", game_facts()),
        false,
    );
    let out = ok(query(&db, GAME).list_facts(ListFactsInput {
        corpus_id: GAME.into(),
        kind: Some("variable".into()),
        topic: None,
    }));
    assert_eq!(out.facts.len(), 1);
    assert_eq!(out.facts[0].anchors, vec!["game:ram_ff0010"]);
}

#[test]
fn corpus_overview_exposes_the_platform_fact_data() {
    let dir = TempDir::new().unwrap();
    let db = game_db(Some("0xffffff"));
    let overview = |db: &Arc<SqliteBackend>| {
        ok(query(db, GAME).corpus_overview(CorpusOverviewInput {
            corpus_id: GAME.into(),
        }))
    };
    assert!(overview(&db).platform.is_none());

    let facts = vec![fact(
        "platform.toy16",
        "platform",
        "The toy16 console",
        json!({"cpu": "toy16", "word_bits": 16}),
    )];
    run_import(&db, GAME, &write_facts(dir.path(), "p.json", facts), false);
    assert_eq!(
        overview(&db).platform,
        Some(json!({"cpu": "toy16", "word_bits": 16}))
    );
}
