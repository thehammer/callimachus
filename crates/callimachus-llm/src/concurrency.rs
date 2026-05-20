//! Adaptive concurrency limiter for LLM providers.
//!
//! [`AdaptiveLimiter`] is a tokio-semaphore-based gate that optionally
//! adjusts its concurrency width based on Anthropic rate-limit response
//! headers:
//!
//! - Starts at width 1 (adaptive) or a fixed `width` (fixed).
//! - On the **first** header observation: sizes to `requests_limit / 20`,
//!   clamped to `[4, 32]`.
//! - On subsequent observations:
//!   - `remaining < 10% of limit`  → halve width (floor 1)
//!   - `remaining > 50% of limit`  → +25% toward initial (ceiling initial)
//! - Adjustments are skipped when the delta ≤ 20% to avoid thrash.
//! - When `fixed_override` is set, no adaptation occurs.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
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
}

/// Concurrency statistics collected over a pass.
#[derive(Debug, Clone)]
pub struct ConcurrencyStats {
    pub requests_made: u64,
    pub peak_concurrency: u32,
    pub avg_concurrency: f64,
    pub current_permits: u32,
    pub initial_permits: u32,
}

// ─── LimiterPermit ────────────────────────────────────────────────────────────

/// RAII permit returned by [`AdaptiveLimiter::acquire`].
///
/// Drop to release the slot.  If the limiter has queued a drain (to shrink
/// concurrency), this permit is *forgotten* (permanently consumed) rather
/// than released, reducing the semaphore capacity by one.
pub struct LimiterPermit {
    permit: Option<OwnedSemaphorePermit>,
    inner: Arc<Inner>,
}

// SAFETY: OwnedSemaphorePermit is Send; Arc<Inner> is Send + Sync.
unsafe impl Send for LimiterPermit {}

impl Drop for LimiterPermit {
    fn drop(&mut self) {
        let permit = self.permit.take().expect("LimiterPermit dropped twice");
        self.inner.running.fetch_sub(1, Ordering::AcqRel);

        // If the limiter wants to shrink, absorb this slot instead of releasing it.
        let drained =
            self.inner
                .pending_drain
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                    if v > 0 { Some(v - 1) } else { None }
                });
        if drained.is_ok() {
            permit.forget();
            // Reflect the permanent capacity reduction.
            self.inner.current_width.fetch_sub(1, Ordering::SeqCst);
        }
        // else: permit drops normally → semaphore releases the slot.
    }
}

// ─── Inner shared state ───────────────────────────────────────────────────────

#[derive(Debug)]
struct Inner {
    semaphore: Arc<tokio::sync::Semaphore>,
    /// Effective concurrency width (number of live semaphore permits).
    current_width: AtomicU32,
    /// Width after first header observation; 0 means "not yet set".
    initial_width: AtomicU32,
    /// When Some, no adaptation occurs.
    fixed_override: Option<u32>,
    /// Permits to drain on the next N releases.
    pending_drain: AtomicU32,
    /// Currently-running request count (for stats).
    running: AtomicU32,
    requests_made: AtomicU64,
    peak_concurrency: AtomicU32,
    /// Sum of `running` at each acquire — for computing average.
    concurrency_sum: AtomicU64,
    /// True once the first header observation has been processed.
    initialized: AtomicBool,
    label: String,
}

// ─── AdaptiveLimiter ──────────────────────────────────────────────────────────

/// Adaptive semaphore gate for LLM concurrency control.
///
/// Cheap to clone — all state lives behind an `Arc`.
#[derive(Clone, Debug)]
pub struct AdaptiveLimiter {
    inner: Arc<Inner>,
}

impl AdaptiveLimiter {
    /// Fixed-width limiter: always `width` concurrent slots, no adaptation.
    pub fn new_fixed(width: u32, label: impl Into<String>) -> Self {
        let width = width.max(1);
        Self {
            inner: Arc::new(Inner {
                semaphore: Arc::new(tokio::sync::Semaphore::new(width as usize)),
                current_width: AtomicU32::new(width),
                initial_width: AtomicU32::new(width),
                fixed_override: Some(width),
                pending_drain: AtomicU32::new(0),
                running: AtomicU32::new(0),
                requests_made: AtomicU64::new(0),
                peak_concurrency: AtomicU32::new(0),
                concurrency_sum: AtomicU64::new(0),
                initialized: AtomicBool::new(true),
                label: label.into(),
            }),
        }
    }

    /// Adaptive-width limiter: starts at width 1, self-sizes on first header observation.
    pub fn new_adaptive(label: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
                current_width: AtomicU32::new(1),
                initial_width: AtomicU32::new(0), // set on first observe
                fixed_override: None,
                pending_drain: AtomicU32::new(0),
                running: AtomicU32::new(0),
                requests_made: AtomicU64::new(0),
                peak_concurrency: AtomicU32::new(0),
                concurrency_sum: AtomicU64::new(0),
                initialized: AtomicBool::new(false),
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

    /// Feed rate-limit headers from an API response.
    ///
    /// The first call initialises the width; subsequent calls may trigger
    /// up or down adjustments.  No-ops when a fixed override is set.
    pub fn observe(&self, snap: RateLimitSnapshot, _avg_tokens: u64) {
        if self.inner.fixed_override.is_some() {
            return;
        }
        let requests_limit = snap.requests_limit;
        if requests_limit == 0 {
            return;
        }
        let current = self.inner.current_width.load(Ordering::SeqCst);

        // ── First observation: size to requests_limit / 20, clamped [4, 32] ──
        if !self.inner.initialized.load(Ordering::SeqCst) {
            let target = (requests_limit / 20).clamp(4, 32);
            let label = &self.inner.label;
            tracing::info!(
                "[{label}] rate limits: req/min={}, tokens/min={}, initial concurrency={}",
                requests_limit,
                snap.tokens_limit,
                target,
            );
            self.inner.initial_width.store(target, Ordering::SeqCst);
            self.resize(current, target);
            self.inner.initialized.store(true, Ordering::SeqCst);
            return;
        }

        let initial = self.inner.initial_width.load(Ordering::SeqCst).max(1);
        let remaining_frac = snap.requests_remaining as f64 / requests_limit as f64;

        let target: u32 = if remaining_frac < 0.10 {
            // Critically low — halve.
            (current / 2).max(1)
        } else if remaining_frac > 0.50 {
            // Plenty of headroom — grow +25% toward initial.
            let bump = (current / 4).max(1);
            (current + bump).min(initial)
        } else {
            return; // 10–50%: steady state, no adjustment.
        };

        if target == current {
            return;
        }

        // Skip tiny adjustments to avoid log spam.
        let delta_frac = (target as f64 - current as f64).abs() / current as f64;
        if delta_frac <= 0.20 {
            return;
        }

        let label = &self.inner.label;
        tracing::info!(
            "[{label}] concurrency adjusted: {current} → {target} \
             (requests remaining: {}/{})",
            snap.requests_remaining,
            requests_limit,
        );
        self.resize(current, target);
    }

    fn resize(&self, current: u32, target: u32) {
        if target > current {
            let delta = target - current;
            self.inner.semaphore.add_permits(delta as usize);
            self.inner.current_width.store(target, Ordering::SeqCst);
        } else if target < current {
            let needed_drain = current - target;
            // Immediately drain any idle permits.
            let mut drained = 0u32;
            while drained < needed_drain {
                match Arc::clone(&self.inner.semaphore).try_acquire_owned() {
                    Ok(permit) => {
                        permit.forget();
                        drained += 1;
                    }
                    Err(_) => break,
                }
            }
            let deferred = needed_drain - drained;
            if deferred > 0 {
                self.inner
                    .pending_drain
                    .fetch_add(deferred, Ordering::SeqCst);
            }
            // Record the new target immediately — deferred slots will follow.
            self.inner.current_width.store(target, Ordering::SeqCst);
        }
    }

    /// Width after the first header observation (`0` if not yet initialised).
    pub fn initial(&self) -> u32 {
        let v = self.inner.initial_width.load(Ordering::Relaxed);
        if v == 0 {
            // Not yet initialised; return current width (1 for adaptive start).
            self.inner.current_width.load(Ordering::Relaxed)
        } else {
            v
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
            initial_permits: self.inner.initial_width.load(Ordering::Relaxed),
        }
    }

    /// Reset accumulated stats (call between passes to get per-pass figures).
    ///
    /// Does **not** reset the learned concurrency width — the limiter carries
    /// its calibrated state into subsequent passes.
    pub fn reset(&self) {
        self.inner.requests_made.store(0, Ordering::SeqCst);
        self.inner.peak_concurrency.store(0, Ordering::SeqCst);
        self.inner.concurrency_sum.store(0, Ordering::SeqCst);
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(limit: u32, remaining: u32) -> RateLimitSnapshot {
        RateLimitSnapshot {
            requests_limit: limit,
            requests_remaining: remaining,
            tokens_limit: 1_000_000,
            tokens_remaining: 500_000,
            requests_reset: None,
        }
    }

    // ── Fixed limiter ─────────────────────────────────────────────────────────

    #[test]
    fn fixed_limiter_ignores_observe() {
        let lim = AdaptiveLimiter::new_fixed(8, "test");
        assert_eq!(lim.initial(), 8);

        // observe should do nothing for fixed.
        lim.observe(snap(20_000, 100), 0);
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

    // ── Adaptive limiter: first observation ──────────────────────────────────

    #[test]
    fn adaptive_sizes_on_first_observe() {
        let lim = AdaptiveLimiter::new_adaptive("test");
        assert_eq!(lim.initial(), 1, "starts at 1 before first observe");
        assert!(!lim.inner.initialized.load(Ordering::SeqCst));

        // 20,000 req/min → target = 20000/20 = 1000, clamped to 32.
        lim.observe(snap(20_000, 18_000), 0);
        assert!(lim.inner.initialized.load(Ordering::SeqCst));
        assert_eq!(lim.initial(), 32);
        assert_eq!(lim.inner.current_width.load(Ordering::SeqCst), 32);
    }

    #[test]
    fn adaptive_clamps_initial_width_to_min_4() {
        let lim = AdaptiveLimiter::new_adaptive("test");
        // Very low limit → 60/20 = 3, clamped to 4.
        lim.observe(snap(60, 50), 0);
        assert_eq!(lim.initial(), 4);
    }

    // ── Adaptive limiter: subsequent adjustments ──────────────────────────────

    #[test]
    fn critically_low_remaining_halves_width() {
        let lim = AdaptiveLimiter::new_adaptive("test");
        lim.observe(snap(20_000, 18_000), 0); // initialise to 32
        assert_eq!(lim.initial(), 32);

        // remaining = 500 / 20000 = 2.5% < 10%  →  32/2 = 16
        lim.observe(snap(20_000, 500), 0);
        assert_eq!(lim.inner.current_width.load(Ordering::SeqCst), 16);
    }

    #[test]
    fn plenty_of_headroom_grows_toward_initial() {
        let lim = AdaptiveLimiter::new_adaptive("test");
        lim.observe(snap(20_000, 18_000), 0); // initialise to 32

        // Manually shrink to 8 to simulate a previous reduction.
        lim.inner.current_width.store(8, Ordering::SeqCst);
        lim.inner.semaphore.add_permits(0); // no-op to keep semaphore consistent
        // Just test the logic path without actually resizing semaphore.

        // remaining = 15000 / 20000 = 75% > 50%  →  8 + 8/4 = 8 + 2 = 10, capped at initial=32
        lim.observe(snap(20_000, 15_000), 0);
        let new_width = lim.inner.current_width.load(Ordering::SeqCst);
        // Should be 10 (8 + bump of 2).
        assert_eq!(new_width, 10);
    }

    #[test]
    fn steady_state_range_no_adjustment() {
        let lim = AdaptiveLimiter::new_adaptive("test");
        lim.observe(snap(20_000, 18_000), 0); // initialise to 32

        // remaining = 5000 / 20000 = 25%  →  in steady range, no change
        let before = lim.inner.current_width.load(Ordering::SeqCst);
        lim.observe(snap(20_000, 5_000), 0);
        let after = lim.inner.current_width.load(Ordering::SeqCst);
        assert_eq!(before, after, "no adjustment in 10–50% range");
    }

    #[test]
    fn tiny_delta_skipped() {
        let lim = AdaptiveLimiter::new_adaptive("test");
        lim.observe(snap(20_000, 18_000), 0); // initialise to 32

        // Set current to 100 so that halving gives 50 — delta is 50%.
        // But if current = 100, remaining < 10% triggers halve to 50.
        // Let's test where delta is tiny: set current=32, remaining=499 (<10% of 5000).
        // 32/2 = 16; delta_frac = 16/32 = 50% > 20%, so this WILL fire.
        // To test small delta: current=32, target=30 (delta_frac = 2/32 = 6.25%).
        // Artificially construct: remaining=9% * limit → halve from 3 to 1? Let's just test
        // the grow path with tiny delta.
        // current=10, remaining=75%: bump = 10/4 = 2 (delta_frac=20%), should be skipped.
        lim.inner.current_width.store(10, Ordering::SeqCst);
        let before = lim.inner.current_width.load(Ordering::SeqCst);
        lim.observe(snap(20_000, 15_001), 0); // 75% > 50%, bump = 10/4 = 2, delta_frac = 2/10 = 20%
        // delta_frac <= 0.20, so it should be skipped.
        let after = lim.inner.current_width.load(Ordering::SeqCst);
        assert_eq!(before, after, "tiny delta (≤20%) should be skipped");
    }

    // ── Stats ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn stats_track_requests_and_peak() {
        let lim = AdaptiveLimiter::new_fixed(4, "test");
        {
            let _p1 = lim.acquire().await;
            let _p2 = lim.acquire().await;
            // peak should be 2 here
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

    // ── Drain / shrink correctness ────────────────────────────────────────────

    #[tokio::test]
    async fn shrink_reduces_available_slots() {
        let _lim = AdaptiveLimiter::new_fixed(4, "test");
        // After first observe (adaptive path), width would go from 4 to 2 (halved).
        // For this test we manually drive resize via observe on an adaptive limiter.
        let lim2 = AdaptiveLimiter::new_adaptive("shrink-test");
        // Initialise to 8.
        lim2.inner.semaphore.add_permits(7); // 1 + 7 = 8
        lim2.inner.current_width.store(8, Ordering::SeqCst);
        lim2.inner.initial_width.store(8, Ordering::SeqCst);
        lim2.inner.initialized.store(true, Ordering::SeqCst);

        // Trigger halve: remaining 5% of limit.
        lim2.observe(snap(20_000, 1_000), 0); // 5% < 10% → halve to 4
        let w = lim2.inner.current_width.load(Ordering::SeqCst);
        assert_eq!(w, 4, "width should be halved to 4");

        // Only 4 permits should be acquirable without blocking.
        let p1 = lim2.acquire().await;
        let p2 = lim2.acquire().await;
        let p3 = lim2.acquire().await;
        let p4 = lim2.acquire().await;
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(20), lim2.acquire()).await;
        assert!(
            result.is_err(),
            "5th acquire should block after shrink to 4"
        );
        drop((p1, p2, p3, p4));
    }

    #[test]
    fn fixed_new_fixed_ignores_second_observe() {
        let lim = AdaptiveLimiter::new_fixed(8, "fixed");
        lim.observe(snap(20_000, 100), 0); // would shrink adaptive, but fixed ignores
        assert_eq!(lim.inner.current_width.load(Ordering::SeqCst), 8);
    }
}
