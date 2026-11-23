//! Tests for the CLI side of grounded passes: `calli inspect grounding` and the
//! `--grounding` run-control flag. Shares the fictional `toy16` fixture with the
//! core tests.

#[path = "../../../callimachus-core/tests/common/grounding_fixture.rs"]
mod fixture;

use callimachus_core::indexing::{IndexOptions, grounding::GroundingMode};
use fixture::*;

use crate::commands::inspect::grounding_text;
use crate::commands::run_control::RunControl;

// ── calli inspect grounding ──────────────────────────────────────────────────

#[test]
fn inspect_grounding_prints_the_block_and_the_annotated_source() {
    let db = toy_world();
    let out = grounding_text(GAME, FUNC, None, db).unwrap();

    assert!(
        out.starts_with("FUN_00004000 [function] grounding=full\n"),
        "{out}"
    );
    let (block, source) = out
        .split_once("Annotated source:")
        .expect("annotated source section");
    assert!(block.contains(HEADER), "{out}");
    for line in [
        "- reads DAT_00a10001 = Version port (reference, confirmed). Variant: region bits 7-6 select 50/60 Hz and language (confirmed)",
        "- writes unaff_A2+0x10 = obj.x: x position, whole pixels (s16, confirmed)",
        "- calls FUN_00001000 = decompress a blob into RAM (confirmed)",
        "- constant 0x2c0 = pickup tile (confirmed)",
    ] {
        assert!(block.contains(line), "missing {line:?}\n{out}");
    }
    assert!(source.contains("unaff_A2->x /* obj+0x10 s16 */"), "{out}");
    assert!(source.contains("void FUN_00004000(void)"), "{out}");
}

#[test]
fn inspect_grounding_facts_mode_has_the_block_but_no_annotated_source() {
    let out = grounding_text(GAME, FUNC, Some("facts"), toy_world()).unwrap();
    assert!(out.contains("grounding=facts"), "{out}");
    assert!(out.contains(HEADER), "{out}");
    assert!(!out.contains("Annotated source:"), "{out}");
}

#[test]
fn inspect_grounding_none_mode_says_there_is_no_context() {
    let out = grounding_text(GAME, FUNC, Some("none"), toy_world()).unwrap();
    assert!(out.contains("grounding=none"), "{out}");
    assert!(out.contains("(no verified context)"), "{out}");
    assert!(
        !out.contains(HEADER) && !out.contains("Annotated source:"),
        "{out}"
    );
}

#[test]
fn inspect_grounding_defaults_to_none_for_a_corpus_without_facts() {
    let db = bare_game();
    add_function_with_source(db.as_ref(), FUNC, Some(0x4000), FUNC_SOURCE);
    let out = grounding_text(GAME, FUNC, None, db).unwrap();
    assert!(out.contains("grounding=none"), "{out}");
    assert!(out.contains("(no verified context)"), "{out}");
}

#[test]
fn inspect_grounding_rejects_an_unknown_entity_and_an_unknown_mode() {
    let err = grounding_text(GAME, "FUN_0000dead", None, toy_world())
        .unwrap_err()
        .to_string();
    assert!(err.contains("FUN_0000dead"), "{err}");

    let err = grounding_text(GAME, FUNC, Some("everything"), toy_world())
        .unwrap_err()
        .to_string();
    assert!(err.contains("everything") && err.contains("none"), "{err}");
}

// ── --grounding ──────────────────────────────────────────────────────────────

fn applied(grounding: Option<&str>) -> anyhow::Result<IndexOptions> {
    let mut opts = IndexOptions::default();
    RunControl {
        grounding: grounding.map(String::from),
        ..Default::default()
    }
    .apply(&mut opts)?;
    Ok(opts)
}

#[test]
fn the_grounding_flag_sets_the_index_option() {
    assert_eq!(
        applied(Some("none")).unwrap().grounding,
        Some(GroundingMode::None)
    );
    assert_eq!(
        applied(Some("facts")).unwrap().grounding,
        Some(GroundingMode::Facts)
    );
    assert_eq!(
        applied(Some("full")).unwrap().grounding,
        Some(GroundingMode::Full)
    );
}

#[test]
fn without_the_flag_the_corpus_default_applies() {
    assert_eq!(applied(None).unwrap().grounding, None);
}

#[test]
fn a_bad_grounding_value_is_an_error_naming_it() {
    let err = applied(Some("loud")).unwrap_err().to_string();
    assert!(err.contains("loud"), "{err}");
}
