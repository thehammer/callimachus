/// OpenAI embeddings provider.
///
/// This struct implements **only** the `embed` + `supports_embeddings` methods
/// of `LlmProvider`. Calling `complete` will panic — it exists solely to
/// satisfy the trait bound when the caller needs a boxed `LlmProvider` for
/// the embed pass.
///
/// # Example
///
/// ```rust,no_run
/// use callimachus_llm::OpenAiEmbeddingProvider;
/// let provider = OpenAiEmbeddingProvider::from_env().unwrap();
/// ```
use std::sync::{Arc, Mutex};

use reqwest::Client;
use serde::Deserialize;

use crate::{
    error::{LlmError, Result},
    provider::{CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage},
};

const DEFAULT_MODEL: &str = "text-embedding-3-small";
const OPENAI_EMBED_URL: &str = "https://api.openai.com/v1/embeddings";

pub struct OpenAiEmbeddingProvider {
    api_key: String,
    model: String,
    client: Client,
    usage: Arc<Mutex<ProviderUsage>>,
}

impl OpenAiEmbeddingProvider {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            client: Client::new(),
            usage: Arc::new(Mutex::new(ProviderUsage::default())),
        }
    }

    /// Build from `OPENAI_API_KEY` environment variable.
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| LlmError::Other("OPENAI_API_KEY not set".into()))?;
        Ok(Self::new(key, None))
    }
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedObject>,
    usage: Option<EmbedUsage>,
}

#[derive(Deserialize)]
struct EmbedObject {
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct EmbedUsage {
    prompt_tokens: u32,
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiEmbeddingProvider {
    /// **Not implemented** — `OpenAiEmbeddingProvider` is for embeddings only.
    ///
    /// # Panics
    ///
    /// Always panics. Use `AnthropicApiProvider` or `ClaudeCodeProvider` for text completion.
    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse> {
        panic!(
            "OpenAiEmbeddingProvider is for embeddings only; use a completion provider for text generation"
        )
    }

    fn name(&self) -> &str {
        "openai-embed"
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

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
        });

        let resp = self
            .client
            .post(OPENAI_EMBED_URL)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(format!("OpenAI request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Other(format!(
                "OpenAI embeddings returned {status}: {body}"
            )));
        }

        let parsed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Other(format!("failed to parse OpenAI embed response: {e}")))?;

        let vector = parsed
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or_else(|| LlmError::Other("empty embedding data from OpenAI".into()))?;

        // Track token usage.
        if let Some(u) = parsed.usage {
            let mut usage = self.usage.lock().unwrap();
            usage.input_tokens += u.prompt_tokens as u64;
            usage.calls += 1;
        }

        Ok(vector)
    }

    fn supports_embeddings(&self) -> bool {
        true
    }
}
