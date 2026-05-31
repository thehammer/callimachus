use crate::error::{LlmError, Result};

/// An embedding provider turns text into dense float vectors.
///
/// Implementations must be cheap to clone behind an `Arc` and safe to call
/// concurrently. The batch entry point is the primary one; `embed` is a
/// single-text convenience that defaults to a one-element batch.
#[async_trait::async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a batch of texts in a single call. Returns one vector per input,
    /// in input order. Errors if the provider cannot satisfy the whole batch.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Embed a single text. Default impl delegates to `embed_batch`.
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed_batch(&[text.to_string()]).await?;
        out.pop().ok_or_else(|| {
            LlmError::Other(
                "embedding provider returned no vector for single input".into(),
            )
        })
    }

    /// The model identifier written to storage and used as the Layer-2 cache
    /// key (e.g. `"voyage-code-3"`). Must be the real model name, not a label.
    fn name(&self) -> &str;
}

/// Blanket impl so `Box<dyn EmbeddingProvider>` works where the trait is required.
#[async_trait::async_trait]
impl EmbeddingProvider for Box<dyn EmbeddingProvider> {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        (**self).embed_batch(texts).await
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        (**self).embed(text).await
    }
    fn name(&self) -> &str {
        (**self).name()
    }
}
