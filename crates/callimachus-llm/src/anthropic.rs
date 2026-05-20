use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{
    concurrency::{AdaptiveLimiter, ConcurrencyStats, RateLimitSnapshot},
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
    /// Shared adaptive limiter — all tier variants created via `with_model`
    /// share the same `Arc` so headers observed on any variant adapt the
    /// same semaphore.
    limiter: Arc<AdaptiveLimiter>,
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
            limiter: Arc::new(AdaptiveLimiter::new_adaptive("anthropic")),
        }
    }

    /// Create a variant of this provider that uses `model` as its default.
    ///
    /// The returned provider **shares** the same `reqwest::Client` (connection
    /// pool), `api_key`, `base_url`, `Arc<Mutex<ProviderUsage>>`, and
    /// `Arc<AdaptiveLimiter>` with the parent.  Cost, token accounting, and
    /// concurrency adaptation therefore accumulate in one place across all
    /// tier variants.
    ///
    /// `reqwest::Client` is cheap to clone: the clone shares the same
    /// connection pool internally.
    pub fn with_model(&self, model: impl Into<String>) -> AnthropicApiProvider {
        AnthropicApiProvider {
            client: self.client.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            default_model: model.into(),
            usage: Arc::clone(&self.usage),
            limiter: Arc::clone(&self.limiter),
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
            // Acquire a concurrency slot per attempt so that the slot is
            // released before any retry backoff sleep.  This prevents a
            // 429-triggered sleep from holding a permit and blocking other
            // concurrent entities from making progress.
            let _permit = self.limiter.acquire().await;

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
                // Parse rate-limit headers and feed them to the adaptive limiter
                // before consuming the response body.
                let snap = parse_rate_limit_headers(resp.headers());
                let output_tokens_hint = resp
                    .headers()
                    .get("anthropic-ratelimit-tokens-remaining")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                if let Some(snap) = snap {
                    self.limiter.observe(snap, output_tokens_hint);
                }

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
                        // Release the slot before sleeping so other concurrent
                        // entities can proceed while this one backs off.
                        drop(_permit);
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    }
                }
                500 | 529 => {
                    last_err = LlmError::ApiError {
                        status: status_code,
                        message,
                    };
                    if attempt + 1 < MAX_RETRIES {
                        // Release the slot before the retry delay.
                        drop(_permit);
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
        &self.default_model
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

    /// Build a variant of this provider that defaults to `model`.
    ///
    /// The returned `Arc<dyn LlmProvider>` shares this provider's connection
    /// pool, usage `Arc`, and `AdaptiveLimiter` — see
    /// [`AnthropicApiProvider::with_model`] for details.
    fn with_model_override(&self, model: &str) -> Option<Arc<dyn LlmProvider>> {
        Some(Arc::new(self.with_model(model)))
    }

    /// Make a minimal 1-token request so response headers initialise the
    /// adaptive limiter before the first real pass begins.
    ///
    /// Failures are logged as warnings and ignored — the limiter will still
    /// adapt on the first real request; we just won't have a good starting
    /// width for `buffer_unordered`.
    async fn probe_rate_limits(&self) {
        if self.limiter.is_initialized() {
            return;
        }
        let req = CompletionRequest {
            prompt: "hi".to_string(),
            model: None,
            max_tokens: Some(1),
            chunk_id: None,
        };
        match self.complete(req).await {
            Ok(_) => {
                let stats = self.limiter.stats();
                tracing::info!(
                    "[pipeline] rate limits discovered: initial concurrency={}",
                    stats.initial_permits,
                );
            }
            Err(e) => {
                tracing::warn!(
                    "[pipeline] probe_rate_limits failed: {e}; starting at concurrency=1"
                );
            }
        }
    }

    fn concurrency_limiter(&self) -> Option<Arc<AdaptiveLimiter>> {
        Some(Arc::clone(&self.limiter))
    }

    fn concurrency_stats(&self) -> Option<ConcurrencyStats> {
        Some(self.limiter.stats())
    }
}

// ─── Header parsing ───────────────────────────────────────────────────────────

/// Parse Anthropic rate-limit headers from a response.
///
/// Returns `None` if the required `anthropic-ratelimit-requests-limit` header
/// is absent or unparseable — callers treat `None` as "no header data".
fn parse_rate_limit_headers(headers: &reqwest::header::HeaderMap) -> Option<RateLimitSnapshot> {
    let get_u64 = |name: &str| -> Option<u64> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
    };
    let get_u32 = |name: &str| -> Option<u32> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
    };

    let requests_limit = get_u32("anthropic-ratelimit-requests-limit")?;
    let requests_remaining = get_u32("anthropic-ratelimit-requests-remaining").unwrap_or(0);
    let tokens_limit = get_u64("anthropic-ratelimit-tokens-limit").unwrap_or(0);
    let tokens_remaining = get_u64("anthropic-ratelimit-tokens-remaining").unwrap_or(0);

    // Parse the reset timestamp if present (RFC 3339 format).
    let requests_reset = headers
        .get("anthropic-ratelimit-requests-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    Some(RateLimitSnapshot {
        requests_limit,
        requests_remaining,
        tokens_limit,
        tokens_remaining,
        requests_reset,
    })
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
        // name() returns the actual model name, not the provider label.
        assert_eq!(p.name(), "claude-sonnet-4-5");
        assert!(p.supports_parallel());
    }

    #[test]
    fn name_reflects_configured_model() {
        let p = AnthropicApiProvider::new(
            "k".to_string(),
            Some("claude-haiku-4-5-20251001".to_string()),
            None,
        );
        assert_eq!(p.name(), "claude-haiku-4-5-20251001");
    }

    #[tokio::test]
    async fn with_model_sends_correct_model_on_wire() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(make_success_body("ok", "claude-haiku-4-5")),
            )
            .mount(&server)
            .await;

        let parent = provider_with_base_url(&server.uri());
        let variant = parent.with_model("claude-haiku-4-5");
        variant.complete(req("hello")).await.unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let body: serde_json::Value = received[0].body_json().unwrap();
        assert_eq!(body["model"], "claude-haiku-4-5");
    }

    #[tokio::test]
    async fn with_model_shares_usage_with_parent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(make_success_body("ok", "claude-sonnet-4-5")),
            )
            .mount(&server)
            .await;

        let parent = provider_with_base_url(&server.uri());
        let variant = parent.with_model("model-b");

        // One call on the parent, one on the variant — both share the same Arc.
        parent.complete(req("call one")).await.unwrap();
        variant.complete(req("call two")).await.unwrap();

        assert_eq!(
            parent.usage().calls,
            2,
            "usage should be shared across variants"
        );
    }

    #[tokio::test]
    async fn rate_limit_headers_initialise_adaptive_limiter() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("anthropic-ratelimit-requests-limit", "20000")
                    .insert_header("anthropic-ratelimit-requests-remaining", "18000")
                    .insert_header("anthropic-ratelimit-tokens-limit", "1000000")
                    .insert_header("anthropic-ratelimit-tokens-remaining", "900000")
                    .set_body_json(make_success_body("ok", "claude-sonnet-4-5")),
            )
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        // Before any call: limiter should start at width 1 (adaptive).
        assert_eq!(p.limiter.initial(), 1);

        p.complete(req("hi")).await.unwrap();

        // After first successful call with rate-limit headers:
        // 20000/20 = 1000, clamped to 32.
        let stats = p.concurrency_stats().unwrap();
        assert!(
            stats.requests_made >= 1,
            "should have counted at least 1 request"
        );
        // initial() should now reflect the header-driven width.
        assert_eq!(p.limiter.initial(), 32, "limiter should have sized to 32");
    }

    /// Permit is acquired *inside* the retry loop: after a 429 the slot is
    /// released before the backoff sleep so other concurrent entities can
    /// proceed.  Verify that the limiter only holds one permit at a time and
    /// that the semaphore is not exhausted after a failed attempt.
    #[tokio::test]
    async fn permit_released_before_retry_sleep() {
        let server = MockServer::start().await;
        // First response: 429 (triggers backoff + retry).
        // Second response: 200 success.
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(429).set_body_json(make_error_body("rate limit exceeded")),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(make_success_body("ok", "claude-sonnet-4-5")),
            )
            .mount(&server)
            .await;

        // Use a fixed-width limiter of 1 so we can verify the permit is not
        // permanently held: if the permit were held across the backoff the
        // second acquire() inside the loop would deadlock.
        let p = AnthropicApiProvider {
            client: reqwest::Client::new(),
            api_key: "test-key".to_string(),
            base_url: server.uri(),
            default_model: DEFAULT_MODEL.to_string(),
            usage: std::sync::Arc::new(std::sync::Mutex::new(
                crate::provider::ProviderUsage::default(),
            )),
            limiter: std::sync::Arc::new(crate::concurrency::AdaptiveLimiter::new_fixed(1, "test")),
        };

        // With the old code (permit outside loop) this would deadlock on the
        // second attempt because the semaphore has width 1 and the permit is
        // still held.  With the fix it succeeds.
        let res = tokio::time::timeout(std::time::Duration::from_secs(5), p.complete(req("hi")))
            .await
            .expect("should not time out — permit must be released before retry sleep")
            .expect("second attempt should succeed");

        assert_eq!(res.text, "ok");
    }

    /// probe_rate_limits() makes a real (mocked) request and seeds the adaptive
    /// limiter so that is_initialized() returns true afterwards.
    #[tokio::test]
    async fn probe_rate_limits_initialises_limiter() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("anthropic-ratelimit-requests-limit", "20000")
                    .insert_header("anthropic-ratelimit-requests-remaining", "18000")
                    .insert_header("anthropic-ratelimit-tokens-limit", "1000000")
                    .insert_header("anthropic-ratelimit-tokens-remaining", "900000")
                    .set_body_json(make_success_body("ok", "claude-sonnet-4-5")),
            )
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        assert!(
            !p.limiter.is_initialized(),
            "limiter should start uninitialised"
        );

        use crate::provider::LlmProvider as _;
        p.probe_rate_limits().await;

        assert!(
            p.limiter.is_initialized(),
            "limiter should be initialised after probe"
        );
        assert_eq!(
            p.limiter.initial(),
            32,
            "should have sized to 32 from 20000-req/min header"
        );

        // A second probe call is a no-op (guard at top of the method).
        let before = p.limiter.stats().initial_permits;
        p.probe_rate_limits().await;
        assert_eq!(p.limiter.stats().initial_permits, before);
    }

    #[tokio::test]
    async fn missing_rate_limit_headers_keeps_width_at_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(make_success_body("ok", "claude-sonnet-4-5")),
            )
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        p.complete(req("hi")).await.unwrap();

        // No rate-limit headers → limiter stays uninitialised at width 1.
        assert_eq!(p.limiter.initial(), 1);
    }

    #[tokio::test]
    async fn with_model_shares_limiter_with_parent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("anthropic-ratelimit-requests-limit", "20000")
                    .insert_header("anthropic-ratelimit-requests-remaining", "18000")
                    .insert_header("anthropic-ratelimit-tokens-limit", "1000000")
                    .insert_header("anthropic-ratelimit-tokens-remaining", "900000")
                    .set_body_json(make_success_body("ok", "claude-sonnet-4-5")),
            )
            .mount(&server)
            .await;

        let parent = provider_with_base_url(&server.uri());
        let variant = parent.with_model("claude-haiku-4-5");

        variant.complete(req("hi")).await.unwrap();

        // parent.limiter and variant.limiter are the same Arc.
        assert!(
            Arc::ptr_eq(&parent.limiter, &variant.limiter),
            "limiter Arc should be shared across model variants"
        );
        assert_eq!(parent.limiter.initial(), 32);
    }
}
