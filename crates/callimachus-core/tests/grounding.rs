//! Behavioural tests for grounded LLM passes: the verified-context block and
//! annotated source built from the fact base (`grounding::build`), how the
//! summarize, purpose and contract passes use them, and the Layer-2 cache
//! contract (a changed fact re-derives; no grounding keeps today's keys).
//!
//! Fictional platform `toy16`. The real code adapter cannot be used here (it
//! dev-depends on core), so the passes run against small test adapters; the
//! code adapter's own prompts are covered in its `grounded_passes` tests.

#[path = "common/grounding_fixture.rs"]
mod fixture;
#[path = "common/recording_provider.rs"]
mod recording;

use std::sync::{Arc, Mutex};

use callimachus_adapter_contract::FieldName;
use callimachus_core::adapter::{
    DiscoveredSource, EntityMerge, ExtractedContract, ExtractedPurpose, ExtractedSemantic,
    ExtractedStructure, LocationRef, SourceAdapter,
};
use callimachus_core::corrections::link_addresses::link_addresses;
use callimachus_core::indexing::file_shape::file_shapes_by_uri;
use callimachus_core::indexing::grounding::{
    GroundingContext, GroundingMode, HEADER as CORE_HEADER, MAX_CHARS, MAX_LINES, build,
};
use callimachus_core::indexing::pipeline::{IndexOptions, IndexPipeline, IndexResult};
use callimachus_core::storage::{SqliteBackend, StorageBackend};
use callimachus_core::types::chunk::hash_content;
use callimachus_core::types::{Chunk, Entity, Layer2CacheKey, Pass};
use callimachus_llm::{
    CompletionRequest, DryRunProvider, EstimatingProvider, LlmProvider,
    estimating::estimate_input_tokens,
};
use fixture::*;
use recording::RecordingProvider;
use serde_json::json;

// ── test adapters ────────────────────────────────────────────────────────────

/// What an adapter was asked to do: pass, entity/depth, the content it was
/// handed and the grounding block (if any).
#[derive(Debug, Clone)]
struct Seen {
    pass: &'static str,
    name: String,
    content: String,
    grounding: Option<String>,
}

type Log = Arc<Mutex<Vec<Seen>>>;

async fn ask(llm: &dyn LlmProvider, pass: &str, prompt: String) {
    llm.complete(CompletionRequest {
        prompt,
        max_tokens: Some(100),
        pass: pass.to_string(),
        ..Default::default()
    })
    .await
    .unwrap();
}

macro_rules! toy_adapter {
    ($name:ident { $($methods:tt)* }) => {
        struct $name {
            log: Log,
        }

        impl $name {
            fn new() -> (Arc<Self>, Log) {
                let log: Log = Arc::default();
                (Arc::new(Self { log: log.clone() }), log)
            }
        }

        #[async_trait::async_trait]
        impl SourceAdapter for $name {
            fn kind(&self) -> &str {
                "code"
            }
            fn version(&self) -> &str {
                "0.1.0"
            }
            fn summary_levels(&self) -> Vec<&'static str> {
                vec!["function"]
            }
            async fn discover(&self, s: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
                Ok(vec![DiscoveredSource {
                    path: s.into(),
                    kind: "c".into(),
                    meta: serde_json::Value::Null,
                }])
            }
            async fn chunk(&self, _s: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
                Ok(vec![])
            }
            async fn extract_structure(&self, _c: &Chunk) -> anyhow::Result<ExtractedStructure> {
                Ok(ExtractedStructure {
                    parent_path: None,
                    child_paths: vec![],
                    structural_entities: vec![],
                    structural_edges: vec![],
                })
            }
            async fn extract_with_llm(
                &self,
                _c: &Chunk,
                _l: &dyn LlmProvider,
            ) -> anyhow::Result<Option<ExtractedSemantic>> {
                Ok(None)
            }
            async fn resolve_aliases(
                &self,
                _e: &[Entity],
                _l: &dyn LlmProvider,
            ) -> anyhow::Result<Vec<EntityMerge>> {
                Ok(vec![])
            }
            fn format_location(&self, c: &Chunk) -> String {
                c.location.path.clone()
            }
            fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
                Ok(LocationRef { corpus_id: GAME.into(), path: uri.into() })
            }
            $($methods)*
        }
    };
}

// An adapter that knows nothing about grounding: only the plain methods (the
// way the book, wiki and capture adapters are). The grounded defaults must
// route to them and drop the block.
toy_adapter!(Plain {
    async fn summarize(
        &self,
        c: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
    ) -> anyhow::Result<Option<String>> {
        self.log.lock().unwrap().push(Seen {
            pass: "summarize",
            name: depth.into(),
            content: c.content.clone(),
            grounding: None,
        });
        ask(llm, "summarize", c.content.clone()).await;
        Ok(Some("a summary".into()))
    }
    async fn extract_purpose(
        &self,
        e: &Entity,
        content: &str,
        _summary: Option<&str>,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedPurpose>> {
        self.log.lock().unwrap().push(Seen {
            pass: "purpose",
            name: e.canonical_name.clone(),
            content: content.into(),
            grounding: None,
        });
        ask(llm, "purpose", content.to_string()).await;
        Ok(Some(ExtractedPurpose { purpose: "p".into(), blocks: vec![] }))
    }
    async fn extract_contract(
        &self,
        e: &Entity,
        content: &str,
        _summary: Option<&str>,
        _purpose: Option<&str>,
        _signals: &serde_json::Value,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedContract>> {
        self.log.lock().unwrap().push(Seen {
            pass: "contract",
            name: e.canonical_name.clone(),
            content: content.into(),
            grounding: None,
        });
        ask(llm, "contract", content.to_string()).await;
        Ok(Some(ExtractedContract::default()))
    }
});

fn prompt_of(block: Option<&str>, content: &str) -> String {
    match block {
        Some(b) => format!("{b}\n{content}"),
        None => content.to_string(),
    }
}

// An adapter that implements the grounded methods the way the code adapter does
// (block into the prompt, annotated content as handed over) and annotates C.
toy_adapter!(Grounded {
    async fn summarize(
        &self,
        c: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
    ) -> anyhow::Result<Option<String>> {
        self.summarize_grounded(c, llm, depth, None).await
    }
    async fn summarize_grounded(
        &self,
        c: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
        grounding: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        self.log.lock().unwrap().push(Seen {
            pass: "summarize",
            name: depth.into(),
            content: c.content.clone(),
            grounding: grounding.map(String::from),
        });
        ask(llm, "summarize", prompt_of(grounding, &c.content)).await;
        Ok(Some("a summary".into()))
    }
    async fn extract_purpose(
        &self,
        e: &Entity,
        content: &str,
        summary: Option<&str>,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedPurpose>> {
        self.extract_purpose_grounded(e, content, summary, None, llm).await
    }
    async fn extract_purpose_grounded(
        &self,
        e: &Entity,
        content: &str,
        _summary: Option<&str>,
        grounding: Option<&str>,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedPurpose>> {
        self.log.lock().unwrap().push(Seen {
            pass: "purpose",
            name: e.canonical_name.clone(),
            content: content.into(),
            grounding: grounding.map(String::from),
        });
        ask(llm, "purpose", prompt_of(grounding, content)).await;
        Ok(Some(ExtractedPurpose { purpose: "p".into(), blocks: vec![] }))
    }
    async fn extract_contract(
        &self,
        e: &Entity,
        content: &str,
        summary: Option<&str>,
        purpose: Option<&str>,
        signals: &serde_json::Value,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedContract>> {
        self.extract_contract_grounded(e, content, summary, purpose, signals, None, llm).await
    }
    async fn extract_contract_grounded(
        &self,
        e: &Entity,
        content: &str,
        _summary: Option<&str>,
        _purpose: Option<&str>,
        _signals: &serde_json::Value,
        grounding: Option<&str>,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedContract>> {
        self.log.lock().unwrap().push(Seen {
            pass: "contract",
            name: e.canonical_name.clone(),
            content: content.into(),
            grounding: grounding.map(String::from),
        });
        ask(llm, "contract", prompt_of(grounding, content)).await;
        Ok(Some(ExtractedContract::default()))
    }
    fn annotate_source(&self, content: &str, language: &str, fields: &[FieldName]) -> String {
        if language != "c" {
            return content.to_string();
        }
        let mut out = content.to_string();
        for f in fields {
            out = out.replace(
                &format!("*(short *)({} + 0x{:x})", f.base, f.offset),
                &format!("{}->{}", f.base, f.field),
            );
        }
        out
    }
});

// ── helpers ──────────────────────────────────────────────────────────────────

fn ctx_for(
    db: &Arc<SqliteBackend>,
    adapter: Arc<dyn SourceAdapter>,
    requested: Option<GroundingMode>,
) -> GroundingContext {
    GroundingContext::load(db.clone(), &game_corpus(db), adapter, requested).unwrap()
}

fn ground(
    db: &Arc<SqliteBackend>,
    entity: &Entity,
    content: &str,
    mode: GroundingMode,
) -> callimachus_core::indexing::grounding::Grounding {
    let (adapter, _log) = Grounded::new();
    build(&ctx_for(db, adapter, Some(mode)), entity, content, mode)
}

async fn run_passes(
    db: &Arc<SqliteBackend>,
    adapter: Arc<dyn SourceAdapter>,
    llm: Arc<dyn LlmProvider>,
    passes: Vec<Pass>,
    tweak: impl FnOnce(&mut IndexOptions),
) -> IndexResult {
    let corpus = game_corpus(db);
    let mut opts = IndexOptions { passes, concurrency: Some(4), ..Default::default() };
    tweak(&mut opts);
    IndexPipeline { db: db.clone(), adapter, llm, embedder: None }
        .run(&corpus, opts)
        .await
        .unwrap()
}

const ALL_THREE: [Pass; 3] = [Pass::Summarize, Pass::Purpose, Pass::Contract];

fn seen(log: &Log, pass: &str) -> Vec<Seen> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|s| s.pass == pass && (s.name == FUNC || s.name == "function"))
        .cloned()
        .collect()
}

fn add_reads(db: &Arc<SqliteBackend>, from: &str, n: usize, base: u64, summary: impl Fn(usize) -> String) {
    let mut facts = Vec::new();
    for i in 0..n {
        let addr = base + i as u64;
        add_entity(db.as_ref(), GAME, &format!("DAT_{addr:08x}"), "variable", Some(addr));
        add_edge(db.as_ref(), &format!("r{base:x}-{i}"), from, &format!("game:dat_{addr:08x}"), "reads");
        facts.push(var_fact(&format!("var.v{base:x}-{i}"), &summary(i), "confirmed", &format!("0x{addr:x}"), "wram"));
    }
    import_facts(db, GAME, facts);
}

fn add_function(db: &Arc<SqliteBackend>, name: &str) -> Entity {
    add_entity(db.as_ref(), GAME, name, "function", None);
    db.entity_get_by_id(&format!("game:{}", name.to_lowercase())).unwrap().unwrap()
}

// ── the block ────────────────────────────────────────────────────────────────

#[test]
fn header_constant_is_the_documented_sentence() {
    assert_eq!(
        CORE_HEADER,
        "Verified context from the project's fact base (prefer it over guesses; if the code contradicts a fact, say so):"
    );
    assert_eq!(CORE_HEADER, HEADER);
}

#[test]
fn full_mode_block_lists_the_functions_facts_in_priority_order_under_the_exact_header() {
    let db = toy_world();
    let g = ground(&db, &function_entity(&db), FUNC_SOURCE, GroundingMode::Full);

    assert_eq!(g.block.lines().next(), Some(HEADER));
    // Priority: facts about accessed targets (with their reference and
    // variants), then field facts, then callee routines, then constants.
    assert_eq!(
        lines_of(&g.block),
        vec![
            "- reads DAT_00a10001 = Version port (reference, confirmed). Variant: region bits 7-6 select 50/60 Hz and language (confirmed)",
            "- writes unaff_A2+0x10 = obj.x: x position, whole pixels (s16, confirmed)",
            "- calls FUN_00001000 = decompress a blob into RAM (confirmed)",
            "- constant 0x2c0 = pickup tile (confirmed)",
        ]
    );
    let annotated = g.annotated_content.as_deref().expect("full mode annotates");
    assert!(annotated.contains("unaff_A2->x"), "{annotated}");
    assert!(!annotated.contains("unaff_A2 + 0x10"), "{annotated}");
    assert!(!g.is_empty());
}

#[test]
fn facts_mode_gives_the_same_block_but_no_annotated_source() {
    let db = toy_world();
    let e = function_entity(&db);
    let full = ground(&db, &e, FUNC_SOURCE, GroundingMode::Full);
    let facts = ground(&db, &e, FUNC_SOURCE, GroundingMode::Facts);
    assert_eq!(facts.block, full.block);
    assert_eq!(facts.annotated_content, None);
    assert!(!facts.is_empty());
    assert_ne!(facts.hash, full.hash, "the mode is part of the grounding identity");
}

#[test]
fn none_mode_gives_nothing() {
    let db = toy_world();
    let g = ground(&db, &function_entity(&db), FUNC_SOURCE, GroundingMode::None);
    assert_eq!(g.block, "");
    assert_eq!(g.annotated_content, None);
    assert!(g.is_empty());
    assert_eq!(g.block_opt(), None);
    assert_eq!(g.content_or(FUNC_SOURCE), FUNC_SOURCE);
}

#[test]
fn an_entity_without_any_known_fact_has_an_empty_grounding() {
    let db = toy_world();
    let e = add_function(&db, "FUN_00009000");
    let g = ground(&db, &e, "void FUN_00009000(void) { }", GroundingMode::Full);
    assert_eq!(g.block, "");
    assert_eq!(g.annotated_content, None);
    assert!(g.is_empty());
}

#[test]
fn a_fact_anchored_to_the_entity_itself_is_listed_first() {
    let db = toy_world();
    // FUN_00001000 carries the routine fact; the function also reads a variable.
    add_reads(&db, "game:fun_00001000", 1, 0xb0000, |_| "a counter".into());
    let e = db.entity_get_by_id("game:fun_00001000").unwrap().unwrap();
    let g = ground(&db, &e, "", GroundingMode::Facts);
    let lines = lines_of(&g.block);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].starts_with("- ") && lines[0].contains("decompress a blob into RAM"), "{lines:?}");
    assert!(lines[1].starts_with("- reads DAT_000b0000 = a counter"), "{lines:?}");
}

#[test]
fn the_same_fact_reached_twice_is_listed_once() {
    let db = toy_world();
    // A second read of the same port at another site.
    add_edge(db.as_ref(), "e-dup", FUNC_ID, "game:dat_00a10001", "reads");
    let g = ground(&db, &function_entity(&db), FUNC_SOURCE, GroundingMode::Facts);
    let reads = g.block.lines().filter(|l| l.starts_with("- reads DAT_00a10001")).count();
    assert_eq!(reads, 1, "{}", g.block);
}

#[test]
fn unknown_confidence_reads_meaning_open() {
    let db = bare_game();
    let f = add_function(&db, "FUN_00004000");
    add_entity(db.as_ref(), GAME, "DAT_000d0000", "variable", Some(0xd0000));
    add_entity(db.as_ref(), GAME, "DAT_000d0001", "variable", Some(0xd0001));
    add_edge(db.as_ref(), "u1", &f.id, "game:dat_000d0000", "reads");
    add_edge(db.as_ref(), "u2", &f.id, "game:dat_000d0001", "reads");
    import_facts(
        &db,
        GAME,
        vec![
            var_fact("var.open", "mystery flag", "unknown", "0xd0000", "wram"),
            var_fact("var.guess", "likely a timer", "inferred", "0xd0001", "wram"),
        ],
    );
    let g = ground(&db, &f, "", GroundingMode::Facts);
    let lines = lines_of(&g.block);
    assert!(lines.contains(&"- reads DAT_000d0000 = mystery flag (meaning open)"), "{lines:?}");
    assert!(lines.contains(&"- reads DAT_000d0001 = likely a timer (inferred)"), "{lines:?}");
}

#[test]
fn an_address_named_target_inside_a_known_region_is_described_by_its_region() {
    let db = bare_game();
    let f = add_function(&db, "FUN_00004000");
    add_entity(db.as_ref(), GAME, "DAT_00012000", "variable", Some(0x12000));
    add_edge(db.as_ref(), "g1", &f.id, "game:dat_00012000", "reads");
    import_facts(
        &db,
        GAME,
        vec![fact(
            "region.rom",
            "region",
            "Cartridge ROM",
            "confirmed",
            json!({"space": "rom", "start": "0x10000", "end": "0x1ffff"}),
        )],
    );
    let g = ground(&db, &f, "", GroundingMode::Facts);
    assert_eq!(
        lines_of(&g.block),
        vec!["- reads DAT_00012000: in ROM region \"rom\" (confirmed)"]
    );
}

#[test]
fn a_callee_name_shared_by_several_candidates_is_listed_once_as_ambiguous() {
    let db = bare_game();
    let caller = add_function(&db, "FUN_caller");
    add_entity_with_id(db.as_ref(), GAME, "game:0x5000", "caseD_1", "function", Some(0x5000));
    add_entity_with_id(db.as_ref(), GAME, "game:0x6000", "caseD_1", "function", Some(0x6000));
    add_edge(db.as_ref(), "c1", &caller.id, "game:0x5000", "calls");
    add_edge(db.as_ref(), "c2", &caller.id, "game:0x6000", "calls");
    import_facts(
        &db,
        GAME,
        vec![fact(
            "routine.five",
            "routine",
            "spawns the boss",
            "confirmed",
            json!({"space": "rom", "address": "0x5000", "role": "spawn"}),
        )],
    );
    let g = ground(&db, &caller, "", GroundingMode::Facts);
    assert_eq!(lines_of(&g.block), vec!["- calls caseD_1 (ambiguous: 2 candidates)"]);
    assert!(!g.block.contains("spawns the boss"), "one candidate's fact must not be attributed to the name");
}

// ── caps ─────────────────────────────────────────────────────────────────────

#[test]
fn sixty_facts_are_capped_at_forty_lines_keeping_the_highest_priority_ones() {
    let db = bare_game();
    add_entity(db.as_ref(), GAME, "FUN_00007000", "function", Some(0x7000));
    let f = db.entity_get_by_id("game:fun_00007000").unwrap().unwrap();
    // 1 fact about the entity itself + 58 accessed variables + 1 constant = 60 lines.
    import_facts(
        &db,
        GAME,
        vec![
            fact("routine.cap", "routine", "cap anchor", "confirmed",
                 json!({"space": "rom", "address": "0x7000", "role": "x"})),
            constant_fact(),
        ],
    );
    add_reads(&db, &f.id, 58, 0xb0000, |i| format!("var {i:02}"));

    let g = ground(&db, &f, "if (x == 0x2C0) {}", GroundingMode::Facts);
    let lines = lines_of(&g.block);
    assert_eq!(MAX_LINES, 40);
    assert_eq!(lines.len(), 41, "40 fact lines plus the omitted note: {lines:?}");
    assert_eq!(lines[40], "- … 20 more omitted");
    assert!(g.block.chars().count() < 2_500, "{} chars", g.block.chars().count());
    assert!(lines[0].contains("cap anchor"), "the entity's own fact survives: {lines:?}");
    assert!(
        !lines.iter().any(|l| l.contains("constant 0x2c0")),
        "lowest priority (constants) is dropped first: {lines:?}"
    );
    assert!(lines[1..40].iter().all(|l| l.starts_with("- reads DAT_")), "{lines:?}");
}

#[test]
fn the_block_never_exceeds_the_character_cap() {
    let db = bare_game();
    let f = add_function(&db, "FUN_00004000");
    add_reads(&db, &f.id, 12, 0xc0000, |i| format!("{i:02} {}", "w".repeat(270)));

    let g = ground(&db, &f, "", GroundingMode::Facts);
    assert!(g.block.chars().count() <= MAX_CHARS, "{} chars", g.block.chars().count());
    let lines = lines_of(&g.block);
    let note = lines.last().unwrap();
    let omitted: usize = note
        .strip_prefix("- … ")
        .and_then(|r| r.strip_suffix(" more omitted"))
        .unwrap_or_else(|| panic!("no omitted note: {note}"))
        .parse()
        .unwrap();
    let kept = lines.len() - 1;
    assert!(kept >= 1 && omitted >= 1, "kept {kept}, omitted {omitted}");
    assert_eq!(kept + omitted, 12, "every fact is either listed or counted");
}

// ── mode selection ───────────────────────────────────────────────────────────

#[test]
fn modes_parse_from_their_names_and_reject_anything_else() {
    assert_eq!("none".parse::<GroundingMode>(), Ok(GroundingMode::None));
    assert_eq!("facts".parse::<GroundingMode>(), Ok(GroundingMode::Facts));
    assert_eq!("full".parse::<GroundingMode>(), Ok(GroundingMode::Full));
    assert_eq!("FULL".parse::<GroundingMode>(), Ok(GroundingMode::Full));
    let err = "some".parse::<GroundingMode>().unwrap_err();
    assert!(err.contains("some") && err.contains("none"), "{err}");
}

#[test]
fn default_mode_is_none_without_facts_and_full_with_a_fact() {
    let (adapter, _) = Grounded::new();
    let db = bare_game();
    assert_eq!(ctx_for(&db, adapter.clone(), None).mode, GroundingMode::None);

    import_facts(&db, GAME, vec![var_fact("var.a", "a", "confirmed", "0xff0010", "wram")]);
    assert_eq!(ctx_for(&db, adapter.clone(), None).mode, GroundingMode::Full);
    // An explicit request always wins.
    assert_eq!(ctx_for(&db, adapter.clone(), Some(GroundingMode::Facts)).mode, GroundingMode::Facts);
    assert_eq!(ctx_for(&db, adapter, Some(GroundingMode::None)).mode, GroundingMode::None);
}

#[test]
fn default_mode_is_full_when_the_corpus_has_a_reference_link_even_without_facts() {
    let db = bare_game();
    add_corpus(db.as_ref(), REF, corpus_config(Some("reference")));
    db.collection_insert(&callimachus_core::Collection::new(
        COLL,
        COLL,
        callimachus_core::types::CollectionKind::Domain,
    ))
    .unwrap();
    for c in [GAME, REF] {
        db.collection_add_member(COLL, c, callimachus_core::types::MemberType::Corpus).unwrap();
    }
    add_entity(db.as_ref(), GAME, "DAT_00a10001", "variable", Some(0xa10001));
    import_facts(&db, REF, vec![var_fact("io.version", "Version port", "confirmed", "0xA10001", "io")]);

    let (adapter, _) = Grounded::new();
    assert_eq!(ctx_for(&db, adapter.clone(), None).mode, GroundingMode::None, "no link yet");
    assert_eq!(link_addresses(db.as_ref(), COLL).unwrap(), 1);
    assert_eq!(ctx_for(&db, adapter, None).mode, GroundingMode::Full);
}

// ── passes ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn full_mode_hands_every_pass_the_block_and_the_annotated_source() {
    let db = toy_world();
    let (adapter, log) = Grounded::new();
    run_passes(&db, adapter, Arc::new(RecordingProvider::new()), ALL_THREE.to_vec(), |o| {
        o.grounding = Some(GroundingMode::Full);
    })
    .await;

    for pass in ["summarize", "purpose", "contract"] {
        let calls = seen(&log, pass);
        assert_eq!(calls.len(), 1, "{pass}: {calls:?}");
        let block = calls[0].grounding.as_deref().unwrap_or_else(|| panic!("{pass}: no block"));
        assert!(block.starts_with(HEADER), "{pass}: {block}");
        assert!(block.contains("- writes unaff_A2+0x10 = obj.x"), "{pass}: {block}");
        assert!(calls[0].content.contains("unaff_A2->x"), "{pass}: {}", calls[0].content);
        assert!(!calls[0].content.contains("unaff_A2 + 0x10"), "{pass}: {}", calls[0].content);
    }
}

#[tokio::test]
async fn facts_mode_hands_the_passes_the_block_with_the_original_source() {
    let db = toy_world();
    let (adapter, log) = Grounded::new();
    run_passes(&db, adapter, Arc::new(RecordingProvider::new()), ALL_THREE.to_vec(), |o| {
        o.grounding = Some(GroundingMode::Facts);
    })
    .await;
    for pass in ["summarize", "purpose", "contract"] {
        let calls = seen(&log, pass);
        assert_eq!(calls.len(), 1, "{pass}");
        assert!(calls[0].grounding.as_deref().is_some_and(|b| b.starts_with(HEADER)), "{pass}");
        assert_eq!(calls[0].content, FUNC_SOURCE, "{pass}");
    }
}

#[tokio::test]
async fn none_mode_runs_the_passes_exactly_as_before_facts_existed() {
    let db = toy_world();
    let (adapter, log) = Grounded::new();
    run_passes(&db, adapter, Arc::new(RecordingProvider::new()), ALL_THREE.to_vec(), |o| {
        o.grounding = Some(GroundingMode::None);
    })
    .await;
    for pass in ["summarize", "purpose", "contract"] {
        let calls = seen(&log, pass);
        assert_eq!(calls.len(), 1, "{pass}");
        assert_eq!(calls[0].grounding, None, "{pass}");
        assert_eq!(calls[0].content, FUNC_SOURCE, "{pass}");
    }
}

#[tokio::test]
async fn without_a_flag_a_corpus_with_facts_is_grounded_and_one_without_is_not() {
    let db = toy_world();
    let (adapter, log) = Grounded::new();
    run_passes(&db, adapter, Arc::new(RecordingProvider::new()), vec![Pass::Purpose], |_| {}).await;
    assert!(seen(&log, "purpose")[0].grounding.is_some(), "default rule gives Full here");

    let bare = bare_game();
    add_function_with_source(bare.as_ref(), FUNC, Some(0x4000), FUNC_SOURCE);
    let (adapter, log) = Grounded::new();
    run_passes(&bare, adapter, Arc::new(RecordingProvider::new()), ALL_THREE.to_vec(), |_| {}).await;
    for pass in ["summarize", "purpose", "contract"] {
        let calls = seen(&log, pass);
        assert_eq!(calls.len(), 1, "{pass}");
        assert_eq!(calls[0].grounding, None, "{pass}");
        assert_eq!(calls[0].content, FUNC_SOURCE, "{pass}");
    }
}

#[tokio::test]
async fn an_adapter_that_knows_nothing_of_grounding_still_runs_and_never_sees_the_block() {
    let db = toy_world();
    let (adapter, log) = Plain::new();
    let llm = Arc::new(RecordingProvider::new());
    run_passes(&db, adapter, llm.clone(), ALL_THREE.to_vec(), |o| {
        o.grounding = Some(GroundingMode::Full);
    })
    .await;

    // The passes completed and stored their artifacts.
    assert_eq!(db.purpose_list(GAME).unwrap().len(), 1);
    assert_eq!(db.contract_list(GAME).unwrap().iter().filter(|c| c.entity_id == FUNC_ID).count(), 1);
    assert!(db.summary_list(GAME).unwrap().iter().any(|s| s.depth == "function"));
    // The defaulted grounded methods drop the block: no header in any prompt.
    assert!(llm.calls() >= 3);
    for pass in ["summarize", "purpose", "contract"] {
        assert!(llm.prompts(pass, "").iter().all(|p| !p.contains(HEADER)), "{pass}");
    }
    let content_seen: Vec<_> = log.lock().unwrap().iter().map(|s| s.content.clone()).collect();
    assert!(content_seen.iter().any(|c| c == FUNC_SOURCE), "{content_seen:?}");
}

// ── estimate ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn estimate_counts_the_block_and_annotated_source_in_prompt_length() {
    let db = toy_world();
    let g = ground(&db, &function_entity(&db), FUNC_SOURCE, GroundingMode::Full);
    let annotated = g.annotated_content.clone().unwrap();

    let estimate = |mode: GroundingMode| {
        let db = db.clone();
        async move {
            let (adapter, _log) = Grounded::new();
            let llm: Arc<dyn LlmProvider> =
                Arc::new(EstimatingProvider::new(Arc::new(DryRunProvider::new()), 0));
            let res = run_passes(&db, adapter, llm, vec![Pass::Purpose], |o| {
                o.estimate = true;
                o.grounding = Some(mode);
            })
            .await;
            res.estimates.iter().find(|e| e.pass == "purpose").unwrap().input_tokens
        }
    };

    let none = estimate(GroundingMode::None).await;
    let full = estimate(GroundingMode::Full).await;
    assert_eq!(none, estimate_input_tokens(FUNC_SOURCE.len(), 0));
    assert_eq!(full, estimate_input_tokens(g.block.len() + 1 + annotated.len(), 0));
    assert!(full > none);
}

// ── Layer-2 cache ────────────────────────────────────────────────────────────

fn shape_of(db: &Arc<SqliteBackend>, e: &Entity) -> String {
    file_shapes_by_uri(&db.entity_list(GAME).unwrap())
        .get(&e.first_location.as_ref().unwrap().uri())
        .cloned()
        .unwrap_or_default()
}

/// The keys the passes used before grounding existed.
fn legacy_keys(db: &Arc<SqliteBackend>, e: &Entity) -> Vec<Layer2CacheKey> {
    let loc = e.first_location.as_ref().unwrap();
    let chunk = db.chunk_get_by_uri(&loc.uri()).unwrap().unwrap();
    let key = |kind: &str, id: String, content_hash: String, shape: String| Layer2CacheKey {
        artifact_kind: kind.into(),
        entity_id: Some(id),
        content_hash,
        file_shape_hash: shape,
        model: "recording".into(),
        stable_sampling: false,
    };
    vec![
        key("purpose", e.id.clone(), String::new(), shape_of(db, e)),
        key("contract", e.id.clone(), String::new(), shape_of(db, e)),
        key("summary", format!("function:{}", chunk.id), hash_content(&chunk.content), String::new()),
    ]
}

fn cached(db: &Arc<SqliteBackend>, k: &Layer2CacheKey) -> bool {
    db.layer2_cache_get(k).unwrap().is_some()
}

#[tokio::test]
async fn ungrounded_entities_keep_exactly_the_cache_keys_they_always_had() {
    // Explicit none on a corpus full of facts.
    let db = toy_world();
    let (adapter, _) = Grounded::new();
    run_passes(&db, adapter, Arc::new(RecordingProvider::new()), ALL_THREE.to_vec(), |o| {
        o.grounding = Some(GroundingMode::None);
    })
    .await;
    for k in legacy_keys(&db, &function_entity(&db)) {
        assert!(cached(&db, &k), "missing legacy key {k:?}");
    }

    // The default rule on a corpus with no facts at all.
    let bare = bare_game();
    let e = add_function_with_source(bare.as_ref(), FUNC, Some(0x4000), FUNC_SOURCE);
    let (adapter, _) = Grounded::new();
    run_passes(&bare, adapter, Arc::new(RecordingProvider::new()), ALL_THREE.to_vec(), |_| {}).await;
    for k in legacy_keys(&bare, &e) {
        assert!(cached(&bare, &k), "missing legacy key {k:?}");
    }
}

#[tokio::test]
async fn a_grounded_run_stores_under_different_keys_than_an_ungrounded_one() {
    let db = toy_world();
    let (adapter, _) = Grounded::new();
    run_passes(&db, adapter, Arc::new(RecordingProvider::new()), ALL_THREE.to_vec(), |o| {
        o.grounding = Some(GroundingMode::Full);
    })
    .await;
    for k in legacy_keys(&db, &function_entity(&db)) {
        assert!(!cached(&db, &k), "grounded output must not sit under the legacy key {k:?}");
    }
}

#[tokio::test]
async fn rerunning_with_unchanged_facts_is_served_from_the_cache() {
    let db = toy_world();
    let (adapter, log) = Grounded::new();
    let llm = Arc::new(RecordingProvider::new());
    for _ in 0..2 {
        run_passes(&db, adapter.clone(), llm.clone(), ALL_THREE.to_vec(), |o| {
            o.grounding = Some(GroundingMode::Full);
            o.full = true;
        })
        .await;
    }
    for pass in ["summarize", "purpose", "contract"] {
        assert_eq!(seen(&log, pass).len(), 1, "{pass} derived once, then cached");
    }
}

#[tokio::test]
async fn changing_a_fact_the_function_depends_on_rederives_its_artifacts() {
    let db = toy_world();
    let (adapter, log) = Grounded::new();
    let llm = Arc::new(RecordingProvider::new());
    let run = || {
        run_passes(&db, adapter.clone(), llm.clone(), ALL_THREE.to_vec(), |o| {
            o.grounding = Some(GroundingMode::Full);
            o.full = true;
        })
    };
    run().await;

    // The callee's role was refined.
    import_facts(&db, GAME, vec![routine_fact("unpack a blob into RAM, byte-swapped")]);
    run().await;

    for pass in ["summarize", "purpose", "contract"] {
        let calls = seen(&log, pass);
        assert_eq!(calls.len(), 2, "{pass} is derived again");
        let latest = calls[1].grounding.as_deref().unwrap();
        assert!(latest.contains("unpack a blob into RAM, byte-swapped"), "{pass}: {latest}");
        assert!(!latest.contains("decompress a blob"), "{pass}: {latest}");
    }
}

#[tokio::test]
async fn adding_a_fact_the_function_never_touches_does_not_rederive_it() {
    let db = toy_world();
    let (adapter, log) = Grounded::new();
    let llm = Arc::new(RecordingProvider::new());
    let run = || {
        run_passes(&db, adapter.clone(), llm.clone(), ALL_THREE.to_vec(), |o| {
            o.grounding = Some(GroundingMode::Full);
            o.full = true;
        })
    };
    run().await;
    add_entity(db.as_ref(), GAME, "RAM_ff0010", "variable", Some(0xff0010));
    import_facts(&db, GAME, vec![var_fact("var.counter", "Frame counter", "confirmed", "0xFF0010", "wram")]);
    run().await;
    for pass in ["summarize", "purpose", "contract"] {
        assert_eq!(seen(&log, pass).len(), 1, "{pass} stays cached");
    }
}
