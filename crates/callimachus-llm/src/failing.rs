//! A test [`LlmProvider`] that fails deterministically after N successes.
//!
//! Wraps an inner provider and lets the first `succeed_n` `complete` calls
//! through, then returns an error for the next `fail_m` calls, then delegates
//! again. Used to simulate a backfill that dies mid-iteration so resume
//! behaviour can be tested (see the `walk_resumes_past_partial_sha` regression
//! test). `embed` always delegates — the failure lever is the completion path.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::{
    error::{LlmError, Result},
    provider::{CompletionRequest, CompletionResponse, LlmProvider, ProviderUsage},
};

/// See the module docs.
pub struct FailingProvider {
    inner: Arc<dyn LlmProvider>,
    succeed_n: u64,
    fail_m: u64,
    calls: AtomicU64,
}

impl FailingProvider {
    /// Wrap `inner`, succeeding for the first `succeed_n` completion calls then
    /// failing for the following `fail_m`.
    pub fn new(inner: Arc<dyn LlmProvider>, succeed_n: u64, fail_m: u64) -> Self {
        Self {
            inner,
            succeed_n,
            fail_m,
            calls: AtomicU64::new(0),
        }
    }

    /// Number of `complete` calls observed so far (across success + failure).
    pub fn call_count(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LlmProvider for FailingProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        // 1-based index of this call.
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.succeed_n && n <= self.succeed_n + self.fail_m {
            return Err(LlmError::Other(format!(
                "FailingProvider: injected failure on call {n} \
                 (succeed_n={}, fail_m={})",
                self.succeed_n, self.fail_m
            )));
        }
        self.inner.complete(req).await
    }

    fn name(&self) -> &str {
        "failing"
    }

    fn supports_parallel(&self) -> bool {
        // Force serial execution so the success/failure window is deterministic.
        false
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dry_run::DryRunProvider;

    fn req() -> CompletionRequest {
        CompletionRequest {
            prompt: "hello".to_string(),
            model: None,
            max_tokens: None,
            chunk_id: None,
            kind: "test".to_string(),
            pass: "test".to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn succeeds_then_fails_then_succeeds() {
        let inner = Arc::new(DryRunProvider::new());
        let p = FailingProvider::new(inner, 2, 1);
        assert!(p.complete(req()).await.is_ok()); // 1
        assert!(p.complete(req()).await.is_ok()); // 2
        assert!(p.complete(req()).await.is_err()); // 3 (fail)
        assert!(p.complete(req()).await.is_ok()); // 4 (recovered)
        assert_eq!(p.call_count(), 4);
    }
}
