//! Run-wide caps on LLM spend: call count, input tokens and wall time.
//!
//! [`CappedProvider`] decorates each tier provider. All decorators of one run
//! share a [`CapState`], so the counts span every pass. Once a cap is reached,
//! further `complete` calls fail with [`LlmError::QuotaExhausted`], which the
//! passes already treat as "stop cleanly, resume by re-running". Calls admitted
//! before the cap was reached finish normally.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use callimachus_llm::{
    CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderUsage,
    estimating::estimate_input_tokens,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunCaps {
    pub max_calls: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub stop_after: Option<Duration>,
}

impl RunCaps {
    pub fn is_set(&self) -> bool {
        self.max_calls.is_some() || self.max_input_tokens.is_some() || self.stop_after.is_some()
    }
}

/// Counters shared by every [`CappedProvider`] of one run.
pub struct CapState {
    caps: RunCaps,
    started: Instant,
    calls: AtomicU64,
    input_tokens: AtomicU64,
    reached: Mutex<Option<&'static str>>,
}

impl CapState {
    pub fn new(caps: RunCaps) -> Arc<Self> {
        Arc::new(Self {
            caps,
            started: Instant::now(),
            calls: AtomicU64::new(0),
            input_tokens: AtomicU64::new(0),
            reached: Mutex::new(None),
        })
    }

    /// Admit one call, or return the name of the cap that is reached.
    fn admit(&self) -> Result<(), &'static str> {
        let mut reached = self.reached.lock().unwrap();
        if let Some(which) = *reached {
            return Err(which);
        }
        let hit = if self
            .caps
            .stop_after
            .is_some_and(|d| self.started.elapsed() >= d)
        {
            Some("stop_after")
        } else if self
            .caps
            .max_input_tokens
            .is_some_and(|m| self.input_tokens.load(Ordering::SeqCst) >= m)
        {
            Some("max_input_tokens")
        } else if self
            .caps
            .max_calls
            .is_some_and(|m| self.calls.load(Ordering::SeqCst) >= m)
        {
            Some("max_calls")
        } else {
            None
        };
        if let Some(which) = hit {
            *reached = Some(which);
            return Err(which);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub struct CappedProvider {
    inner: Arc<dyn LlmProvider>,
    state: Arc<CapState>,
}

impl CappedProvider {
    pub fn wrap(inner: Arc<dyn LlmProvider>, state: &Arc<CapState>) -> Arc<dyn LlmProvider> {
        Arc::new(Self {
            inner,
            state: Arc::clone(state),
        })
    }
}

#[async_trait::async_trait]
impl LlmProvider for CappedProvider {
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> callimachus_llm::error::Result<CompletionResponse> {
        self.state
            .admit()
            .map_err(|which| LlmError::QuotaExhausted {
                message: format!("run cap reached: {which}"),
                resets_at: None,
            })?;
        let prompt_estimate = estimate_input_tokens(req.prompt.len(), 0);
        let response = self.inner.complete(req).await?;
        // Prefer the provider's real count; fall back to a size estimate.
        let used = match u64::from(response.input_tokens) {
            0 => prompt_estimate,
            n => n,
        };
        self.state.input_tokens.fetch_add(used, Ordering::SeqCst);
        Ok(response)
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

    fn with_model_override(&self, model: &str) -> Option<Arc<dyn LlmProvider>> {
        self.inner
            .with_model_override(model)
            .map(|p| CappedProvider::wrap(p, &self.state))
    }

    async fn probe_rate_limits(&self) {
        self.inner.probe_rate_limits().await
    }

    fn concurrency_limiter(&self) -> Option<Arc<callimachus_llm::AdaptiveLimiter>> {
        self.inner.concurrency_limiter()
    }

    fn budget(&self) -> Option<Arc<callimachus_llm::TokenBudget>> {
        self.inner.budget()
    }

    fn concurrency_stats(&self) -> Option<callimachus_llm::ConcurrencyStats> {
        self.inner.concurrency_stats()
    }
}

#[cfg(test)]
mod tests {
    use callimachus_llm::DryRunProvider;

    use super::*;

    async fn call(p: &Arc<dyn LlmProvider>) -> Result<CompletionResponse, LlmError> {
        p.complete(CompletionRequest {
            prompt: "x".repeat(350),
            ..Default::default()
        })
        .await
    }

    fn reason(e: LlmError) -> String {
        match e {
            LlmError::QuotaExhausted { message, resets_at } => {
                assert!(resets_at.is_none());
                message
            }
            other => panic!("expected quota error, got {other}"),
        }
    }

    #[tokio::test]
    async fn max_calls_admits_exactly_n_across_wrappers() {
        let dry = Arc::new(DryRunProvider::new());
        let state = CapState::new(RunCaps {
            max_calls: Some(3),
            ..Default::default()
        });
        let a = CappedProvider::wrap(dry.clone(), &state);
        let b = CappedProvider::wrap(dry.clone(), &state);
        call(&a).await.unwrap();
        call(&b).await.unwrap();
        call(&a).await.unwrap();
        assert_eq!(
            reason(call(&b).await.unwrap_err()),
            "run cap reached: max_calls"
        );
        assert_eq!(dry.call_count(), 3);
    }

    #[tokio::test]
    async fn max_input_tokens_stops_after_budget_spent() {
        let dry = Arc::new(DryRunProvider::new());
        // DryRun reports 0 tokens, so the prompt-size estimate (100) is used.
        let state = CapState::new(RunCaps {
            max_input_tokens: Some(150),
            ..Default::default()
        });
        let p = CappedProvider::wrap(dry, &state);
        call(&p).await.unwrap();
        call(&p).await.unwrap();
        assert!(reason(call(&p).await.unwrap_err()).contains("max_input_tokens"));
    }

    #[tokio::test]
    async fn stop_after_zero_blocks_immediately() {
        let state = CapState::new(RunCaps {
            stop_after: Some(Duration::ZERO),
            ..Default::default()
        });
        let p = CappedProvider::wrap(Arc::new(DryRunProvider::new()), &state);
        assert!(reason(call(&p).await.unwrap_err()).contains("stop_after"));
    }
}
