//! The real code adapter inside grounded passes: what the summarize, purpose
//! and contract prompts actually contain once the fact base is consulted, and
//! that corpora without facts get exactly the prompts they always did.
//!
//! The world is the fictional `toy16` fixture shared with the core tests; the
//! LLM is a recording provider, so nothing leaves the process.

#[path = "../../../callimachus-core/tests/common/grounding_fixture.rs"]
mod fixture;
#[path = "../../../callimachus-core/tests/common/recording_provider.rs"]
mod recording;

use std::sync::Arc;

use callimachus_adapter_code::CodeAdapter;
use callimachus_adapter_contract::SourceAdapter;
use callimachus_core::indexing::grounding::{GroundingContext, GroundingMode, build};
use callimachus_core::indexing::pipeline::{IndexOptions, IndexPipeline};
use callimachus_core::storage::{SqliteBackend, StorageBackend};
use callimachus_core::types::Pass;
use fixture::*;
use recording::RecordingProvider;

const SUMMARY_NEEDLE: &str = "Symbol: FUN_00004000";
const ENTITY_NEEDLE: &str = "Entity: FUN_00004000";

async fn run(
    db: &Arc<SqliteBackend>,
    llm: &Arc<RecordingProvider>,
    passes: Vec<Pass>,
    grounding: Option<GroundingMode>,
) {
    IndexPipeline {
        db: db.clone(),
        adapter: Arc::new(CodeAdapter::new()),
        llm: llm.clone(),
        embedder: None,
    }
    .run(
        &game_corpus(db),
        IndexOptions {
            passes,
            concurrency: Some(2),
            grounding,
            ..IndexOptions::default()
        },
    )
    .await
    .unwrap();
}

/// The three prompts the passes sent for `FUN_00004000`.
struct Prompts {
    summarize: String,
    purpose: String,
    contract: String,
}

async fn grounded_prompts(mode: Option<GroundingMode>) -> Prompts {
    let db = toy_world();
    let llm = Arc::new(RecordingProvider::new());
    run(
        &db,
        &llm,
        vec![Pass::Summarize, Pass::Purpose, Pass::Contract],
        mode,
    )
    .await;
    Prompts {
        summarize: llm.prompt("summarize", SUMMARY_NEEDLE),
        purpose: llm.prompt("purpose", ENTITY_NEEDLE),
        contract: llm.prompt("contract", ENTITY_NEEDLE),
    }
}

fn find(prompt: &str, needle: &str) -> usize {
    prompt
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in prompt:\n{prompt}"))
}

// ── the block and the annotated source, as the code adapter renders them ─────

#[test]
fn the_code_adapter_names_register_fields_in_the_annotated_source() {
    let db = toy_world();
    let adapter: Arc<dyn SourceAdapter> = Arc::new(CodeAdapter::new());
    let ctx = GroundingContext::load(
        db.clone(),
        &game_corpus(&db),
        adapter,
        Some(GroundingMode::Full),
    )
    .unwrap();
    let g = build(
        &ctx,
        &function_entity(&db),
        FUNC_SOURCE,
        GroundingMode::Full,
    );

    let annotated = g.annotated_content.expect("annotated source in full mode");
    assert!(
        annotated.contains("unaff_A2->x /* obj+0x10 s16 */"),
        "{annotated}"
    );
    assert!(!annotated.contains("unaff_A2 + 0x10"), "{annotated}");
    assert_eq!(g.block.lines().next(), Some(HEADER));
    assert_eq!(
        lines_of(&g.block),
        vec![
            "- reads DAT_00a10001 = Version port (reference, confirmed). Variant: region bits 7-6 select 50/60 Hz and language (confirmed)",
            "- writes unaff_A2+0x10 = obj.x: x position, whole pixels (s16, confirmed)",
            "- calls FUN_00001000 = decompress a blob into RAM (confirmed)",
            "- constant 0x2c0 = pickup tile (confirmed)",
        ]
    );
}

// ── prompts of the three passes ──────────────────────────────────────────────

#[tokio::test]
async fn every_grounded_prompt_carries_the_block_between_kind_and_the_code() {
    let p = grounded_prompts(Some(GroundingMode::Full)).await;
    for (pass, prompt) in [
        ("summarize", &p.summarize),
        ("purpose", &p.purpose),
        ("contract", &p.contract),
    ] {
        let (kind, header, code) = (
            find(prompt, "Kind:"),
            find(prompt, HEADER),
            find(prompt, "<code>"),
        );
        assert!(
            kind < header && header < code,
            "{pass}: Kind@{kind} header@{header} code@{code}\n{prompt}"
        );
        assert!(prompt.contains("- reads DAT_00a10001 = Version port (reference, confirmed). Variant: region bits 7-6 select 50/60 Hz and language (confirmed)"), "{pass}");
        assert!(
            prompt.contains(
                "- writes unaff_A2+0x10 = obj.x: x position, whole pixels (s16, confirmed)"
            ),
            "{pass}"
        );
        assert!(
            prompt.contains("- calls FUN_00001000 = decompress a blob into RAM (confirmed)"),
            "{pass}"
        );
        assert!(
            prompt.contains("- constant 0x2c0 = pickup tile (confirmed)"),
            "{pass}"
        );
        assert!(
            prompt.contains(INSTRUCTION),
            "{pass}: the instruction to use the verified names"
        );
    }
}

#[tokio::test]
async fn full_mode_prompts_show_the_code_with_fields_named() {
    let p = grounded_prompts(Some(GroundingMode::Full)).await;
    for (pass, prompt) in [
        ("summarize", &p.summarize),
        ("purpose", &p.purpose),
        ("contract", &p.contract),
    ] {
        let code = &prompt[find(prompt, "<code>")..];
        assert!(code.contains("unaff_A2->x"), "{pass}: {code}");
        assert!(
            !code.contains("*(short *)(unaff_A2 + 0x10)"),
            "{pass}: {code}"
        );
    }
}

#[tokio::test]
async fn facts_mode_prompts_carry_the_block_but_the_code_is_untouched() {
    let p = grounded_prompts(Some(GroundingMode::Facts)).await;
    for (pass, prompt) in [
        ("summarize", &p.summarize),
        ("purpose", &p.purpose),
        ("contract", &p.contract),
    ] {
        assert!(prompt.contains(HEADER), "{pass}");
        assert!(prompt.contains(INSTRUCTION), "{pass}");
        assert!(prompt.contains("*(short *)(unaff_A2 + 0x10)"), "{pass}");
        assert!(!prompt.contains("unaff_A2->x"), "{pass}");
    }
}

#[tokio::test]
async fn the_default_rule_grounds_a_corpus_that_has_facts() {
    let p = grounded_prompts(None).await;
    for (pass, prompt) in [
        ("summarize", &p.summarize),
        ("purpose", &p.purpose),
        ("contract", &p.contract),
    ] {
        assert!(prompt.contains(HEADER), "{pass}");
        assert!(prompt.contains("unaff_A2->x"), "{pass}");
    }
}

// ── ungrounded corpora are untouched ─────────────────────────────────────────

/// Run the three passes with `mode`, then ask the adapter directly for the
/// ungrounded prompts the same inputs would have produced, and compare.
async fn assert_prompts_match_the_ungrounded_adapter(
    db: &Arc<SqliteBackend>,
    mode: Option<GroundingMode>,
    name: &str,
) {
    let entity = db
        .entity_get_by_id(&format!("{GAME}:{}", name.to_lowercase()))
        .unwrap()
        .unwrap();
    let chunk = db
        .chunk_get_by_uri(&entity.first_location.as_ref().unwrap().uri())
        .unwrap()
        .unwrap();
    let language = chunk.language.clone().unwrap();

    // Each pass on its own, in an order that keeps its inputs empty: contract
    // sees no purpose or summary yet, purpose sees no summary.
    let llm = Arc::new(RecordingProvider::new());
    run(db, &llm, vec![Pass::Contract], mode).await;
    run(db, &llm, vec![Pass::Purpose], mode).await;
    run(db, &llm, vec![Pass::Summarize], mode).await;

    let direct = RecordingProvider::new();
    let adapter = CodeAdapter::new();
    adapter
        .extract_contract(
            &entity,
            &chunk.content,
            None,
            None,
            &serde_json::json!({"language": language, "entity_name": name}),
            &direct,
        )
        .await
        .unwrap();
    adapter
        .extract_purpose(&entity, &chunk.content, None, &direct)
        .await
        .unwrap();
    adapter
        .summarize(&chunk, &direct, "function")
        .await
        .unwrap();

    let needle = format!("Entity: {name}");
    let sym = format!("Symbol: {name}");
    assert_eq!(
        llm.prompt("contract", &needle),
        direct.prompt("contract", &needle),
        "contract"
    );
    assert_eq!(
        llm.prompt("purpose", &needle),
        direct.prompt("purpose", &needle),
        "purpose"
    );
    assert_eq!(
        llm.prompt("summarize", &sym),
        direct.prompt("summarize", &sym),
        "summarize"
    );

    for prompt in [
        llm.prompt("contract", &needle),
        llm.prompt("purpose", &needle),
        llm.prompt("summarize", &sym),
    ] {
        assert!(!prompt.contains(HEADER) && !prompt.contains(INSTRUCTION));
    }
}

#[tokio::test]
async fn grounding_none_gives_byte_identical_prompts_even_when_facts_exist() {
    let db = toy_world();
    assert_prompts_match_the_ungrounded_adapter(&db, Some(GroundingMode::None), FUNC).await;
}

#[tokio::test]
async fn a_corpus_without_facts_gets_byte_identical_prompts_by_default() {
    let db = bare_game();
    let src = "pub fn tick(frame: u32) -> u32 {\n    frame.wrapping_add(1)\n}\n";
    add_function_in_language(db.as_ref(), "tick", None, src, "rust");
    assert_prompts_match_the_ungrounded_adapter(&db, None, "tick").await;
}

#[tokio::test]
async fn a_function_with_no_known_facts_in_a_grounded_corpus_keeps_its_old_prompts() {
    let db = toy_world();
    add_function_in_language(
        db.as_ref(),
        "FUN_00009000",
        None,
        "void FUN_00009000(void)\n{\n  return;\n}\n",
        "c",
    );
    assert_prompts_match_the_ungrounded_adapter(&db, None, "FUN_00009000").await;
}
