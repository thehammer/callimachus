//! Token-budget admission controller for LLM providers.
//!
//! [`TokenBudget`] maintains per-model-family token+request sub-budgets
//! and only admits a call once it has debited an estimated cost against
//! the remaining budget.
//!
//! [`CallSizeEstimator`] learns per-(kind, pass) token averages from
//! observed `usage` blocks, using Welford online updates and a simple
//! p95 estimate.  Falls back to a global warm-start prior on cold start
//! (5 000 input / 800 output tokens).
//!
//! # Admission flow
//!
//! 1. Caller calls [`TokenBudget::reserve`], passing the model family,
//!    estimator key, prompt text, and max_tokens.
//! 2. The estimator produces an estimated token cost.
//! 3. If the family budget has capacity, the cost is debited and a
//!    [`BudgetReservation`] is returned immediately.
//! 4. If capacity is unavailable, the call sleeps on either a
//!    time-until-reset sleep or a [`Notify`] wakeup, then re-checks.
//! 5. On success, the caller calls [`BudgetReservation::settle`] with
//!    actual usage and the authoritative header snapshot.
//! 6. On any error, the reservation is simply dropped — the Drop impl
//!    refunds the full estimate and notifies waiters.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

use crate::concurrency::RateLimitSnapshot;

// ─── ModelFamily ─────────────────────────────────────────────────────────────

/// Coarse Anthropic model-family classification.
///
/// Each family has its own independent rate-limit quota; the budget
/// maintains a separate [`FamilyBudget`] per variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelFamily {
    Haiku,
    Sonnet,
    Opus,
}

impl std::fmt::Display for ModelFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelFamily::Haiku => write!(f, "haiku"),
            ModelFamily::Sonnet => write!(f, "sonnet"),
            ModelFamily::Opus => write!(f, "opus"),
        }
    }
}

/// Classify a model name string into a [`ModelFamily`].
///
/// Defaults to [`ModelFamily::Sonnet`] for unrecognised names.
pub fn model_family_of(model: &str) -> ModelFamily {
    let lc = model.to_lowercase();
    if lc.contains("haiku") {
        ModelFamily::Haiku
    } else if lc.contains("opus") {
        ModelFamily::Opus
    } else {
        ModelFamily::Sonnet
    }
}

// ─── EstimatorKey ─────────────────────────────────────────────────────────────

/// Key for per-(kind, pass) call-size statistics.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EstimatorKey {
    pub kind: String,
    pub pass: String,
}

impl EstimatorKey {
    pub fn new(kind: impl Into<String>, pass: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            pass: pass.into(),
        }
    }
}

// ─── KindStats ────────────────────────────────────────────────────────────────

const P95_WINDOW: usize = 100;

#[derive(Clone)]
struct KindStats {
    mean_input: f64,
    mean_output: f64,
    /// Number of observations (used for Welford update and n>=10 guard).
    n: u64,
    /// Rolling window of recent input observations for p95 estimation.
    recent_inputs: Vec<u64>,
    /// Rolling window of recent output observations for p95 estimation.
    recent_outputs: Vec<u64>,
}

impl KindStats {
    fn new(mean_input: f64, mean_output: f64) -> Self {
        Self {
            mean_input,
            mean_output,
            n: 0,
            recent_inputs: Vec::new(),
            recent_outputs: Vec::new(),
        }
    }

    /// Welford online mean update + rolling p95 window.
    fn update(&mut self, actual_input: u64, actual_output: u64) {
        self.n += 1;
        let n = self.n as f64;
        self.mean_input += (actual_input as f64 - self.mean_input) / n;
        self.mean_output += (actual_output as f64 - self.mean_output) / n;

        // Evict oldest when the window is full.
        if self.recent_inputs.len() >= P95_WINDOW {
            self.recent_inputs.remove(0);
            self.recent_outputs.remove(0);
        }
        self.recent_inputs.push(actual_input);
        self.recent_outputs.push(actual_output);
    }

    fn p95_input(&self) -> u64 {
        percentile_95(&self.recent_inputs)
    }

    fn p95_output(&self) -> u64 {
        percentile_95(&self.recent_outputs)
    }
}

fn percentile_95(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() as f64 * 0.95) as usize).min(sorted.len() - 1);
    sorted[idx]
}

// ─── CallSizeEstimator ────────────────────────────────────────────────────────

/// Learns per-(kind, pass) token-cost distributions and estimates call size.
///
/// Thread-safe — uses an interior `RwLock`.
pub struct CallSizeEstimator {
    table: RwLock<HashMap<EstimatorKey, KindStats>>,
    /// Warm-start prior: 5 000 input / 800 output tokens.
    global_prior: KindStats,
}

impl CallSizeEstimator {
    pub fn new() -> Self {
        Self {
            table: RwLock::new(HashMap::new()),
            global_prior: KindStats::new(5_000.0, 800.0),
        }
    }

    /// Estimate total token cost for a single call.
    ///
    /// - input  = max(prompt.chars()/3.5, kind_mean_input)
    /// - output = min(max_tokens, kind_mean_output × 1.5)
    /// - once n ≥ 10: use max(mean × 1.2, p95) for large-tail protection
    pub fn estimate(&self, key: &EstimatorKey, prompt: &str, max_tokens: u32) -> u64 {
        let table = self.table.read().unwrap();
        let stats = table.get(key).unwrap_or(&self.global_prior);

        let prompt_tokens = (prompt.chars().count() as f64 / 3.5) as u64;

        let input_estimate = if stats.n >= 10 {
            let large_tail = ((stats.mean_input * 1.2) as u64).max(stats.p95_input());
            prompt_tokens.max(large_tail)
        } else {
            prompt_tokens.max(stats.mean_input as u64)
        };

        let output_estimate = if stats.n >= 10 {
            let large_tail = ((stats.mean_output * 1.2) as u64).max(stats.p95_output());
            large_tail.min(max_tokens as u64)
        } else {
            ((stats.mean_output * 1.5) as u64).min(max_tokens as u64)
        };

        input_estimate + output_estimate
    }

    /// Record actual token usage; updates per-key statistics via Welford update.
    pub fn observe(&self, key: &EstimatorKey, actual_input: u64, actual_output: u64) {
        let mut table = self.table.write().unwrap();
        let stats = table
            .entry(key.clone())
            .or_insert_with(|| self.global_prior.clone());
        stats.update(actual_input, actual_output);
    }

    /// Inflate mean estimates for `key` by 1.5× (called on 429).
    ///
    /// Makes the estimator more conservative so subsequent calls are less
    /// likely to over-commit against the remaining budget.
    pub fn inflate(&self, key: &EstimatorKey) {
        let mut table = self.table.write().unwrap();
        if let Some(stats) = table.get_mut(key) {
            stats.mean_input *= 1.5;
            stats.mean_output *= 1.5;
        } else {
            let mut stats = self.global_prior.clone();
            stats.mean_input *= 1.5;
            stats.mean_output *= 1.5;
            table.insert(key.clone(), stats);
        }
    }
}

impl Default for CallSizeEstimator {
    fn default() -> Self {
        Self::new()
    }
}

// ─── FamilyBudget ─────────────────────────────────────────────────────────────

/// Seconds for the cold-start window.
const COLD_START_WINDOW_SECS: u64 = 10;
/// Number of settled calls before the cold-start cap is lifted.
const COLD_START_SETTLED_THRESHOLD: u64 = 20;

struct FamilyBudget {
    tokens_limit: u64,
    /// Signed — local debits can briefly go negative.
    tokens_remaining: i64,
    tokens_reset: Instant,
    requests_limit: u32,
    requests_remaining: i64,
    requests_reset: Instant,
    /// Running sum of estimated tokens for all inflight calls.
    inflight_tokens_debit: u64,
    /// How many calls have settled (used for cold-start guard).
    cold_start_settled: u64,
    /// When this family's budget was first seen (for cold-start window).
    cold_start_start: Instant,
    /// Hard freeze set by `on_429`; Drop refunds do NOT clear this.
    /// Cleared only when a call successfully settles.
    frozen_until: Option<Instant>,
}

impl FamilyBudget {
    /// Uninitialised sentinel: tokens_limit = u64::MAX, first call passes through.
    fn uninit() -> Self {
        Self {
            tokens_limit: u64::MAX,
            tokens_remaining: i64::MAX,
            tokens_reset: Instant::now(),
            requests_limit: u32::MAX,
            requests_remaining: i64::MAX,
            requests_reset: Instant::now(),
            inflight_tokens_debit: 0,
            cold_start_settled: 0,
            cold_start_start: Instant::now(),
            frozen_until: None,
        }
    }

    fn is_uninit(&self) -> bool {
        self.tokens_limit == u64::MAX
    }

    /// True while we're in the cold-start window (first 10 s or <20 settled calls).
    fn is_cold_start(&self) -> bool {
        !self.is_uninit()
            && (self.cold_start_settled < COLD_START_SETTLED_THRESHOLD
                || self.cold_start_start.elapsed().as_secs() < COLD_START_WINDOW_SECS)
    }

    /// During cold start: cap at 25 % of token limit.
    fn effective_token_limit(&self) -> u64 {
        if self.is_cold_start() {
            self.tokens_limit / 4
        } else {
            self.tokens_limit
        }
    }

    /// Whether the budget has capacity for `estimated_tokens` right now.
    fn can_admit(&self, estimated_tokens: u64) -> bool {
        if self.is_uninit() {
            return true;
        }
        // Hard freeze from a 429 — refuse admission until the window expires.
        // This flag is set by on_429 and is NOT cleared by Drop refunds, only by settle.
        if let Some(frozen_until) = self.frozen_until
            && Instant::now() < frozen_until
        {
            return false;
        }
        let eff_limit = self.effective_token_limit();
        // During cold start, enforce an inflight-debit hard cap.
        if self.is_cold_start() && self.inflight_tokens_debit >= eff_limit {
            return false;
        }
        let net_remaining = self.tokens_remaining - self.inflight_tokens_debit as i64;
        net_remaining >= estimated_tokens as i64
    }

    fn time_until_reset(&self) -> Duration {
        let now = Instant::now();
        let token_wait = if self.tokens_reset > now {
            self.tokens_reset - now
        } else {
            Duration::from_millis(100)
        };
        let freeze_wait = self
            .frozen_until
            .map(|t| if t > now { t - now } else { Duration::ZERO })
            .unwrap_or(Duration::ZERO);
        token_wait.max(freeze_wait)
    }
}

// ─── BudgetState ─────────────────────────────────────────────────────────────

struct BudgetState {
    families: HashMap<ModelFamily, FamilyBudget>,
    inflight_requests: u32,
}

impl BudgetState {
    fn new() -> Self {
        Self {
            families: HashMap::new(),
            inflight_requests: 0,
        }
    }
}

// ─── TokenBudget ─────────────────────────────────────────────────────────────

/// Per-provider token-budget gate.
///
/// Cheap to clone — all mutable state is behind `Arc`s.
#[derive(Clone)]
pub struct TokenBudget {
    inner: Arc<Mutex<BudgetState>>,
    notify: Arc<Notify>,
    label: String,
    estimator: Arc<CallSizeEstimator>,
}

impl std::fmt::Debug for TokenBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenBudget")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl TokenBudget {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BudgetState::new())),
            notify: Arc::new(Notify::new()),
            label: label.into(),
            estimator: Arc::new(CallSizeEstimator::new()),
        }
    }

    pub fn estimator(&self) -> &Arc<CallSizeEstimator> {
        &self.estimator
    }

    /// True once the family has been seeded by a header snapshot.
    pub fn is_initialized(&self, family: &ModelFamily) -> bool {
        let state = self.inner.lock().unwrap();
        state
            .families
            .get(family)
            .map(|f| !f.is_uninit())
            .unwrap_or(false)
    }

    /// Acquire a budget slot for one call.
    ///
    /// Blocks until the estimated cost can be debited from the family
    /// budget.  Returns a [`BudgetReservation`] that must be either
    /// settled (on success) or dropped (on error).
    pub async fn reserve(
        &self,
        family: ModelFamily,
        key: EstimatorKey,
        prompt: &str,
        max_tokens: u32,
    ) -> BudgetReservation {
        let estimated_tokens = self.estimator.estimate(&key, prompt, max_tokens);

        loop {
            let wait_for: Option<Duration> = {
                let mut state = self.inner.lock().unwrap();
                let fb = state
                    .families
                    .entry(family)
                    .or_insert_with(FamilyBudget::uninit);

                if fb.can_admit(estimated_tokens) {
                    // Debit from tokens_remaining only for initialised families
                    // (uninit uses i64::MAX so the subtraction is safe, but we
                    // skip it to keep tokens_remaining meaningful after settle).
                    if !fb.is_uninit() {
                        fb.tokens_remaining -= estimated_tokens as i64;
                    }
                    fb.inflight_tokens_debit += estimated_tokens;
                    state.inflight_requests += 1;
                    None
                } else {
                    Some(fb.time_until_reset())
                }
            };

            match wait_for {
                None => {
                    return BudgetReservation {
                        budget: self.clone(),
                        debited_tokens: estimated_tokens,
                        family,
                        key,
                        consumed: false,
                    };
                }
                Some(delay) => {
                    let notify = Arc::clone(&self.notify);
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = notify.notified() => {}
                    }
                }
            }
        }
    }

    /// Handle a 429 response: inflate the estimator and freeze the family
    /// budget until `retry_after` has elapsed.
    pub fn on_429(&self, family: ModelFamily, retry_after: Duration, key: &EstimatorKey) {
        self.estimator.inflate(key);

        let mut state = self.inner.lock().unwrap();
        let fb = state
            .families
            .entry(family)
            .or_insert_with(FamilyBudget::uninit);

        fb.tokens_remaining = 0;
        fb.requests_remaining = 0;
        fb.tokens_reset = Instant::now() + retry_after;
        fb.requests_reset = Instant::now() + retry_after;
        // Hard freeze — not cleared by Drop refunds, only by a successful settle.
        fb.frozen_until = Some(Instant::now() + retry_after);
    }

    /// Label passed at construction (used in log messages).
    pub fn label(&self) -> &str {
        &self.label
    }
}

// ─── BudgetReservation ────────────────────────────────────────────────────────

/// RAII handle returned by [`TokenBudget::reserve`].
///
/// Call [`settle`](BudgetReservation::settle) on success to sync the
/// authoritative header snapshot back into the budget.  Simply drop on
/// error — the full estimated cost is refunded automatically.
pub struct BudgetReservation {
    budget: TokenBudget,
    debited_tokens: u64,
    family: ModelFamily,
    key: EstimatorKey,
    /// Prevents double-refund when `settle` has already consumed this.
    consumed: bool,
}

impl Drop for BudgetReservation {
    fn drop(&mut self) {
        if self.consumed {
            return;
        }
        // Refund the full estimate back into the budget.
        let mut state = self.budget.inner.lock().unwrap();
        if let Some(fb) = state.families.get_mut(&self.family) {
            fb.inflight_tokens_debit = fb.inflight_tokens_debit.saturating_sub(self.debited_tokens);
            if !fb.is_uninit() {
                fb.tokens_remaining += self.debited_tokens as i64;
            }
        }
        state.inflight_requests = state.inflight_requests.saturating_sub(1);
        drop(state);
        self.budget.notify.notify_waiters();
    }
}

impl BudgetReservation {
    /// Settle the reservation with actual usage and an authoritative header snapshot.
    ///
    /// 1. Updates the estimator with actual input/output token counts.
    /// 2. Syncs `tokens_remaining`, `tokens_limit`, `requests_remaining`,
    ///    `requests_limit`, and reset instants from the header snapshot.
    /// 3. Refunds the inflight debit (estimated minus zero — headers are authoritative).
    /// 4. Notifies waiters blocked in [`TokenBudget::reserve`].
    pub fn settle(mut self, actual_input: u64, actual_output: u64, snap: RateLimitSnapshot) {
        // Mark consumed first so Drop skips the refund.
        self.consumed = true;

        let budget = &self.budget;
        budget
            .estimator
            .observe(&self.key, actual_input, actual_output);

        let mut state = budget.inner.lock().unwrap();
        if let Some(fb) = state.families.get_mut(&self.family) {
            // Refund inflight debit — headers now carry the authoritative count.
            fb.inflight_tokens_debit = fb.inflight_tokens_debit.saturating_sub(self.debited_tokens);

            // Sync from authoritative headers.
            fb.tokens_remaining = snap.tokens_remaining as i64;
            fb.tokens_limit = snap.tokens_limit;
            fb.requests_remaining = snap.requests_remaining as i64;
            fb.requests_limit = snap.requests_limit;

            if let Some(reset_dt) = snap.tokens_reset {
                let delta = reset_dt - chrono::Utc::now();
                if let Ok(std_delta) = delta.to_std() {
                    fb.tokens_reset = Instant::now() + std_delta;
                }
            }
            if let Some(reset_dt) = snap.requests_reset {
                let delta = reset_dt - chrono::Utc::now();
                if let Ok(std_delta) = delta.to_std() {
                    fb.requests_reset = Instant::now() + std_delta;
                }
            }

            fb.cold_start_settled += 1;
            // A successful call clears any hard freeze — we have confirmed capacity.
            fb.frozen_until = None;
        }
        state.inflight_requests = state.inflight_requests.saturating_sub(1);

        drop(state);
        budget.notify.notify_waiters();
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn key() -> EstimatorKey {
        EstimatorKey::new("function", "purpose")
    }

    fn tiny_snap(tokens_remaining: u64, tokens_limit: u64) -> RateLimitSnapshot {
        RateLimitSnapshot {
            requests_limit: 1000,
            requests_remaining: 500,
            tokens_limit,
            tokens_remaining,
            requests_reset: None,
            tokens_reset: None,
        }
    }

    // ── reservation_drop_without_settle_refunds_full_estimate ────────────────

    #[tokio::test]
    async fn reservation_drop_without_settle_refunds_full_estimate() {
        let budget = TokenBudget::new("test");

        // Seed the budget with a known limit so can_admit has real numbers.
        {
            let reservation = budget
                .reserve(ModelFamily::Sonnet, key(), "hello world", 100)
                .await;
            reservation.settle(10, 20, tiny_snap(100_000, 200_000));
        }

        // Now reserve again — this debit should be fully refunded on drop.
        let before = {
            let state = budget.inner.lock().unwrap();
            state
                .families
                .get(&ModelFamily::Sonnet)
                .map(|f| f.tokens_remaining)
                .unwrap_or(0)
        };

        let reservation = budget
            .reserve(ModelFamily::Sonnet, key(), "another call", 500)
            .await;
        let debited = reservation.debited_tokens;

        // Drop without settling — should refund.
        drop(reservation);

        let after = {
            let state = budget.inner.lock().unwrap();
            state
                .families
                .get(&ModelFamily::Sonnet)
                .map(|f| f.tokens_remaining)
                .unwrap_or(0)
        };

        assert_eq!(
            after, before,
            "tokens_remaining should be restored after drop; debited={debited}"
        );
    }

    // ── reserve_blocks_until_reset_when_budget_exhausted ────────────────────

    #[tokio::test]
    async fn reserve_blocks_until_reset_when_budget_exhausted() {
        let budget = TokenBudget::new("test");

        // Seed with a tiny budget (100 tokens) and short reset.
        {
            let r = budget.reserve(ModelFamily::Sonnet, key(), "seed", 1).await;
            r.settle(1, 1, tiny_snap(100, 100));
        }

        // Manually drain the remaining budget.
        {
            let mut state = budget.inner.lock().unwrap();
            let fb = state.families.get_mut(&ModelFamily::Sonnet).unwrap();
            fb.tokens_remaining = 0;
            // Set reset 150 ms from now.
            fb.tokens_reset = Instant::now() + Duration::from_millis(150);
        }

        let start = std::time::Instant::now();

        // This reserve should block until the notify fires after we refill the budget.
        let budget_clone = budget.clone();
        let handle = tokio::spawn(async move {
            budget_clone
                .reserve(ModelFamily::Sonnet, key(), "blocked call", 10)
                .await
        });

        // Give the spawned task a moment to start and block.
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Refill the budget and notify waiters.
        {
            let mut state = budget.inner.lock().unwrap();
            let fb = state.families.get_mut(&ModelFamily::Sonnet).unwrap();
            fb.tokens_remaining = 10_000;
        }
        budget.notify.notify_waiters();

        let reservation = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("reserve should unblock within 2 s")
            .expect("task should not panic");

        // It should have blocked for at least ~20 ms.
        assert!(
            start.elapsed() >= Duration::from_millis(15),
            "reserve should have blocked"
        );
        drop(reservation);
    }

    // ── settle_observes_actual_and_snaps_remaining ───────────────────────────

    #[tokio::test]
    async fn settle_observes_actual_and_snaps_remaining() {
        let budget = TokenBudget::new("test");

        let reservation = budget
            .reserve(ModelFamily::Haiku, key(), "some prompt text here", 200)
            .await;

        let snap = tiny_snap(87_654, 200_000);
        reservation.settle(1_234, 567, snap);

        // Family budget should now reflect the header snapshot.
        let state = budget.inner.lock().unwrap();
        let fb = state.families.get(&ModelFamily::Haiku).unwrap();
        assert_eq!(fb.tokens_remaining, 87_654_i64);
        assert_eq!(fb.tokens_limit, 200_000);

        // Estimator should have been updated.
        drop(state);
        let estimate_after = budget.estimator.estimate(&key(), "hello", 100);
        // After one observation (1234 input, 567 output), estimate should be
        // informed by the actual values (Welford mean after 1 sample = actual value).
        // Global prior was 5000/800; after 1 observe the n=1 mean == actual.
        // estimate: input=max(prompt_tokens, mean_input), output=min(max_tokens, mean_output*1.5)
        // With n<10: mean_input=1234, mean_output=567
        // prompt_tokens("hello") = 5/3.5 ≈ 1
        // input = max(1, 1234) = 1234
        // output = min(100, 567*1.5) = min(100, 850) = 100
        assert_eq!(estimate_after, 1234 + 100);
    }

    // ── on_429_makes_estimator_conservative ─────────────────────────────────

    #[tokio::test]
    async fn on_429_makes_estimator_conservative() {
        let budget = TokenBudget::new("test");

        // Seed the estimator with some observations.
        let k = key();
        budget.estimator.observe(&k, 1_000, 300);
        budget.estimator.observe(&k, 1_000, 300);

        // Estimate before 429.
        let before = budget.estimator.estimate(&k, "hi", 500);

        // Simulate a 429.
        budget.on_429(ModelFamily::Sonnet, Duration::from_millis(100), &k);

        // Estimate after 429 — should be larger (more conservative).
        let after = budget.estimator.estimate(&k, "hi", 500);

        assert!(
            after > before,
            "estimate after 429 ({after}) should exceed estimate before ({before})"
        );

        // Budget should be frozen.
        let state = budget.inner.lock().unwrap();
        let fb = state.families.get(&ModelFamily::Sonnet).unwrap();
        assert_eq!(fb.tokens_remaining, 0);
        assert_eq!(fb.requests_remaining, 0);
    }

    // ── cold_start_caps_admission_to_25_percent ──────────────────────────────

    #[tokio::test]
    async fn cold_start_caps_admission_to_25_percent() {
        let budget = TokenBudget::new("test");

        // Seed the family with a 100 000-token limit.
        {
            let r = budget.reserve(ModelFamily::Opus, key(), "seed", 1).await;
            r.settle(1, 1, tiny_snap(100_000, 100_000));
        }

        // We're now in cold start (0 settled calls so far after seed).
        // Effective limit = 100_000 / 4 = 25_000.

        // Drain the inflight debit to 24 990.
        {
            let mut state = budget.inner.lock().unwrap();
            let fb = state.families.get_mut(&ModelFamily::Opus).unwrap();
            fb.inflight_tokens_debit = 24_990;
            // Keep tokens_remaining high so the regular check doesn't block us.
            fb.tokens_remaining = 100_000;
            // Ensure we're in cold start (reset settled counter).
            fb.cold_start_settled = 1;
        }

        // A small reservation (10 tokens) should pass (24990 + 10 = 25000 == limit).
        let small_estimate = budget.estimator.estimate(&key(), "hi", 10);
        // Override estimate table so we know exactly 10 tokens will be requested.
        // We'll do this by directly checking can_admit logic; instead let's just
        // verify that after filling to 25000 the next call gets blocked.

        // Fill inflight right up to the cold-start cap.
        {
            let mut state = budget.inner.lock().unwrap();
            let fb = state.families.get_mut(&ModelFamily::Opus).unwrap();
            fb.inflight_tokens_debit = 25_000; // exactly at cold-start cap
        }

        // Next reserve should block since inflight >= effective_limit (25 000).
        let budget_clone = budget.clone();
        let blocked = tokio::time::timeout(
            Duration::from_millis(50),
            budget_clone.reserve(
                ModelFamily::Opus,
                EstimatorKey::new("fn", "purpose"),
                "next call",
                100,
            ),
        )
        .await;

        assert!(
            blocked.is_err(),
            "reserve should time out while cold-start cap is saturated (inflight={small_estimate})"
        );
    }

    // ── on_429_freeze_survives_concurrent_drop_refunds ──────────────────────
    //
    // Regression for the thundering-herd bug: N concurrent reservations all
    // receive a 429 simultaneously.  After on_429() zeros tokens_remaining,
    // all N Drops refund their estimates back in.  Without frozen_until the
    // budget looks full again and admits the next wave immediately.

    #[tokio::test]
    async fn on_429_freeze_survives_concurrent_drop_refunds() {
        let budget = TokenBudget::new("test");

        // Seed with a known limit.
        {
            let r = budget.reserve(ModelFamily::Haiku, key(), "seed", 1).await;
            r.settle(1, 1, tiny_snap(200_000, 200_000));
        }

        // Grab 5 concurrent reservations (simulating 5 inflight calls).
        let mut reservations = Vec::new();
        for _ in 0..5 {
            let r = budget
                .reserve(ModelFamily::Haiku, key(), "concurrent call", 500)
                .await;
            reservations.push(r);
        }

        // Simulate all 5 hitting a 429.
        let retry = Duration::from_millis(500);
        for r in &reservations {
            budget.on_429(ModelFamily::Haiku, retry, &r.key);
        }

        // Drop all 5 reservations (refunds their estimates back into tokens_remaining).
        drop(reservations);

        // The budget should still be frozen — reserve must NOT admit immediately.
        let budget_clone = budget.clone();
        let blocked = tokio::time::timeout(
            Duration::from_millis(50),
            budget_clone.reserve(ModelFamily::Haiku, key(), "post-429 call", 100),
        )
        .await;

        assert!(
            blocked.is_err(),
            "budget should remain frozen after concurrent Drop refunds"
        );
    }

    // ── model_family_of classification ──────────────────────────────────────

    #[test]
    fn model_family_classification() {
        assert_eq!(
            model_family_of("claude-haiku-4-5-20251001"),
            ModelFamily::Haiku
        );
        assert_eq!(model_family_of("claude-sonnet-4-5"), ModelFamily::Sonnet);
        assert_eq!(model_family_of("claude-opus-4-20250514"), ModelFamily::Opus);
        assert_eq!(model_family_of("unknown-model"), ModelFamily::Sonnet);
    }

    // ── estimator basic correctness ──────────────────────────────────────────

    #[test]
    fn estimator_uses_global_prior_before_observations() {
        let est = CallSizeEstimator::new();
        let k = EstimatorKey::new("fn", "purpose");
        // Prior: 5000 input / 800 output.
        // prompt "hi" → 2/3.5 ≈ 0 tokens
        // input = max(0, 5000) = 5000
        // output = min(2000, 800 * 1.5) = min(2000, 1200) = 1200
        let est_val = est.estimate(&k, "hi", 2000);
        assert_eq!(est_val, 5000 + 1200);
    }

    #[test]
    fn estimator_inflate_increases_estimate() {
        let est = CallSizeEstimator::new();
        let k = EstimatorKey::new("fn", "purpose");
        let before = est.estimate(&k, "hi", 2000);
        est.inflate(&k);
        let after = est.estimate(&k, "hi", 2000);
        assert!(after > before, "inflate should increase estimate");
    }
}
