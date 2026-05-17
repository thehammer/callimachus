use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{
    error::{LlmError, Result},
    pricing,
    provider::{CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage},
};

const DEFAULT_MODEL: &str = "claude-sonnet-4-5";
const DEFAULT_MAX_TOKENS: u32 = 2000;
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_RETRIES: u32 = 3;

#[derive(Debug)]
pub struct AnthropicApiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    usage: Arc<Mutex<ProviderUsage>>,
}

impl AnthropicApiProvider {
    /// Construct from environment. Returns `LlmError::NoProvider` if
    /// `ANTHROPIC_API_KEY` is not set.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| LlmError::NoProvider)?;
        Ok(Self::new(api_key, None, None))
    }

    /// Construct with explicit key and optional overrides.
    pub fn new(api_key: String, model: Option<String>, base_url: Option<String>) -> Self {
        AnthropicApiProvider {
            client: reqwest::Client::new(),
            api_key,
            base_url: base_url
                .unwrap_or_else(|| "https://api.anthropic.com/v1/messages".to_string()),
            default_model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            usage: Arc::new(Mutex::new(ProviderUsage::default())),
        }
    }
}

// ---------- Anthropic API wire types ----------

#[derive(Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ApiMessage>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
    model: String,
    usage: ApiUsage,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ApiUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    message: String,
}

// ---------- LlmProvider impl ----------

#[async_trait::async_trait]
impl LlmProvider for AnthropicApiProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);

        let body = ApiRequest {
            model: model.clone(),
            max_tokens,
            messages: vec![ApiMessage {
                role: "user".to_string(),
                content: req.prompt.clone(),
            }],
        };

        let mut last_err = LlmError::Other("no attempts made".to_string());

        for attempt in 0..MAX_RETRIES {
            let resp = self
                .client
                .post(&self.base_url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| LlmError::Other(e.to_string()))?;

            let status = resp.status();

            if status.is_success() {
                let api_resp: ApiResponse = resp
                    .json()
                    .await
                    .map_err(|e| LlmError::Other(e.to_string()))?;

                let text = api_resp
                    .content
                    .into_iter()
                    .filter(|b| b.kind == "text")
                    .filter_map(|b| b.text)
                    .collect::<Vec<_>>()
                    .join("");

                let input_tokens = api_resp.usage.input_tokens as u64;
                let output_tokens = api_resp.usage.output_tokens as u64;
                let cost = pricing::estimate_cost_usd(&api_resp.model, input_tokens, output_tokens);

                let mut u = self.usage.lock().unwrap();
                u.input_tokens += input_tokens;
                u.output_tokens += output_tokens;
                u.cost_usd += cost;
                u.calls += 1;

                return Ok(CompletionResponse {
                    text,
                    input_tokens: api_resp.usage.input_tokens,
                    output_tokens: api_resp.usage.output_tokens,
                    model_used: api_resp.model,
                });
            }

            let status_code = status.as_u16();

            // Parse error body for message.
            let message = resp
                .json::<ApiErrorBody>()
                .await
                .map(|b| b.error.message)
                .unwrap_or_else(|_| "unknown error".to_string());

            match status_code {
                429 => {
                    // Rate limited — exponential backoff: attempt 0→1s, 1→4s, 2→16s.
                    let backoff_secs = match attempt {
                        0 => 1,
                        1 => 4,
                        _ => 16,
                    };
                    last_err = LlmError::RateLimited {
                        retry_after_secs: backoff_secs,
                    };
                    if attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    }
                }
                500 | 529 => {
                    last_err = LlmError::ApiError {
                        status: status_code,
                        message,
                    };
                    if attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
                _ => {
                    // Other 4xx — no retry.
                    return Err(LlmError::ApiError {
                        status: status_code,
                        message,
                    });
                }
            }
        }

        Err(last_err)
    }

    fn name(&self) -> &str {
        "anthropic-api"
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

    /// Anthropic does not currently offer an embeddings API.
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(LlmError::Other(
            "Anthropic API does not support embeddings; configure an embedding provider (e.g. set OPENAI_API_KEY and embedding.provider = \"openai\" in config)"
                .into(),
        ))
    }

    fn supports_embeddings(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_success_body(text: &str, model: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 20}
        })
    }

    fn make_error_body(message: &str) -> serde_json::Value {
        serde_json::json!({"error": {"type": "error", "message": message}})
    }

    fn provider_with_base_url(base_url: &str) -> AnthropicApiProvider {
        AnthropicApiProvider::new("test-key".to_string(), None, Some(base_url.to_string()))
    }

    fn req(prompt: &str) -> CompletionRequest {
        CompletionRequest {
            prompt: prompt.to_string(),
            model: None,
            max_tokens: None,
            chunk_id: None,
        }
    }

    #[tokio::test]
    async fn successful_completion() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(header("x-api-key", "test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(make_success_body("Hello!", "claude-sonnet-4-5")),
            )
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        let res = p.complete(req("hi")).await.unwrap();
        assert_eq!(res.text, "Hello!");
        assert_eq!(res.input_tokens, 10);
        assert_eq!(res.output_tokens, 20);
        assert_eq!(p.usage().calls, 1);
        assert!(p.usage().cost_usd > 0.0);
    }

    #[tokio::test]
    async fn rate_limit_returns_error_after_retries() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429).set_body_json(make_error_body("rate limit exceeded")),
            )
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        let err = p.complete(req("hi")).await.unwrap_err();
        assert!(matches!(err, LlmError::RateLimited { .. }));
    }

    #[tokio::test]
    async fn server_error_retries_then_fails() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(make_error_body("internal error")),
            )
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        let err = p.complete(req("hi")).await.unwrap_err();
        assert!(matches!(err, LlmError::ApiError { status: 500, .. }));
    }

    #[tokio::test]
    async fn client_error_no_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(make_error_body("bad request")))
            .expect(1) // must not retry
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        let err = p.complete(req("hi")).await.unwrap_err();
        assert!(matches!(err, LlmError::ApiError { status: 400, .. }));
    }

    #[test]
    fn missing_api_key_returns_no_provider() {
        // Ensure the env var is absent for this test.
        // SAFETY: single-threaded test, no concurrent env access.
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        let err = AnthropicApiProvider::from_env().unwrap_err();
        assert!(matches!(err, LlmError::NoProvider));
    }

    #[test]
    fn name_and_parallel() {
        let p = AnthropicApiProvider::new("k".to_string(), None, None);
        assert_eq!(p.name(), "anthropic-api");
        assert!(p.supports_parallel());
    }
}
