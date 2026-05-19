//! Integration tests for `CodeAdapter::extract_contract` covering the LLM
//! response path, including the regression for parse-failure handling.
//!
//! Background: the `Err(_)` arm of the JSON parse step previously leaked
//! raw LLM response text (including markdown fence markers) into the
//! `risks` field of the returned contract.  These tests document the
//! correct behaviour and would fail if that bug were re-introduced.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use callimachus_adapter_code::CodeAdapter;
use callimachus_core::adapter::SourceAdapter;
use callimachus_core::types::{Entity, Location};
use callimachus_llm::{
    CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderUsage,
};

// ── Stub LLM ─────────────────────────────────────────────────────────────────

/// A minimal `LlmProvider` that returns a fixed response string for every
/// completion request.  Call counts are tracked via `usage()`.
struct StubLlm {
    response_text: String,
    usage: Arc<Mutex<ProviderUsage>>,
}

impl StubLlm {
    fn new(text: impl Into<String>) -> Self {
        Self {
            response_text: text.into(),
            usage: Arc::new(Mutex::new(ProviderUsage::default())),
        }
    }
}

#[async_trait]
impl LlmProvider for StubLlm {
    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        self.usage.lock().unwrap().calls += 1;
        Ok(CompletionResponse {
            text: self.response_text.clone(),
            input_tokens: 0,
            output_tokens: 0,
            model_used: "stub".to_string(),
        })
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    fn usage(&self) -> ProviderUsage {
        self.usage.lock().unwrap().clone()
    }

    fn reset_usage(&self) {
        *self.usage.lock().unwrap() = ProviderUsage::default();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a minimal `Entity` whose URI ends in `.rs`, so `extract_contract`
/// routes to the Rust static analyser.
fn rust_entity(name: &str) -> Entity {
    let mut e = Entity::new(
        "ent-1".to_string(),
        "corpus-1".to_string(),
        name.to_string(),
        "function".to_string(),
    );
    e.first_location = Some(Location::new("corpus-1", "src/lib.rs"));
    e
}

/// A public, fallible Rust function used as the source snippet in both tests.
/// `analyze_rust` will set `is_public=true` and `is_fallible=true` for it.
const SOURCE: &str = r#"pub fn do_thing(x: i32) -> Result<i32, String> {
    if x < 0 { Err("neg".to_string()) } else { Ok(x) }
}"#;

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Regression test: when the LLM returns a markdown fence wrapping *invalid*
/// JSON, the `Err` arm must NOT propagate raw text into the `risks` vector.
#[tokio::test]
async fn parse_failure_does_not_leak_raw_text_into_risks() {
    let bad_json = "```json\n{this is not valid json at all,}\n```";
    let llm = StubLlm::new(bad_json);
    let adapter = CodeAdapter;
    let entity = rust_entity("do_thing");

    let result = adapter
        .extract_contract(&entity, SOURCE, None, None, &serde_json::json!({}), &llm)
        .await
        .expect("extract_contract should not error");

    let c = result.expect("should return Some contract");

    // Core regression assertion: raw LLM text must not appear in risks.
    assert!(
        c.risks.is_empty(),
        "risks must be empty on parse failure, got: {:?}",
        c.risks
    );

    // Defensive check: no risk should start with a fence marker.
    assert!(
        c.risks.iter().all(|r| !r.starts_with("```")),
        "no risk entry should contain raw fence markers"
    );

    // LLM-inferred fields default on parse failure.
    assert!(
        c.assumptions.is_empty(),
        "assumptions must be empty on parse failure"
    );
    assert!(
        c.intent_gap.is_none(),
        "intent_gap must be None on parse failure"
    );
    assert!(
        c.caller_notes.is_none(),
        "caller_notes must be None on parse failure"
    );
}

/// Happy-path test: when the LLM returns a markdown fence wrapping *valid*
/// JSON, the fields are parsed correctly and fences are stripped.
#[tokio::test]
async fn valid_json_in_fence_is_parsed() {
    let good_json = "```json\n{\
        \"assumptions\":[\"x must be small\"],\
        \"risks\":[\"overflow on large x\"],\
        \"intent_gap\":null,\
        \"caller_notes\":\"call only after init\",\
        \"verified_by_names\":[],\
        \"discards_result_callees\":[]\
    }\n```";

    let llm = StubLlm::new(good_json);
    let adapter = CodeAdapter;
    let entity = rust_entity("do_thing");

    let result = adapter
        .extract_contract(&entity, SOURCE, None, None, &serde_json::json!({}), &llm)
        .await
        .expect("extract_contract should not error");

    let c = result.expect("should return Some contract");

    assert_eq!(c.assumptions, vec!["x must be small"]);
    assert_eq!(c.risks, vec!["overflow on large x"]);

    // The fence must have been stripped before parsing — no markers in risks.
    assert!(
        c.risks.iter().all(|r| !r.starts_with("```")),
        "fence markers must not appear in parsed risks"
    );

    assert_eq!(c.caller_notes.as_deref(), Some("call only after init"));
    assert!(c.intent_gap.is_none());
}
