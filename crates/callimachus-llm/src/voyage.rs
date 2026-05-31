/// Voyage AI embedding provider.
///
/// Calls the Voyage AI embeddings API to produce dense float vectors for
/// code chunks. The default model is `voyage-code-3`.
///
/// # Example
///
/// ```rust,no_run
/// use callimachus_llm::VoyageEmbeddingProvider;
/// let provider = VoyageEmbeddingProvider::new("my-voyage-key".to_string(), None);
/// ```
use std::sync::{Arc, Mutex};

use reqwest::Client;
use serde::Deserialize;

use crate::{
    embedding::EmbeddingProvider,
    error::{LlmError, Result},
    provider::ProviderUsage,
};

const VOYAGE_EMBED_URL: &str = "https://api.voyageai.com/v1/embeddings";
const DEFAULT_MODEL: &str = "voyage-code-3";

pub struct VoyageEmbeddingProvider {
    api_key: String,
    model: String,
    client: Client,
    usage: Arc<Mutex<ProviderUsage>>,
    input_type: &'static str,
    /// Base URL, overrideable in tests.
    base_url: String,
}

impl VoyageEmbeddingProvider {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            client: Client::new(),
            usage: Arc::new(Mutex::new(ProviderUsage::default())),
            input_type: "document",
            base_url: VOYAGE_EMBED_URL.to_string(),
        }
    }

    #[cfg(test)]
    fn with_base_url(api_key: String, model: Option<String>, base_url: String) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            client: Client::new(),
            usage: Arc::new(Mutex::new(ProviderUsage::default())),
            input_type: "document",
            base_url,
        }
    }
}

#[derive(Deserialize)]
struct VoyageResponse {
    data: Vec<VoyageObject>,
    usage: Option<VoyageUsage>,
}

#[derive(Deserialize)]
struct VoyageObject {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct VoyageUsage {
    total_tokens: u32,
}

#[async_trait::async_trait]
impl EmbeddingProvider for VoyageEmbeddingProvider {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
            "input_type": self.input_type,
        });

        let resp = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Other(format!("Voyage request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Other(format!(
                "Voyage embeddings returned {status}: {body}"
            )));
        }

        let mut parsed: VoyageResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Other(format!("failed to parse Voyage embed response: {e}")))?;

        // Sort by index — do not assume API returns in input order.
        parsed.data.sort_by_key(|d| d.index);

        if parsed.data.len() != texts.len() {
            return Err(LlmError::Other(format!(
                "Voyage returned {} embeddings for {} inputs",
                parsed.data.len(),
                texts.len()
            )));
        }

        // Accumulate usage.
        if let Some(u) = parsed.usage {
            let mut usage = self.usage.lock().unwrap();
            usage.input_tokens += u.total_tokens as u64;
            usage.calls += 1;
        }

        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }

    fn name(&self) -> &str {
        &self.model
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn provider_with_base_url(base_url: &str) -> VoyageEmbeddingProvider {
        VoyageEmbeddingProvider::with_base_url(
            "test-key".to_string(),
            None,
            format!("{base_url}/v1/embeddings"),
        )
    }

    #[tokio::test]
    async fn embed_batch_returns_vectors_in_index_order() {
        let server = MockServer::start().await;

        // Response has data out of order (index 1 before index 0) to prove sorting.
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [
                    { "object": "embedding", "embedding": [0.0, 0.2, 0.0], "index": 1 },
                    { "object": "embedding", "embedding": [1.0, 0.0, 0.0], "index": 0 }
                ],
                "model": "voyage-code-3",
                "usage": { "total_tokens": 10 }
            })))
            .mount(&server)
            .await;

        let provider = provider_with_base_url(&server.uri());
        let texts = vec!["first".to_string(), "second".to_string()];
        let result = provider.embed_batch(&texts).await.unwrap();

        assert_eq!(result.len(), 2);
        // Index 0 → first vector.
        assert_eq!(result[0][0], 1.0);
        // Index 1 → second vector.
        assert_eq!(result[1][1], 0.2);
    }

    #[tokio::test]
    async fn non_2xx_response_yields_error_with_status_and_body() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(401).set_body_string("Unauthorized: invalid API key"),
            )
            .mount(&server)
            .await;

        let provider = provider_with_base_url(&server.uri());
        let err = provider
            .embed_batch(&["text".to_string()])
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("401"), "expected status in error: {msg}");
        assert!(
            msg.contains("Unauthorized"),
            "expected body in error: {msg}"
        );
    }

    #[tokio::test]
    async fn name_returns_model_string() {
        let provider =
            VoyageEmbeddingProvider::new("key".to_string(), Some("voyage-code-3".to_string()));
        assert_eq!(provider.name(), "voyage-code-3");
    }

    #[tokio::test]
    async fn default_model_is_voyage_code_3() {
        let provider = VoyageEmbeddingProvider::new("key".to_string(), None);
        assert_eq!(provider.name(), "voyage-code-3");
    }
}
