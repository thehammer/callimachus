//! A provider that sends nothing: it records how big each request would have
//! been and returns the `"[dry-run]"` stub, so a pass can render its real
//! prompts for a cost estimate without spending quota.
//!
//! The provider reports the wrapped provider's `name()` so the passes'
//! "artifact already exists for this model" checks behave exactly as in a real
//! run, and per-model variants (tier routing) share one set of counters.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::{
    error::Result,
    provider::{CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage},
};

/// Fixed per-call input overhead of the lean `claude` CLI invocation
/// (system prompt, tool-free preamble), in tokens.
pub const CLAUDE_CODE_CALL_OVERHEAD_TOKENS: u64 = 700;
/// The API provider adds no hidden per-call input.
pub const API_CALL_OVERHEAD_TOKENS: u64 = 0;

const CHARS_PER_TOKEN: f64 = 3.5;
/// Assumed fraction of `max_tokens` actually produced.
const OUTPUT_FRACTION: f64 = 0.5;
/// Used when a request carries no `max_tokens`.
const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Estimated input tokens for a prompt of `chars` characters.
pub fn estimate_input_tokens(chars: usize, overhead: u64) -> u64 {
    (chars as f64 / CHARS_PER_TOKEN).ceil() as u64 + overhead
}

/// Estimated output tokens for a request with the given `max_tokens`.
pub fn estimate_output_tokens(max_tokens: Option<u32>) -> u64 {
    (f64::from(max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)) * OUTPUT_FRACTION).round() as u64
}

#[derive(Debug, Default)]
struct Counters {
    calls: AtomicU64,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
}

pub struct EstimatingProvider {
    inner: Arc<dyn LlmProvider>,
    overhead: u64,
    counters: Arc<Counters>,
}

impl EstimatingProvider {
    /// Wrap `inner` (used only for its name and model variants). `overhead` is
    /// the per-call input overhead in tokens.
    pub fn new(inner: Arc<dyn LlmProvider>, overhead: u64) -> Self {
        Self {
            inner,
            overhead,
            counters: Arc::new(Counters::default()),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for EstimatingProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        self.counters.calls.fetch_add(1, Ordering::SeqCst);
        self.counters.input_tokens.fetch_add(
            estimate_input_tokens(req.prompt.len(), self.overhead),
            Ordering::SeqCst,
        );
        self.counters
            .output_tokens
            .fetch_add(estimate_output_tokens(req.max_tokens), Ordering::SeqCst);
        Ok(CompletionResponse {
            text: "[dry-run]".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model_used: self.inner.name().to_string(),
        })
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    /// Accumulated *estimated* usage; `calls` counts prompts that would be sent.
    fn usage(&self) -> ProviderUsage {
        ProviderUsage {
            input_tokens: self.counters.input_tokens.load(Ordering::SeqCst),
            output_tokens: self.counters.output_tokens.load(Ordering::SeqCst),
            calls: self.counters.calls.load(Ordering::SeqCst),
            ..ProviderUsage::default()
        }
    }

    fn reset_usage(&self) {
        self.counters.calls.store(0, Ordering::SeqCst);
        self.counters.input_tokens.store(0, Ordering::SeqCst);
        self.counters.output_tokens.store(0, Ordering::SeqCst);
    }

    fn with_model_override(&self, model: &str) -> Option<Arc<dyn LlmProvider>> {
        let inner = self.inner.with_model_override(model)?;
        Some(Arc::new(EstimatingProvider {
            inner,
            overhead: self.overhead,
            counters: Arc::clone(&self.counters),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DryRunProvider;

    #[tokio::test]
    async fn records_size_without_calling_inner() {
        let dry = Arc::new(DryRunProvider::new());
        let est = EstimatingProvider::new(dry.clone(), 700);
        let res = est
            .complete(CompletionRequest {
                prompt: "x".repeat(350),
                max_tokens: Some(200),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(res.text, "[dry-run]");
        assert_eq!(dry.call_count(), 0);
        let u = est.usage();
        assert_eq!(u.calls, 1);
        assert_eq!(u.input_tokens, 100 + 700);
        assert_eq!(u.output_tokens, 100);
        est.reset_usage();
        assert_eq!(est.usage().calls, 0);
    }

    #[test]
    fn name_follows_inner() {
        let est = EstimatingProvider::new(Arc::new(DryRunProvider::new()), 0);
        assert_eq!(est.name(), "dry-run");
    }
}
