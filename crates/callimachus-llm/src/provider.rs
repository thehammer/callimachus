use std::sync::Arc;

use crate::{
    budget::TokenBudget,
    concurrency::{AdaptiveLimiter, ConcurrencyStats},
    error::{LlmError, Result},
};

/// Maps an LLM model name (as returned by `LlmClient::name()`) to a coarse
/// tier label used by storage to order artifacts by quality.
/// Returns one of: `"haiku"`, `"sonnet"`, `"opus"`, `"unknown"`.
pub fn model_tier(model_name: &str) -> &'static str {
    let lc = model_name.to_lowercase();
    if lc.contains("opus") {
        "opus"
    } else if lc.contains("sonnet") {
        "sonnet"
    } else if lc.contains("haiku") {
        "haiku"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tier_tests {
    use super::model_tier;

    #[test]
    fn opus_variants() {
        assert_eq!(model_tier("claude-opus-4-20250514"), "opus");
        assert_eq!(model_tier("claude-opus-4-5"), "opus");
        assert_eq!(model_tier("Claude-Opus-3"), "opus"); // mixed case
    }

    #[test]
    fn sonnet_variants() {
        assert_eq!(model_tier("claude-sonnet-4-5-20250929"), "sonnet");
        assert_eq!(model_tier("Claude-Sonnet-3-5"), "sonnet");
    }

    #[test]
    fn haiku_variants() {
        assert_eq!(model_tier("claude-haiku-4-5-20251001"), "haiku");
        assert_eq!(model_tier("Claude-Haiku-3"), "haiku");
    }

    #[test]
    fn unknown_models() {
        assert_eq!(model_tier("gpt-4"), "unknown");
        assert_eq!(model_tier("unknown"), "unknown");
        assert_eq!(model_tier(""), "unknown");
        assert_eq!(model_tier("dry-run"), "unknown");
    }
}

#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse>;
    fn name(&self) -> &str;
    /// Whether this provider supports concurrent calls safely.
    fn supports_parallel(&self) -> bool;
    fn usage(&self) -> ProviderUsage;
    fn reset_usage(&self);

    /// Generate an embedding vector for a single text input.
    /// Returns `Err` if the provider does not support embeddings.
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(LlmError::Other(
            "embeddings not supported by this provider".into(),
        ))
    }

    /// Whether this provider supports embedding generation.
    fn supports_embeddings(&self) -> bool {
        false
    }

    /// Return a variant of this provider that uses `model` as its default,
    /// sharing the same connection pool and usage accounting `Arc` with the
    /// parent.
    ///
    /// Returns `None` for providers that don't support per-model variants
    /// (e.g. `DryRunProvider`, `ClaudeCodeProvider`).  The pipeline uses this
    /// to build tier variants for `AnthropicApiProvider` while degrading
    /// gracefully for other providers.
    fn with_model_override(&self, _model: &str) -> Option<Arc<dyn LlmProvider>> {
        None
    }

    /// Probe the provider's rate limits so the token budget is seeded for
    /// this model's family before the first real pass begins.
    ///
    /// The default implementation is a no-op.  `AnthropicApiProvider` overrides
    /// this to make a minimal 1-token request so the response headers can seed
    /// the budget's initial window before `buffer_unordered` is sized.
    async fn probe_rate_limits(&self) {}

    /// Return the shared [`AdaptiveLimiter`] for this provider, if any.
    ///
    /// Only `AnthropicApiProvider` returns `Some`.  All other providers
    /// return `None`.
    fn concurrency_limiter(&self) -> Option<Arc<AdaptiveLimiter>> {
        None
    }

    /// Return the shared [`TokenBudget`] for this provider, if any.
    ///
    /// Only `AnthropicApiProvider` returns `Some`.  All other providers
    /// return `None` and pipeline falls back to fixed default width.
    fn budget(&self) -> Option<Arc<TokenBudget>> {
        None
    }

    /// Snapshot of concurrency stats accumulated since the last [`AdaptiveLimiter::reset`].
    fn concurrency_stats(&self) -> Option<ConcurrencyStats> {
        None
    }
}

/// Blanket impl so `Box<dyn LlmProvider>` can be used wherever `LlmProvider` is required.
#[async_trait::async_trait]
impl LlmProvider for Box<dyn LlmProvider> {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        (**self).complete(req).await
    }
    fn name(&self) -> &str {
        (**self).name()
    }
    fn supports_parallel(&self) -> bool {
        (**self).supports_parallel()
    }
    fn usage(&self) -> ProviderUsage {
        (**self).usage()
    }
    fn reset_usage(&self) {
        (**self).reset_usage()
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        (**self).embed(text).await
    }
    fn supports_embeddings(&self) -> bool {
        (**self).supports_embeddings()
    }
    fn with_model_override(&self, model: &str) -> Option<Arc<dyn LlmProvider>> {
        (**self).with_model_override(model)
    }
    async fn probe_rate_limits(&self) {
        (**self).probe_rate_limits().await
    }
    fn concurrency_limiter(&self) -> Option<Arc<AdaptiveLimiter>> {
        (**self).concurrency_limiter()
    }
    fn budget(&self) -> Option<Arc<TokenBudget>> {
        (**self).budget()
    }
    fn concurrency_stats(&self) -> Option<ConcurrencyStats> {
        (**self).concurrency_stats()
    }
}

#[derive(Debug)]
pub struct CompletionRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    /// For logging/attribution only.
    pub chunk_id: Option<String>,
    /// Entity or chunk kind (e.g. `"function"`, `"class"`, `"summarize"`).
    /// Used as part of the [`crate::budget::EstimatorKey`] for per-kind
    /// token-size learning.
    pub kind: String,
    /// Pipeline pass name (e.g. `"purpose"`, `"contract"`, `"summarize"`).
    /// Used as part of the [`crate::budget::EstimatorKey`].
    pub pass: String,
}

#[derive(Debug)]
pub struct CompletionResponse {
    pub text: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub model_used: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Always 0.0 for subscription mode.
    pub cost_usd: f64,
    pub calls: u64,
}
