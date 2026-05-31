use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use crate::{
    embedding::EmbeddingProvider,
    error::Result,
    provider::{CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage},
};

/// Returns canned responses immediately — no network or subprocess required.
/// Useful for tests and offline development.
pub struct DryRunProvider {
    usage: Arc<Mutex<ProviderUsage>>,
    /// Total LLM calls served (`complete` + `embed`). The linchpin for
    /// cache-hit-skips-LLM tests: a cache hit must not increment this.
    calls: Arc<AtomicU64>,
    /// Last sampling parameters seen by `complete`, recorded for tests that
    /// assert stable-sampling plumbing reached the provider.
    last_sampling: Arc<Mutex<(Option<f32>, Option<u64>)>>,
}

impl DryRunProvider {
    pub fn new() -> Self {
        DryRunProvider {
            usage: Arc::new(Mutex::new(ProviderUsage::default())),
            calls: Arc::new(AtomicU64::new(0)),
            last_sampling: Arc::new(Mutex::new((None, None))),
        }
    }

    /// Total LLM calls served since the last [`LlmProvider::reset_usage`].
    pub fn call_count(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    /// The temperature/seed of the most recent `complete` call.
    pub fn last_sampling(&self) -> (Option<f32>, Option<u64>) {
        *self.last_sampling.lock().unwrap()
    }
}

impl Default for DryRunProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl LlmProvider for DryRunProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let text = if req.prompt.contains("entities") || req.prompt.contains("extract") {
            r#"{"entities":[],"edges":[],"summary_text":"[dry-run]"}"#.to_string()
        } else {
            "[dry-run]".to_string()
        };

        *self.last_sampling.lock().unwrap() = (req.temperature, req.seed);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut u = self.usage.lock().unwrap();
        u.calls += 1;

        Ok(CompletionResponse {
            text,
            input_tokens: 0,
            output_tokens: 0,
            model_used: "dry-run".to_string(),
        })
    }

    fn name(&self) -> &str {
        "dry-run"
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    fn usage(&self) -> ProviderUsage {
        self.usage.lock().unwrap().clone()
    }

    fn reset_usage(&self) {
        *self.usage.lock().unwrap() = ProviderUsage::default();
        self.calls.store(0, Ordering::SeqCst);
    }

    /// Returns a deterministic unit vector for dry-run/test usage.
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // 8-dimensional unit vector along first axis.
        let mut v = vec![0.0f32; 8];
        v[0] = 1.0;
        Ok(v)
    }

    fn supports_embeddings(&self) -> bool {
        true
    }
}

/// `DryRunProvider` also implements `EmbeddingProvider` so it can be used as
/// both the LLM and the embedder in tests that check cache-hit behaviour.
/// The `call_count()` counter is shared between `complete` and `embed_batch`,
/// allowing a single test double to cover both roles.
#[async_trait::async_trait]
impl EmbeddingProvider for DryRunProvider {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(texts.len() as u64, Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|_| {
                let mut v = vec![0.0f32; 8];
                v[0] = 1.0;
                v
            })
            .collect())
    }

    fn name(&self) -> &str {
        "dry-run"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn extraction_prompt_returns_json() {
        let p = DryRunProvider::new();
        let res = p
            .complete(CompletionRequest {
                prompt: "extract the entities from this text".to_string(),
                model: None,
                max_tokens: None,
                chunk_id: None,
                kind: "test".to_string(),
                pass: "test".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(res.text.contains("entities"));
        assert!(res.text.contains("dry-run"));
    }

    #[tokio::test]
    async fn entities_keyword_returns_json() {
        let p = DryRunProvider::new();
        let res = p
            .complete(CompletionRequest {
                prompt: "list the entities".to_string(),
                model: None,
                max_tokens: None,
                chunk_id: None,
                kind: "test".to_string(),
                pass: "test".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(res.text.contains(r#""entities":[]"#));
    }

    #[tokio::test]
    async fn plain_prompt_returns_dry_run_string() {
        let p = DryRunProvider::new();
        let res = p
            .complete(CompletionRequest {
                prompt: "summarize this document".to_string(),
                model: None,
                max_tokens: None,
                chunk_id: None,
                kind: "test".to_string(),
                pass: "test".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(res.text, "[dry-run]");
    }

    #[tokio::test]
    async fn usage_tracks_calls() {
        let p = DryRunProvider::new();
        assert_eq!(p.usage().calls, 0);
        p.complete(CompletionRequest {
            prompt: "hello".to_string(),
            model: None,
            max_tokens: None,
            chunk_id: None,
            kind: "test".to_string(),
            pass: "test".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(p.usage().calls, 1);
        assert_eq!(p.usage().cost_usd, 0.0);
        p.reset_usage();
        assert_eq!(p.usage().calls, 0);
    }

    #[test]
    fn name_and_parallel() {
        let p = DryRunProvider::new();
        // Both LlmProvider::name and EmbeddingProvider::name return "dry-run".
        assert_eq!(LlmProvider::name(&p), "dry-run");
        assert!(p.supports_parallel());
    }
}
