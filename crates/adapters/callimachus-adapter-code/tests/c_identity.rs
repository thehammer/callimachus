//! Guards that the `analyse_c` refactor and the prompt options leave plain `.c`
//! corpora untouched: same structure, byte-identical prompts.

use std::sync::Mutex;

use callimachus_adapter_code::{CodeAdapter, analyse_c, extractor::extract_structure};
use callimachus_adapter_contract::{Entity, SourceAdapter};
use callimachus_core::types::{Chunk, Location};
use callimachus_llm::{
    CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderUsage,
};

const SRC: &str = "void FUN_00001c04(void)\n{\n  RAM_ff0010 = IO_c00004;\n  FUN_00002000();\n  PTR_DAT_00012000[1] = 0;\n}\n";

fn chunk() -> Chunk {
    let mut c = Chunk::new(
        "corp".into(),
        None,
        "function".into(),
        Location::new("corp", "src/FUN_00001c04.c"),
        SRC.into(),
    );
    c.language = Some("c".into());
    c
}

#[test]
fn analyse_c_is_what_extract_structure_returns_for_c() {
    let c = chunk();
    let lang = callimachus_adapter_code::languages::for_extension("c").unwrap();
    let via_extract = extract_structure(&c, lang).unwrap();
    let direct = analyse_c(&c, &c.content).unwrap();
    let norm = |mut s: callimachus_adapter_code::ExtractedCodeStructure| {
        let mut e: Vec<String> = s.entities.drain(..).map(|e| format!("{e:?}")).collect();
        let mut d: Vec<String> = s
            .edges
            .drain(..)
            .map(|e| format!("{:?}", (e.id, e.kind, e.occurrence_count)))
            .collect();
        e.sort();
        d.sort();
        (e, d)
    };
    assert_eq!(norm(via_extract), norm(direct));
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl LlmProvider for Recorder {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        self.0.lock().unwrap().push(req.prompt);
        Ok(CompletionResponse {
            text: "ok".into(),
            input_tokens: 0,
            output_tokens: 0,
            model_used: "rec".into(),
        })
    }
    fn name(&self) -> &str {
        "rec"
    }
    fn supports_parallel(&self) -> bool {
        true
    }
    fn usage(&self) -> ProviderUsage {
        ProviderUsage::default()
    }
    fn reset_usage(&self) {}
}

#[tokio::test]
async fn c_summary_prompt_is_byte_identical_to_the_original_template() {
    let rec = Recorder::default();
    CodeAdapter::new()
        .summarize(&chunk(), &rec, "function")
        .await
        .unwrap();
    // The prompt as built before the prompt-options change.
    let expected = format!(
        r#"You are summarizing a code chunk for a searchable index.

Language: C
Symbol: FUN_00001c04.c
Kind: function (function|class|interface|module)

Code:
<code>
{SRC}
</code>

Write a 1-3 sentence summary of what this function does. Focus on purpose and behavior,
not implementation details. If the code is self-explanatory from its name, a single
sentence is sufficient.

Return ONLY the summary text, no JSON, no preamble."#
    );
    assert_eq!(rec.0.lock().unwrap()[0], expected);
}

#[tokio::test]
async fn c_purpose_and_contract_prompts_have_no_disassembly_line() {
    let rec = Recorder::default();
    let a = CodeAdapter::new();
    let mut e = Entity::new(
        "corp:f".into(),
        "corp".into(),
        "FUN_00001c04".into(),
        "function".into(),
    );
    e.first_location = Some(Location::new("corp", "src/FUN_00001c04.c"));
    // Content mentioning the marker must not trigger the hint on a plain adapter.
    let content = format!("{SRC}/* ---- disassembly ---- */\n");
    a.extract_purpose(&e, &content, None, &rec).await.unwrap();
    a.extract_contract(&e, &content, None, None, &serde_json::json!({}), &rec)
        .await
        .unwrap();
    for p in rec.0.lock().unwrap().iter() {
        assert!(!p.contains("disassembly of the same routine"), "{p}");
    }
}
