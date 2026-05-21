use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{
    budget::{ModelFamily, TokenBudget, model_family_of},
    concurrency::{AdaptiveLimiter, ConcurrencyStats, RateLimitSnapshot},
    error::{LlmError, Result},
    pricing,
    provider::{CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage},
};

const DEFAULT_MODEL: &str = "claude-haiku-4-5";
const DEFAULT_MAX_TOKENS: u32 = 2000;
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_RETRIES: u32 = 3;
/// Fixed safety-cap semaphore width (the budget gate is the real control).
const SAFETY_CAP: u32 = 64;

#[derive(Debug)]
pub struct AnthropicApiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    default_model: String,
    usage: Arc<Mutex<ProviderUsage>>,
    /// Shared fixed safety-cap limiter.
    limiter: Arc<AdaptiveLimiter>,
    /// Shared token-budget admission controller (per model family).
    budget: Arc<TokenBudget>,
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
            limiter: Arc::new(AdaptiveLimiter::new_fixed(SAFETY_CAP, "anthropic")),
            budget: Arc::new(TokenBudget::new("anthropic")),
        }
    }

    /// Create a variant of this provider that uses `model` as its default.
    ///
    /// The returned provider **shares** the same `reqwest::Client` (connection
    /// pool), `api_key`, `base_url`, `Arc<Mutex<ProviderUsage>>`,
    /// `Arc<AdaptiveLimiter>`, and `Arc<TokenBudget>` with the parent.
    /// Cost, token accounting, and budget adaptation therefore accumulate in
    /// one place across all tier variants.
    pub fn with_model(&self, model: impl Into<String>) -> AnthropicApiProvider {
        AnthropicApiProvider {
            client: self.client.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            default_model: model.into(),
            usage: Arc::clone(&self.usage),
            limiter: Arc::clone(&self.limiter),
            budget: Arc::clone(&self.budget),
        }
    }

    fn model_family(&self) -> ModelFamily {
        model_family_of(&self.default_model)
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
        use crate::budget::EstimatorKey;

        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
        let family = model_family_of(&model);
        let key = EstimatorKey::new(req.kind.clone(), req.pass.clone());

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
            // Acquisition order: budget reserve first, then safety-cap limiter.
            // The reservation is released (refunded) on error; settled on success.
            let reservation = self
                .budget
                .reserve(family, key.clone(), &req.prompt, max_tokens)
                .await;
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
                let snap = parse_rate_limit_headers(resp.headers());

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
                drop(u);

                // Settle the reservation with actual usage and authoritative headers.
                if let Some(snap) = snap {
                    reservation.settle(input_tokens, output_tokens, snap);
                }
                // If no headers, reservation drops here → full estimate refunded.

                return Ok(CompletionResponse {
                    text,
                    input_tokens: api_resp.usage.input_tokens,
                    output_tokens: api_resp.usage.output_tokens,
                    model_used: api_resp.model,
                });
            }

            let status_code = status.as_u16();

            let message = resp
                .json::<ApiErrorBody>()
                .await
                .map(|b| b.error.message)
                .unwrap_or_else(|_| "unknown error".to_string());

            match status_code {
                429 => {
                    let backoff_secs: u64 = match attempt {
                        0 => 1,
                        1 => 4,
                        _ => 16,
                    };
                    last_err = LlmError::RateLimited {
                        retry_after_secs: backoff_secs,
                    };
                    // Notify the budget about the 429 before dropping the reservation.
                    self.budget
                        .on_429(family, Duration::from_secs(backoff_secs), &key);
                    // Drop reservation (refund) and permit before sleeping.
                    drop(reservation);
                    drop(_permit);
                    if attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    }
                }
                500 | 529 => {
                    last_err = LlmError::ApiError {
                        status: status_code,
                        message,
                    };
                    drop(reservation);
                    drop(_permit);
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
            "Anthropic API does not support embeddings; configure an embedding provider \
             (e.g. set OPENAI_API_KEY and embedding.provider = \"openai\" in config)"
                .into(),
        ))
    }

    fn supports_embeddings(&self) -> bool {
        false
    }

    /// Build a variant of this provider that defaults to `model`.
    fn with_model_override(&self, model: &str) -> Option<Arc<dyn LlmProvider>> {
        Some(Arc::new(self.with_model(model)))
    }

    /// Make a minimal 1-token probe request to seed the budget for this
    /// model's family before the first real pass begins.
    ///
    /// Failures are logged as warnings and ignored — the budget will still
    /// seed on the first real request.
    async fn probe_rate_limits(&self) {
        let family = self.model_family();
        if self.budget.is_initialized(&family) {
            return;
        }
        let req = CompletionRequest {
            prompt: "hi".to_string(),
            model: None,
            max_tokens: Some(1),
            chunk_id: None,
            kind: "probe".to_string(),
            pass: "probe".to_string(),
        };
        match self.complete(req).await {
            Ok(_) => {
                tracing::info!(
                    "[pipeline] rate limits discovered: family={family} label={}",
                    self.budget.label(),
                );
            }
            Err(e) => {
                tracing::warn!("[pipeline] probe_rate_limits failed for family={family}: {e}");
            }
        }
    }

    fn concurrency_limiter(&self) -> Option<Arc<AdaptiveLimiter>> {
        Some(Arc::clone(&self.limiter))
    }

    fn budget(&self) -> Option<Arc<TokenBudget>> {
        Some(Arc::clone(&self.budget))
    }

    fn concurrency_stats(&self) -> Option<ConcurrencyStats> {
        Some(self.limiter.stats())
    }
}

// ─── Header parsing ───────────────────────────────────────────────────────────

/// Parse Anthropic rate-limit headers from a response.
///
/// Returns `None` if the required `anthropic-ratelimit-requests-limit` header
/// is absent or unparseable.
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
    let get_datetime = |name: &str| -> Option<chrono::DateTime<chrono::Utc>> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
    };

    let requests_limit = get_u32("anthropic-ratelimit-requests-limit")?;
    let requests_remaining = get_u32("anthropic-ratelimit-requests-remaining").unwrap_or(0);
    let tokens_limit = get_u64("anthropic-ratelimit-tokens-limit").unwrap_or(0);
    let tokens_remaining = get_u64("anthropic-ratelimit-tokens-remaining").unwrap_or(0);
    let requests_reset = get_datetime("anthropic-ratelimit-requests-reset");
    let tokens_reset = get_datetime("anthropic-ratelimit-tokens-reset");

    Some(RateLimitSnapshot {
        requests_limit,
        requests_remaining,
        tokens_limit,
        tokens_remaining,
        requests_reset,
        tokens_reset,
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
            kind: "test".to_string(),
            pass: "test".to_string(),
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
                    .set_body_json(make_success_body("Hello!", "claude-haiku-4-5")),
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
        // SAFETY: single-threaded test, no concurrent env access.
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        let err = AnthropicApiProvider::from_env().unwrap_err();
        assert!(matches!(err, LlmError::NoProvider));
    }

    #[test]
    fn name_and_parallel() {
        let p = AnthropicApiProvider::new("k".to_string(), None, None);
        assert_eq!(p.name(), "claude-haiku-4-5");
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
                    .set_body_json(make_success_body("ok", "claude-haiku-4-5")),
            )
            .mount(&server)
            .await;

        let parent = provider_with_base_url(&server.uri());
        let variant = parent.with_model("model-b");

        parent.complete(req("call one")).await.unwrap();
        variant.complete(req("call two")).await.unwrap();

        assert_eq!(
            parent.usage().calls,
            2,
            "usage should be shared across variants"
        );
    }

    #[tokio::test]
    async fn rate_limit_headers_seed_budget() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("anthropic-ratelimit-requests-limit", "20000")
                    .insert_header("anthropic-ratelimit-requests-remaining", "18000")
                    .insert_header("anthropic-ratelimit-tokens-limit", "1000000")
                    .insert_header("anthropic-ratelimit-tokens-remaining", "900000")
                    .set_body_json(make_success_body("ok", "claude-haiku-4-5")),
            )
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        assert!(!p.budget.is_initialized(&ModelFamily::Haiku));

        p.complete(req("hi")).await.unwrap();

        // After first successful call with rate-limit headers, budget should be seeded.
        assert!(
            p.budget.is_initialized(&ModelFamily::Haiku),
            "budget should be initialised after first response with headers"
        );
    }

    /// Permit is acquired *inside* the retry loop: after a 429 the slot is
    /// released before the backoff sleep so other concurrent entities can
    /// proceed.
    #[tokio::test]
    async fn permit_released_before_retry_sleep() {
        let server = MockServer::start().await;
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
                    .set_body_json(make_success_body("ok", "claude-haiku-4-5")),
            )
            .mount(&server)
            .await;

        // Use a fixed-width limiter of 1 to verify permit release before retry.
        let p = AnthropicApiProvider {
            client: reqwest::Client::new(),
            api_key: "test-key".to_string(),
            base_url: server.uri(),
            default_model: DEFAULT_MODEL.to_string(),
            usage: std::sync::Arc::new(std::sync::Mutex::new(
                crate::provider::ProviderUsage::default(),
            )),
            limiter: std::sync::Arc::new(crate::concurrency::AdaptiveLimiter::new_fixed(1, "test")),
            budget: std::sync::Arc::new(TokenBudget::new("test")),
        };

        let res = tokio::time::timeout(std::time::Duration::from_secs(5), p.complete(req("hi")))
            .await
            .expect("should not time out")
            .expect("second attempt should succeed");

        assert_eq!(res.text, "ok");
    }

    #[tokio::test]
    async fn probe_rate_limits_seeds_budget() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("anthropic-ratelimit-requests-limit", "20000")
                    .insert_header("anthropic-ratelimit-requests-remaining", "18000")
                    .insert_header("anthropic-ratelimit-tokens-limit", "1000000")
                    .insert_header("anthropic-ratelimit-tokens-remaining", "900000")
                    .set_body_json(make_success_body("ok", "claude-haiku-4-5")),
            )
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        assert!(
            !p.budget.is_initialized(&ModelFamily::Haiku),
            "budget should start uninitialised"
        );

        use crate::provider::LlmProvider as _;
        p.probe_rate_limits().await;

        assert!(
            p.budget.is_initialized(&ModelFamily::Haiku),
            "budget should be initialised after probe"
        );

        // A second probe is a no-op.
        p.probe_rate_limits().await; // should not make another request
    }

    #[tokio::test]
    async fn missing_rate_limit_headers_leaves_budget_uninit() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(make_success_body("ok", "claude-haiku-4-5")),
            )
            .mount(&server)
            .await;

        let p = provider_with_base_url(&server.uri());
        p.complete(req("hi")).await.unwrap();

        // No rate-limit headers → budget stays uninitialised (passes through).
        assert!(!p.budget.is_initialized(&ModelFamily::Haiku));
    }

    #[tokio::test]
    async fn with_model_shares_budget_with_parent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("anthropic-ratelimit-requests-limit", "20000")
                    .insert_header("anthropic-ratelimit-requests-remaining", "18000")
                    .insert_header("anthropic-ratelimit-tokens-limit", "1000000")
                    .insert_header("anthropic-ratelimit-tokens-remaining", "900000")
                    .set_body_json(make_success_body("ok", "claude-haiku-4-5")),
            )
            .mount(&server)
            .await;

        let parent = provider_with_base_url(&server.uri());
        let variant = parent.with_model("claude-haiku-4-5");

        // parent and variant should share the same Arc<TokenBudget>.
        assert!(
            Arc::ptr_eq(&parent.budget, &variant.budget),
            "budget Arc should be shared across model variants"
        );

        variant.complete(req("hi")).await.unwrap();
    }
}
