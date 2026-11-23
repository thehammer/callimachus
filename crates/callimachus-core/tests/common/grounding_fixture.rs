//! Shared fixture for the grounded-passes tests: a fictional `toy16` world made
//! of a game corpus and a reference corpus in one collection, with facts,
//! layouts, edges and one decompiled-style function that touches all of them.
//!
//! Included with `#[path]` by the core, code-adapter and CLI test targets, so it
//! uses only `callimachus_core`'s public API plus `serde_json` and `tempfile`.
#![allow(dead_code)]

use std::sync::Arc;

use callimachus_core::Collection;
use callimachus_core::corrections::fact_import::{ImportReport, import};
use callimachus_core::corrections::link_addresses::link_addresses;
use callimachus_core::storage::{SqliteBackend, StorageBackend};
use callimachus_core::types::{Chunk, CollectionKind, Corpus, Edge, Entity, Location, MemberType};
use serde_json::{Value, json};
use tempfile::TempDir;

pub const GAME: &str = "game";
pub const REF: &str = "ref";
pub const COLL: &str = "toy16";

pub const FUNC: &str = "FUN_00004000";
pub const FUNC_ID: &str = "game:fun_00004000";
pub const FUNC_PATH: &str = "src/main.c#FUN_00004000";

pub const HEADER: &str = "Verified context from the project's fact base (prefer it over guesses; if the code contradicts a fact, say so):";
pub const INSTRUCTION: &str =
    "Use the names from the verified context (e.g. obj.x, the port names) in your answer.";

/// The decompiled-style source of `FUN_00004000`: a register-relative field
/// write, a port read, a call, and a compare against a known constant.
pub const FUNC_SOURCE: &str = "void FUN_00004000(void)\n{\n  short sVar1;\n\n  sVar1 = FUN_00001000();\n  *(short *)(unaff_A2 + 0x10) = sVar1 + (short)_DAT_00a10001;\n  if (sVar1 == 0x2C0) {\n    return;\n  }\n  return;\n}\n";

// ── low-level builders ───────────────────────────────────────────────────────

pub fn add_corpus(db: &dyn StorageBackend, id: &str, cfg: Value) {
    db.corpus_insert(&Corpus::new(
        id.to_string(),
        id.to_string(),
        "code".to_string(),
        "/tmp/dummy".to_string(),
    ))
    .unwrap();
    db.corpus_set_config(id, &cfg).unwrap();
}

pub fn corpus_config(role: Option<&str>) -> Value {
    let mut c = json!({ "cpu_spaces": ["rom", "wram", "io"], "address_mask": "0xffffff" });
    if let Some(r) = role {
        c["role"] = json!(r);
    }
    c
}

/// An entity `{corpus}:{lowercased name}`; `addr` adds the `0x..` alias the C
/// adapter gives address-named entities.
pub fn add_entity(
    db: &dyn StorageBackend,
    corpus: &str,
    name: &str,
    kind: &str,
    addr: Option<u64>,
) -> String {
    let id = format!("{corpus}:{}", name.to_lowercase());
    add_entity_with_id(db, corpus, &id, name, kind, addr);
    id
}

pub fn add_entity_with_id(
    db: &dyn StorageBackend,
    corpus: &str,
    id: &str,
    name: &str,
    kind: &str,
    addr: Option<u64>,
) {
    let mut e = Entity::new(
        id.to_string(),
        corpus.to_string(),
        name.to_string(),
        kind.to_string(),
    );
    if let Some(a) = addr {
        e.aliases.push(format!("0x{a:x}"));
    }
    db.entity_upsert(&e).unwrap();
}

pub fn add_edge(db: &dyn StorageBackend, id: &str, from: &str, to: &str, kind: &str) {
    db.edge_upsert(&Edge::new(
        id.into(),
        GAME.into(),
        from.into(),
        to.into(),
        kind.into(),
        Location::new(GAME, "src/main.c"),
    ))
    .unwrap();
}

/// A function entity `game:{lowercase name}` with a stored chunk holding
/// `source` (language `c`); the entity's first location points at that chunk.
pub fn add_function_with_source(
    db: &dyn StorageBackend,
    name: &str,
    addr: Option<u64>,
    source: &str,
) -> Entity {
    add_function_in_language(db, name, addr, source, "c")
}

/// [`add_function_with_source`] for a source in another language.
pub fn add_function_in_language(
    db: &dyn StorageBackend,
    name: &str,
    addr: Option<u64>,
    source: &str,
    language: &str,
) -> Entity {
    let id = format!("{GAME}:{}", name.to_lowercase());
    let loc = Location::new(GAME, format!("src/main.c#{name}"));
    let mut chunk = Chunk::new(
        GAME.into(),
        None,
        "function".into(),
        loc.clone(),
        source.to_string(),
    );
    chunk.language = Some(language.into());
    db.chunk_upsert(&chunk).unwrap();
    let mut e = Entity::new(id, GAME.into(), name.into(), "function".into());
    if let Some(a) = addr {
        e.aliases.push(format!("0x{a:x}"));
    }
    e.first_location = Some(loc);
    db.entity_upsert(&e).unwrap();
    e
}

// ── facts ────────────────────────────────────────────────────────────────────

pub fn fact(id: &str, kind: &str, summary: &str, confidence: &str, data: Value) -> Value {
    json!({
        "id": id, "kind": kind, "topic": "memory", "summary": summary,
        "data": data, "confidence": confidence, "evidence": ["test"],
    })
}

pub fn var_fact(id: &str, summary: &str, confidence: &str, addr: &str, space: &str) -> Value {
    fact(
        id,
        "variable",
        summary,
        confidence,
        json!({"address": addr, "space": space}),
    )
}

pub fn routine_fact(summary: &str) -> Value {
    fact(
        "routine.unpack",
        "routine",
        summary,
        "confirmed",
        json!({"space": "rom", "address": "0x1000", "role": "unpack"}),
    )
}

pub fn constant_fact() -> Value {
    fact(
        "const.pickup-tile",
        "constant",
        "pickup tile",
        "confirmed",
        json!({"value": "0x2c0"}),
    )
}

pub fn layout_fact() -> Value {
    json!({
        "id": "layout.obj", "kind": "layout", "topic": "other",
        "summary": "Active object record",
        "data": {
            "name": "obj",
            "bases": ["unaff_A2"],
            "functions": [FUNC],
            "fields": [{"offset": "0x10", "name": "x", "type": "s16",
                        "summary": "x position, whole pixels", "confidence": "confirmed"}]
        },
        "confidence": "inferred", "evidence": ["ev.x"]
    })
}

pub fn import_facts(db: &Arc<SqliteBackend>, corpus: &str, facts: Vec<Value>) -> ImportReport {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("facts.json");
    std::fs::write(&p, json!({"format": 1, "facts": facts}).to_string()).unwrap();
    let report = import(db.as_ref(), corpus, &[p], false).unwrap();
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    report
}

// ── worlds ───────────────────────────────────────────────────────────────────

/// An empty game corpus (decompiler-style code corpus with a toy16 address map).
pub fn bare_game() -> Arc<SqliteBackend> {
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    add_corpus(db.as_ref(), GAME, corpus_config(None));
    db
}

/// The full toy16 world:
///
/// * `FUN_00004000` (source [`FUNC_SOURCE`]) writes the field entity
///   `unaff_A2+0x10` (layout `obj`, field `x`), reads `DAT_00a10001` (linked to
///   the reference fact `io.version`, which `variant.region-bits` affects),
///   and calls `FUN_00001000` (a `routine` fact);
/// * a `constant` fact for `0x2c0`, which the source compares against.
pub fn toy_world() -> Arc<SqliteBackend> {
    let db = bare_game();
    add_corpus(db.as_ref(), REF, corpus_config(Some("reference")));
    db.collection_insert(&Collection::new(COLL, COLL, CollectionKind::Domain))
        .unwrap();
    db.collection_add_member(COLL, GAME, MemberType::Corpus)
        .unwrap();
    db.collection_add_member(COLL, REF, MemberType::Corpus)
        .unwrap();

    add_entity(db.as_ref(), GAME, "FUN_00001000", "function", Some(0x1000));
    add_function_with_source(db.as_ref(), FUNC, Some(0x4000), FUNC_SOURCE);
    add_entity(
        db.as_ref(),
        GAME,
        "DAT_00a10001",
        "variable",
        Some(0xa10001),
    );
    // Register-style bases are function-qualified: `{name}@{function}`.
    add_entity_with_id(
        db.as_ref(),
        GAME,
        "game:unaff_a2+0x10@fun_00004000",
        "unaff_A2+0x10",
        "field",
        None,
    );
    add_edge(
        db.as_ref(),
        "e1",
        FUNC_ID,
        "game:unaff_a2+0x10@fun_00004000",
        "writes",
    );
    add_edge(db.as_ref(), "e2", FUNC_ID, "game:dat_00a10001", "reads");
    add_edge(db.as_ref(), "e3", FUNC_ID, "game:fun_00001000", "calls");

    import_facts(
        &db,
        REF,
        vec![
            var_fact("io.version", "Version port", "confirmed", "0xA10001", "io"),
            fact(
                "variant.region-bits",
                "variant",
                "region bits 7-6 select 50/60 Hz and language",
                "confirmed",
                json!({"variant": "region", "affects": ["io.version"]}),
            ),
        ],
    );
    import_facts(
        &db,
        GAME,
        vec![
            routine_fact("decompress a blob into RAM"),
            constant_fact(),
            layout_fact(),
        ],
    );
    assert_eq!(link_addresses(db.as_ref(), COLL).unwrap(), 1);
    db
}

pub fn game_corpus(db: &Arc<SqliteBackend>) -> Corpus {
    db.corpus_get(GAME).unwrap().unwrap()
}

pub fn function_entity(db: &Arc<SqliteBackend>) -> Entity {
    db.entity_get_by_id(FUNC_ID).unwrap().expect("FUN_00004000")
}

/// The fact lines of a block (everything after the header), in order.
pub fn lines_of(block: &str) -> Vec<&str> {
    block.lines().skip(1).collect()
}
