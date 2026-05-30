//! A provider decorator that forces deterministic sampling.
//!
//! When `--stable-sampling` is requested, the indexing pipeline wraps each
//! Layer-2 LLM provider in a [`StableSamplingProvider`]. It sets
//! `temperature = 0.0` and a deterministic `seed` (derived from the request
//! prompt) on every completion request that does not already specify them,
//! then forwards to the inner provider.
//!
//! The seed is `sha256(prompt)[..8]` interpreted as a little-endian `u64`.
//! Because the prompt is itself a deterministic function of the entity body
//! and its surrounding file context, a prompt-derived seed yields the same
//! run-to-run reproducibility as one derived from the Layer-2 cache key, while
//! avoiding the need to thread the cache key through every adapter method.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::{
    budget::TokenBudget,
    concurrency::{AdaptiveLimiter, ConcurrencyStats},
    error::Result,
    provider::{CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage},
};

/// Wraps a provider and pins sampling parameters for deterministic output.
pub struct StableSamplingProvider {
    inner: Arc<dyn LlmProvider>,
}

impl StableSamplingProvider {
    pub fn new(inner: Arc<dyn LlmProvider>) -> Self {
        Self { inner }
    }

    /// Wrap `inner` in a `StableSamplingProvider`, returned as a trait object.
    pub fn wrap(inner: Arc<dyn LlmProvider>) -> Arc<dyn LlmProvider> {
        Arc::new(Self::new(inner))
    }
}

/// Deterministic seed derived from the request prompt.
pub fn seed_from_prompt(prompt: &str) -> u64 {
    let digest = Sha256::digest(prompt.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes)
}

#[async_trait::async_trait]
impl LlmProvider for StableSamplingProvider {
    async fn complete(&self, mut req: CompletionRequest) -> Result<CompletionResponse> {
        if req.temperature.is_none() {
            req.temperature = Some(0.0);
        }
        if req.seed.is_none() {
            req.seed = Some(seed_from_prompt(&req.prompt));
        }
        self.inner.complete(req).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn supports_parallel(&self) -> bool {
        self.inner.supports_parallel()
    }

    fn usage(&self) -> ProviderUsage {
        self.inner.usage()
    }

    fn reset_usage(&self) {
        self.inner.reset_usage()
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.inner.embed(text).await
    }

    fn supports_embeddings(&self) -> bool {
        self.inner.supports_embeddings()
    }

    fn with_model_override(&self, model: &str) -> Option<Arc<dyn LlmProvider>> {
        self.inner
            .with_model_override(model)
            .map(StableSamplingProvider::wrap)
    }

    async fn probe_rate_limits(&self) {
        self.inner.probe_rate_limits().await
    }

    fn concurrency_limiter(&self) -> Option<Arc<AdaptiveLimiter>> {
        self.inner.concurrency_limiter()
    }

    fn budget(&self) -> Option<Arc<TokenBudget>> {
        self.inner.budget()
    }

    fn concurrency_stats(&self) -> Option<ConcurrencyStats> {
        self.inner.concurrency_stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DryRunProvider;

    #[tokio::test]
    async fn sets_temperature_zero_and_seed() {
        let dry = Arc::new(DryRunProvider::new());
        let wrapped = StableSamplingProvider::new(Arc::clone(&dry) as Arc<dyn LlmProvider>);
        wrapped
            .complete(CompletionRequest {
                prompt: "hello world".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        let (temp, seed) = dry.last_sampling();
        assert_eq!(temp, Some(0.0));
        assert_eq!(seed, Some(seed_from_prompt("hello world")));
    }

    #[test]
    fn seed_is_deterministic() {
        assert_eq!(seed_from_prompt("abc"), seed_from_prompt("abc"));
        assert_ne!(seed_from_prompt("abc"), seed_from_prompt("abd"));
    }
}
