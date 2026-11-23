//! An `LlmProvider` that records every prompt and answers `[dry-run]`, so a test
//! can read exactly what a pass sent. No network, no real model.
#![allow(dead_code)]

use std::sync::Mutex;

use callimachus_llm::{
    CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage, error::Result,
};

#[derive(Default)]
pub struct RecordingProvider {
    /// `(pass, prompt)` per call, in arrival order.
    seen: Mutex<Vec<(String, String)>>,
}

impl RecordingProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    pub fn clear(&self) {
        self.seen.lock().unwrap().clear();
    }

    /// Prompts of one pass (`purpose`, `contract`, `summarize`) that mention `needle`.
    pub fn prompts(&self, pass: &str, needle: &str) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, q)| p == pass && q.contains(needle))
            .map(|(_, q)| q.clone())
            .collect()
    }

    /// The single prompt of `pass` that mentions `needle`.
    pub fn prompt(&self, pass: &str, needle: &str) -> String {
        let mut found = self.prompts(pass, needle);
        assert_eq!(
            found.len(),
            1,
            "expected exactly one {pass} prompt mentioning {needle:?}, got {}",
            found.len()
        );
        found.remove(0)
    }
}

#[async_trait::async_trait]
impl LlmProvider for RecordingProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        self.seen
            .lock()
            .unwrap()
            .push((req.pass.clone(), req.prompt));
        Ok(CompletionResponse {
            text: "[dry-run]".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model_used: "recording".to_string(),
        })
    }

    fn name(&self) -> &str {
        "recording"
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    fn usage(&self) -> ProviderUsage {
        ProviderUsage::default()
    }

    fn reset_usage(&self) {}
}
