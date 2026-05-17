use crate::error::{LlmError, Result};

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
}

#[derive(Debug)]
pub struct CompletionRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    /// For logging/attribution only.
    pub chunk_id: Option<String>,
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
