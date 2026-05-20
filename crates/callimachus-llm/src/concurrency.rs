//! Fixed-cap concurrency limiter for LLM providers.
//!
//! [`AdaptiveLimiter`] is a thin tokio-semaphore wrapper that caps the
//! number of concurrent LLM requests to a fixed width (default 64).
//! Rate-limit budget enforcement is handled by
//! [`crate::budget::TokenBudget`]; this semaphore is purely a safety
//! guard against unbounded memory growth from queued futures.

use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

use chrono::{DateTime, Utc};
use tokio::sync::OwnedSemaphorePermit;

// ─── Public data types ────────────────────────────────────────────────────────

/// Rate-limit headers parsed from an Anthropic API response.
#[derive(Debug, Clone)]
pub struct RateLimitSnapshot {
    pub requests_limit: u32,
    pub requests_remaining: u32,
    pub tokens_limit: u64,
    pub tokens_remaining: u64,
    pub requests_reset: Option<DateTime<Utc>>,
    pub tokens_reset: Option<DateTime<Utc>>,
}

/// Concurrency statistics collected over a pass.
#[derive(Debug, Clone)]
pub struct ConcurrencyStats {
    pub requests_made: u64,
    pub peak_concurrency: u32,
    pub avg_concurrency: f64,
    pub current_permits: u32,
}

// ─── LimiterPermit ────────────────────────────────────────────────────────────

/// RAII permit returned by [`AdaptiveLimiter::acquire`].
///
/// Drop to release the slot.
pub struct LimiterPermit {
    permit: Option<OwnedSemaphorePermit>,
    inner: Arc<Inner>,
}

// SAFETY: OwnedSemaphorePermit is Send; Arc<Inner> is Send + Sync.
unsafe impl Send for LimiterPermit {}

impl Drop for LimiterPermit {
    fn drop(&mut self) {
        let _permit = self.permit.take().expect("LimiterPermit dropped twice");
        self.inner.running.fetch_sub(1, Ordering::AcqRel);
        // permit drops here, releasing the semaphore slot.
    }
}

// ─── Inner shared state ───────────────────────────────────────────────────────

#[derive(Debug)]
struct Inner {
    semaphore: Arc<tokio::sync::Semaphore>,
    /// Effective concurrency width.
    current_width: AtomicU32,
    /// Currently-running request count (for stats).
    running: AtomicU32,
    requests_made: AtomicU64,
    peak_concurrency: AtomicU32,
    /// Sum of `running` at each acquire — for computing average.
    concurrency_sum: AtomicU64,
    label: String,
}

// ─── AdaptiveLimiter ──────────────────────────────────────────────────────────

/// Fixed-width semaphore safety cap for LLM concurrency.
///
/// Cheap to clone — all state lives behind an `Arc`.
///
/// The actual rate-limit enforcement is handled by
/// [`crate::budget::TokenBudget`].  This limiter exists only to bound the
/// number of in-flight tokio tasks (and therefore memory) even when the
/// budget gate is saturated.
#[derive(Clone, Debug)]
pub struct AdaptiveLimiter {
    inner: Arc<Inner>,
}

impl AdaptiveLimiter {
    /// Fixed-width limiter: always `width` concurrent slots.
    pub fn new_fixed(width: u32, label: impl Into<String>) -> Self {
        let width = width.max(1);
        Self {
            inner: Arc::new(Inner {
                semaphore: Arc::new(tokio::sync::Semaphore::new(width as usize)),
                current_width: AtomicU32::new(width),
                running: AtomicU32::new(0),
                requests_made: AtomicU64::new(0),
                peak_concurrency: AtomicU32::new(0),
                concurrency_sum: AtomicU64::new(0),
                label: label.into(),
            }),
        }
    }

    /// Acquire a concurrency slot.  Awaits until a slot is available.
    ///
    /// Drop the returned [`LimiterPermit`] when the request completes (or
    /// fails) to release the slot.
    pub async fn acquire(&self) -> LimiterPermit {
        let permit = Arc::clone(&self.inner.semaphore)
            .acquire_owned()
            .await
            .expect("limiter semaphore should not be closed");
        let now_running = self.inner.running.fetch_add(1, Ordering::AcqRel) + 1;
        self.inner
            .peak_concurrency
            .fetch_max(now_running, Ordering::Relaxed);
        self.inner.requests_made.fetch_add(1, Ordering::Relaxed);
        self.inner
            .concurrency_sum
            .fetch_add(now_running as u64, Ordering::Relaxed);
        LimiterPermit {
            permit: Some(permit),
            inner: Arc::clone(&self.inner),
        }
    }

    /// Snapshot of concurrency statistics accumulated so far.
    pub fn stats(&self) -> ConcurrencyStats {
        let requests_made = self.inner.requests_made.load(Ordering::Relaxed);
        let peak = self.inner.peak_concurrency.load(Ordering::Relaxed);
        let sum = self.inner.concurrency_sum.load(Ordering::Relaxed);
        let avg = if requests_made > 0 {
            sum as f64 / requests_made as f64
        } else {
            0.0
        };
        ConcurrencyStats {
            requests_made,
            peak_concurrency: peak,
            avg_concurrency: avg,
            current_permits: self.inner.current_width.load(Ordering::Relaxed),
        }
    }

    /// Reset accumulated stats (call between passes to get per-pass figures).
    pub fn reset(&self) {
        self.inner.requests_made.store(0, Ordering::SeqCst);
        self.inner.peak_concurrency.store(0, Ordering::SeqCst);
        self.inner.concurrency_sum.store(0, Ordering::SeqCst);
    }

    pub fn label(&self) -> &str {
        &self.inner.label
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_limiter_has_correct_initial_width() {
        let lim = AdaptiveLimiter::new_fixed(8, "test");
        assert_eq!(lim.inner.current_width.load(Ordering::SeqCst), 8);
    }

    #[tokio::test]
    async fn fixed_limiter_limits_concurrency() {
        let lim = AdaptiveLimiter::new_fixed(2, "test");
        let _p1 = lim.acquire().await;
        let _p2 = lim.acquire().await;
        // A third acquire should timeout since we only have 2 slots.
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(20), lim.acquire()).await;
        assert!(result.is_err(), "third acquire should block with width=2");
    }

    #[tokio::test]
    async fn stats_track_requests_and_peak() {
        let lim = AdaptiveLimiter::new_fixed(4, "test");
        {
            let _p1 = lim.acquire().await;
            let _p2 = lim.acquire().await;
        }
        let stats = lim.stats();
        assert_eq!(stats.requests_made, 2);
        assert_eq!(stats.peak_concurrency, 2);
        assert!(stats.avg_concurrency > 0.0);
    }

    #[tokio::test]
    async fn reset_clears_stats() {
        let lim = AdaptiveLimiter::new_fixed(2, "test");
        {
            let _p = lim.acquire().await;
        }
        lim.reset();
        let stats = lim.stats();
        assert_eq!(stats.requests_made, 0);
        assert_eq!(stats.peak_concurrency, 0);
        assert_eq!(stats.avg_concurrency, 0.0);
    }
}
