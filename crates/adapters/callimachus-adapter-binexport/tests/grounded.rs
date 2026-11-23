//! The binary-export adapter with a verified-context block: the block reaches
//! the summarize, purpose and contract prompts, and the C-first trimming of
//! long routines still applies. Fictional `toy16` content; a recording provider
//! stands in for the LLM.

#[path = "../../../callimachus-core/tests/common/recording_provider.rs"]
mod recording;

use callimachus_adapter_binexport::{BinExportAdapter, content};
use callimachus_adapter_contract::SourceAdapter;
use callimachus_core::types::{Chunk, Entity, Location};
use recording::RecordingProvider;

const HEADER: &str = "Verified context from the project's fact base (prefer it over guesses; if the code contradicts a fact, say so):";
const INSTRUCTION: &str =
    "Use the names from the verified context (e.g. obj.x, the port names) in your answer.";
const BLOCK: &str = "Verified context from the project's fact base (prefer it over guesses; if the code contradicts a fact, say so):\n- reads DAT_00a10001 = Version port (reference, confirmed)\n- calls FUN_00001000 = decompress a blob into RAM (confirmed)";

/// A routine whose C is ~2,800 characters and whose disassembly is far over
/// budget, so the prompt must keep all the C and drop assembly lines.
fn long_routine() -> String {
    let mut s = format!("// F @ 0x004000\nvoid F(void)\n{{\n{}}}\n", "  x = 1;\n".repeat(350));
    s.push_str(content::DISASSEMBLY_MARKER);
    s.push('\n');
    for i in 0..400 {
        s.push_str(&format!("/* {i:06x}  move.w d0,d1 ; pad pad pad */\n"));
    }
    s
}

fn entity() -> Entity {
    Entity::new("toy:0x4000".into(), "toy".into(), "FUN_00004000".into(), "function".into())
}

fn chunk(content: String) -> Chunk {
    Chunk::new("toy".into(), None, "function".into(), Location::new("toy", "fn/00004000"), content)
}

fn assert_grounded_and_trimmed(prompt: &str, what: &str) {
    let c_part = content::split_content(&long_routine()).0.to_string();
    let (header, code) = (prompt.find(HEADER), prompt.find("<code>"));
    assert!(header.is_some() && header < code, "{what}: block before the code\n{prompt}");
    assert!(prompt.contains("- calls FUN_00001000 = decompress a blob into RAM (confirmed)"), "{what}");
    assert!(prompt.contains(INSTRUCTION), "{what}");
    // C first: every character of the C survives; the assembly is what is cut.
    assert!(prompt.contains(&c_part), "{what}: all of the C is kept");
    assert!(prompt.contains("more instructions */"), "{what}: assembly lines were dropped");
    assert!(!prompt.contains("/* 00018f"), "{what}: the last assembly lines are not sent");
}

#[tokio::test]
async fn purpose_prompt_carries_the_block_and_keeps_c_first_trimming() {
    let rec = RecordingProvider::new();
    BinExportAdapter::new()
        .extract_purpose_grounded(&entity(), &long_routine(), None, Some(BLOCK), &rec)
        .await
        .unwrap();
    assert_grounded_and_trimmed(&rec.prompt("purpose", "FUN_00004000"), "purpose");
}

#[tokio::test]
async fn contract_prompt_carries_the_block_and_keeps_c_first_trimming() {
    let rec = RecordingProvider::new();
    BinExportAdapter::new()
        .extract_contract_grounded(
            &entity(),
            &long_routine(),
            None,
            None,
            &serde_json::json!({"language": "c", "entity_name": "FUN_00004000"}),
            Some(BLOCK),
            &rec,
        )
        .await
        .unwrap();
    assert_grounded_and_trimmed(&rec.prompt("contract", "FUN_00004000"), "contract");
}

#[tokio::test]
async fn function_summary_prompt_carries_the_block_and_keeps_c_first_trimming() {
    let rec = RecordingProvider::new();
    BinExportAdapter::new()
        .summarize_grounded(&chunk(long_routine()), &rec, "function", Some(BLOCK))
        .await
        .unwrap();
    assert_grounded_and_trimmed(&rec.prompt("summarize", "Code:"), "summarize");
}

#[tokio::test]
async fn without_a_block_the_grounded_methods_give_the_prompts_they_always_gave() {
    let a = BinExportAdapter::new();
    let (plain, grounded) = (RecordingProvider::new(), RecordingProvider::new());

    a.extract_purpose(&entity(), &long_routine(), Some("a summary"), &plain).await.unwrap();
    a.extract_purpose_grounded(&entity(), &long_routine(), Some("a summary"), None, &grounded)
        .await
        .unwrap();
    a.summarize(&chunk(long_routine()), &plain, "function").await.unwrap();
    a.summarize_grounded(&chunk(long_routine()), &grounded, "function", None).await.unwrap();

    for pass in ["purpose", "summarize"] {
        let (p, g) = (plain.prompt(pass, ""), grounded.prompt(pass, ""));
        assert_eq!(p, g, "{pass}");
        assert!(!p.contains(HEADER) && !p.contains(INSTRUCTION), "{pass}");
    }
}
